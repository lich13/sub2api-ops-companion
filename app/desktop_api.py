"""Small, credential-free DTOs for the native client. No model probes."""
from __future__ import annotations

import asyncio
import hashlib
import json
import re
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from typing import Any

from fastapi import APIRouter, HTTPException, Request
from fastapi.encoders import jsonable_encoder
from pydantic import BaseModel, ConfigDict, Field, StrictBool

from .audit import write_audit
from .bark import _urlopen_no_redirect, sanitize_error_text
from .config_service import ConfigConflict, ConfigService
from .key_fallback import deadline_is_future, execute_sub2api_set_schedulable

PREFIX = "/api/desktop/v1"
ERROR_WHERE = "e.account_id IS NOT NULL AND e.error_phase IN ('upstream', 'account_auth') AND e.error_owner = 'provider'"
ERROR_FIELDS = """e.id, e.account_id, e.group_id, e.created_at, e.platform, e.model,
 e.requested_model, e.upstream_model, e.status_code, e.upstream_status_code,
 e.provider_error_code, e.error_type, e.error_message, e.upstream_error_message,
 e.request_id, e.resolved, a.name AS account_name, g.name AS group_name"""


def clean(value: Any, limit: int = 600) -> str:
    return sanitize_error_text(value, limit=limit) if value not in (None, "") else ""


_REQUEST_DUMP = re.compile(r'''(?i)["']?\b(?:request(?:_body|body)?|body|messages|prompt|input|content|credentials|headers|authorization)["']?\s*[:=]''')


def safe_error_text(value: Any, limit: int = 600) -> str:
    text = str(value or "")
    # Errors sometimes stringify the entire request, including opaque credentials.
    # A whitelist of object keys alone does not protect such string values.
    if _REQUEST_DUMP.search(text.replace('\\"', '"')):
        return "上游错误包含请求或凭据数据，详细内容已隐藏"
    return clean(text, limit)


def safe_error_body(value: Any) -> str:
    """Only return error-shaped fields; never mirror arbitrary upstream JSON."""
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except (ValueError, TypeError):
            # Plaintext may be a request dump. Only return its short summary.
            return safe_error_text(value.splitlines()[0] if value else "", 1200)
    if isinstance(value, dict):
        allowed = {k: v for k, v in value.items() if k in {"error", "code", "type", "message", "detail", "reason", "status"}}
        def scrub(obj: Any) -> Any:
            if isinstance(obj, dict):
                return {k: scrub(v) for k, v in obj.items() if k in {"error", "code", "type", "message", "detail", "reason", "status"}}
            if isinstance(obj, list):
                return [scrub(item) for item in obj[:10]]
            return safe_error_text(obj, 1500)
        return clean(json.dumps(scrub(allowed), ensure_ascii=False, indent=2), 6000) if allowed else ""
    return ""


def error_dto(row: dict[str, Any], detail: bool = False) -> dict[str, Any]:
    fields = ("id", "account_id", "group_id", "created_at", "platform", "model", "requested_model",
              "upstream_model", "status_code", "upstream_status_code", "provider_error_code", "error_type",
              "request_id", "resolved", "account_name", "group_name")
    result = {key: row.get(key) for key in fields}
    for key, value in result.items():
        if isinstance(value, str):
            result[key] = clean(value, 300)
    result["message"] = safe_error_text(row.get("upstream_error_message") or row.get("error_message"))
    if detail:
        raw = row.get("upstream_error_detail") or row.get("error_body") or ""
        result["content"] = safe_error_body(raw)
        result["content_limited"] = bool(raw)  # Always identify this as a sanitized excerpt.
    return result


def account_dto(row: dict[str, Any], now: datetime, managed: set[int]) -> dict[str, Any]:
    fields = ("id", "name", "platform", "type", "status", "schedulable", "updated_at", "group_ids",
              "last_success_at", "last_error_at", "last_error_id", "last_error_code", "last_error_status")
    value = {key: row.get(key) for key in fields}
    value["name"] = clean(row.get("name"), 160)
    value["group_ids"] = row.get("group_ids") or []
    value["managed"] = row["id"] in managed
    value["error_message"] = safe_error_text(row.get("last_error_message") or row.get("error_message"))
    reasons = []
    if row.get("status") != "active":
        reasons.append({"code": "status", "label": "认证异常" if row.get("status") == "error" else "账号非 active"})
    if row.get("schedulable") is not True:
        reasons.append({"code": "disabled", "label": "调度已关闭"})
    for field, label in (("temp_unschedulable_until", "冷却中"), ("rate_limit_reset_at", "限流中"), ("overload_until", "过载保护")):
        if deadline_is_future(row.get(field), now):
            reasons.append({"code": field, "label": label, "until": row[field]})
    if row.get("expires_at") and row.get("auto_pause_on_expired") and not deadline_is_future(row["expires_at"], now):
        reasons.append({"code": "expired", "label": "到期暂停"})
    if row.get("needs_reauth"):
        reasons.append({"code": "reauth", "label": "需要重新认证"})
    value["blockers"] = reasons
    value["available"] = not reasons
    success, error = row.get("last_success_at"), row.get("last_error_at")
    value["success_after_error"] = bool(success and error and success > error)
    value["version"] = hashlib.sha256(json.dumps([row.get(k) for k in ("id", "platform", "type", "schedulable", "updated_at")], default=str).encode()).hexdigest()
    return value


