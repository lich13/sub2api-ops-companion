"""Explicit, account-pinned ModelTrace jobs; credentials and samples never leave memory."""
from __future__ import annotations

import asyncio
import fcntl
import hashlib
import json
import re
import threading
import time
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import quote, urlsplit

import httpx
from fastapi import HTTPException
from pydantic import BaseModel, ConfigDict, Field

from .account_locks import AccountLease
from .atomic_config import write_json
from .audit import write_audit
from .codex_identity import codex_identity, codex_originator
from .model_test_stream import TestFailure, execute
from .modeltrace import analyze, bank, challenges

ACTIVE = {"queued", "running", "retrying"}
ERRORS = {"auth_or_quota": "认证失败或额度不足", "rate_limited": "账号受到限流",
          "invalid_model_or_request": "模型不可用或请求不兼容", "upstream_error": "上游请求失败",
          "output_limit": "输出超过保护上限", "inconsistent_stream": "响应内容不一致",
          "incomplete_stream": "响应未完整结束", "invalid_stream": "响应格式无效",
          "protocol_mismatch": "上游响应不是受支持的模型协议", "network_error": "连接中断", "account_changed": "账号配置已变化", "internal_error": "测试无法完成"}
_LOCK = threading.RLock()


def stamp():
    return datetime.now(timezone.utc).isoformat()


class ModelTestRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    model_id: str = Field(min_length=1, max_length=200, pattern=r"^[A-Za-z0-9._:/-]+$")
    expected_version: str = Field(min_length=64, max_length=64)
    request_id: str = Field(min_length=16, max_length=64, pattern=r"^[a-zA-Z0-9-]+$")
    concurrency: int = Field(default=1, ge=1, le=3, strict=True)
    expected_operation_version: str | None = Field(default=None, min_length=64, max_length=64)


class ResultStore:
    def __init__(self, path: Path):
        self.path = path
        self.existed = path.exists() or path.with_suffix('.lock').exists()

    def read(self):
        try:
            data = json.loads(self.path.read_text())
        except FileNotFoundError:
            if self.existed:
                raise ValueError("模型测试状态文件丢失") from None
            return {"version": 1, "accounts": {}, "requests": {}, "jobs": {}}
        if not isinstance(data, dict) or data.get("version") != 1 or not isinstance(data.get("accounts"), dict):
            raise ValueError("模型测试状态损坏")
        for value in data["accounts"].values():
            if not isinstance(value, dict) or not value.get("id") or not isinstance(value.get("account_id"), int):
                raise ValueError("模型测试状态损坏")
        self.existed = True
        data.setdefault('requests', {})
        if not isinstance(data['requests'], dict):
            raise ValueError("模型测试状态损坏")
        data.setdefault('jobs', {v['id']: v for v in data['accounts'].values()})
        if not isinstance(data['jobs'], dict) or any(not isinstance(v, dict) or not v.get('id') for v in data['jobs'].values()):
            raise ValueError('模型测试状态损坏')
        return data

    @contextmanager
    def transaction(self):
        with _LOCK:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            with self.path.with_suffix('.lock').open('a+') as lock:
                Path(lock.name).chmod(0o600)
                fcntl.flock(lock, fcntl.LOCK_EX)
                try:
                    data = self.read()
                    yield data
                    write_json(self.path, data)
                    self.existed = True
                finally:
                    fcntl.flock(lock, fcntl.LOCK_UN)


def target_signature(row):
    value = {k: row.get(k) for k in ("id", "parent_account_id", "platform", "type", "credentials", "proxy_id")}
    value['protocol'] = {k: (row.get('extra') or {}).get(k) for k in
                         ('openai_responses_mode', 'openai_responses_supported')}
    return hashlib.sha256(json.dumps(value, sort_keys=True, default=str).encode()).hexdigest()


