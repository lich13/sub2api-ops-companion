"""Account model policies with durable previews and a mark-linked outbox.

Only model_mapping is sent to Sub2API's JSONB merge endpoint. Credentials are
neither selected by this module nor stored in its task/preview state.
"""
from __future__ import annotations

import asyncio
import fcntl
import hashlib
import json
import re
import threading
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
import urllib.request

from fastapi import HTTPException
from pydantic import BaseModel, ConfigDict, Field

from .account_locks import AccountLease
from .atomic_config import write_json
from .audit import write_audit
from .bark import _urlopen_no_redirect, sanitize_error_text
from .capacity_alerts import mark_view

_LOCK = threading.RLock()
ID = re.compile(r"[A-Za-z0-9._:/-]{1,200}")
SOURCE = re.compile(r"(?:[A-Za-z0-9._:/-]{1,199}\*?|\*)")
SELECT = """SELECT id,name,platform,type,deleted_at,
 nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id,
 credentials->'model_mapping' AS model_mapping,
 (coalesce((extra->>'openai_passthrough')::boolean,false) OR coalesce((extra->>'openai_oauth_passthrough')::boolean,false)) AS passthrough
 FROM accounts"""


def now():
    return datetime.now(timezone.utc).isoformat()


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False, default=str).encode()).hexdigest()


class MappingRule(BaseModel):
    model_config = ConfigDict(extra="forbid")
    source: str = Field(min_length=1, max_length=200)
    target: str = Field(min_length=1, max_length=200)


class Profile(BaseModel):
    model_config = ConfigDict(extra="forbid")
    whitelist: list[str] = Field(default_factory=list, max_length=300)
    mappings: list[MappingRule] = Field(default_factory=list, max_length=300)


class ProfilesRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_version: str = Field(min_length=64, max_length=64)
    normal: Profile
    degraded: Profile


class ApplyRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    preview_version: str = Field(min_length=64, max_length=64)
    request_id: str = Field(min_length=16, max_length=64, pattern=r"^[a-zA-Z0-9-]+$")


def combine(value: dict) -> dict[str, str]:
    result = {}
    for name in value.get("whitelist", []):
        if not isinstance(name, str) or not ID.fullmatch(name):
            raise HTTPException(422, "白名单必须是精确模型 ID")
        if name in result:
            raise HTTPException(422, f"重复的模型：{name}")
        result[name] = name
    for rule in value.get("mappings", []):
        source, target = rule.get("source"), rule.get("target")
        if not isinstance(source, str) or not SOURCE.fullmatch(source) or not isinstance(target, str) or not ID.fullmatch(target):
            raise HTTPException(422, "映射源只允许末尾通配符，目标必须是精确模型 ID")
        if source in result or source == target:
            raise HTTPException(422, f"模型条目冲突：{source}")
        result[source] = target
    return result


def split(mapping: Any) -> dict:
    if mapping is None:
        mapping = {}
    if not isinstance(mapping, dict) or any(not isinstance(k, str) or not isinstance(v, str) for k, v in mapping.items()):
        raise ValueError("账号模型配置无效")
    return {"whitelist": [k for k, v in mapping.items() if k == v],
            "mappings": [{"source": k, "target": v} for k, v in mapping.items() if k != v]}


def eligible(row):
    return bool(row and not row.get("deleted_at") and row.get("platform") == "openai"
                and row.get("type") in {"oauth", "apikey"} and not row.get("parent_account_id"))


def account_version(row):
    return digest({k: row.get(k) for k in ("id", "platform", "type", "parent_account_id", "deleted_at", "passthrough", "model_mapping")})


class ProfileStore:
    def __init__(self, path):
        self.path = Path(path)
        self.existed = self.path.exists() or self.path.with_suffix('.lock').exists()

    def read(self):
        try:
            data = json.loads(self.path.read_text())
        except FileNotFoundError:
            if self.existed:
                raise ValueError("账号模型模板状态丢失") from None
            return {"version": 1, "revision": 0, "configured": False,
                    "normal": {"whitelist": [], "mappings": []}, "degraded": {"whitelist": [], "mappings": []},
                    "jobs": {}, "requests": {}, "receipts": {}}
        if not isinstance(data, dict) or data.get("version") != 1 or type(data.get("configured")) is not bool:
            raise ValueError("账号模型模板状态无效")
        for key in ("normal", "degraded"):
            combine(Profile.model_validate(data[key]).model_dump())
        if any(not isinstance(data.get(key), dict) for key in ("jobs", "requests", "receipts")):
            raise ValueError("账号模型任务状态无效")
        self.existed = True
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


