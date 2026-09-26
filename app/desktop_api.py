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
from contextlib import contextmanager, nullcontext
from datetime import datetime, timezone
from typing import Any, Literal

from fastapi import APIRouter, HTTPException, Request
from fastapi.encoders import jsonable_encoder
from fastapi.responses import StreamingResponse
from pydantic import BaseModel, ConfigDict, Field, StrictBool

from .audit import write_audit
from .bark import _urlopen_no_redirect, sanitize_error_text
from .config_service import ConfigConflict, ConfigService
from .key_fallback import deadline_is_future, execute_sub2api_set_schedulable, latest_completed_oauth_result
from .quota_snapshot import usage_windows
from .desktop_usage import attach_stats, project_usage, read_stats, reset_credits, stats_specs
from .desktop_actions import DesktopActions, PriorityRequest, TestRequest
from .desktop_errors import DesktopErrorMiddleware, DesktopRoute
from .account_quality import QualityCache, SUPPORTED

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
        result["content_limited"] = "..." in result["content"]
    return result


def account_dto(row: dict[str, Any], now: datetime, managed: set[int], quota_result: dict[str, Any] | None = None) -> dict[str, Any]:
    fields = ("id", "name", "platform", "type", "status", "schedulable", "updated_at", "group_ids", "priority",
              "last_success_at", "last_error_at", "last_error_id", "last_error_code", "last_error_status")
    value = {key: row.get(key) for key in fields}
    value["name"] = clean(row.get("name"), 160)
    value["priority"] = int(value["priority"] or 0)
    value["group_ids"] = row.get("group_ids") or []
    value["usage_windows"] = usage_windows(row, now, quota_result)
    value["managed"] = row["id"] in managed
    value["recoverable"] = recoverable_state(row, now)
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
    value["version"] = hashlib.sha256(json.dumps([row.get(k) for k in ("id", "platform", "type", "schedulable", "updated_at", "priority")], default=str).encode()).hexdigest()
    return value


def recoverable_state(row: dict[str, Any], now: datetime) -> bool:
    if row.get("status") == "error" or any(deadline_is_future(row.get(field), now) for field in
            ("rate_limit_reset_at", "overload_until", "temp_unschedulable_until")):
        return True
    extra = row.get("extra") or {}
    limits = extra.get("model_rate_limits") if isinstance(extra, dict) else None
    return isinstance(limits, dict) and any(isinstance(info, dict) and
        deadline_is_future(info.get("rate_limit_reset_at"), now) for info in limits.values())


ACCOUNT_SQL = f"""
SELECT a.id, a.name, a.platform, a.type, a.status, a.schedulable, a.updated_at, a.priority, a.error_message, a.extra,
 nullif(to_jsonb(a)->>'parent_account_id','')::bigint AS parent_account_id,
 coalesce(nullif(a.credentials->>'plan_type',''),nullif(a.credentials->>'chatgpt_plan_type',''),a.extra->>'plan_type') AS quota_plan_type,
 a.credentials->>'subscription_tier' AS quota_grok_tier, a.credentials->>'entitlement_status' AS quota_grok_entitlement,
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

GROUP_SQL = """
SELECT g.id,g.name,g.platform,g.sort_order,u.id AS log_id,u.account_id,
 a.name AS account_name,u.model,u.upstream_model,u.created_at AS called_at