class ModelTests:
    def __init__(self, service):
        self.s = service
        self.store = ResultStore(Path(service.r.settings.usage_query_state_path).with_name('model-test-results.json'))
        self.tasks: dict[str, asyncio.Task] = {}
        self.start_lock = asyncio.Lock()
        self.execute = execute
        self.job_slots = asyncio.Semaphore(2)
        self.contexts: dict[str, dict] = {}
        self.closing = False
        with self.store.transaction() as data:
            for row in data['jobs'].values():
                if row.get('status') in {'running', 'retrying'}:
                    row.update(status='interrupted', error='服务重启，测试已中断', completed_at=stamp(), can_retry=False)
                    if data['accounts'].get(str(row['account_id']), {}).get('id') == row['id']:
                        data['accounts'][str(row['account_id'])] = row

    def get(self, job_id):
        if not re.fullmatch('[a-f0-9]{32}', job_id):
            raise HTTPException(404, '测试结果不存在')
        for value in self.store.read()['jobs'].values():
            if value['id'] == job_id:
                return self.public(self.progress(value))
        raise HTTPException(404, '测试结果不存在')

    def latest(self, account_id):
        value = self.store.read()['accounts'].get(str(account_id))
        return self.public(self.progress(value)) if value else None

    @staticmethod
    def progress(value):
        if value['status'] in {'running', 'retrying'}:
            return {**value, 'duration_ms': max(0, round((datetime.now(timezone.utc) - datetime.fromisoformat(value['started_at'])).total_seconds() * 1000))}
        return value

    def update(self, job_id, **changes):
        with self.store.transaction() as data:
            row = data['jobs'].get(job_id)
            if row is None:
                raise ValueError('任务已被替换')
            row.update(changes)
            if data['accounts'].get(str(row['account_id']), {}).get('id') == job_id:
                data['accounts'][str(row['account_id'])] = row
            return dict(row)

    async def account(self, aid):
        row = await asyncio.to_thread(self.s.r.db.fetch_one,
            "SELECT id,name,platform,type,status,schedulable,credentials,extra,proxy_id,"
            "nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id,"
            "ARRAY(SELECT ag.group_id FROM account_groups ag JOIN groups g ON g.id=ag.group_id "
            "AND g.deleted_at IS NULL WHERE ag.account_id=accounts.id ORDER BY ag.group_id) AS group_ids "
            "FROM accounts WHERE id=%(id)s AND deleted_at IS NULL", {'id': aid})
        if not row or row['platform'] != 'openai' or row['type'] not in {'oauth', 'apikey'}:
            raise HTTPException(422, '仅支持 Codex OAuth 和 Key 账号')
        return row

    async def codex_identity(self) -> tuple[str, str]:
        try:
            rows = await asyncio.to_thread(
                self.s.r.db.fetch_all,
                "SELECT key,value FROM settings WHERE key IN "
                "('openai_codex_user_agent','openai_codex_client_version','openai_codex_client_version_synced')",
                {},
            )
        except Exception:
            rows = []
        values = {str(row.get("key")): row.get("value") for row in rows if isinstance(row, dict)}
        return codex_identity(values)

    async def target(self, row, model):
        owner = await self.account(row['parent_account_id']) if row.get('parent_account_id') else row
        if owner.get('parent_account_id'):
            raise HTTPException(422, '母账号关系无效')
        if owner['type'] != row['type']:
            raise HTTPException(422, '母账号凭据类型不一致')
        credentials = owner.get('credentials') or {}
        if not await self.s.actions.model_allowed(row, model):
            raise HTTPException(422, '所选模型不在当前分组白名单中')
        # ModelTrace is an account-pinned diagnostic.  The selected group
        # allowlist is the source of truth and the request must reach the
        # selected account with the exact model the user chose; model_mapping
        # is intentionally not a routing step here.
        forwarded = model
        if not re.fullmatch(r'[A-Za-z0-9._:/-]{1,200}', forwarded) or any(x in forwarded.lower() for x in ('image', 'dall-e', 'tts-', 'whisper-', 'realtime', 'audio')):
            raise HTTPException(422, '请选择该账号支持的文本模型')
        oauth = owner['type'] == 'oauth'
        token = credentials.get('access_token' if oauth else 'api_key')
        if not isinstance(token, str) or not token:
            raise HTTPException(409, '账号凭据不可用')
        base = 'https://chatgpt.com/backend-api/codex' if oauth else str(credentials.get('base_url') or 'https://api.openai.com')
        explicit = base.endswith('#')
        base = base.removesuffix('#').rstrip('/')
        extra = owner.get('extra') or {}
        mode = extra.get('openai_responses_mode', 'auto')
        use_responses = oauth or mode == 'force_responses' or (mode != 'force_chat_completions' and extra.get('openai_responses_supported') is not False)
        protocol = 'responses' if use_responses else 'chat_completions'
        endpoint = '/responses' if use_responses else '/chat/completions'
        url = base + (endpoint if oauth or explicit or base.endswith('/v1') else '/v1' + endpoint)
        parsed = urlsplit(url)
        if parsed.scheme not in {'https', 'http'} or not parsed.hostname or parsed.username or parsed.password:
            raise HTTPException(422, '上游地址无效')
        headers = {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json', 'Accept': 'text/event-stream'}
        client_version, user_agent = await self.codex_identity()
        headers['User-Agent'] = user_agent
        if oauth:
            headers.update({'OpenAI-Beta': 'responses=experimental', 'Originator': codex_originator(user_agent),
                            'Version': client_version, 'User-Agent': user_agent})
            if credentials.get('chatgpt_account_id'):
                headers['ChatGPT-Account-ID'] = str(credentials['chatgpt_account_id'])
        proxy = None
        proxy_id = row.get('proxy_id') or owner.get('proxy_id')
        if proxy_id:
            p = await asyncio.to_thread(self.s.r.db.fetch_one,
                'SELECT protocol,host,port,username,password FROM proxies WHERE id=%(id)s AND deleted_at IS NULL', {'id': proxy_id})
            if not p:
                raise HTTPException(409, '账号代理不可用')
            auth = quote(p['username'], safe='') + ':' + quote(p.get('password') or '', safe='') + '@' if p.get('username') else ''
            proxy = f"{p['protocol']}://{auth}{p['host']}:{p['port']}"
        return {'url': url, 'headers': headers, 'proxy': proxy, 'model': forwarded, 'oauth': oauth, 'protocol': protocol}, owner

    def resume(self):
        for job in self.store.read()['jobs'].values():
            if job.get('status') == 'queued' and job['id'] not in self.tasks:
                self.launch(job)

    def launch(self, job, context=None):
        task = asyncio.create_task(self.wait_and_run(job, context))
        self.tasks[job['id']] = task
        task.add_done_callback(lambda value: self.finished(job['id'], value))

    async def start(self, aid, payload):
        async with self.start_lock:
            data = self.store.read()
            previous = data['requests'].get(payload.request_id)
            if previous:
                old = self.get(previous)
                if old['account_id'] != aid or old['requested_model'] != payload.model_id or old.get('concurrency', 1) != payload.concurrency:
                    raise HTTPException(409, {'code': 'idempotency_conflict', 'message': '请求 ID 已用于其他测试'})
                return old
            row = await self.account(aid)
            conflict = False
            if payload.expected_operation_version:
                from .operation_versions import versions
                live = await self.s.actions.account(aid)
                if versions(live)['model_test'] != payload.expected_operation_version:
                    conflict = True
            else:
                try:
                    await self.s.actions.account(aid, payload.expected_version)
                except HTTPException as exc:
                    if exc.status_code != 409:
                        raise
                    conflict = True
            target, owner = await self.target(row, payload.model_id)
            job_id = uuid.uuid4().hex
            job = {'id': job_id, 'request_id': payload.request_id, 'account_id': aid,
                   'account_name': row['name'], 'requested_model': payload.model_id,
                   'forwarded_model': target['model'], 'returned_models': [],
                   'status': 'queued', 'completed_groups': 0, 'valid_groups': 0, 'attempts': 0,
                   'queued_at': stamp(), 'started_at': None, 'completed_at': None, 'duration_ms': 0, 'error': '',
                   'report': None, 'bank_version': None, 'concurrency': payload.concurrency,
                   'target_signature': target_signature(row), 'owner_signature': target_signature(owner),
                   'groups': [{'index': i+1, 'status': 'queued', 'attempts': 0, 'ttft_ms': None,
                               'duration_ms': None, 'error': ''} for i in range(3)]}
            if conflict:
                job.update(status='needs_confirmation', error='测试目标已变化，请重新确认')
            with self.store.transaction() as data:
                # Keep active work and the latest terminal summary for each account.
                for old_id, old in list(data['jobs'].items()):
                    if old['account_id'] == aid and old['status'] not in ACTIVE:
                        self.contexts.pop(old_id, None)
                        old.update(report=None, groups=[], can_retry=False)
                data['jobs'][job_id] = job
                data['accounts'][str(aid)] = job
                data['requests'][payload.request_id] = job_id
            if not conflict:
                self.launch(job)
            return self.public(job)

    @staticmethod
    def public(job):
        return {k: v for k, v in job.items() if k not in {'target_signature', 'owner_signature'}}

    async def wait_and_run(self, job, context=None):
        lease, slot = None, False
        try:
            row = await self.account(job['account_id'])
            lease = AccountLease(self.s.r.db, row)
            while True:
                if not self.job_slots.locked() and lease.acquire():
                    await self.job_slots.acquire()
                    slot = True
                    break
                await asyncio.sleep(.25)
            row = await self.account(job['account_id'])
            target, owner = await self.target(row, job['requested_model'])
            if target_signature(row) != job['target_signature'] or target_signature(owner) != job['owner_signature']:
                self.update(job['id'], status='needs_confirmation', error='测试目标已变化，请重新选择', completed_at=stamp())
                return
            if context is None:
                snapshot = self.s.r.fingerprint_bank.capture() if getattr(self.s.r, 'fingerprint_bank', None) is not None else bank()
                context = {'samples': {}, 'challenges': list(challenges()), 'snapshot': snapshot,
                           'row': row, 'owner': owner, 'target': target}
                self.contexts[job['id']] = context
            else:
                # Retried groups use the original identity, bank and request target.
                target = context['target']
            job = self.update(job['id'], status='running', started_at=stamp(), error='', completed_at=None,
                              bank_version=context['snapshot'][1], can_retry=False)
            write_audit(self.s.r.settings.audit_path, 'model_test_identity',
                        {'account_id': row['id'], 'job_id': job['id'], 'user_agent': target['headers']['User-Agent'],
                         'protocol': target['protocol'], 'host': urlsplit(target['url']).hostname})
            await self.run(job, row, owner, target, context)
        except asyncio.CancelledError:
            current = self.get(job['id'])
            if not (self.closing and current['status'] == 'queued'):
                self.update(job['id'], status='interrupted' if self.closing else 'cancelled',
                            error='服务重启，测试已中断' if self.closing else '测试已停止', completed_at=stamp(), can_retry=False)
        except HTTPException as exc:
            self.update(job['id'], status='needs_confirmation', error=str(exc.detail)[:200], completed_at=stamp())
        except Exception:
            self.update(job['id'], status='failed', error='测试无法完成', completed_at=stamp())
        finally:
            if lease:
                lease.release()
            if slot:
                self.job_slots.release()

    async def run(self, job, row, owner, target, context):
        started, job_id = time.monotonic(), job['id']
        samples, bank_snapshot = context['samples'], context['snapshot']
        groups, returned = job['groups'], set(job.get('returned_models') or [])
        slots, analysis_lock, progress_lock = asyncio.Semaphore(job.get('concurrency', 1)), asyncio.Lock(), asyncio.Lock()
        attempts, fatal, early, children = 0, None, False, []

        async def persist(**changes):
            async with progress_lock:
                writing = asyncio.create_task(asyncio.to_thread(self.update, job_id, **changes))
                cancelled = False
                while not writing.done():
                    try:
                        await asyncio.shield(writing)
                    except asyncio.CancelledError:
                        cancelled = True
                result = writing.result()
                if cancelled:
                    raise asyncio.CancelledError
                return result

        async def progress():
            await persist(groups=[dict(g) for g in groups], attempts=attempts,
                          duration_ms=round((time.monotonic()-started)*1000))

        def stop_peers():
            for child in children:
                if child is not asyncio.current_task() and not child.done():
                    child.cancel()

        async def sample(index, expected, prompt, client):
            nonlocal attempts, fatal, early
            group, group_started = groups[index], time.monotonic()
            try:
                for attempt in range(3):
                    failed = None
                    async with slots:
                        if early or fatal:
                            group['status'] = 'skipped' if early else 'cancelled'
                            return
                        if target_signature(await self.account(row['id'])) != target_signature(row):
                            raise TestFailure('account_changed')
                        if owner['id'] != row['id'] and target_signature(await self.account(owner['id'])) != target_signature(owner):
                            raise TestFailure('account_changed')
                        attempts += 1
                        group.update(status='running', attempts=attempt+1, error='', ttft_ms=None, retryable=False,
                                     started_at=stamp(), diagnostics={})
                        await progress()  # Register before sending a potentially billable request.
                        write_audit(self.s.r.settings.audit_path, 'model_test_attempt',
                                    {'account_id': row['id'], 'job_id': job_id, 'group': index+1, 'attempt': attempt+1})
                        async def first_text(elapsed):
                            group['ttft_ms'] = elapsed
                            await progress()
                        async def stage(value):
                            group['status'] = value
                            await progress()
                        try:
                            options = {'client': client, 'on_first_text': first_text, 'on_stage': stage,
                                       'diagnostics': group['diagnostics']} if self.execute is execute else {}
                            text, model = await self.execute(**target, prompt=prompt, expected=expected, **options)
                            if model and re.fullmatch(r'[A-Za-z0-9._:/-]{1,200}', model):
                                secrets = [v for k, v in target['headers'].items() if k.lower() in {'authorization', 'chatgpt-account-id'}]
                                if not any(v.removeprefix('Bearer ') in model for v in secrets):
                                    returned.add(model)
                            samples[index] = text
                            group.update(status='analyzing', duration_ms=round((time.monotonic()-group_started)*1000))
                        except (TestFailure, httpx.HTTPError, TimeoutError) as exc:
                            failed = exc if isinstance(exc, TestFailure) else TestFailure('network_error', True)
                        finally:
                            write_audit(self.s.r.settings.audit_path, 'model_test_transport',
                                        {'account_id': row['id'], 'job_id': job_id, 'group': index+1,
                                         'attempt': attempt+1, **group['diagnostics']})
                    if failed:
                        group.update(error=ERRORS[failed.code], retryable=failed.retryable)
                        if not failed.retryable:
                            raise failed
                        if attempt == 2:
                            group['status'] = 'failed'
                            return
                        group['status'] = 'retrying'
                        await progress()
                        await asyncio.sleep((1, 3)[attempt])
                        continue
                    async with analysis_lock:
                        analysis_started = time.monotonic()
                        try:
                            report = await asyncio.to_thread(analyze, [samples[i] for i in sorted(samples)], snapshot=bank_snapshot)
                        except Exception:
                            group.update(status='completed', error='样本已完成，本地分析失败')
                            return
                        group.update(status='completed', analysis_ms=round((time.monotonic()-analysis_started)*1000))
                        await persist(completed_groups=len(samples), valid_groups=(report or {}).get('used_outputs', 0),
                                      returned_models=sorted(returned), report=report)
                        if report and report['probability'] >= .99:
                            early = True
                            stop_peers()
                        await progress()
                    return
            except asyncio.CancelledError:
                group['status'] = 'skipped' if early else 'cancelled'
                raise
            except TestFailure as exc:
                group.update(status='failed', error=ERRORS[exc.code], retryable=False)
                fatal = exc.code
                stop_peers()
            finally:
                group['duration_ms'] = round((time.monotonic()-group_started)*1000)

        try:
            async with httpx.AsyncClient(timeout=httpx.Timeout(None, connect=10, write=10, pool=10),
                    follow_redirects=False, trust_env=False, proxy=target['proxy']) as client:
                children = [asyncio.create_task(sample(i, expected, prompt, client))
                            for i, (expected, prompt) in enumerate(context['challenges']) if i not in samples]
                try:
                    results = await asyncio.gather(*children, return_exceptions=True)
                    if any(isinstance(value, Exception) for value in results):
                        fatal = fatal or 'internal_error'
                finally:
                    stop_peers()
                    await asyncio.gather(*children, return_exceptions=True)
            current = self.get(job_id)
            retryable = any(g.get('retryable') and g['status'] == 'failed' for g in groups) and not fatal and not early
            status = 'failed' if fatal or (not samples and not early) else 'completed'
            error = ERRORS[fatal] if fatal else '' if current.get('report') else '有效样本不足，无法识别'
            await persist(status=status, error=error, can_retry=retryable,
                          completion_reason='confidence_99' if early else 'fatal_error' if fatal else 'samples_finished')
        finally:
            await persist(groups=groups, completed_at=stamp(), duration_ms=round((time.monotonic()-started)*1000))
            final = self.get(job_id)
            write_audit(self.s.r.settings.audit_path, 'model_test_result',
                        {'account_id': row['id'], 'job_id': job_id, 'status': final['status'],
                         'attempts': attempts, 'valid_groups': final['valid_groups'],
                         'completion_reason': final.get('completion_reason')})
            # Only the latest task per account retains in-memory samples for an explicit retry.
            for old_id, old in list(self.contexts.items()):
                if old_id != job_id and old['row']['id'] == row['id']:
                    self.contexts.pop(old_id, None)

    async def retry_failed(self, job_id, request_id):
        async with self.start_lock:
            data = self.store.read()
            if request_id in data['requests']:
                if data['requests'][request_id] != job_id:
                    raise HTTPException(409, {'code': 'idempotency_conflict', 'message': '请求 ID 已使用'})
                return self.get(job_id)
            job = self.get(job_id)
            context = self.contexts.get(job_id)
            if job['status'] in ACTIVE:
                return job
            if not job.get('can_retry') or context is None:
                raise HTTPException(409, {'code': 'retry_unavailable', 'message': '原测试上下文已失效，请重新测试'})
            stored = self.store.read()['jobs'][job_id]
            for group in stored['groups']:
                if group['index']-1 not in context['samples']:
                    group.update(status='queued', error='', attempts=0, ttft_ms=None)
            stored.update(status='queued', can_retry=False, queued_at=stamp(), error='',
                          previous_attempts=stored.get('previous_attempts', 0)+stored['attempts'])
            with self.store.transaction() as data:
                data['jobs'][job_id] = stored
                data['accounts'][str(job['account_id'])] = stored
                data['requests'][request_id] = job_id
            self.launch(stored, context)
            return self.public(stored)

    def finished(self, job_id, task):
        self.tasks.pop(job_id, None)
        if task.cancelled() and not self.closing:
            self.update(job_id, status='cancelled', error='测试已停止', completed_at=stamp())
        elif not task.cancelled():
            task.exception()

    async def cancel(self, job_id):
        self.get(job_id)
        task = self.tasks.get(job_id)
        if task:
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
        elif self.get(job_id)['status'] in {'queued', 'needs_confirmation'}:
            self.update(job_id, status='cancelled', error='测试已停止', completed_at=stamp())
        return self.get(job_id)

    async def close(self):
        self.closing = True
        tasks = list(self.tasks.values())
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        self.contexts.clear()
