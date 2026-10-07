"""Authenticated Sub2API route selection and connection diagnostics."""
from __future__ import annotations

import hashlib
import json
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

from .bark import _urlopen_no_redirect
from .connection_config import connection_config


@dataclass(frozen=True)
class ProbeResult:
    state: str
    endpoint: str
    status_code: int | None = None
    message: str = ""
    retryable: bool = False

    def public(self, checked_at: str | None = None) -> dict[str, Any]:
        return {
            "state": self.state,
            "endpoint": self.endpoint,
            "http_status": self.status_code,
            "message": self.message,
            "retryable": self.retryable,
            "checked_at": checked_at or datetime.now(timezone.utc).isoformat(),
        }


class UpstreamProbeError(RuntimeError):
    def __init__(self, result: ProbeResult):
        super().__init__(result.message)
        self.result = result


class UpstreamRouter:
    """Choose a configured Sub2API endpoint without hiding auth failures."""

    def __init__(self, settings):
        self.settings = settings
        self._lock = threading.RLock()
        self._probe_lock = threading.Lock()
        self._active: str | None = None
        self._status = ProbeResult("network_unreachable", "none", message="尚未检查上游")
        self._checked_at = ""
        self._auth: dict[str, float] = {}
        self._failure_digest = ""
        self._failure_result: ProbeResult | None = None
        self._failure_count = 0
        self._retry_after = 0.0

    def _candidates(self) -> list[tuple[str, str]]:
        try:
            config = connection_config(self.settings)
        except Exception:
            return []
        candidates: list[tuple[str, str]] = []
        for kind, value in (("verify", config.get("verify_base_url", "")), ("public", config.get("base_url", ""))):
            if value and all(value != current for _, current in candidates):
                candidates.append((kind, value))
        return candidates

    def active_url(self) -> str:
        with self._lock:
            candidates = self._candidates()
            if self._active and any(value == self._active for _, value in candidates):
                return self._active
            return candidates[0][1] if candidates else ""

    def status(self) -> dict[str, Any]:
        with self._lock:
            return self._status.public(self._checked_at or None)

    def invalidate(self) -> None:
        with self._lock:
            self._auth.clear()
            self._active = None
            self._failure_digest = ""
            self._failure_result = None
            self._failure_count = 0
            self._retry_after = 0.0

    @staticmethod
    def _valid_response(body: Any) -> bool:
        # Sub2API v0.2.14 uses the code/data envelope; older direct-list
        # responses remain accepted for installations that still return it.
        return isinstance(body, list) or (
            isinstance(body, dict) and type(body.get("code")) is int and body["code"] == 0 and isinstance(body.get("data"), list)
        )

    @staticmethod
    def _probe(endpoint: str, base: str, key: str) -> ProbeResult:
        request = urllib.request.Request(
            base.rstrip("/") + "/api/v1/admin/groups/all",
            headers={"x-api-key": key, "Accept": "application/json"},
        )
        try:
            with _urlopen_no_redirect(request, timeout=5) as response:
                status = int(getattr(response, "status", 200))
                raw = response.read(2_000_000)
                if status in (401, 403):
                    return ProbeResult("auth_rejected", endpoint, status, "管理员 API Key 被上游拒绝，请重新连接")
                if status == 404:
                    return ProbeResult("route_not_found", endpoint, status, "Sub2API 管理接口路径不存在", retryable=True)
                if not 200 <= status < 300:
                    return ProbeResult("upstream_error", endpoint, status, f"Sub2API 返回 HTTP {status}", retryable=status >= 500)
                try:
                    body = json.loads(raw)
                except (TypeError, ValueError):
                    return ProbeResult("protocol_mismatch", endpoint, status, "Sub2API 返回内容不是有效 JSON")
                if isinstance(body, dict) and type(body.get("code")) is int and body["code"] != 0:
                    return ProbeResult("upstream_error", endpoint, status, "Sub2API 管理接口返回错误")
                if not UpstreamRouter._valid_response(body):
                    return ProbeResult("protocol_mismatch", endpoint, status, "Sub2API 管理响应结构不兼容")
                return ProbeResult("ok", endpoint, status, "Sub2API 管理权限已验证")
        except urllib.error.HTTPError as exc:
            if exc.code in (401, 403):
                return ProbeResult("auth_rejected", endpoint, exc.code, "管理员 API Key 被上游拒绝，请重新连接")
            if exc.code == 404:
                return ProbeResult("route_not_found", endpoint, exc.code, "Sub2API 管理接口路径不存在", retryable=True)
            return ProbeResult("upstream_error", endpoint, exc.code, f"Sub2API 返回 HTTP {exc.code}", retryable=exc.code >= 500)
        except (TimeoutError, OSError, urllib.error.URLError):
            return ProbeResult("network_unreachable", endpoint, None, "无法连接 Sub2API 上游", retryable=True)
        except Exception:
            return ProbeResult("network_unreachable", endpoint, None, "无法连接 Sub2API 上游", retryable=True)

    def authenticate(self, key: str, *, fresh: bool = False) -> ProbeResult:
        # Collapse concurrent reads into one probe/cache decision. State readers
        # keep their own short lock and never wait on network I/O.
        with self._probe_lock:
            return self._authenticate(key, fresh=fresh)

    def _authenticate(self, key: str, *, fresh: bool = False) -> ProbeResult:
        if not key or len(key) > 4096 or any(ord(c) < 33 for c in key):
            result = ProbeResult("auth_rejected", "none", 401, "管理员 API Key 无效")
            self._save_status(result)
            with self._lock:
                self._auth.clear()
            raise UpstreamProbeError(result)
        candidates = self._candidates()
        route_signature = "\0".join(value for _, value in candidates)
        digest = hashlib.sha256((route_signature + "\0" + key).encode()).hexdigest()
        with self._lock:
            now = time.monotonic()
            if not fresh and self._auth.get(digest, 0) > now and self._status.state == "ok":
                return self._status
            if (not fresh and self._failure_digest == digest and self._failure_result is not None
                    and now < self._retry_after):
                raise UpstreamProbeError(self._failure_result)
        if not candidates:
            result = ProbeResult("network_unreachable", "none", None, "未配置 Sub2API 服务地址")
            self._save_status(result)
            raise UpstreamProbeError(result)
        last = ProbeResult("network_unreachable", candidates[0][0], message="无法连接 Sub2API 上游", retryable=True)
        for endpoint, base in candidates:
            result = self._probe(endpoint, base, key)
            last = result
            if result.state == "ok":
                with self._lock:
                    self._active = base
                    self._status = result
                    self._checked_at = datetime.now(timezone.utc).isoformat()
                    self._auth = {digest: time.monotonic() + 15}
                    self._failure_digest = ""
                    self._failure_result = None
                    self._failure_count = 0
                    self._retry_after = 0.0
                return result
            if result.state not in {"network_unreachable", "route_not_found"}:
                break
        self._save_status(last)
        with self._lock:
            self._auth.clear()
            if last.retryable:
                if self._failure_digest == digest:
                    self._failure_count += 1
                else:
                    self._failure_digest, self._failure_count = digest, 1
                self._failure_result = last
                self._retry_after = time.monotonic() + min(5 * (2 ** min(self._failure_count - 1, 4)), 60)
            else:
                self._failure_digest = ""
                self._failure_result = None
                self._failure_count = 0
                self._retry_after = 0.0
        raise UpstreamProbeError(last)

    def _save_status(self, result: ProbeResult) -> None:
        with self._lock:
            self._status = result
            self._checked_at = datetime.now(timezone.utc).isoformat()
