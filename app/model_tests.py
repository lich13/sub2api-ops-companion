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
from .codex_identity import codex_identity
from .model_test_stream import TestFailure, execute
from .modeltrace import analyze, bank, challenges

ACTIVE = {"running", "retrying"}
ERRORS = {"auth_or_quota": "认证失败或额度不足", "rate_limited": "账号受到限流",
          "invalid_model_or_request": "模型不可用或请求不兼容", "upstream_error": "上游请求失败",
          "output_limit": "输出超过保护上限", "inconsistent_stream": "响应内容不一致",
          "incomplete_stream": "响应未完整结束", "invalid_stream": "响应格式无效",
          "network_error": "连接中断", "account_changed": "账号配置已变化", "internal_error": "测试无法完成"}
_LOCK = threading.RLock()


def stamp():
    return datetime.now(timezone.utc).isoformat()


class ModelTestRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    model_id: str = Field(min_length=1, max_length=200, pattern=r"^[A-Za-z0-9._:/-]+$")
    expected_version: str = Field(min_length=64, max_length=64)
    request_id: str = Field(min_length=16, max_length=64, pattern=r"^[a-zA-Z0-9-]+$")


class ResultStore:
    def __init__(self, path: Path):
        self.path = path
        self.existed = path.exists()

    def read(self):
        try:
            data = json.loads(self.path.read_text())
        except FileNotFoundError:
            if self.existed:
                raise ValueError("模型测试状态文件丢失") from None
            return {"version": 1, "accounts": {}, "requests": {}}
        if not isinstance(data, dict) or data.get("version") != 1 or not isinstance(data.get("accounts"), dict):
            raise ValueError("模型测试状态损坏")
        for value in data["accounts"].values():
            if not isinstance(value, dict) or not value.get("id") or not isinstance(value.get("account_id"), int):
                raise ValueError("模型测试状态损坏")
        self.existed = True
        data.setdefault('requests', {})
        if not isinstance(data['requests'], dict):
            raise ValueError("模型测试状态损坏")
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
    return hashlib.sha256(json.dumps({k: row.get(k) for k in
        ("id", "parent_account_id", "platform", "type", "credentials", "proxy_id", "extra")}, sort_keys=True, default=str).encode()).hexdigest()


