"""Durable user intents. Credentials, model output and authorization keys stay in memory."""
from __future__ import annotations

import asyncio
import copy
import fcntl
import json
import re
import threading
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Literal

from fastapi import HTTPException
from pydantic import BaseModel, ConfigDict, Field, ValidationError

from .atomic_config import write_json
from .audit import write_audit
from .operation_versions import versions, digest

PENDING = {'queued', 'running', 'checking', 'needs_confirmation'}
SHORT = {'schedulable', 'degradation_mark'}
_LOCK = threading.RLock()


def stamp():
    return datetime.now(timezone.utc).isoformat()


def busy():
    return HTTPException(409, {'code': 'account_busy', 'message': '等待账号空闲'})


class OperationRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    action: Literal['priority', 'groups', 'recover', 'usage', 'reset_quota', 'delete', 'test', 'schedulable', 'degradation_mark', 'account_template']
    request_id: str = Field(min_length=16, max_length=64, pattern=r'^[A-Za-z0-9-]+$')
    client_id: str = Field(min_length=16, max_length=64, pattern=r'^[A-Za-z0-9-]+$')
    expected_version: str = Field(min_length=64, max_length=64)
    payload: dict = Field(default_factory=dict)


class OperationStore:
    def __init__(self, path):
        self.path = Path(path)
        self.existed = self.path.exists() or self.path.with_suffix('.lock').exists()

    def read(self):
        try:
            data = json.loads(self.path.read_text())
        except FileNotFoundError:
            if self.existed:
                raise ValueError('操作状态文件丢失') from None
            return {'version': 1, 'jobs': {}, 'requests': {}, 'batches': {}, 'batch_requests': {}}
        if (not isinstance(data, dict) or data.get('version') != 1
                or not isinstance(data.get('jobs'), dict) or not isinstance(data.get('requests'), dict)):
            raise ValueError('操作状态无法读取')
        for job in data['jobs'].values():
            if (not isinstance(job, dict) or not isinstance(job.get('account_id'), int)
                    or not isinstance(job.get('payload'), dict) or not isinstance(job.get('status'), str)):
                raise ValueError('操作任务无法读取')
        for field in ('batches', 'batch_requests'):
            if not isinstance(data.setdefault(field, {}), dict):
                raise ValueError('批量操作状态无法读取')
        for bid, batch in data['batches'].items():
            if (not isinstance(batch, dict) or not isinstance(batch.get('job_ids'), list)
                    or any(jid not in data['jobs'] or data['jobs'][jid].get('batch_id') != bid for jid in batch['job_ids'])):
                raise ValueError('批量操作任务无法读取')
        if any(bid not in data['batches'] for bid in data['batch_requests'].values()):
            raise ValueError('批量操作索引无法读取')
        self.existed = True
        return data

    @contextmanager
    def transaction(self):
        with _LOCK:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            with self.path.with_suffix('.lock').open('a+') as handle:
                Path(handle.name).chmod(0o600)
                fcntl.flock(handle, fcntl.LOCK_EX)
                try:
                    data = self.read()
                    yield data
                    write_json(self.path, data)
                    self.existed = True
                finally:
                    fcntl.flock(handle, fcntl.LOCK_UN)


