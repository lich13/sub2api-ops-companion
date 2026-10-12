"""Bounded forwarding of native proxy diagnostics; no proxy credentials escape."""
from __future__ import annotations

import asyncio
import hashlib
import ipaddress
import math
from typing import Any

import httpx
from fastapi import HTTPException

from .bark import sanitize_error_text


def _text(value: Any, limit: int = 240) -> str:
    if not isinstance(value, str):
        return ''
    # Native messages are diagnostic strings, never an arbitrary response dump.
    if any(part in value.lower() for part in ('authorization', 'password', 'cookie', '://', 'bearer ', 'api_key', 'token=')):
        return '检测详情含敏感信息，已隐藏'
    return sanitize_error_text(value, limit=limit)


def _number(value: Any, maximum: float = 1e12) -> float | int | None:
    return value if type(value) in (int, float) and math.isfinite(value) and 0 <= value <= maximum else None


def _ip(value: Any) -> str:
    if not isinstance(value, str):
        return ''
    try:
        return str(ipaddress.ip_address(value))
    except (ValueError, TypeError):
        return ''


def safe_result(action: str, data: Any) -> dict:
    if not isinstance(data, dict):
        raise HTTPException(502, '代理检测响应格式不兼容')
    if action == 'test':
        if type(data.get('success')) is not bool:
            raise HTTPException(502, '代理检测响应格式不兼容')
        return {'success': data['success'], 'message': _text(data.get('message')),
                'latency_ms': _number(data.get('latency_ms')), 'ip_address': _ip(data.get('ip_address')),
                **{key: _text(data.get(key), 100) for key in ('city', 'region', 'country', 'country_code')}}
    if (_number(data.get('score'), 100) is None or not isinstance(data.get('grade'), str)
            or not isinstance(data.get('items'), list) or len(data['items']) > 32):
        raise HTTPException(502, '代理质量响应格式不兼容')
    items = []
    for item in data['items']:
        if not isinstance(item, dict) or item.get('status') not in {'pass', 'warn', 'fail', 'challenge'}:
            raise HTTPException(502, '代理质量响应格式不兼容')
        items.append({'target': _text(item.get('target'), 80), 'status': item['status'],
                      'http_status': _number(item.get('http_status'), 599),
                      'latency_ms': _number(item.get('latency_ms')), 'message': _text(item.get('message'))})
    return {'score': data['score'], 'grade': _text(data['grade'], 40), 'summary': _text(data.get('summary')),
            'exit_ip': _ip(data.get('exit_ip')), 'country': _text(data.get('country'), 100),
            'country_code': _text(data.get('country_code'), 10), 'base_latency_ms': _number(data.get('base_latency_ms')),
            **{key: _number(data.get(key)) for key in ('passed_count', 'warn_count', 'failed_count', 'challenge_count', 'checked_at')},
            'items': items}


class ProxyDiagnostics:
    def __init__(self, actions: Any) -> None:
        self.actions = actions
        self._running: dict[tuple, asyncio.Task] = {}

    async def run(self, key: str, proxy_id: int, action: str) -> dict:
        if type(proxy_id) is not int or proxy_id <= 0 or action not in {'test', 'quality-check'}:
            raise HTTPException(422, '代理检测参数无效')
        identity = (hashlib.sha256(key.encode()).hexdigest(), proxy_id, action)
        task = self._running.get(identity)
        if task is None:
            task = asyncio.create_task(self._request(key, proxy_id, action))
            self._running[identity] = task
            def finished(done):
                if self._running.get(identity) is done:
                    self._running.pop(identity, None)
                if not done.cancelled():
                    done.exception()  # retrieve failures even after a client disconnects
            task.add_done_callback(finished)
        return await asyncio.shield(task)

    async def _request(self, key: str, proxy_id: int, action: str) -> dict:
        try:
            async with asyncio.timeout(90):
                async with self.actions.client(key, timeout=90) as client:
                    async with client.stream('POST', f'proxies/{proxy_id}/{action}', json={}) as response:
                        if response.status_code in {404, 405, 501}:
                            raise HTTPException(502, 'Sub2API 不支持此代理检测接口')
                        if not 200 <= response.status_code < 300:
                            raise HTTPException(502, f'代理检测失败 [http_{response.status_code}]')
                        raw = bytearray()
                        async for chunk in response.aiter_bytes():
                            raw.extend(chunk)
                            if len(raw) > 1024 * 1024:
                                raise HTTPException(502, '代理检测响应过大')
                        import json
                        body = json.loads(raw)
                        if not isinstance(body, dict) or type(body.get('code')) is not int or body['code'] != 0:
                            raise HTTPException(502, '代理检测未成功，请重试')
                        return safe_result(action, body.get('data'))
        except (TimeoutError, httpx.TimeoutException):
            raise HTTPException(504, '代理检测超时，未重放请求') from None
        except (httpx.HTTPError, ValueError):
            raise HTTPException(502, '代理检测响应不可用，未重放请求') from None