ACCOUNT_SQL = f"""
SELECT a.id, a.name, a.platform, a.type, a.status, a.schedulable, a.updated_at, a.error_message,
 a.temp_unschedulable_until, a.rate_limit_reset_at, a.overload_until, a.expires_at, a.auto_pause_on_expired,
 coalesce(a.extra->>'grok_needs_reauth','false') = 'true' AS needs_reauth,
 ARRAY(SELECT ag.group_id FROM account_groups ag JOIN groups g ON g.id=ag.group_id AND g.deleted_at IS NULL WHERE ag.account_id=a.id ORDER BY ag.group_id) AS group_ids,
 u.created_at AS last_success_at, e.created_at AS last_error_at, e.id AS last_error_id,
 e.provider_error_code AS last_error_code, coalesce(e.upstream_status_code,e.status_code) AS last_error_status,
 coalesce(nullif(e.upstream_error_message,''),e.error_message) AS last_error_message
FROM accounts a
LEFT JOIN LATERAL (SELECT created_at FROM usage_logs WHERE account_id=a.id ORDER BY created_at DESC,id DESC LIMIT 1) u ON true
LEFT JOIN LATERAL (SELECT e.* FROM ops_error_logs e WHERE e.account_id=a.id AND {ERROR_WHERE} ORDER BY e.created_at DESC,e.id DESC LIMIT 1) e ON true
WHERE a.deleted_at IS NULL {{filter}} ORDER BY a.id
"""


