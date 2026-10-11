"""Safe account editors. Only explicit queued intents enter these write paths."""
from __future__ import annotations

import asyncio
from datetime import datetime, timezone

from fastapi import HTTPException
from pydantic import BaseModel, ConfigDict, Field, StrictInt

from .account_locks import AccountLease
from .account_templates import Template, combine, split, ID
from .audit import write_audit
from .bark import sanitize_error_text
from .operation_versions import versions

EDIT_ACTIONS = {'priority', 'concurrency', 'load_factor', 'model_config', 'proxy', 'sync_models'}


class SettingsVersion(BaseModel):
    model_config = ConfigDict(extra='forbid')
    expected_version: str = Field(min_length=64, max_length=64)


class ConcurrencyRequest(SettingsVersion):
    concurrency: StrictInt = Field(ge=1, le=2147483647)


class LoadFactorRequest(SettingsVersion):
    load_factor: StrictInt | None = Field(ge=1, le=10000)


class ProxyRequest(SettingsVersion):
    proxy_id: StrictInt | None = Field(ge=1, le=9223372036854775807)


class ModelConfigRequest(Template):
    expected_version: str = Field(min_length=64, max_length=64)


def capabilities(row: dict) -> dict:
    extra = row.get('extra') or {}
    inherited = bool(row.get('parent_account_id') or extra.get('parent_account_id'))
    passthrough = row.get('platform') == 'openai' and any(extra.get(k) in (True, 'true') for k in ('openai_passthrough', 'openai_oauth_passthrough'))
    model_reason = '继承母账号凭据，不能独立修改模型配置' if inherited else '透传模式会绕过模型配置' if passthrough else ''
    sync_supported = row.get('platform') in {'openai', 'grok', 'anthropic', 'gemini', 'antigravity', 'kimi', 'zhipu', 'deepseek', 'opencode', 'opencodego'}
    if row.get('platform') != 'antigravity' and row.get('type') not in {'oauth', 'apikey'}:
        sync_supported = False
    if row.get('platform') == 'gemini' and row.get('type') == 'oauth' and row.get('model_sync_project'):
        sync_supported = False
    return {
        'priority': {'editable': True, 'reason': ''},
        'concurrency': {'editable': True, 'reason': ''},
        'load_factor': {'editable': True, 'reason': ''},
        'model_config': {'editable': not bool(model_reason), 'reason': model_reason},
        'proxy': {'editable': not inherited, 'reason': '代理继承母账号，不能独立修改' if inherited else ''},
        'sync_models': {'editable': sync_supported and not inherited,
                        'reason': '请在母账号同步模型' if inherited else '' if sync_supported else '该账号不支持上游模型同步'},
    }


def current_value(row: dict, action: str) -> dict:
    if action == 'model_config':
        return split(row.get('model_mapping'))
    if action == 'proxy':
        return {'proxy_id': row.get('proxy_id')}
    if action == 'sync_models':
        return {}
    return {action: row.get(action)}


def target_reached(row: dict, action: str, payload: dict) -> bool:
    if action == 'sync_models' or not capabilities(row)[action]['editable']:
        return False
    if action == 'model_config':
        return (row.get('model_mapping') or {}) == combine(payload)
    field = 'proxy_id' if action == 'proxy' else action
    return row.get(field) == payload[field]