FROM groups g LEFT JOIN LATERAL (
 SELECT latest.* FROM (
   SELECT DISTINCT ON (l.account_id) l.id,l.account_id,l.model,l.upstream_model,l.created_at
   FROM usage_logs l JOIN accounts live ON live.id=l.account_id AND live.deleted_at IS NULL
   WHERE l.group_id=g.id
   ORDER BY l.account_id,l.created_at DESC,l.id DESC
 ) latest ORDER BY created_at DESC,id DESC LIMIT 3
) u ON true LEFT JOIN accounts a ON a.id=u.account_id
WHERE g.deleted_at IS NULL ORDER BY g.sort_order,g.id,u.created_at DESC,u.id DESC
"""


def group_dtos(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    groups: dict[int, dict[str, Any]] = {}
    for row in rows:
        group = groups.setdefault(row["id"], {**row, "recent_accounts": []})
        if row.get("account_id") and not any(item["account_id"] == row["account_id"] for item in group["recent_accounts"]):
            item = {key: row.get(key) for key in ("log_id", "account_id", "account_name", "model", "upstream_model", "called_at")}
            for key in ("account_name", "model", "upstream_model"):
                item[key] = clean(item.get(key), 160)
            group["recent_accounts"].append(item)
    for group in groups.values():
        for field in ("name", "account_name", "model", "upstream_model", "upstream_response_model"):
            group[field] = clean(group.get(field), 160)
    return list(groups.values())


class DesktopService:
    def __init__(self, runtime: Any) -> None:
        self.r = runtime
        self.config = ConfigService(runtime)
        self._auth: dict[str, float] = {}
        self._auth_lock = threading.Lock()
        self._snapshot_lock = threading.Lock()
        self._cached: dict[str, Any] | None = None
        self._cached_at = 0.0
        self._stats: dict = {}
        self._stats_at = 0.0
        self._stats_signature: list = []
        self._usage_locks: dict[int, threading.Lock] = {}
        self._usage_locks_guard = threading.Lock()
        self._uncertain_resets: set[int] = set()
        self.actions = DesktopActions(self)
        self.quality = QualityCache(getattr(runtime, "db", None))

    async def close(self) -> None:
        self.quality.close()
        await self.actions.close()

    def account_lock(self, account_id: int) -> threading.Lock:
        with self._usage_locks_guard:
            return self._usage_locks.setdefault(account_id, threading.Lock())

    @contextmanager
    def recovery_guard(self, row: dict[str, Any]):
        monitor = getattr(self.r, "oauth_monitor", None)
        lock = monitor._run_lock if row["platform"] == "openai" and monitor else None
        if lock and not lock.acquire(blocking=False):
            raise HTTPException(409, "OAuth 查询或恢复正在进行，请稍后操作")
        try:
            yield
        finally:
            if lock:
                lock.release()

    @contextmanager
    def account_operation(self, account_id: int, version: str, *, coordinate_monitor: bool = True):
        if account_id < 1:
            raise HTTPException(422, "账号编号无效")
        lock = self.account_lock(account_id)
        if not lock.acquire(blocking=False):
            raise HTTPException(409, "此账号正在执行操作，请等待结果")
        try:
            row = self.r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": account_id})
            if not row:
                raise HTTPException(404, "账号不存在")
            with self.recovery_guard(row) if coordinate_monitor else nullcontext():
                row = self.r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": account_id})
                if not row or account_dto(row, datetime.now(timezone.utc), set())["version"] != version:
                    raise HTTPException(409, "账号已变化，请刷新后重试")
                yield row
        finally:
            lock.release()

    def _account_write(self, account_id: int, action: str, key: str) -> str:
        path = f"/api/v1/admin/accounts/{account_id}" + ("/recover-state" if action == "recover" else "")
        request = urllib.request.Request(self.r.oauth_base_url().rstrip("/") + path,
            method="POST" if action == "recover" else "DELETE", data=b"{}" if action == "recover" else None,
            headers={"x-api-key": key, "Accept": "application/json", "Content-Type": "application/json"})
        try:
            with _urlopen_no_redirect(request, timeout=5) as response:
                body = json.loads(response.read(2_000_000))
                if 200 <= response.status < 300 and isinstance(body, dict) and body.get("code") == 0:
                    return "ok"
                return "upstream_rejected"
        except urllib.error.HTTPError as exc:
            return f"http_{exc.code}"
        except Exception:
            return "result_uncertain"

    def delete_account(self, account_id: int, payload: "DeleteRequest", key: str) -> dict[str, Any]:
        controller = self.r.key_fallback_controller
        if controller is None:
            raise HTTPException(503, "调度控制器未就绪")
        detached = False
        try:
            with (self.account_operation(account_id, payload.expected_version, coordinate_monitor=False) as row,
                  self.config.thread_lock, controller._lock, self.recovery_guard(row)):
                # Recheck after obtaining the fallback lock; never delete from a stale selection.
                live = self.r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": account_id})
                if not live or account_dto(live, datetime.now(timezone.utc), set())["version"] != payload.expected_version:
                    raise HTTPException(409, "账号已变化，请刷新后重试")
                config = controller.load_config()
                if not config.valid:
                    raise HTTPException(409, "托管配置无法读取，删除已中止")
                if account_id in config.managed_account_ids:
                    if not payload.detach_managed:
                        raise HTTPException(409, "此账号由 Key 回退托管，请刷新并确认解除托管后删除")
                    try:
                        controller._write_config_unlocked(openai_enabled=config.openai_enabled, grok_enabled=config.grok_enabled,
                            managed_account_ids=[i for i in config.managed_account_ids if i != account_id],
                            config_version=config.config_version + 1, updated_by="desktop:admin")
                    except Exception:
                        raise HTTPException(503, "解除托管保存失败，未执行删除") from None
                    detached = True
                    write_audit(self.r.settings.audit_path, "desktop_detach_managed", {"account_id": account_id})
                code = self._account_write(account_id, "delete", key)
                try:
                    live = self.r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": account_id})
                    verified = live is None
                except Exception:
                    verified = False
                write_audit(self.r.settings.audit_path, "desktop_delete", {"account_id": account_id,
                    "verified": verified, "detached": detached, "code": code})
                if not verified:
                    raise HTTPException(502, {"message": ("已解除托管；" if detached else "") + "删除未确认，请刷新核对实际状态",
                        "detached": detached, "code": code if code != "ok" else "readback_mismatch"})
                return {"account_id": account_id, "deleted": True, "verified": True, "detached": detached}
        finally:
            self.invalidate()

    def recover_account(self, account_id: int, payload: "AccountVersionRequest", key: str) -> dict[str, Any]:
        try:
            with self.account_operation(account_id, payload.expected_version) as row:
                if not recoverable_state(row, datetime.now(timezone.utc)):
                    raise HTTPException(409, "账号已无可恢复状态，请刷新")
                code = self._account_write(account_id, "recover", key)
                try:
                    live = self.r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": account_id})
                    verified = bool(live and (live["platform"], live["type"]) == (row["platform"], row["type"])
                        and not recoverable_state(live, datetime.now(timezone.utc)))
                except Exception:
                    verified = False
                write_audit(self.r.settings.audit_path, "desktop_recover_state", {"account_id": account_id,
                    "verified": verified, "code": code})
                if not verified:
                    raise HTTPException(502, {"message": "恢复状态未确认，请刷新核对实际状态",
                        "code": code if code != "ok" else "readback_mismatch"})
                return {"account_id": account_id, "verified": True, "message": "状态已恢复"}
        finally:
            self.invalidate()

    def recoveries(self, before_id: int | None = None, limit: int = 50) -> dict[str, Any]:
        monitor = getattr(self.r, "oauth_monitor", None)
        records = list((monitor.store.cached_snapshot().get("recovery_history") or {}).values()) if monitor else []
        records = sorted((r for r in records if before_id is None or r["id"] < before_id), key=lambda r: r["id"], reverse=True)
        names = {r["id"]: r["name"] for r in self.r.db.fetch_all("SELECT id,name FROM accounts WHERE deleted_at IS NULL")} if records else {}
        records = [r for r in records if r.get("account_id") in names]
        items = [{**{k: row.get(k) for k in ("id", "account_id", "test_completed_at", "recovered_at", "legacy")},
                  "account_name": clean(row.get("account_name") or names.get(row.get("account_id")), 160),
                  "model_id": clean(row.get("model_id"), 160)} for row in records[:limit]]
        return {"items": items, "next_cursor": items[-1]["id"] if len(records) > limit else None}

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
            self._stats_at = 0

    def snapshot(self) -> dict[str, Any]:
        with self._snapshot_lock:
            if self._cached and time.monotonic() - self._cached_at < 1.5:
                return self._cached
            r = self.r
            now = datetime.now(timezone.utc)
            config = r.key_fallback_controller.load_config() if r.key_fallback_controller else None
            managed = set(config.managed_account_ids) if config else set()
            monitor = getattr(r, "oauth_monitor", None)
            state = monitor.store.cached_snapshot() if monitor else {}
            saved, scheduler = state.get("oauth_results", {}), state.get("scheduler", {})
            accounts, specs = [], []
            for row in r.db.fetch_all(ACCOUNT_SQL.format(filter="")):
                result = latest_completed_oauth_result(saved.get(str(row["id"])), scheduler.get(str(row["id"])))
                account = account_dto(row, now, managed, result)
                account["usage"] = project_usage(row, now, result)
                specs.extend(stats_specs(row["id"], account["usage"]))
                accounts.append(account)
            # Rolling windows move continuously; cache the batch for 15 seconds.
            # Fixed window/reset changes invalidate within the current minute.
            signature = [(s["account_id"], s["key"], s["start_at"][:16]) for s in specs]
            if time.monotonic() - self._stats_at >= 15 or signature != self._stats_signature:
                try:
                    self._stats = read_stats(r.db, specs)
                except Exception:
                    # Missing statistics are unknown, never synthesized as zero.
                    self._stats = {}
                self._stats_at, self._stats_signature = time.monotonic(), signature
            for account in accounts:
                attach_stats(account["id"], account["usage"], self._stats, free_token_limit=500_000)
                account["usage_windows"] = account["usage"]["windows"]
            qualities = self.quality.get([a["id"] for a in accounts if a["platform"] in ("openai", "grok") and a["type"] in ("oauth", "apikey")])
            for account in accounts:
                if account["id"] in qualities:
                    account["quality"] = qualities[account["id"]]
            groups = group_dtos(r.db.fetch_all(GROUP_SQL))
            errors = self.errors(None, None, 20)
            self._cached = jsonable_encoder({"schema_version": 1, "observed_at": now, "accounts": accounts,
                                           "groups": groups, "errors": errors["items"],
                                           "recoveries": self.recoveries(limit=20)["items"]})
            self._cached_at = time.monotonic()
            return self._cached

    def quality_detail(self, account_id: int) -> dict[str, Any]:
        row = self.r.db.fetch_one(f"SELECT id FROM accounts WHERE id=%(id)s AND {SUPPORTED}", {"id": account_id})
        if not row:
            raise HTTPException(404, "账号不存在或不支持质量评分")
        return self.quality.get([account_id], detail=True)[account_id]

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
        with self.account_operation(account_id, payload.expected_version):
            return self._set_schedulable(account_id, payload, key)

    def _set_schedulable(self, account_id: int, payload: Any, key: str) -> dict[str, Any]:
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

    def usage_action(self, account_id: int, payload: UsageActionRequest, key: str) -> dict[str, Any]:
        if account_id <= 0:
            raise HTTPException(422, "账号编号无效")
        lock = self.account_lock(account_id)
        if not lock.acquire(blocking=False):
            raise HTTPException(409, "此账号正在执行用量操作，请等待结果")
        monitor_lock = None
        try:
            row = self.r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": account_id})
            if not row:
                raise HTTPException(404, "账号已删除")
            now = datetime.now(timezone.utc)
            if account_dto(row, now, set())["version"] != payload.expected_version:
                raise HTTPException(409, "账号已变化，请刷新后再操作")
            usage = project_usage(row, now)
            if payload.action not in usage["actions"]:
                raise HTTPException(422, "此账号不支持该用量操作")
            if payload.action in {"reset_quota", "probe_quota"} and not payload.confirmed:
                raise HTTPException(409, "请先确认重置次数消耗或模型探测请求")
            if payload.action == "reset_quota":
                if account_id in self._uncertain_resets:
                    raise HTTPException(409, "上次重置结果不确定，请先查询次数确认，禁止重复重置")
                if not (reset_credits(row.get("extra") or {}, now)["available"] or 0):
                    raise HTTPException(409, "没有已确认可用的重置次数，请先查询次数")
            monitor = getattr(self.r, "oauth_monitor", None)
            if row["platform"] == "openai" and monitor:
                monitor_lock = monitor._run_lock
                if not monitor_lock.acquire(blocking=False):
                    monitor_lock = None
                    raise HTTPException(409, "OAuth 查询或恢复正在进行，请稍后操作")
            action_paths = {
                "query_usage": ("GET", f"/accounts/{account_id}/usage?source=active&force=true", 30),
                "query_reset_credits": ("POST", f"/openai/accounts/{account_id}/quota/refresh", 30),
                "reset_quota": ("POST", f"/openai/accounts/{account_id}/reset-quota", 90),
                "probe_quota": ("GET", f"/grok/accounts/{account_id}/quota", 90),
            }
            method, path, timeout = action_paths[payload.action]
            request = urllib.request.Request(self.r.oauth_base_url().rstrip("/") + "/api/v1/admin" + path,
                method=method, data=b"{}" if method == "POST" else None,
                headers={"x-api-key": key, "Accept": "application/json", "Content-Type": "application/json"})
            if payload.action == "reset_quota":
                self._uncertain_resets.add(account_id)
            code = "unknown"
            try:
                with _urlopen_no_redirect(request, timeout=timeout) as response:
                    body = json.loads(response.read(2_000_000))
                    if not 200 <= response.status < 300 or body.get("code") != 0:
                        code = "upstream_rejected"
                        raise ValueError("upstream rejected")
                data = body.get("data") or {}
                if not isinstance(data, dict):
                    raise ValueError("invalid result")
                if payload.action == "reset_quota" and data.get("code") not in (None, "success", "ok", 0):
                    code = clean(data.get("code"), 80)
                    raise ValueError("reset rejected")
                if payload.action == "probe_quota" and (data.get("probe_error") or int(data.get("status_code") or 200) >= 400):
                    code = "probe_failed"
                    raise ValueError("probe failed")
                if payload.action == "query_reset_credits" and data.get("cache_persisted") is False:
                    code = "snapshot_not_saved"
                    raise ValueError("query snapshot not saved")
                code = "ok"
            except urllib.error.HTTPError as exc:
                code = f"http_{exc.code}"
            except Exception:
                if code == "unknown":
                    code = "result_uncertain"
            finally:
                self.invalidate()
                write_audit(self.r.settings.audit_path, "desktop_usage_action",
                            {"account_id": account_id, "action": payload.action, "code": code})
            if code != "ok":
                raise HTTPException(502, {"message": "操作未确认，未自动重放；请先查询确认实际结果", "code": code})
            live = self.r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": account_id})
            if not live or (live["platform"], live["type"]) != (row["platform"], row["type"]):
                raise HTTPException(409, "操作后账号已变化，请刷新确认结果")
            warning = payload.action == "reset_quota" and (data.get("cache_refreshed") is False or data.get("warning_code"))
            if payload.action == "query_reset_credits" or payload.action == "reset_quota" and not warning:
                self._uncertain_resets.discard(account_id)
            # Re-read the actual saved state, never mirror arbitrary action JSON.
            account = next((a for a in self.snapshot()["accounts"] if a["id"] == account_id), None)
            return {"message": "重置已执行，但快照更新未确认；请查询次数，勿重复重置" if warning else {"query_usage": "用量查询完成", "query_reset_credits": "重置次数已查询",
                                "reset_quota": "重置已执行", "probe_quota": "探测完成"}[payload.action],
                    "account": account}
        finally:
            if monitor_lock:
                monitor_lock.release()
            lock.release()


class ScheduleRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    schedulable: StrictBool
    expected_version: str = Field(min_length=64, max_length=64)
    detach_managed: StrictBool = False


class AccountVersionRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_version: str = Field(min_length=64, max_length=64)


class DeleteRequest(AccountVersionRequest):
    detach_managed: StrictBool = False


class ConfigRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_revision: str = Field(min_length=64, max_length=64)
    changes: dict[str, Any]


class UsageActionRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    action: Literal["query_usage", "query_reset_credits", "reset_quota", "probe_quota"]
    expected_version: str = Field(min_length=64, max_length=64)
    confirmed: StrictBool = False


def install_desktop_api(app: Any, runtime: Any) -> DesktopService:
    service = DesktopService(runtime)
    router = APIRouter(prefix=PREFIX, route_class=DesktopRoute)
    app.add_middleware(DesktopErrorMiddleware)

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

    @router.get("/accounts/{account_id}/quality")
    async def quality_detail(account_id: int, request: Request) -> Any:
        await auth(request)
        if account_id < 1:
            raise HTTPException(422, "账号编号无效")
        return await asyncio.to_thread(service.quality_detail, account_id)

    @router.get("/errors/{error_id}")
    async def error_detail(error_id: int, request: Request) -> Any:
        await auth(request)
        return await asyncio.to_thread(service.error_detail, error_id)

    @router.post("/accounts/{account_id}/schedulable")
    async def schedule(account_id: int, payload: ScheduleRequest, request: Request) -> Any:
        key = await auth(request)
        async with service.config.lock:
            return await asyncio.to_thread(service.set_schedulable, account_id, payload, key)

    @router.get("/recoveries")
    async def recoveries(request: Request, before_id: int | None = None, limit: int = 50) -> Any:
        await auth(request)
        if not 1 <= limit <= 100 or (before_id is not None and before_id < 1):
            raise HTTPException(422, "分页参数无效")
        return await asyncio.to_thread(service.recoveries, before_id, limit)

    @router.delete("/accounts/{account_id}")
    async def delete_account(account_id: int, payload: DeleteRequest, request: Request) -> Any:
        key = await auth(request)
        return await asyncio.to_thread(service.delete_account, account_id, payload, key)

    @router.post("/accounts/{account_id}/recover-state")
    async def recover_account(account_id: int, payload: AccountVersionRequest, request: Request) -> Any:
        key = await auth(request)
        return await asyncio.to_thread(service.recover_account, account_id, payload, key)

    @router.post("/accounts/{account_id}/priority")
    async def priority(account_id: int, payload: PriorityRequest, request: Request) -> Any:
        key = await auth(request)
        return await service.actions.set_priority(account_id, payload, key)

    @router.get("/accounts/{account_id}/models")
    async def models(account_id: int, request: Request) -> Any:
        key = await auth(request)
        return await service.actions.models(account_id, key)

    @router.post("/accounts/{account_id}/test")
    async def test(account_id: int, payload: TestRequest, request: Request) -> Any:
        key = await auth(request)
        async def stream():
            try:
                locks = await service.actions.prepare_test(account_id, payload)
                async for chunk in service.actions.test_stream(account_id, payload, key, locks):
                    yield chunk
            except HTTPException as exc:
                yield "data: " + json.dumps({"type": "error", "error": safe_error_text(exc.detail, 240)}) + "\n\n"
        return StreamingResponse(stream(),
            media_type="text/event-stream", headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"})

    @router.post("/quota-refresh")
    async def quota_refresh(request: Request) -> Any:
        key = await auth(request)
        return await service.actions.start_batch(key)

    @router.get("/quota-refresh")
    async def quota_progress(request: Request) -> Any:
        await auth(request)
        return service.actions.batch_view()

    @router.get("/config")
    async def config(request: Request) -> Any:
        await auth(request)
        return await asyncio.to_thread(service.config.snapshot)

    @router.post("/accounts/{account_id}/usage-action")
    async def usage_action(account_id: int, payload: UsageActionRequest, request: Request) -> Any:
        key = await auth(request)
        return await asyncio.to_thread(service.usage_action, account_id, payload, key)

    @router.put("/config/{section}")
    async def save_config(section: str, payload: ConfigRequest, request: Request) -> Any:
        if section not in {"oauth", "bark", "key_fallback"}:
            raise HTTPException(404, "设置分区不存在")
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
        if action != "bark-test":
            raise HTTPException(404, "未知操作")
        await auth(request)
        if action == "bark-test":
            result = await asyncio.to_thread(runtime.bark_notifier.push_test)
            if not result.success:
                raise HTTPException(502, f"Bark 测试失败：{result.error_code}")
            return {"message": "Bark 测试消息已发送"}
        raise HTTPException(404, "未知操作")

    app.include_router(router)
    return service