class DesktopService:
    def __init__(self, runtime: Any) -> None:
        self.r = runtime
        self.config = ConfigService(runtime)
        self._auth: dict[str, float] = {}
        self._auth_lock = threading.Lock()
        self._snapshot_lock = threading.Lock()
        self._cached: dict[str, Any] | None = None
        self._cached_at = 0.0

    def authenticate(self, key: str, *, fresh: bool = False) -> None:
        if not key or len(key) > 4096 or any(ord(c) < 33 for c in key):
            raise HTTPException(401, "管理员 API Key 无效")
        base = self.r.oauth_base_url()
        digest = hashlib.sha256((base + "\0" + key).encode()).hexdigest()
        with self._auth_lock:
            now = time.monotonic()
            if not fresh and self._auth.get(digest, 0) > now:
                return
            request = urllib.request.Request(base + "/api/v1/admin/groups/all", headers={"x-api-key": key, "Accept": "application/json"})
            try:
                with _urlopen_no_redirect(request, timeout=5) as response:
                    body = json.loads(response.read(2_000_000))
                    if response.status != 200 or body.get("code") != 0 or not isinstance(body.get("data"), list):
                        raise ValueError("invalid admin response")
            except urllib.error.HTTPError as exc:
                self._auth.pop(digest, None)
                raise HTTPException(401 if exc.code in (401, 403) else 502, "管理员 Key 无效" if exc.code in (401, 403) else "Sub2API 验证暂不可用") from None
            except Exception:
                self._auth.pop(digest, None)
                raise HTTPException(502, "无法验证 Sub2API 管理员权限") from None
            self._auth = {k: expiry for k, expiry in self._auth.items() if expiry > now}
            if len(self._auth) > 64:
                self._auth.clear()
            self._auth[digest] = time.monotonic() + 15

    def invalidate(self) -> None:
        with self._snapshot_lock:
            self._cached_at = 0

    def snapshot(self) -> dict[str, Any]:
        with self._snapshot_lock:
            if self._cached and time.monotonic() - self._cached_at < 1.5:
                return self._cached
            r = self.r
            now = datetime.now(timezone.utc)
            config = r.key_fallback_controller.load_config() if r.key_fallback_controller else None
            managed = set(config.managed_account_ids) if config else set()
            accounts = [account_dto(row, now, managed) for row in r.db.fetch_all(ACCOUNT_SQL.format(filter=""))]
            groups = r.db.fetch_all("""
                SELECT g.id,g.name,g.platform,g.sort_order, u.id AS log_id,u.account_id,
                 a.name AS account_name,u.model,u.upstream_model,u.created_at AS called_at
                FROM groups g LEFT JOIN LATERAL (
                  SELECT id,account_id,model,upstream_model,created_at FROM usage_logs
                  WHERE group_id=g.id ORDER BY created_at DESC,id DESC LIMIT 1
                ) u ON true LEFT JOIN accounts a ON a.id=u.account_id
                WHERE g.deleted_at IS NULL ORDER BY g.sort_order,g.id
            """)
            for group in groups:
                for field in ("name", "account_name", "model", "upstream_model", "upstream_response_model"):
                    group[field] = clean(group.get(field), 160)
            errors = self.errors(None, None, 20)
            guard = r.build_model_guard_panel()
            incidents = []
            for item in guard.get("incidents", [])[:100]:
                allowed = {key: value for key, value in item.items() if key in {
                    "account_id", "account_name", "platform", "account_type", "type", "requested_model", "upstream_model", "response_model",
                    "classification", "reason", "action", "action_reason", "first_seen_at", "last_seen_at", "count", "historical", "outcome", "log_id",
                    "first_at", "last_at", "latest_at", "history", "status", "message", "model", "removed", "removal_reason", "kind",
                }}
                incidents.append({k: clean(v, 500) if isinstance(v, str) else v for k, v in allowed.items()})
            self._cached = jsonable_encoder({"schema_version": 1, "observed_at": now, "accounts": accounts,
                                           "groups": groups, "errors": errors["items"], "incidents": incidents})
            self._cached_at = time.monotonic()
            return self._cached

    def errors(self, account_id: int | None, before_id: int | None, limit: int = 50) -> dict[str, Any]:
        rows = self.r.db.fetch_all(f"""SELECT {ERROR_FIELDS} FROM ops_error_logs e
          LEFT JOIN accounts a ON a.id=e.account_id LEFT JOIN groups g ON g.id=e.group_id
          WHERE {ERROR_WHERE} AND (%(account_id)s::bigint IS NULL OR e.account_id=%(account_id)s)
            AND (%(before_id)s::bigint IS NULL OR e.id < %(before_id)s)
          ORDER BY e.id DESC LIMIT %(limit)s""", {"account_id": account_id, "before_id": before_id, "limit": limit + 1})
        return {"items": [error_dto(row) for row in rows[:limit]], "next_cursor": rows[limit-1]["id"] if len(rows) > limit else None}

    def error_detail(self, error_id: int) -> dict[str, Any]:
        row = self.r.db.fetch_one(f"""SELECT {ERROR_FIELDS},e.error_body,e.upstream_error_detail FROM ops_error_logs e
          LEFT JOIN accounts a ON a.id=e.account_id LEFT JOIN groups g ON g.id=e.group_id
          WHERE {ERROR_WHERE} AND e.id=%(id)s""", {"id": error_id})
        if not row:
            raise HTTPException(404, "错误记录不存在或已过保留期")
        return error_dto(row, detail=True)

    def set_schedulable(self, account_id: int, payload: Any, key: str) -> dict[str, Any]:
        r, controller = self.r, self.r.key_fallback_controller
        if controller is None:
            raise HTTPException(503, "调度控制器未就绪")
        # The automatic controller takes this same lock for evaluation + dispatch.
        with self.config.thread_lock, controller._lock:
            row = r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": account_id})
            if not row:
                raise HTTPException(404, "账号不存在")
            config = controller.load_config()
            if not config.valid:
                raise HTTPException(409, "托管配置无法读取，操作已中止")
            account = account_dto(row, datetime.now(timezone.utc), set(config.managed_account_ids))
            if payload.expected_version != account["version"]:
                raise HTTPException(409, "账号已变化，请刷新后重试")
            detached = False
            if account["managed"]:
                if not payload.detach_managed:
                    raise HTTPException(409, "此账号由 Key 回退托管，请确认解除托管")
                # Preserve other selections, including any stale IDs, instead of revalidating all of them.
                controller._write_config_unlocked(openai_enabled=config.openai_enabled, grok_enabled=config.grok_enabled,
                    managed_account_ids=[i for i in config.managed_account_ids if i != account_id],
                    config_version=config.config_version + 1, updated_by="desktop:admin")
                detached = True
                write_audit(r.settings.audit_path, "desktop_detach_managed", {"account_id": account_id})
            result = execute_sub2api_set_schedulable(account_id, payload.schedulable, base_url=r.oauth_base_url(),
                admin_token=key, timeout_seconds=3, urlopen=_urlopen_no_redirect)
            live = r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": account_id})
            verified = bool(result.get("success") and live and live.get("schedulable") is payload.schedulable
                            and live.get("platform") == row.get("platform") and live.get("type") == row.get("type"))
            write_audit(r.settings.audit_path, "desktop_schedulable", {"account_id": account_id, "schedulable": payload.schedulable,
                        "verified": verified, "detached": detached, "error_code": result.get("error_code", "")})
        self.invalidate()
        if not verified:
            raise HTTPException(502, {"message": "调度写入未确认，请刷新查看实际状态", "detached": detached,
                                      "code": result.get("error_code") or "readback_mismatch"})
        return {"verified": True, "detached": detached, "account_id": account_id, "schedulable": payload.schedulable}


class ScheduleRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    schedulable: StrictBool
    expected_version: str = Field(min_length=64, max_length=64)
    detach_managed: StrictBool = False


class ConfigRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_revision: str = Field(min_length=64, max_length=64)
    changes: dict[str, Any]


def install_desktop_api(app: Any, runtime: Any) -> DesktopService:
    service = DesktopService(runtime)
    router = APIRouter(prefix=PREFIX)

    async def auth(request: Request) -> str:
        key = request.headers.get("x-api-key", "")
        await asyncio.to_thread(service.authenticate, key, fresh=request.method != "GET")
        return key

    @router.get("/capabilities")
    async def capabilities(request: Request) -> dict[str, Any]:
        await auth(request)
        return {"api_version": 1, "service": "Sub2API Ops Companion", "platforms": ["macos"], "version": runtime.APP_VERSION}

    @router.get("/snapshot")
    async def snapshot(request: Request) -> Any:
        await auth(request)
        return await asyncio.to_thread(service.snapshot)

    @router.get("/errors")
    async def errors(request: Request, account_id: int | None = None, before_id: int | None = None, limit: int = 50) -> Any:
        await auth(request)
        if not 1 <= limit <= 100 or (account_id is not None and account_id < 1) or (before_id is not None and before_id < 1):
            raise HTTPException(422, "分页参数无效")
        return await asyncio.to_thread(service.errors, account_id, before_id, limit)

    @router.get("/errors/{error_id}")
    async def error_detail(error_id: int, request: Request) -> Any:
        await auth(request)
        return await asyncio.to_thread(service.error_detail, error_id)

    @router.post("/accounts/{account_id}/schedulable")
    async def schedule(account_id: int, payload: ScheduleRequest, request: Request) -> Any:
        key = await auth(request)
        async with service.config.lock:
            return await asyncio.to_thread(service.set_schedulable, account_id, payload, key)

    @router.get("/config")
    async def config(request: Request) -> Any:
        await auth(request)
        return await asyncio.to_thread(service.config.snapshot)

    @router.put("/config/{section}")
    async def save_config(section: str, payload: ConfigRequest, request: Request) -> Any:
        await auth(request)
        try:
            result = await service.config.save(section, payload.changes, "desktop:admin", payload.expected_revision)
        except ConfigConflict as exc:
            raise HTTPException(409, str(exc)) from None
        except ValueError as exc:
            raise HTTPException(422, str(exc)) from None
        service.invalidate()
        return result

    @router.post("/actions/{action}")
    async def action_run(action: str, request: Request) -> Any:
        await auth(request)
        if action == "bark-test":
            result = await asyncio.to_thread(runtime.bark_notifier.push_test)
            if not result.success:
                raise HTTPException(502, f"Bark 测试失败：{result.error_code}")
            return {"message": "Bark 测试消息已发送"}
        if action == "telegram-test":
            bot = runtime.telegram_bot
            if bot is None or not bot.enabled or not await bot.allowed_chat_ids():
                raise HTTPException(409, "请先配置并配对 Telegram")
            await bot.notify("Sub2Ops 客户端消息测试")
            return {"message": "Telegram Bot 测试消息已发送"}
        if action == "telegram-pairing":
            return {"pairing_code": await service.config.regenerate_pairing("desktop:admin")}
        raise HTTPException(404, "未知操作")

    app.include_router(router)
    return service