class ModelTests:
    def __init__(self, service):
        self.s = service
        self.store = ResultStore(Path(service.r.settings.usage_query_state_path).with_name('model-test-results.json'))
        self.tasks: dict[str, asyncio.Task] = {}
        self.start_lock = asyncio.Lock()
        self.execute = execute
        with self.store.transaction() as data:
            for row in data['accounts'].values():
                if row.get('status') in ACTIVE:
                    row.update(status='interrupted', error='服务重启，测试已中断', completed_at=stamp())

    def get(self, job_id):
        if not re.fullmatch('[a-f0-9]{32}', job_id):
            raise HTTPException(404, '测试结果不存在')
        for value in self.store.read()['accounts'].values():
            if value['id'] == job_id:
                return value
        raise HTTPException(404, '测试结果不存在')

    def latest(self, account_id):
        return self.store.read()['accounts'].get(str(account_id))

    def update(self, job_id, **changes):
        with self.store.transaction() as data:
            row = next((v for v in data['accounts'].values() if v['id'] == job_id), None)
            if row is None:
                raise ValueError('任务已被替换')
            row.update(changes)
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
        url = base + ('/responses' if oauth or explicit or base.endswith('/v1') else '/v1/responses')
        parsed = urlsplit(url)
        if parsed.scheme not in {'https', 'http'} or not parsed.hostname or parsed.username or parsed.password:
            raise HTTPException(422, '上游地址无效')
        headers = {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json', 'Accept': 'text/event-stream'}
        if oauth:
            client_version, user_agent = await self.codex_identity()
            headers.update({'OpenAI-Beta': 'responses=experimental', 'Originator': 'codex_cli_rs',
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
        return {'url': url, 'headers': headers, 'proxy': proxy, 'model': forwarded, 'oauth': oauth}, owner

    async def start(self, aid, payload):
        async with self.start_lock:
            old = self.latest(aid)
            if old and old['request_id'] == payload.request_id:
                if old['requested_model'] != payload.model_id:
                    raise HTTPException(409, '请求 ID 已用于其他模型')
                return old
            if old and old['status'] in ACTIVE:
                raise HTTPException(409, '此账号已有测试正在执行')
            if payload.request_id in self.store.read()['requests']:
                raise HTTPException(409, '此请求已经处理，请读取最近测试结果')
            row = await self.account(aid)
            lease = AccountLease(self.s.r.db, row)
            if not lease.acquire():
                raise HTTPException(409, '此账号或母账号正在执行操作')
            try:
                await self.s.actions.account(aid, payload.expected_version)
                target, owner = await self.target(row, payload.model_id)
                bank_snapshot = (self.s.r.fingerprint_bank.capture()
                                 if getattr(self.s.r, "fingerprint_bank", None) is not None else bank())
                _, version = bank_snapshot
                job_id = uuid.uuid4().hex
                job = {'id': job_id, 'request_id': payload.request_id, 'account_id': aid,
                       'account_name': row['name'], 'requested_model': payload.model_id,
                       'forwarded_model': target['model'], 'returned_models': [],
                       'status': 'running', 'completed_groups': 0, 'valid_groups': 0, 'attempts': 0,
                       'started_at': stamp(), 'completed_at': None, 'duration_ms': 0, 'error': '',
                       'report': None, 'bank_version': version}
                with self.store.transaction() as data:
                    data['accounts'][str(aid)] = job
                    data['requests'][payload.request_id] = job_id
                task = asyncio.create_task(self.run(job, row, owner, target, lease, bank_snapshot))
                self.tasks[job_id] = task
                task.add_done_callback(lambda value: self.finished(job_id, lease, value))
                return job
            except BaseException:
                lease.release()
                raise

    async def run(self, job, row, owner, target, lease, bank_snapshot):
        started = time.monotonic()
        samples, returned, completed, attempts = [], set(), 0, 0
        job_id = job['id']
        try:
            for expected, prompt in challenges():
                for attempt in range(3):
                    if target_signature(await self.account(row['id'])) != target_signature(row):
                        raise TestFailure('account_changed')
                    if owner['id'] != row['id'] and target_signature(await self.account(owner['id'])) != target_signature(owner):
                        raise TestFailure('account_changed')
                    attempts += 1
                    self.update(job_id, status='running', attempts=attempts, error='')
                    write_audit(self.s.r.settings.audit_path, 'model_test_attempt', {'account_id': row['id'], 'job_id': job_id, 'attempt': attempts})
                    try:
                        text, model = await self.execute(**target, prompt=prompt, expected=expected)
                        if model and re.fullmatch(r'[A-Za-z0-9._:/-]{1,200}', model):
                            # Never expose a reflected credential as a model identifier.
                            secrets_in_headers = [v for k, v in target['headers'].items() if k.lower() in {'authorization', 'chatgpt-account-id'}]
                            if model not in secrets_in_headers and not any(v.removeprefix('Bearer ') in model for v in secrets_in_headers):
                                returned.add(model)
                        samples.append(text)
                        break
                    except (TestFailure, httpx.HTTPError, TimeoutError) as exc:
                        failed = exc if isinstance(exc, TestFailure) else TestFailure('network_error', True)
                        if not failed.retryable or attempt == 2:
                            raise failed
                        self.update(job_id, status='retrying', error=ERRORS[failed.code])
                        await asyncio.sleep((1, 3)[attempt])
                completed += 1
                report = analyze(samples, snapshot=bank_snapshot)
                self.update(job_id, completed_groups=completed, valid_groups=(report or {}).get('used_outputs', 0),
                            returned_models=sorted(returned), report=report,
                            duration_ms=round((time.monotonic() - started) * 1000))
            self.update(job_id, status='completed', error='' if report else '有效样本不足，无法识别')
        except asyncio.CancelledError:
            self.update(job_id, status='cancelled', error='测试已停止')
        except Exception as exc:
            code = exc.code if isinstance(exc, TestFailure) else 'internal_error'
            try:
                self.update(job_id, status='failed', error=ERRORS[code])
            except (OSError, ValueError):
                pass
        finally:
            samples.clear()
            try:
                final = self.update(job_id, completed_at=stamp(), duration_ms=round((time.monotonic() - started) * 1000))
                write_audit(self.s.r.settings.audit_path, 'model_test_result', {'account_id': row['id'], 'job_id': job_id,
                            'status': final['status'], 'attempts': attempts, 'valid_groups': final['valid_groups']})
            finally:
                # The task callback releases the lease after the coroutine has
                # finished (and also covers cancellation before first await).
                pass

    def finished(self, job_id, lease, task):
        lease.release()
        self.tasks.pop(job_id, None)
        if task.cancelled():
            try:
                self.update(job_id, status='cancelled', error='测试已停止', completed_at=stamp())
            except (OSError, ValueError):
                pass
        else:
            task.exception()  # Consume persistence errors without logging sensitive task locals.

    async def cancel(self, job_id):
        self.get(job_id)
        task = self.tasks.get(job_id)
        if task:
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
        return self.get(job_id)

    async def close(self):
        tasks = list(self.tasks.values())
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