class AccountModelProfiles:
    def __init__(self, service):
        self.s, self.r = service, service.r
        self.store = ProfileStore(Path(self.r.settings.usage_query_state_path).with_name('account-model-profiles.json'))
        self.writer = self._write_mapping

    def audit(self, action, **data):
        write_audit(self.r.settings.audit_path, 'account_model_profile_' + action, data)

    def account(self, aid):
        return self.r.db.fetch_one(SELECT + " WHERE id=%(id)s", {'id': aid})

    @staticmethod
    def config_version(data):
        return digest({k: data[k] for k in ('revision', 'configured', 'normal', 'degraded')})

    def view(self):
        data = self.store.read()
        latest = max(data['jobs'].values(), key=lambda job: job['created_at'], default=None)
        return {k: data[k] for k in ('configured', 'normal', 'degraded')} | {'version': self.config_version(data), 'latest_job': latest}

    def save(self, payload):
        values = {k: payload.model_dump()[k] for k in ('normal', 'degraded')}
        for value in values.values():
            combine(value)
        with self.store.transaction() as data:
            if self.config_version(data) != payload.expected_version:
                raise HTTPException(409, '模板已变化，请重新读取')
            data.update(values, configured=True, revision=data['revision'] + 1)
        self.audit('saved', revision=data['revision'])
        return self.view()

    def initialize_sources(self):
        """Explicit upgrade step; never run on an arbitrary new installation."""
        if self.store.read()['configured']:
            return self.view()
        normal, degraded = self.account(396), self.account(387)
        if not eligible(normal) or not eligible(degraded) or normal['name'] != 'tmq' or degraded['name'] != 'yx':
            raise ValueError('模板来源账号不符，未导入')
        values = {'normal': split(normal.get('model_mapping')), 'degraded': split(degraded.get('model_mapping'))}
        for value in values.values():
            combine(value)
        with self.store.transaction() as data:
            if not data['configured']:
                data.update(values, configured=True, revision=data['revision'] + 1)
        self.audit('initialized', source_ids=[396, 387])
        return self.view()

    def item(self, row, profiles, mark):
        target = profiles['degraded' if mark['marked'] else 'normal']
        before = split(row.get('model_mapping'))
        conflict = '透传模式绕过账号模型限制' if row.get('passthrough') else ''
        return {'account_id': row['id'], 'name': sanitize_error_text(row['name'], 160), 'marked': mark['marked'],
                'before': before, 'after': target, 'expected_version': account_version(row), 'mark_version': mark['version'],
                'profile_version': self.config_version(profiles), 'status': 'conflict' if conflict else 'unchanged' if (row.get('model_mapping') or {}) == combine(target) else 'queued',
                'error': conflict, 'unrestricted': not combine(target)}

    def preview(self):
        profiles = self.store.read()
        if not profiles['configured']:
            raise HTTPException(409, '请先保存两套模板')
        marks = self.r.capacity_alerts.store.snapshot()['marks']
        rows = self.r.db.fetch_all(SELECT + " WHERE deleted_at IS NULL AND platform='openai' AND type IN ('oauth','apikey') ORDER BY id")
        items = [self.item(row, profiles, mark_view(row['id'], marks.get(str(row['id'])))) for row in rows if eligible(row)]
        return {'items': items, 'version': digest([self.config_version(profiles), items])}

    def apply(self, payload):
        existing = self.store.read()['requests'].get(payload.request_id)
        if existing:
            job = self.job(existing)
            if job['preview_version'] != payload.preview_version:
                raise HTTPException(409, '请求 ID 已用于其他预览')
            return job
        preview = self.preview()
        if preview['version'] != payload.preview_version:
            raise HTTPException(409, '账号或模板已变化，请重新预览')
        job = {'id': uuid.uuid4().hex, 'preview_version': payload.preview_version, 'created_at': now(), 'items': preview['items']}
        with self.store.transaction() as data:
            previous = data['requests'].get(payload.request_id)
            if previous:
                if data['jobs'][previous]['preview_version'] != payload.preview_version:
                    raise HTTPException(409, '请求 ID 已用于其他预览')
                return data['jobs'][previous]
            data['jobs'][job['id']] = job
            data['requests'][payload.request_id] = job['id']
        self.audit('queued', job_id=job['id'], count=len(job['items']))
        return job

    def job(self, job_id):
        if not re.fullmatch('[a-f0-9]{32}', job_id) or job_id not in self.store.read()['jobs']:
            raise HTTPException(404, '应用任务不存在')
        return self.store.read()['jobs'][job_id]

    def mark_intent_factory(self, aid):
        # A damaged/unconfigured template must never prevent the mark and Bark
        # suppression from being durably saved.
        try:
            profiles, row = self.store.read(), self.account(aid)
        except Exception:
            return lambda mark: {'id': uuid.uuid4().hex, 'account_id': aid, 'status': 'failed', 'error': '模板状态不可读取'}
        if not profiles['configured'] or not eligible(row):
            return lambda mark: None
        def intent(mark):
            identity = {'id': uuid.uuid4().hex, 'account_id': aid, 'created_at': now(), 'origin': 'mark'}
            try:
                return {**self.item(row, profiles, mark), **identity}
            except (HTTPException, ValueError, TypeError):
                return {**identity, 'status': 'failed', 'error': '账号模型配置无法核对，请重新预览'}
        return intent

    def mark_status(self, aid):
        value = self.r.capacity_alerts.store.snapshot().get('profile_intents', {}).get(str(aid))
        return {k: value.get(k) for k in ('status', 'error')} if value else None

    def _save_item(self, source, item, **changes):
        if source == 'mark':
            with self.r.capacity_alerts.store.transaction() as data:
                current = data.get('profile_intents', {}).get(str(item['account_id']))
                if not current or current['id'] != item['id']:
                    return False
                current.update(changes)
        else:
            with self.store.transaction() as data:
                current = next(v for v in data['jobs'][source]['items'] if v['account_id'] == item['account_id'])
                current.update(changes)
        return True

    def _write_mapping(self, aid, mapping):
        token = self.r.oauth_monitor.store.admin_token()
        base = self.r.oauth_base_url().rstrip('/')
        if not token or not base:
            return 'missing_connection'
        request = urllib.request.Request(base + '/api/v1/admin/accounts/bulk-update', method='POST',
            headers={'x-api-key': token, 'Content-Type': 'application/json'},
            data=json.dumps({'account_ids': [aid], 'credentials': {'model_mapping': mapping}}).encode())
        try:
            with _urlopen_no_redirect(request, timeout=5) as response:
                payload = json.loads(response.read(1_000_000))
                return 'ok' if 200 <= response.status < 300 and payload.get('code') == 0 else 'rejected'
        except Exception:
            return 'uncertain'

    def process(self, source, item):
        aid = item['account_id']
        row = self.account(aid)
        if not eligible(row):
            self._save_item(source, item, status='conflict', error='账号已删除或不再是独立 OpenAI 账号')
            return
        lease = AccountLease(self.r.db, row)
        if not lease.acquire():
            return
        try:
            row = self.account(aid)
            profiles = self.store.read()
            state = self.r.capacity_alerts.store.snapshot()
            if source == 'mark' and state.get('profile_intents', {}).get(str(aid), {}).get('id') != item['id']:
                return
            mark = mark_view(aid, state['marks'].get(str(aid)))
            target = combine(item['after'])
            actual = (row or {}).get('model_mapping') or {}
            if item['status'] == 'writing':
                # Crash/timeout readback only. An unknown result is not replayed.
                verified = eligible(row) and actual == target
                self._save_item(source, item, status='applied' if verified else 'failed',
                                error='' if verified else '写入结果未确认，请重新预览')
                return
            error = ''
            if not eligible(row) or row.get('passthrough'):
                error = '账号资格或透传模式已变化'
            elif mark['version'] != item['mark_version']:
                error = '降智标记已变化，请重新预览'
            elif self.config_version(profiles) != item['profile_version']:
                error = '模板已变化，请重新预览'
            elif account_version(row) != item['expected_version']:
                receipt = profiles['receipts'].get(str(aid), {})
                superseded = (source == 'mark' and receipt.get('previous_version') == item['expected_version']
                              and receipt.get('version') == account_version(row) and receipt.get('at', '') >= item.get('created_at', ''))
                if not superseded:
                    error = '账号模型配置已变化，请重新预览'
            if error:
                self._save_item(source, item, status='conflict', error=error)
                return
            if actual == target:
                self._save_item(source, item, status='unchanged', error='')
                return
            # Persist dispatch before the network boundary. Mark changes replace
            # unsent intents, while an already dispatched write is read back.
            if not self._save_item(source, item, status='writing', started_at=now()):
                return
            latest_mark = mark_view(aid, self.r.capacity_alerts.store.snapshot()['marks'].get(str(aid)))
            if latest_mark['version'] != item['mark_version']:
                self._save_item(source, item, status='conflict', error='降智标记已变化')
                return
            code = self.writer(aid, target)
            final = self.account(aid)
            verified = eligible(final) and (final.get('model_mapping') or {}) == target
            if verified:
                with self.store.transaction() as data:
                    data['receipts'][str(aid)] = {'version': account_version(final), 'previous_version': account_version(row), 'at': now()}
            self._save_item(source, item, status='applied' if verified else 'failed',
                            error='' if verified else '写入结果未确认，请重新预览', completed_at=now())
            self.audit('applied' if verified else 'failed', account_id=aid, source=source, result=code)
            self.s.invalidate()
        finally:
            lease.release()

    def pending(self):
        profiles = self.store.read()
        state = self.r.capacity_alerts.store.snapshot()
        items = [('mark', value) for value in state.get('profile_intents', {}).values() if value.get('status') in {'queued', 'writing'}]
        items += [(key, item) for key, job in profiles['jobs'].items() for item in job['items'] if item['status'] in {'queued', 'writing'}]
        return items

    async def loop(self):
        slots = asyncio.Semaphore(4)
        async def one(source, item):
            async with slots:
                try:
                    await asyncio.to_thread(self.process, source, item)
                except Exception:
                    self.audit('state_unavailable', account_id=item['account_id'])
        while True:
            try:
                pending = await asyncio.to_thread(self.pending)
                await asyncio.gather(*(one(source, item) for source, item in pending))
            except asyncio.CancelledError:
                raise
            except Exception:
                self.audit('state_unavailable')
            await asyncio.sleep(1)