class AccountSettings:
    def __init__(self, service):
        self.s = service

    async def view(self, aid: int) -> dict:
        if aid < 1:
            raise HTTPException(422, '账号编号无效')
        row = await self.s.actions.account(aid)
        try:
            profile = split(row.get('model_mapping'))
        except ValueError:
            raise HTTPException(409, '现有模型配置格式无效，请先在 Sub2API 核对') from None
        return {'account_id': aid, 'priority': int(row.get('priority') or 0),
                'concurrency': row.get('concurrency'), 'load_factor': row.get('load_factor'),
                'effective_load_factor': row.get('load_factor') or row.get('concurrency') or 1,
                'proxy_id': row.get('proxy_id'), 'model_config': profile,
                'capabilities': capabilities(row),
                'versions': {k: v for k, v in versions(row).items() if k in EDIT_ACTIONS}}

    async def proxies(self, key: str) -> list[dict]:
        async with self.s.actions.client(key, 10) as client:
            data = await self.s.actions.json_request(client, 'GET', 'proxies/all')
        if not isinstance(data, list) or len(data) > 10000:
            raise HTTPException(502, '代理列表格式无效')
        result = []
        for item in data:
            if not isinstance(item, dict) or type(item.get('id')) is not int or item['id'] < 1:
                raise HTTPException(502, '代理列表格式无效')
            # No host, port, username, password or URL leaves this boundary.
            result.append({'id': item['id'], 'name': sanitize_error_text(str(item.get('name') or ''), limit=160),
                           'protocol': item.get('protocol') if item.get('protocol') in {'http', 'https', 'socks5', 'socks5h'} else 'unknown',
                           'status': item.get('status') if item.get('status') in {'active', 'inactive', 'error'} else 'unknown'})
        return result

    async def apply(self, aid: int, action: str, payload: dict, expected: str, key: str) -> dict:
        from .account_operations import busy
        row = await self.s.actions.account(aid)
        lease_row = {**row, 'parent_account_id': row.get('parent_account_id') or (row.get('extra') or {}).get('parent_account_id')}
        lease = AccountLease(self.s.r.db, lease_row)
        if not lease.acquire():
            raise busy()
        try:
            live = await self.s.actions.account(aid)
            if row.get('parent_account_id') != live.get('parent_account_id') or versions(live)[action] != expected:
                raise HTTPException(409, '相关字段或账号资格已变化，请核对后重新提交')
            capability = capabilities(live)[action]
            if not capability['editable']:
                raise HTTPException(409, capability['reason'])
            if action == 'proxy' and payload['proxy_id'] is not None:
                choices = await self.proxies(key)
                if not any(p['id'] == payload['proxy_id'] and p['status'] == 'active' for p in choices):
                    raise HTTPException(409, '所选代理已不可用，请重新选择')
            live = await self.s.actions.account(aid)
            if versions(live)[action] != expected:
                raise HTTPException(409, '相关字段已变化，请核对后重新提交')
            auto_reset = getattr(getattr(self.s.r, 'oauth_monitor', None), 'auto_reset', None)
            scalar_edit = action in {'priority', 'concurrency', 'load_factor'} and auto_reset is not None
            if scalar_edit:
                await asyncio.to_thread(auto_reset.prepare_parameter_edit, aid, action, payload[action])
            async with self.s.actions.client(key, 60 if action == 'sync_models' else 10) as client:
                if action == 'sync_models':
                    data = await self.s.actions.json_request(client, 'POST', f'accounts/{aid}/models/sync-upstream')
                    if not isinstance(data, dict) or not isinstance(data.get('models'), list) or len(data['models']) > 10000:
                        raise HTTPException(502, '上游模型同步响应格式无效')
                    models = list(dict.fromkeys(m for m in data['models'] if isinstance(m, str) and ID.fullmatch(m)))
                    if not models:
                        raise HTTPException(422, '上游未返回可用模型，已有草稿已保留')
                    # Only IDs and fixed diagnostic labels are published, never remote metadata or arbitrary warning text.
                    warnings = ['模型能力信息不完整'] if data.get('warnings') else []
                    result = {'models': models, 'warnings': warnings, 'synced_at': datetime.now(timezone.utc).isoformat()}
                else:
                    if action == 'model_config':
                        body = {'account_ids': [aid], 'credentials': {'model_mapping': combine(payload)}}
                        method, path = 'POST', 'accounts/bulk-update'
                    else:
                        body = {'proxy_id': payload['proxy_id'] or 0} if action == 'proxy' else {action: payload[action] if payload[action] is not None else 0}
                        method, path = 'PUT', f'accounts/{aid}'
                    try:
                        await self.s.actions.json_request(client, method, path, json=body)
                    except HTTPException as exc:
                        if exc.status_code not in {502, 504}:
                            raise
                        actual = await self.s.actions.account(aid)
                        if not target_reached(actual, action, payload):
                            raise  # Queue checks actual state next; never blindly replay.
                    actual = await self.s.actions.account(aid)
                    if not target_reached(actual, action, payload):
                        raise HTTPException(502, '字段写入尚未确认，正在核对实际值')
                    if scalar_edit:
                        await asyncio.to_thread(auto_reset.reconcile_parameter_edit, aid)
                    result = {'verified': True, **current_value(actual, action), 'field_version': versions(actual)[action],
                              'settings_versions': {k: v for k, v in versions(actual).items() if k in EDIT_ACTIONS}}
            write_audit(self.s.r.settings.audit_path, 'desktop_account_settings', {'account_id': aid, 'action': action, 'result': 'completed'})
            return result
        finally:
            lease.release()
            await asyncio.to_thread(self.s.invalidate)
