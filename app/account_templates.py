"""Explicit account templates; only model_mapping is merged into an account."""
from __future__ import annotations

import json
import re
import secrets
import urllib.request
from pathlib import Path
from typing import Any
import copy
from fastapi import HTTPException
from pydantic import BaseModel, ConfigDict, Field
from .account_locks import AccountLease
from .audit import write_audit
from .bark import _urlopen_no_redirect
from .operation_versions import digest, versions
from .policy_store import PolicyStore

ID = re.compile(r"[A-Za-z0-9._:/-]{1,200}")
SOURCE = re.compile(r"(?:[A-Za-z0-9._:/-]{1,199}\*?|\*)")
NAMES = {"full": "满血", "degraded": "降智", "takeover": "降智接管"}
SELECT = """SELECT id,name,platform,type,deleted_at,
 nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id,
 credentials->'model_mapping' AS model_mapping, md5(credentials::text) AS credential_version,
 md5(coalesce(credentials->'model_mapping','{}'::jsonb)::text) AS model_mapping_version,
 (coalesce((extra->>'openai_passthrough')::boolean,false) OR coalesce((extra->>'openai_oauth_passthrough')::boolean,false)) AS passthrough
 FROM accounts"""

class MappingRule(BaseModel):
    model_config = ConfigDict(extra="forbid")
    source: str = Field(min_length=1, max_length=200)
    target: str = Field(min_length=1, max_length=200)

class Template(BaseModel):
    model_config = ConfigDict(extra="forbid")
    whitelist: list[str] = Field(default_factory=list, max_length=300)
    mappings: list[MappingRule] = Field(default_factory=list, max_length=300)

class TemplatesRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_version: str = Field(min_length=64, max_length=64)
    full: Template
    degraded: Template
    takeover: Template

class CustomTemplateRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_version: str = Field(min_length=64, max_length=64)
    name: str = Field(min_length=1, max_length=40)
    whitelist: list[str] = Field(default_factory=list, max_length=300)
    mappings: list[MappingRule] = Field(default_factory=list, max_length=300)

class CustomTemplateDeleteRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_version: str = Field(min_length=64, max_length=64)

