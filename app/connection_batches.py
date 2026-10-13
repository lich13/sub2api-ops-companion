"""Account-pinned connection probes accepted as one durable user intent."""
from __future__ import annotations

import json
import re
import time
import uuid
from datetime import datetime, timezone

from fastapi import HTTPException

from .bark import sanitize_error_text
from .operation_versions import digest, versions


def stamp():
    return datetime.now(timezone.utc).isoformat()


class ConnectionBatches:
    def __init__(self, queue):
        self.q = queue
        self.s = queue.s

    @staticmethod
    def handles(job):
        return job.get('action') == 'test' and bool(job.get('batch_id'))

    def has_slot(self, jobs):
        raw = getattr(self.s.r.settings, 'oauth_recovery_test_concurrency', 2)
        limit = max(1, min(8, raw if isinstance(raw, int) and not isinstance(raw, bool) else 2))
        return sum(self.handles(jobs.get(jid, {})) for jid in self.q.tasks) < limit

    async def submit(self, request, key):
        if not request.payload.confirmed:
            raise HTTPException(422, '请先确认连接测试')
        ids = [item.account_id for item in request.accounts]
        if len(ids) != len(set(ids)):
            raise HTTPException(422, '批量账号不能重复')
        signature = digest({**request.model_dump(exclude={'request_id'}),
                            'accounts': sorted([item.model_dump() for item in request.accounts],
                                               key=lambda item: item['account_id'])})
        async with self.q.submit_lock:
            data = self.q.store.read()
            if request.request_id in data['requests']:
                raise HTTPException(409, {'code': 'idempotency_conflict', 'message': '请求 ID 已用于其他操作'})
            previous = data['batch_requests'].get(request.request_id)
            if previous:
                batch = data['batches'][previous]
                if batch['request_hash'] != signature:
                    raise HTTPException(409, {'code': 'idempotency_conflict', 'message': '请求 ID 已用于其他批量操作'})
                for jid in batch['job_ids']:
                    self.q.keys[jid] = key
                return self.q.listing(previous)
            model = str(getattr(self.s.r.settings, 'oauth_recovery_test_model_id', '') or 'gpt-5.6-luna').strip()
            if not re.fullmatch(r'[A-Za-z0-9._:/-]{1,200}', model):
                raise HTTPException(422, '测活模型配置无效')
            rows = await self.s.actions.accounts(ids)
            bid, created = uuid.uuid4().hex, stamp()
            jobs = []
            for item in request.accounts:
                row = rows.get(item.account_id) or {}
                grok = row.get('platform') == 'grok'
                payload = {'confirmed': True, 'mode': 'text' if grok else 'default',
                           'model_id': '' if grok else model}
                job = {'id': uuid.uuid4().hex, 'batch_id': bid, 'account_id': item.account_id,
                       'account_name': sanitize_error_text(str(row.get('name') or f'#{item.account_id}'), limit=200),
                       'action': 'test', 'payload': payload, 'expected_version': item.expected_version,
                       'client_id': request.client_id, 'request_id': uuid.uuid4().hex,
                       'request_hash': digest([payload, item.expected_version]),
                       'parent_account_id': row.get('parent_account_id'), 'created_at': created,
                       'updated_at': created, 'status': 'queued', 'reason': '等待执行', 'result': None}
                if not row or row.get('deleted_at'):
                    job.update(status='failed', reason='账号不存在或已删除')
                elif row.get('platform') not in {'openai', 'grok'} or row.get('type') not in {'oauth', 'apikey'}:
                    job.update(status='failed', reason='此账号不支持连接测试')
                elif versions(row)['test'] != item.expected_version:
                    job.update(status='needs_confirmation', reason='账号凭据、代理或模型配置已变化，请刷新后重新选择')
                jobs.append(job)
            with self.q.store.transaction() as latest:
                latest['batches'][bid] = {'id': bid, 'action': 'test', 'request_hash': signature,
                    'client_id': request.client_id, 'request_id': request.request_id,
                    'created_at': created, 'job_ids': [job['id'] for job in jobs]}
                latest['batch_requests'][request.request_id] = bid
                for job in jobs:
                    latest['jobs'][job['id']] = job
                    latest['requests'][job['request_id']] = job['id']
            for job in jobs:
                self.q.keys[job['id']] = key
                self.q.audit(job, 'accepted')
            return self.q.listing(bid)

    async def cancel(self, bid):
        if not re.fullmatch(r'[a-f0-9]{32}', bid):
            raise HTTPException(404, '批量测试不存在')
        async with self.q.submit_lock:
            with self.q.store.transaction() as data:
                batch = data['batches'].get(bid)
                if not batch or batch.get('action') != 'test':
                    raise HTTPException(404, '批量测试不存在')
                for jid in batch['job_ids']:
                    job = data['jobs'][jid]
                    if job['status'] in {'queued', 'needs_confirmation'}:
                        # A needs-confirmation item may already have been dispatched.
                        if job.get('started_at'):
                            continue
                        job.update(status='cancelled', reason='已停止，未派发测试', updated_at=stamp())
        return self.q.listing(bid)

    async def dispatch(self, row, job, key):
        from .desktop_actions import TestRequest
        from .desktop_api import account_dto
        request = TestRequest(**job['payload'], expected_version=account_dto(
            row, datetime.now(timezone.utc), self.q.managed())['version'])
        locks = await self.s.actions.prepare_test(row['id'], request)
        started = time.monotonic()
        result = {'requested_model': request.model_id, 'actual_model': None, 'success': False}
        terminal = False
        stream = self.s.actions.test_stream(row['id'], request, key, locks, summary=True)
        try:
            async for chunk in stream:
                event = json.loads(chunk.removeprefix('data:').strip())
                if event.get('type') == 'test_start' and isinstance(event.get('model'), str):
                    result['actual_model'] = sanitize_error_text(event['model'], limit=200)
                elif event.get('type') == 'test_complete':
                    terminal = True
                    result['success'] = event.get('success') is True
                    if not result['success']:
                        result['error'] = '上游未确认测试通过'
                    break
                elif event.get('type') == 'error':
                    terminal = True
                    result.update(error=sanitize_error_text(str(event.get('error') or '测试失败'), limit=240))
                    if event.get('uncertain'):
                        result['uncertain'] = True
                    break
        finally:
            await stream.aclose()
        if not terminal:
            result.update(uncertain=True, error='连接结束，未收到测试完成结果；未重放请求')
        result.update(duration_ms=round((time.monotonic() - started) * 1000), completed_at=stamp())
        return result