class AccountOperations:
    def __init__(self, service):
        self.s = service
        self.store = OperationStore(Path(service.r.settings.usage_query_state_path).with_name('account-operations.json'))
        self.tasks = {}
        self.keys = {}
        self.events = {}
        self.closing = False
        self.submit_lock = asyncio.Lock()
        with self.store.transaction() as data:
            for job in data['jobs'].values():
                if job['status'] == 'running':
                    job.update(status='checking', reason='服务重启，核对已派发请求')

    def update(self, jid, **changes):
        with self.store.transaction() as data:
            job = data['jobs'][jid]
            job.update(changes, updated_at=stamp())
            if job['status'] not in PENDING:
                # Uploaded test inputs are needed only while a request is pending.
                for name in ('prompt', 'image_data_url', 'audio_data_url'):
                    job['payload'].pop(name, None)
            result = copy.deepcopy(job)
        return result

    def public(self, job, *, detail=False, after_event=0):
        result = {k: copy.deepcopy(v) for k, v in job.items()
                  if k not in {'client_id', 'request_id', 'request_hash', 'expected_version', 'payload', 'template_target'}}
        result['requested'] = {k: v for k, v in job['payload'].items()
                               if k not in {'prompt', 'image_data_url', 'audio_data_url', 'expected_version'}}
        if detail and job['action'] == 'test':
            events = self.events.get(job['id'], [])
            result['events'] = events[after_event:after_event + 100]
            result['next_event'] = min(len(events), after_event + 100)
        return result

    def get(self, jid, after_event=0):
        if not re.fullmatch('[a-f0-9]{32}', jid):
            raise HTTPException(404, '操作不存在')
        job = self.store.read()['jobs'].get(jid)
        if not job:
            models = getattr(self.s, '_model_tests', None)
            if models:
                return self.model_view(models.get(jid))
            raise HTTPException(404, '操作不存在')
        return self.public(job, detail=True, after_event=after_event)

    def listing(self, batch_id=None):
        data = self.store.read()
        if batch_id is not None:
            if not re.fullmatch('[a-f0-9]{32}', batch_id) or batch_id not in data['batches']:
                raise HTTPException(404, '批量操作不存在')
            batch = data['batches'][batch_id]
            items = [self.public(data['jobs'][jid]) for jid in batch['job_ids']]
            return {'batch_id': batch_id, 'items': items, 'pending': sum(j['status'] in PENDING for j in items)}
        jobs = sorted(data['jobs'].values(), key=lambda j: j['created_at'], reverse=True)
        active = [j for j in jobs if j['status'] in PENDING]
        recent = [j for j in jobs if j['status'] not in PENDING][:30]
        items = [self.public(j) for j in active + recent]
        models = getattr(self.s, '_model_tests', None)
        if models:
            for job in models.store.read()['jobs'].values():
                if job['status'] in {'queued', 'running', 'retrying', 'needs_confirmation'}:
                    items.append(self.model_view(job))
        return {'items': items, 'pending': sum(j['status'] in PENDING for j in items)}

    async def submit_template_batch(self, request, key):
        from .account_templates import eligible, combine, account_version
        ids = [item.account_id for item in request.accounts]
        if len(set(ids)) != len(ids):
            raise HTTPException(422, '批量账号不能重复')
        signature = digest({**request.model_dump(exclude={'request_id'}),
                            'accounts': sorted([item.model_dump() for item in request.accounts], key=lambda x: x['account_id'])})
        async with self.submit_lock:
            data = self.store.read()
            existing = data['batch_requests'].get(request.request_id)
            if existing:
                batch = data['batches'][existing]
                if batch['request_hash'] != signature:
                    raise HTTPException(409, {'code': 'idempotency_conflict', 'message': '请求 ID 已用于其他批量操作'})
                for jid in batch['job_ids']:
                    self.keys[jid] = key
                return self.listing(existing)
            # Freeze only this template. Changes to other templates or its name
            # must not invalidate the queued account intents.
            target = self.s.account_templates.desired(request)
            config = self.s.account_templates.view()
            selected = config['templates'].get(request.template_id)
            if selected is None or combine(selected) != target:
                raise HTTPException(409, '模板内容已变化，请重新预览')
            content_version = config['template_versions'][request.template_id]
            bid, created = uuid.uuid4().hex, stamp()
            jobs = []
            for item in request.accounts:
                row = await asyncio.to_thread(self.s.account_templates.account, item.account_id)
                jid = uuid.uuid4().hex
                payload = {'template_id': request.template_id, 'template_version': content_version}
                job = {'id': jid, 'batch_id': bid, 'account_id': item.account_id,
                       'account_name': str((row or {}).get('name') or f'#{item.account_id}'),
                       'action': 'account_template', 'payload': payload, 'template_target': target,
                       'expected_version': item.expected_version, 'client_id': request.client_id,
                       'request_id': uuid.uuid4().hex, 'request_hash': digest([payload, item.expected_version]),
                       'parent_account_id': (row or {}).get('parent_account_id'), 'created_at': created,
                       'updated_at': created, 'status': 'queued', 'reason': '等待执行', 'result': None}
                if not eligible(row):
                    job.update(status='failed', reason='仅可应用到未删除的独立 OpenAI OAuth 或 Key 账号')
                elif row.get('passthrough'):
                    job.update(status='needs_confirmation', reason='透传模式会绕过模型限制，请先处理透传设置')
                elif (row.get('model_mapping') or {}) == target:
                    job.update(status='completed', reason='', result={'verified': True, 'unchanged': True})
                elif account_version(row) != item.expected_version:
                    job.update(status='needs_confirmation', reason='账号模型配置或凭据已变化，请重新预览')
                jobs.append(job)
            with self.store.transaction() as latest:
                latest['batches'][bid] = {'id': bid, 'request_hash': signature, 'client_id': request.client_id,
                    'request_id': request.request_id, 'created_at': created, 'job_ids': [j['id'] for j in jobs]}
                latest['batch_requests'][request.request_id] = bid
                for job in jobs:
                    latest['jobs'][job['id']] = job
                    latest['requests'][job['request_id']] = job['id']
            for job in jobs:
                self.keys[job['id']] = key
                self.audit(job, 'accepted')
            return self.listing(bid)

    @staticmethod
    def model_view(job):
        return {'id': job['id'], 'account_id': job['account_id'], 'account_name': job['account_name'],
                'action': 'model_test', 'status': 'running' if job['status'] == 'retrying' else job['status'],
                'reason': job.get('error', ''), 'requested': {'model_id': job['requested_model'], 'concurrency': job['concurrency']}}

    def managed(self):
        controller = self.s.r.key_fallback_controller
        if not controller:
            return set()
        config = controller.load_config()
        if not config.valid:
            raise HTTPException(503, '托管配置无法读取')
        return set(config.managed_account_ids)

    def fingerprint(self, row, job):
        if job['action'] == 'account_template':
            return self.s.account_templates.fingerprint(row['id'])
        if job['action'] == 'degradation_mark':
            from .capacity_alerts import mark_view
            state = self.s.r.capacity_alerts.store.snapshot()
            return mark_view(row['id'], state['marks'].get(str(row['id'])))['version']
        return versions(row, row['id'] in self.managed())[job['action']]

    @staticmethod
    def validate_payload(action, payload):
        from .desktop_actions import PriorityRequest, GroupsRequest, TestRequest, DegradationMarkRequest
        from .desktop_api import ScheduleRequest, DeleteRequest, AccountVersionRequest, UsageActionRequest
        from .account_templates import TemplateApplication
        models = {'account_template': TemplateApplication, 'priority': PriorityRequest, 'groups': GroupsRequest, 'test': TestRequest,
                  'schedulable': ScheduleRequest, 'delete': DeleteRequest, 'recover': AccountVersionRequest,
                  'usage': UsageActionRequest, 'reset_quota': UsageActionRequest, 'degradation_mark': DegradationMarkRequest}
        value = dict(payload)
        value['expected_mark_version' if action == 'degradation_mark' else 'expected_version'] = '0' * 64
        parsed = models[action].model_validate(value)
        if action == 'reset_quota' and parsed.action != 'reset_quota':
            raise HTTPException(422, '用卡请求无效')
        if action == 'usage' and parsed.action == 'reset_quota':
            raise HTTPException(422, '用卡需要独立确认')
        if action in {'test', 'reset_quota', 'delete'} and not payload.get('confirmed') and action != 'delete':
            raise HTTPException(422, '请先确认该操作')
        return parsed.model_dump(exclude={'expected_version', 'expected_mark_version'})

    async def submit(self, aid, request, key):
        if aid < 1:
            raise HTTPException(422, '账号编号无效')
        try:
            payload = self.validate_payload(request.action, request.payload)
        except ValidationError:
            raise HTTPException(422, '操作参数无效') from None
        async with self.submit_lock:
            data = self.store.read()
            jid = data['requests'].get(request.request_id)
            if jid:
                old = data['jobs'][jid]
                if (old['account_id'], old['action'], old['client_id']) != (aid, request.action, request.client_id) or old.get('request_hash') != digest([request.action, payload, request.expected_version]):
                    raise HTTPException(409, {'code': 'idempotency_conflict', 'message': '请求 ID 已用于其他操作'})
                self.keys[jid] = key
                return self.public(old)
            row = await self.s.actions.account(aid)
            jid = uuid.uuid4().hex
            job = {'id': jid, 'account_id': aid, 'account_name': row['name'], 'action': request.action,
                   'payload': payload, 'expected_version': request.expected_version, 'client_id': request.client_id,
                   'request_id': request.request_id, 'request_hash': digest([request.action, payload, request.expected_version]),
                   'parent_account_id': row.get('parent_account_id'), 'created_at': stamp(), 'updated_at': stamp(),
                   'status': 'queued', 'reason': '等待执行', 'result': None}
            if request.action == 'account_template':
                try:
                    job['template_target'] = self.s.account_templates.desired(payload)
                except HTTPException as exc:
                    if exc.status_code != 409:
                        raise
                    job.update(status='needs_confirmation', reason=str(exc.detail))
            actual = self.fingerprint(row, job)
            if actual != request.expected_version:
                job.update(status='needs_confirmation', reason='相关字段已变化，请核对后重新提交', current=self.current(row, job))
            with self.store.transaction() as data:
                for old in data['jobs'].values():
                    if (old['account_id'] == aid and old['client_id'] == request.client_id
                            and old['action'] == request.action and old['status'] in {'queued', 'needs_confirmation'}
                            and request.action in {'priority', 'groups', 'schedulable', 'degradation_mark'}):
                        old.update(status='superseded', reason='已由本客户端的新请求替代', updated_at=stamp())
                data['jobs'][jid] = job
                data['requests'][request.request_id] = jid
            self.keys[jid] = key
            self.audit(job, 'accepted')
            if request.action in SHORT and job['status'] == 'queued':
                self.launch(job)
            return self.public(job)

    @staticmethod
    def current(row, job):
        action = job['action']
        if action in {'groups', 'priority', 'schedulable'}:
            field = 'group_ids' if action == 'groups' else action
            return {field: row.get(field)}
        return {k: row.get(k) for k in ('id', 'name', 'platform', 'type', 'status')}

    def achieved(self, row, job):
        p, action = job['payload'], job['action']
        if action == 'delete':
            return row is None
        if not row:
            return False
        if action in {'priority', 'schedulable'}:
            return row.get(action) == p[action] and (action != 'schedulable' or row['id'] not in self.managed())
        if action == 'groups':
            return set(row.get('group_ids') or []) & set(p['scope_group_ids']) == set(p['group_ids'])
        if action == 'recover':
            from .desktop_api import recoverable_state
            return not recoverable_state(row, datetime.now(timezone.utc))
        if action == 'account_template':
            if job['status'] == 'checking' and isinstance(job.get('template_target'), dict):
                from .account_templates import eligible
                live = self.s.account_templates.account(job['account_id'])
                return bool(eligible(live) and not live.get('passthrough')
                            and (live.get('model_mapping') or {}) == job['template_target'])
            return self.s.account_templates.achieved(row['id'], p)
        if action == 'degradation_mark':
            state = self.s.r.capacity_alerts.store.snapshot()
            return bool(state['marks'].get(str(row['id']), {}).get('marked')) == p['marked']
        return False

    def audit(self, job, result):
        write_audit(self.s.r.settings.audit_path, 'desktop_operation',
                    {'operation_id': job['id'], 'account_id': job['account_id'], 'action': job['action'], 'result': result})

    async def read_account(self, aid):
        try:
            return await self.s.actions.account(aid)
        except HTTPException as exc:
            if exc.status_code == 404:
                return None
            raise

    def launch(self, job):
        task = asyncio.create_task(self.run(job['id']))
        self.tasks[job['id']] = task
        def finished(done):
            self.tasks.pop(job['id'], None)
            if not done.cancelled():
                done.exception()
        task.add_done_callback(finished)

    async def loop(self):
        while True:
            try:
                jobs = self.store.read()['jobs']
                occupied = {jobs[jid]['account_id'] for jid in self.tasks if jid in jobs and jobs[jid]['action'] not in SHORT}
                for job in jobs.values():
                    if job['status'] not in {'queued', 'checking'} or job['id'] in self.tasks:
                        continue
                    short = job['action'] in SHORT
                    if not short and job['status'] == 'queued':
                        from .account_locks import AccountLease
                        lease = AccountLease(self.s.r.db, {'id': job['account_id'], 'parent_account_id': job.get('parent_account_id')})
                        if not lease.acquire():
                            continue
                        lease.release()
                    if short or (len(self.tasks) < 8 and job['account_id'] not in occupied):
                        self.launch(job)
                        if not short:
                            occupied.add(job['account_id'])
            except (OSError, ValueError):
                pass  # No reliable state means no dispatch.
            await asyncio.sleep(.25)

    async def run(self, jid):
        job = self.store.read()['jobs'][jid]
        if job['status'] not in {'queued', 'checking'}:
            return
        try:
            row = await self.read_account(job['account_id'])
            if self.achieved(row, job):
                from .desktop_api import account_dto
                result = {'verified': True, **(self.current(row, job) if row else {'deleted': True})}
                if row and job['action'] == 'degradation_mark':
                    from .capacity_alerts import mark_view
                    result['degradation_mark'] = mark_view(row['id'], self.s.r.capacity_alerts.store.snapshot()['marks'].get(str(row['id'])))
                if row:
                    result['version'] = account_dto(row, datetime.now(timezone.utc), self.managed())['version']
                self.update(jid, status='completed', reason='', result=result)
                return
            if job['status'] == 'checking':
                self.update(jid, status='needs_confirmation', reason='已派发请求的结果未确认，未重复执行', current=self.current(row or {}, job))
                return
            if not row or self.fingerprint(row, job) != job['expected_version']:
                self.update(jid, status='needs_confirmation', reason='相关字段、凭据或资格已变化', current=self.current(row or {}, job))
                return
            key = self.keys.get(jid) or self.s.r.oauth_state_store().admin_token()
            if not key:
                self.update(jid, status='needs_confirmation', reason='管理员授权不可用，请重新连接后提交')
                return
            await asyncio.to_thread(self.s.authenticate, key, fresh=True)
            if self.store.read()['jobs'][jid]['status'] != 'queued':
                return
            # State is committed before any upstream request is dispatched.
            job = self.update(jid, status='running', reason='', started_at=stamp())
            from .operation_versions import operation_context
            token = operation_context.set(job)
            try:
                result = await self.dispatch(row, job, key)
            finally:
                operation_context.reset(token)
            self.update(jid, status='completed', reason='', result=result)
            self.audit(job, 'completed')
        except HTTPException as exc:
            detail = exc.detail if isinstance(exc.detail, dict) else {'message': str(exc.detail)}
            if detail.get('code') == 'account_busy':
                self.update(jid, status='queued', reason='等待账号空闲')
            elif exc.status_code == 409:
                self.update(jid, status='needs_confirmation', reason=detail.get('message', '相关状态已变化'))
            elif exc.status_code in {502, 504}:
                # Never replay a write or billable operation after an ambiguous response.
                self.update(jid, status='checking', reason=detail.get('message', '核对请求结果'))
            else:
                self.update(jid, status='failed', reason=detail.get('message', '操作失败'))
        except asyncio.CancelledError:
            state = self.store.read()['jobs'][jid]['status']
            if state == 'running':
                self.update(jid, status='checking' if self.closing else 'cancelled', reason='请求已停止；已派发写入仍需核对')
            raise
        except Exception:
            # Saving acceptance/dispatch may have failed. Never assume a safe replay.
            try:
                self.update(jid, status='needs_confirmation', reason='操作状态无法确认，未重复执行')
            except (OSError, ValueError):
                pass

    async def dispatch(self, row, job, key):
        from .desktop_api import account_dto, ScheduleRequest, DeleteRequest, AccountVersionRequest, UsageActionRequest
        from .desktop_actions import PriorityRequest, GroupsRequest, TestRequest, DegradationMarkRequest
        aid, action = row['id'], job['action']
        p = {**job['payload'], 'expected_version': account_dto(row, datetime.now(timezone.utc), self.managed())['version']}
        if action == 'account_template':
            return await self.thread_write(self.s.account_templates.apply, aid, {**job['payload'], 'expected_version': job['expected_version']}, key)
        if action == 'priority':
            return await self.s.actions.set_priority(aid, PriorityRequest(**p), key)
        if action == 'groups':
            return await self.s.actions.set_groups(aid, GroupsRequest(**p), key)
        if action in {'usage', 'reset_quota'}:
            return await self.thread_write(self.s.usage_action, aid, UsageActionRequest(**p), key)
        if action == 'schedulable':
            return await self.thread_write(self.s.set_schedulable, aid, ScheduleRequest(**p), key)
        if action == 'delete':
            return await self.thread_write(self.s.delete_account, aid, DeleteRequest(**p), key)
        if action == 'recover':
            return await self.thread_write(self.s.recover_account, aid, AccountVersionRequest(**p), key)
        if action == 'degradation_mark':
            p.pop('expected_version')
            return await self.s.actions.set_degradation_mark(aid, DegradationMarkRequest(**p, expected_mark_version=job['expected_version']))
        if action == 'test':
            request = TestRequest(**p)
            locks = await self.s.actions.prepare_test(aid, request)
            self.events[job['id']] = []
            result = {'success': False}
            async for chunk in self.s.actions.test_stream(aid, request, key, locks):
                event = json.loads(chunk.removeprefix('data: ').strip())
                self.events[job['id']].append(event)
                if event['type'] == 'error':
                    result = {'success': False, 'error': event.get('error')}
                elif event['type'] == 'test_complete':
                    result = {'success': event.get('success') is True, 'duration_ms': event.get('duration_ms')}
            return result
        raise HTTPException(422, '未知操作')

    @staticmethod
    async def thread_write(fn, *args):
        task = asyncio.create_task(asyncio.to_thread(fn, *args))
        try:
            return await asyncio.shield(task)
        except asyncio.CancelledError:
            await task  # A Python thread cannot be cancelled after dispatch.
            raise

    async def cancel(self, jid):
        job = self.store.read()['jobs'].get(jid)
        if not job:
            models = getattr(self.s, '_model_tests', None)
            if models:
                return self.model_view(await models.cancel(jid))
            raise HTTPException(404, '操作不存在')
        if job['status'] in {'queued', 'needs_confirmation'}:
            self.update(jid, status='cancelled', reason='已取消')
            task = self.tasks.get(jid)
            if task:
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)
        elif job['status'] == 'running' and job['action'] == 'test':
            task = self.tasks.get(jid)
            if task:
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)
        elif job['status'] in {'running', 'checking'}:
            self.update(jid, reason='请求已派发，等待核对结果')
        return self.get(jid)

    async def close(self):
        self.closing = True
        tasks = list(self.tasks.values())
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        self.keys.clear()
        self.events.clear()