class TemplateApplication(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_version: str = Field(min_length=64, max_length=64)
    template_id: str = Field(min_length=4, max_length=31, pattern=r"^(?:full|degraded|takeover|custom-[a-f0-9]{24})$")
    template_version: str = Field(min_length=64, max_length=64)

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
    return versions(row).get('account_template') or digest({k: row.get(k) for k in ("id", "platform", "type", "parent_account_id", "deleted_at", "passthrough", "model_mapping", "credential_version")})


class AccountTemplates:
    def __init__(self, service):
        self.s, self.r = service, service.r
        self.store = PolicyStore(Path(self.r.settings.usage_query_state_path).with_name('account-templates.json'),
            {"revision": 0, "configured": False, "templates": {k: {"whitelist": [], "mappings": []} for k in NAMES},
             "template_meta": {}, "template_order": list(NAMES), "custom_templates": []})
        self.writer = self._write_mapping

    def account(self, aid):
        return self.r.db.fetch_one(SELECT + " WHERE id=%(id)s", {"id": aid})

    @staticmethod
    def _sync_custom(data: dict) -> list[dict]:
        items = []
        for key in data.get('template_order', []):
            if key in NAMES or key not in data.get('templates', {}):
                continue
            meta = data.get('template_meta', {}).get(key, {})
            items.append({"id": key, "name": meta.get('name', key), **copy.deepcopy(data['templates'][key])})
        data['custom_templates'] = items
        return items

    @staticmethod
    def _normalize(data):
        if (type(data.get('configured')) is not bool or not isinstance(data.get('templates'), dict)
                or type(data.get('revision')) is not int or len(data['templates']) > 32
                or not set(NAMES).issubset(data['templates'])):
            raise ValueError('账号模板状态无效')
        meta = data.setdefault('template_meta', {})
        if not isinstance(meta, dict):
            raise ValueError('账号模板状态无效')
        for key, value in data['templates'].items():
            combine(Template.model_validate(value).model_dump())
            if key in NAMES:
                continue
            name = meta.get(key, {}).get('name') if isinstance(meta.get(key), dict) else None
            if (not re.fullmatch(r'custom-[a-f0-9]{24}', str(key)) or not isinstance(name, str)
                    or not name.strip() or len(name) > 40):
                raise ValueError('自定义模板状态无效')
        order = data.setdefault('template_order', list(NAMES))
        if not isinstance(order, list) or any(not isinstance(key, str) for key in order):
            raise ValueError('账号模板顺序无效')
        data['template_order'] = list(dict.fromkeys(
            [key for key in order if key in data['templates']] + list(data['templates'])))
        AccountTemplates._sync_custom(data)
        return data

    def data(self):
        original = self.store.read()
        normalized = self._normalize(copy.deepcopy(original))
        if normalized == original:
            return normalized
        # Re-read under the file lock so a migration cannot overwrite a concurrent edit.
        with self.store.transaction() as current:
            migrated = self._normalize(copy.deepcopy(current))
            if migrated != current:
                migrated['revision'] = current['revision'] + 1
                current.clear()
                current.update(migrated)
            normalized = copy.deepcopy(current)
        return normalized

    def view(self, aid=None):
        data = self.data()
        result = {'configured': data['configured'], 'version': digest(data), 'templates': data['templates'],
                  'template_meta': data.get('template_meta', {}), 'template_order': data.get('template_order', list(NAMES)),
                  'custom_templates': data.get('custom_templates', [])}
        if aid is not None:
            row = self.account(aid)
            if row is None:
                raise HTTPException(404, '账号不存在')
            result['account'] = {'id': aid, 'eligible': eligible(row), 'passthrough': bool(row.get('passthrough')),
                                 'version': account_version(row), 'config': split(row.get('model_mapping'))}
        return result

    def save(self, payload):
        values = {key: payload.model_dump()[key] for key in NAMES}
        for value in values.values():
            combine(value)
        with self.store.transaction() as data:
            if digest(data) != payload.expected_version:
                raise HTTPException(409, '模板已变化，请重新读取')
            data.setdefault('template_meta', {})
            data.setdefault('template_order', list(NAMES))
            data.setdefault('templates', {}).update(values)
            self._sync_custom(data)
            data.update(configured=True, revision=data.get('revision', 0) + 1)
        write_audit(self.r.settings.audit_path, 'account_templates_saved', {'revision': data['revision']})
        return self.view()

    @staticmethod
    def _payload_value(payload, name):
        if isinstance(payload, BaseModel):
            return getattr(payload, name)
        return payload.get(name)

    def desired(self, payload):
        data = self.data()
        if not data['configured']:
            raise HTTPException(409, '请先保存账号模板')
        if digest(data) != self._payload_value(payload, 'template_version'):
            raise HTTPException(409, '模板已变化，请重新选择')
        template_id = self._payload_value(payload, 'template_id')
        if template_id not in data.get('templates', {}):
            raise HTTPException(422, '账号模板无效')
        return combine(data['templates'][template_id])

    def create_custom(self, payload):
        data = self.data()
        if digest(data) != payload.expected_version:
            raise HTTPException(409, '模板已变化，请重新读取')
        name = payload.name.strip()
        if not name:
            raise HTTPException(422, '模板名称不能为空')
        profile = {"whitelist": list(payload.whitelist), "mappings": [rule.model_dump() for rule in payload.mappings]}
        combine(profile)
        if len(data['templates']) >= 32:
            raise HTTPException(409, '最多保存 32 套模板')
        template_id = f"custom-{secrets.token_hex(12)}"
        with self.store.transaction() as current:
            if digest(current) != payload.expected_version:
                raise HTTPException(409, '模板已变化，请重新读取')
            current.setdefault('templates', {})[template_id] = profile
            current.setdefault('template_meta', {})[template_id] = {"name": name, "builtin": False}
            current.setdefault('template_order', list(NAMES)).append(template_id)
            self._sync_custom(current)
            current['configured'] = True
            current['revision'] = current.get('revision', 0) + 1
        write_audit(self.r.settings.audit_path, 'account_template_created', {'template_id': template_id})
        return self.view()

    def update_custom(self, template_id: str, payload):
        data = self.data()
        if template_id in NAMES or template_id not in data['templates']:
            raise HTTPException(404, '自定义模板不存在')
        name = payload.name.strip()
        if not name:
            raise HTTPException(422, '模板名称不能为空')
        profile = {"whitelist": list(payload.whitelist), "mappings": [rule.model_dump() for rule in payload.mappings]}
        combine(profile)
        with self.store.transaction() as current:
            if digest(current) != payload.expected_version:
                raise HTTPException(409, '模板已变化，请重新读取')
            current['templates'][template_id] = profile
            current.setdefault('template_meta', {})[template_id] = {"name": name, "builtin": False}
            self._sync_custom(current)
            current['revision'] = current.get('revision', 0) + 1
        write_audit(self.r.settings.audit_path, 'account_template_updated', {'template_id': template_id})
        return self.view()

    def delete_custom(self, template_id: str, expected_version: str | CustomTemplateDeleteRequest):
        if isinstance(expected_version, BaseModel):
            expected_version = expected_version.expected_version
        data = self.data()
        if template_id in NAMES or template_id not in data['templates']:
            raise HTTPException(404, '自定义模板不存在')
        with self.store.transaction() as current:
            if digest(current) != expected_version:
                raise HTTPException(409, '模板已变化，请重新读取')
            current['templates'].pop(template_id, None)
            current.setdefault('template_meta', {}).pop(template_id, None)
            current['template_order'] = [item for item in current.get('template_order', []) if item != template_id]
            self._sync_custom(current)
            current['revision'] = current.get('revision', 0) + 1
        write_audit(self.r.settings.audit_path, 'account_template_deleted', {'template_id': template_id})
        return self.view()

    def fingerprint(self, aid):
        return account_version(self.account(aid) or {})

    def achieved(self, aid, payload):
        row = self.account(aid)
        return bool(eligible(row) and not row.get('passthrough') and (row.get('model_mapping') or {}) == self.desired(payload))

    def apply(self, aid, payload, key):
        from .account_operations import busy
        row = self.account(aid)
        lease = AccountLease(self.r.db, row or {'id': aid})
        if not lease.acquire():
            raise busy()
        try:
            row = self.account(aid)
            if not eligible(row):
                raise HTTPException(409, '仅可应用到独立 OpenAI OAuth 或 Key 账号')
            if row.get('passthrough'):
                raise HTTPException(409, '透传模式会绕过模型限制，请先处理透传设置')
            mapping = self.desired(payload)
            if (row.get('model_mapping') or {}) == mapping:
                return {'verified': True, 'account_id': aid, 'unchanged': True}
            if account_version(row) != self._payload_value(payload, 'expected_version'):
                raise HTTPException(409, '账号模型配置或凭据已变化，请重新选择模板')
            code = self.writer(aid, mapping, key)
            live = self.account(aid)
            verified = bool(eligible(live) and not live.get('passthrough') and (live.get('model_mapping') or {}) == mapping)
            write_audit(self.r.settings.audit_path, 'account_template_applied',
                        {'account_id': aid, 'template_id': self._payload_value(payload, 'template_id'), 'verified': verified, 'code': code})
            if not verified:
                raise HTTPException(502, {'code': 'template_unconfirmed', 'message': '模板应用未确认，请核对实际配置'})
            return {'verified': True, 'account_id': aid, 'template_id': self._payload_value(payload, 'template_id')}
        finally:
            lease.release()
            self.s.invalidate()

    def _write_mapping(self, aid, mapping, key):
        request = urllib.request.Request(self.r.oauth_base_url().rstrip('/') + '/api/v1/admin/accounts/bulk-update',
            method='POST', headers={'x-api-key': key, 'Content-Type': 'application/json'},
            data=json.dumps({'account_ids': [aid], 'credentials': {'model_mapping': mapping}}).encode())
        try:
            with _urlopen_no_redirect(request, timeout=5) as response:
                body = json.loads(response.read(2_000_000))
                return 'ok' if 200 <= response.status < 300 and isinstance(body, dict) and body.get('code') == 0 else 'rejected'
        except Exception:
            return 'uncertain'


    def initialize_sources(self, full_account_id: int | None = None, degraded_account_id: int | None = None):
        """Initialize templates from explicitly supplied source accounts.

        Source IDs are deployment input only and are never embedded in the application.
        Only model_mapping is copied; credentials and account settings are never written.
        """
        data = self.data()
        if data['configured'] or not isinstance(full_account_id, int) or not isinstance(degraded_account_id, int):
            return self.view()
        if full_account_id < 1 or degraded_account_id < 1 or full_account_id == degraded_account_id:
            return self.view()
        try:
            rows = self.r.db.fetch_all(SELECT + " WHERE id=ANY(%(ids)s) AND deleted_at IS NULL", {"ids": [full_account_id, degraded_account_id]})
        except Exception:
            return self.view()
        sources = {int(row['id']): row for row in rows if isinstance(row, dict)}
        full, degraded = sources.get(full_account_id), sources.get(degraded_account_id)
        if not (eligible(full) and eligible(degraded)):
            return self.view()
        values = {'full': split(full.get('model_mapping')), 'degraded': split(degraded.get('model_mapping')),
                  'takeover': split(degraded.get('model_mapping'))}
        for value in values.values():
            combine(value)
        already_configured = False
        with self.store.transaction() as current:
            if current['configured']:
                already_configured = True
            else:
                current.update(templates=values, configured=True, revision=current['revision'] + 1)
        if already_configured:
            return self.view()
        write_audit(self.r.settings.audit_path, 'account_templates_initialized', {'source_pair_verified': True})
        return self.view()

    def retire_legacy(self):
        """Quiesce legacy intents without replaying writes or touching marks."""
        legacy = self.store.path.with_name('account-model-profiles.json')
        if legacy.exists():
            try:
                old = PolicyStore(legacy, {})
                with old.transaction() as data:
                    for job in data.get('jobs', {}).values():
                        for item in job.get('items', []):
                            if item.get('status') in {'queued', 'writing'}:
                                match = item.get('status') == 'writing' and self._legacy_matches(item)
                                item.update(status='applied' if match else 'retired', error='' if match else '旧模板任务已退休，未重放')
            except (OSError, ValueError, KeyError, TypeError):
                write_audit(self.r.settings.audit_path, 'account_templates_legacy_state_unavailable', {})
        alerts = self.r.capacity_alerts
        with alerts.store.transaction() as data:
            for item in data.get('profile_intents', {}).values():
                if item.get('status') in {'queued', 'writing'}:
                    match = item.get('status') == 'writing' and self._legacy_matches(item)
                    item.update(status='applied' if match else 'retired', error='' if match else '旧模板联动已退休，未重放')

    def _legacy_matches(self, item):
        try:
            row = self.account(item['account_id'])
        except Exception:
            return False
        return bool(row and (row.get('model_mapping') or {}) == item.get('target'))
