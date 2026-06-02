from __future__ import annotations

import secrets
import hashlib
import hmac
import time
import asyncio
import json
import math
import urllib.error
import urllib.request
from contextlib import asynccontextmanager, suppress
from dataclasses import asdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Annotated, Any
from urllib.parse import parse_qsl, quote, urlencode, urlsplit, urlunsplit
from zoneinfo import ZoneInfo

from fastapi import Depends, FastAPI, Form, HTTPException, Request, Response
from fastapi.responses import HTMLResponse, RedirectResponse
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates

from . import account_ops
from . import usage_query as usage_query_module
from .audit import read_audit, write_audit
from .db import Database
from .group_selection import ALL_GROUP_VALUE, DEFAULT_GROUP_NAME, build_group_selection, unique_group_values
from .guard_engine import GuardEngine, is_oauth_account
from .guard_policy import GuardPolicy, is_whitelisted_account, normalize_account_ids
from .guard_queue import auto_queue_plan, group_queue_rows, membership_key, queue_position, reorder_queue_plan
from .guard_store import GuardStore
from .quality_sort import (
    STABILITY_SORT_OPTIONS,
    normalize_stability_sort,
    sort_speed_rows,
    sort_stability_rows,
)
from .settings import load_settings
from .secure_session import create_session_cookie, read_session_cookie
from .sql import (
    ACCOUNT_OPTIONS_SQL,
    ACCOUNT_ROUTING_CAPABILITY_SQL,
    GROUPS_SQL,
    GUARD_BALANCE_CANDIDATES_SQL,
    GUARD_QUEUE_SQL,
    GUARD_QUEUE_SQL_COMPAT_NO_LOAD_FACTOR,
    QUALITY_ALL_ACCOUNTS_SQL,
    QUALITY_ALL_ACCOUNTS_SQL_COMPAT_NO_LOAD_FACTOR,
    QUALITY_SQL_COMPAT_NO_LOAD_FACTOR,
    SCHEDULED_TEST_ACCOUNTS_SQL_COMPAT_NO_LOAD_FACTOR,
    PLATFORM_OPTIONS_SQL,
    QUALITY_SQL,
    REQUESTS_SQL,
    SPEED_SQL,
    SPEED_SQL_COMPAT_NO_LOAD_FACTOR,
    SCHEDULED_TEST_ACCOUNTS_SQL,
    SCHEDULED_TEST_CAPABILITY_SQL,
    SCHEDULED_TEST_DELETE_SQL,
    SCHEDULED_TEST_RECOVERY_ALERTS_SQL,
    SCHEDULED_TEST_RESULTS_SQL,
    SCHEDULED_TEST_UPSERT_SQL,
    TELEGRAM_ERROR_ALERTS_SQL,
)
from .scheduled_tests import (
    interval_from_cron,
    interval_options,
    next_aligned_run,
    normalize_interval_minutes,
    schedule_cron,
)
from .sso_config import (
    SSORuntimeConfig,
    build_sso_panel_config,
    load_sso_runtime_config,
    save_sso_config,
)
from .sub2api_sso import Sub2APISSOError, normalize_base_url, validate_sub2api_token
from .telegram_bot import TelegramOpsBot
from .time_range import build_time_range, clean_query_string, rolling_hours_range
from .usage_query import (
    TEMPLATE_LABELS,
    UsageQueryConfig,
    UsageQueryStore,
    actual_available,
    account_credentials,
    apply_account_credentials,
    default_template,
    execute_oauth_usage_query,
    execute_usage_query,
    fill_account_credentials,
    is_query_due,
    normalize_template_code,
    normalize_template_type,
    oauth_account_recovery_candidate_from_probe,
    oauth_account_recovery_early_probe_due,
    oauth_account_recovery_probe_due,
    public_config,
    should_pause_for_depleted,
)
from .versioning import APP_VERSION, UpdateError, perform_update, restart_process_soon, version_info

settings = load_settings()
db = Database(settings.database_url)
templates = Jinja2Templates(directory="app/templates")
SESSION_COOKIE = "sub2ops_session"
BEIJING_TZ = ZoneInfo("Asia/Shanghai")
guard_lock = asyncio.Lock()
guard_state: dict[str, Any] = {
    "enabled": settings.guard_enabled,
    "running": False,
    "last_run_at": None,
    "last_error": "",
    "last_error_at": "",
    "last_error_notified": "",
    "last_actions": [],
}
telegram_bot: TelegramOpsBot | None = None
telegram_task: asyncio.Task[None] | None = None
telegram_error_alert_task: asyncio.Task[None] | None = None
telegram_recovery_alert_task: asyncio.Task[None] | None = None
TELEGRAM_PAIRING_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"


def beijing_time(value: Any) -> str:
    if value in (None, ""):
        return "-"
    parsed: datetime | None = None
    if isinstance(value, datetime):
        parsed = value
    elif isinstance(value, str):
        raw = value.strip()
        if not raw:
            return "-"
        try:
            parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
        except ValueError:
            return raw
    if parsed is None:
        return str(value)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=BEIJING_TZ)
    return parsed.astimezone(BEIJING_TZ).strftime("%Y-%m-%d %H:%M")


def beijing_time_after_seconds(value: Any) -> str:
    try:
        seconds = int(float(str(value)))
    except (TypeError, ValueError):
        return "-"
    if seconds <= 0:
        return "已重置"
    return beijing_time(datetime.now(timezone.utc) + timedelta(seconds=seconds))


def integer_with_commas(value: Any) -> str:
    try:
        return f"{int(value or 0):,}"
    except (TypeError, ValueError):
        return "0"


def quota_number(value: Any) -> str:
    if value in (None, ""):
        return "-"
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return "-"
    text = f"{parsed:,.4f}".rstrip("0").rstrip(".")
    return text or "0"


templates.env.filters["bj_time"] = beijing_time
templates.env.filters["bj_after"] = beijing_time_after_seconds
templates.env.filters["int_commas"] = integer_with_commas
templates.env.filters["quota"] = quota_number


@asynccontextmanager
async def lifespan(_: FastAPI):
    global telegram_bot, telegram_task, telegram_error_alert_task, telegram_recovery_alert_task
    db.open()
    guard_task: asyncio.Task[None] | None = None
    await restart_telegram_bot()
    if settings.guard_enabled:
        guard_task = asyncio.create_task(auto_guard_loop())
    telegram_error_alert_task = asyncio.create_task(telegram_error_alert_loop())
    telegram_recovery_alert_task = asyncio.create_task(telegram_recovery_alert_loop())
    try:
        yield
    finally:
        if telegram_task:
            telegram_task.cancel()
            with suppress(asyncio.CancelledError):
                await telegram_task
            telegram_task = None
        if guard_task:
            guard_task.cancel()
            with suppress(asyncio.CancelledError):
                await guard_task
        if telegram_error_alert_task:
            telegram_error_alert_task.cancel()
            with suppress(asyncio.CancelledError):
                await telegram_error_alert_task
            telegram_error_alert_task = None
        if telegram_recovery_alert_task:
            telegram_recovery_alert_task.cancel()
            with suppress(asyncio.CancelledError):
                await telegram_recovery_alert_task
            telegram_recovery_alert_task = None
        telegram_bot = None
        db.close()


app = FastAPI(title=settings.app_name, lifespan=lifespan)
app.mount("/static", StaticFiles(directory="app/static"), name="static")


def cookie_path() -> str:
    return settings.base_path or "/"


def sign_legacy_session(username: str, issued_at: int) -> str:
    payload = f"{username}:{issued_at}"
    signature = hmac.new(
        settings.session_secret.encode("utf-8"),
        payload.encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()
    return f"{payload}:{signature}"


def sign_session(
    username: str,
    issued_at: int,
    *,
    ttl_seconds: int | None = None,
    source: str = "password",
) -> str:
    return create_session_cookie(
        username,
        settings.session_secret,
        store_path=settings.session_store_path,
        issued_at=issued_at,
        ttl_seconds=ttl_seconds or settings.session_ttl_seconds,
        source=source,
    )


def verify_session(value: str | None) -> str | None:
    if not value:
        return None
    encrypted = read_session_cookie(
        value,
        settings.session_secret,
        store_path=settings.session_store_path,
        max_age_seconds=settings.session_ttl_seconds,
    )
    if encrypted:
        return encrypted.username

    parts = value.split(":")
    if len(parts) != 3:
        return None
    username, issued_raw, signature = parts
    try:
        issued_at = int(issued_raw)
    except ValueError:
        return None
    if time.time() - issued_at > settings.session_ttl_seconds:
        return None
    expected = sign_legacy_session(username, issued_at).rsplit(":", 1)[1]
    if not secrets.compare_digest(signature, expected):
        return None
    return username


def safe_next(value: str | None) -> str:
    if not value or not value.startswith("/") or value.startswith("//"):
        return "/"
    return value


def origin_from_url(value: str) -> str:
    parsed = urlsplit(value.strip())
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        return ""
    return f"{parsed.scheme}://{parsed.netloc}"


def frame_ancestors_value(sso_config: SSORuntimeConfig | None = None) -> str:
    config = sso_config or current_sso_config()
    ancestors = ["'self'"]
    base_origin = origin_from_url(config.base_url)
    if base_origin and base_origin not in ancestors:
        ancestors.append(base_origin)
    return "frame-ancestors " + " ".join(ancestors)


def sso_security_headers(*, no_store: bool = False) -> dict[str, str]:
    headers = {
        "Referrer-Policy": "no-referrer",
        "Content-Security-Policy": frame_ancestors_value(),
    }
    if no_store:
        headers["Cache-Control"] = "no-store"
    return headers


def apply_sso_security_headers(response: Response, *, no_store: bool = False) -> Response:
    for key, value in sso_security_headers(no_store=no_store).items():
        response.headers[key] = value
    return response


def no_store_redirect(location: str, status_code: int = 303) -> RedirectResponse:
    response = RedirectResponse(location, status_code=status_code)
    return apply_sso_security_headers(response, no_store=True)  # type: ignore[return-value]


def sso_required_response() -> Response:
    response = Response("Sub2API SSO required", status_code=403, media_type="text/plain")
    return apply_sso_security_headers(response, no_store=True)


def current_sso_config() -> SSORuntimeConfig:
    return load_sso_runtime_config(
        settings.sso_config_path,
        env_enabled=settings.sub2api_sso_enabled,
        env_base_url=settings.sub2api_base_url,
        env_verify_base_url=settings.sub2api_verify_base_url,
        env_required_role=settings.sub2api_sso_required_role,
        env_session_ttl_seconds=settings.sub2api_sso_session_ttl_seconds,
        env_verify_timeout_seconds=settings.sub2api_sso_verify_timeout_seconds,
    )


@app.middleware("http")
async def add_sso_frame_headers(request: Request, call_next: Any) -> Response:
    response = await call_next(request)
    return apply_sso_security_headers(response)


def require_auth(request: Request) -> str:
    username = verify_session(request.cookies.get(SESSION_COOKIE))
    if username is None:
        raise HTTPException(
            status_code=403,
            detail="Sub2API SSO required",
            headers=sso_security_headers(no_store=True),
        )
    return username


AuthUser = Annotated[str, Depends(require_auth)]


def render(request: Request, template: str, context: dict[str, Any]) -> HTMLResponse:
    context.setdefault("app_name", settings.app_name)
    context.setdefault("base_path", settings.base_path)
    context.setdefault("now", datetime.now())
    context.setdefault("current_user", verify_session(request.cookies.get(SESSION_COOKIE)))
    context.setdefault("version", {"current_version": APP_VERSION})
    return templates.TemplateResponse(request, template, context)


def int_param(value: str | None, default: int, minimum: int, maximum: int) -> int:
    try:
        parsed = int(value or default)
    except (TypeError, ValueError):
        return default
    return max(minimum, min(maximum, parsed))


def nullable_int(value: str | None) -> int | None:
    if value in (None, ""):
        return None
    try:
        return int(value)
    except ValueError:
        return None


def load_groups() -> list[dict[str, Any]]:
    return db.fetch_all(GROUPS_SQL)


def load_platform_options() -> list[dict[str, Any]]:
    return db.fetch_all(PLATFORM_OPTIONS_SQL)


def load_account_options(platform: str) -> list[dict[str, Any]]:
    return db.fetch_all(ACCOUNT_OPTIONS_SQL, {"platform": platform})


def load_guard_account_options() -> list[dict[str, Any]]:
    return load_account_options("")


def account_routing_capability() -> dict[str, bool]:
    try:
        row = db.fetch_one(ACCOUNT_ROUTING_CAPABILITY_SQL) or {}
    except Exception:
        row = {}
    return {
        "priority": bool(row.get("account_priority_column_exists")),
        "load_factor": bool(row.get("account_load_factor_column_exists")),
        "group_priority": bool(row.get("account_group_priority_column_exists")),
    }


def load_quality(
    group_names: list[str],
    platform: str,
    range_start: datetime | None,
    range_end: datetime | None,
) -> list[dict[str, Any]]:
    capability = account_routing_capability()
    sql = QUALITY_SQL if capability["load_factor"] else QUALITY_SQL_COMPAT_NO_LOAD_FACTOR
    return db.fetch_all(
        sql,
        {"group_names": group_names, "platform": platform, "range_start": range_start, "range_end": range_end},
    )


def load_speed_quality(
    group_names: list[str],
    platform: str,
    range_start: datetime | None,
    range_end: datetime | None,
) -> list[dict[str, Any]]:
    capability = account_routing_capability()
    sql = SPEED_SQL if capability["load_factor"] else SPEED_SQL_COMPAT_NO_LOAD_FACTOR
    return db.fetch_all(
        sql,
        {"group_names": group_names, "platform": platform, "range_start": range_start, "range_end": range_end},
    )


def usage_query_store() -> UsageQueryStore:
    return UsageQueryStore(settings.usage_query_state_path)


def usage_query_account_row(account_id: int) -> dict[str, Any] | None:
    return account_ops.fallback_account(db, int(account_id), account_routing_capability()["load_factor"])


def usage_query_oauth_account_rows() -> list[dict[str, Any]]:
    return account_ops.current_oauth_accounts(db, account_routing_capability()["load_factor"])


def hydrate_usage_query_config(config: UsageQueryConfig, row: dict[str, Any] | None = None) -> UsageQueryConfig:
    return apply_account_credentials(config, row if row is not None else usage_query_account_row(config.account_id))


def usage_template_options(selected: str) -> list[dict[str, Any]]:
    return [
        {
            "value": value,
            "label": label,
            "selected": value == selected,
            "default_code": default_template(value),
        }
        for value, label in TEMPLATE_LABELS.items()
    ]


def usage_query_view(
    config: UsageQueryConfig,
    result: dict[str, Any],
    *,
    include_editor: bool = False,
) -> dict[str, Any]:
    public = public_config(config)
    if include_editor:
        public["default_code"] = default_template(config.template_type)
    else:
        public.pop("code", None)
    display_result = dict(result)
    if display_result.get("success"):
        recalculated = actual_available(display_result.get("remaining"), config.upstream_multiplier)
        if recalculated is not None:
            display_result["actual_available"] = recalculated
            display_result["upstream_multiplier"] = config.upstream_multiplier
    view = {
        "configured": usage_query_configured(config),
        "config": public,
        "result": display_result,
        "depleted": should_pause_for_depleted(display_result),
    }
    if include_editor:
        view["template_options"] = usage_template_options(config.template_type)
    return view


def oauth_quota_for_row(row: dict[str, Any], result: dict[str, Any] | None = None) -> dict[str, Any]:
    return usage_query_module.oauth_quota_summary_from_result(row, result)


def usage_query_configured(config: UsageQueryConfig) -> bool:
    return bool(config.updated_at or config.enabled or config.access_token)


def usage_query_editor_view(
    account_id: int,
    store: UsageQueryStore | None = None,
) -> dict[str, Any]:
    active_store = store or usage_query_store()
    config = active_store.config(account_id)
    result = active_store.result(account_id)
    return usage_query_view(config, result, include_editor=True)


def usage_query_audit_payload(config: UsageQueryConfig, user: str) -> dict[str, Any]:
    return {
        "user": user,
        "account_id": config.account_id,
        "enabled": config.enabled,
        "template_type": config.template_type,
        "base_url": config.base_url,
        "api_key_set": bool(config.api_key),
        "access_token_set": bool(config.access_token),
        "user_id_set": bool(config.user_id),
        "timeout_seconds": config.timeout_seconds,
        "upstream_multiplier": config.upstream_multiplier,
        "guard_disable_on_zero": config.guard_disable_on_zero,
    }


def oauth_usage_query_base_url() -> str:
    fallback = str(
        getattr(settings, "sub2api_verify_base_url", "") or getattr(settings, "sub2api_base_url", "") or ""
    ).strip()
    try:
        sso_runtime = current_sso_config()
        return str(sso_runtime.verify_base_url or sso_runtime.base_url or fallback).strip()
    except Exception:
        return fallback


def run_oauth_usage_query(
    account_id: int,
    row: dict[str, Any],
    store: UsageQueryStore,
    *,
    timeout_seconds: int = 10,
) -> dict[str, Any]:
    return execute_oauth_usage_query(
        int(account_id),
        oauth_usage_query_base_url(),
        store.sub2api_admin_token(),
        account_row=row,
        timeout_seconds=timeout_seconds,
    )


def scheduled_test_model_for_account(account_id: int) -> str:
    try:
        row = db.fetch_one(
            """
            SELECT model_id
            FROM scheduled_test_plans
            WHERE account_id = %(account_id)s
              AND enabled = true
            ORDER BY updated_at DESC NULLS LAST, id DESC
            LIMIT 1
            """,
            {"account_id": int(account_id)},
        )
    except Exception:
        return ""
    return str((row or {}).get("model_id") or "").strip()


def execute_sub2api_account_test(
    account_id: int,
    model_id: str = "",
    *,
    base_url: str = "",
    admin_token: str = "",
    timeout_seconds: int = 30,
) -> dict[str, Any]:
    base = str(base_url or oauth_usage_query_base_url()).strip().rstrip("/")
    token = str(admin_token or usage_query_store().sub2api_admin_token()).strip()
    if not base or not token:
        return {
            "success": False,
            "error": "缺少 Sub2API 地址或 Admin Token",
            "error_code": "missing_sub2api_admin_credentials",
        }
    body = {"model_id": str(model_id or ""), "prompt": "", "mode": ""}
    request = {
        "url": f"{base}/api/v1/admin/accounts/{int(account_id)}/test",
        "method": "POST",
        "headers": {
            "Accept": "text/event-stream",
            "Content-Type": "application/json",
            "x-api-key": token,
        },
        "body": body,
    }
    try:
        started_at = time.monotonic()
        payload = open_sub2api_account_test_stream(request, timeout_seconds)
        result = parse_sub2api_account_test_sse(payload)
        result["latency_ms"] = int((time.monotonic() - started_at) * 1000)
        return result
    except TimeoutError as exc:
        return {"success": False, "error": str(exc) or "请求超时", "error_code": "timeout"}
    except urllib.error.HTTPError as exc:
        preview = exc.read(200).decode("utf-8", errors="replace")
        return {"success": False, "error": preview or f"HTTP {exc.code}", "error_code": f"http_{exc.code}"}
    except urllib.error.URLError as exc:
        return {"success": False, "error": str(exc.reason), "error_code": "network_error"}
    except Exception as exc:
        return {"success": False, "error": str(exc), "error_code": "account_test_error"}


def open_sub2api_account_test_stream(request: dict[str, Any], timeout_seconds: int) -> str:
    data = json.dumps(request.get("body") or {}, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(
        str(request["url"]),
        data=data,
        headers={str(k): str(v) for k, v in (request.get("headers") or {}).items()},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=max(2, min(60, int(timeout_seconds or 30)))) as response:
        raw = response.read()
    return raw.decode("utf-8", errors="replace")


def parse_sub2api_account_test_sse(payload: str) -> dict[str, Any]:
    texts: list[str] = []
    saw_success = False
    for line in str(payload or "").splitlines():
        line = line.strip()
        if not line.startswith("data:"):
            continue
        raw = line.removeprefix("data:").strip()
        try:
            event = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        event_type = str(event.get("type") or "")
        if event_type == "error":
            return {
                "success": False,
                "error": normalize_account_test_error_message(event),
                "error_code": normalize_account_test_error_code(event),
            }
        if event_type == "content" and event.get("text"):
            texts.append(str(event.get("text") or ""))
        if event_type == "test_complete" and event.get("success") is True:
            saw_success = True
    if not saw_success:
        return {"success": False, "error": "账号测试未返回成功事件", "error_code": "missing_success_event"}
    return {"success": True, "response_text": "".join(texts).strip()}


def normalize_account_test_error_code(event: dict[str, Any]) -> str:
    candidates = [
        event.get("code"),
        event.get("error_code"),
        event.get("status_code"),
    ]
    nested = event.get("error")
    if isinstance(nested, dict):
        candidates.append(nested.get("code"))
    for candidate in candidates:
        text = str(candidate or "").strip()
        if text:
            return text
    return "unknown_test_error"


def normalize_account_test_error_message(event: dict[str, Any]) -> str:
    error = event.get("error")
    if isinstance(error, dict):
        for key in ("message", "error", "detail", "code"):
            text = str(error.get(key) or "").strip()
            if text:
                return text
    return str(error or event.get("message") or event.get("detail") or "账号测试失败")


def usage_query_oauth_config(account_id: int, store: UsageQueryStore) -> UsageQueryConfig:
    config = store.config(account_id)
    if usage_query_configured(config):
        return config
    return UsageQueryConfig(account_id=account_id, enabled=True, template_type="sub2api")


def enrich_usage_query_rows(rows: list[dict[str, Any]], store: UsageQueryStore | None = None) -> list[dict[str, Any]]:
    active_store = store or usage_query_store()
    configs = {config.account_id: config for config in active_store.configs()}
    results = active_store.results()
    enriched: list[dict[str, Any]] = []
    for row in rows:
        account_id = int(row.get("id") or 0)
        item = dict(row)
        config = configs.get(account_id) or UsageQueryConfig(account_id=account_id)
        result = results.get(account_id) or {}
        item["usage_query"] = usage_query_view(config, result)
        item["oauth_quota"] = oauth_quota_for_row(item, result) if is_oauth_account(item) else {}
        enriched.append(item)
    return enriched


def usage_query_dashboard(rows: list[dict[str, Any]], store: UsageQueryStore | None = None) -> dict[str, int]:
    active_store = store or usage_query_store()
    query_rows = [row.get("usage_query") or {} for row in rows]
    configured_count = sum(1 for item in query_rows if item.get("configured"))
    return {
        "configured_count": configured_count,
        "enabled_count": configured_count if active_store.usage_query_enabled() else 0,
        "depleted_count": sum(1 for item in query_rows if item.get("depleted")),
    }


def usage_query_settings(store: UsageQueryStore | None = None) -> dict[str, Any]:
    active_store = store or usage_query_store()
    return {
        "usage_query_enabled": active_store.usage_query_enabled(),
        "guard_disable_on_zero": active_store.guard_disable_on_zero(),
        "auto_query_interval_seconds": active_store.auto_query_interval_seconds(),
        "sub2api_admin_token_saved": bool(active_store.sub2api_admin_token()),
    }


def guard_signal_params() -> dict[str, Any]:
    hours = int_param(str(getattr(settings, "guard_quality_hours", 24)), 24, 1, 720)
    return {
        "range_start": datetime.now(timezone.utc) - timedelta(hours=hours),
        "range_end": None,
        "platform": "",
    }


def load_guard_quality() -> list[dict[str, Any]]:
    capability = account_routing_capability()
    sql = QUALITY_ALL_ACCOUNTS_SQL if capability["load_factor"] else QUALITY_ALL_ACCOUNTS_SQL_COMPAT_NO_LOAD_FACTOR
    return db.fetch_all(sql, guard_signal_params())


def load_guard_queue_quality() -> list[dict[str, Any]]:
    capability = account_routing_capability()
    sql = GUARD_QUEUE_SQL if capability["load_factor"] else GUARD_QUEUE_SQL_COMPAT_NO_LOAD_FACTOR
    return db.fetch_all(sql, guard_signal_params())


def form_truthy(value: Any) -> bool:
    return str(value or "").strip().lower() in {"1", "true", "yes", "on"}


def redirect_with_msg(location: str, message: str) -> RedirectResponse:
    target = safe_next(location)
    parsed = urlsplit(target)
    params = [(key, value) for key, value in parse_qsl(parsed.query, keep_blank_values=True) if key != "msg"]
    params.append(("msg", message))
    query = urlencode(params)
    return RedirectResponse(urlunsplit(("", "", parsed.path, query, parsed.fragment)), status_code=303)


def usage_query_config_from_form(
    account_id: int,
    raw: dict[str, Any],
    existing: UsageQueryConfig,
    user: str,
) -> UsageQueryConfig:
    template_type = str(raw.get("template_type") or existing.template_type or "sub2api")
    selected_template = normalize_template_type(template_type)
    existing_template = normalize_template_type(existing.template_type)
    access_token = str(raw.get("access_token") or "").strip()
    submitted_code = str(raw.get("code") or "").strip()
    existing_default_code = existing.code or default_template(existing_template)
    if (
        selected_template != existing_template
        and normalize_template_code(submitted_code) == normalize_template_code(existing_default_code)
    ):
        submitted_code = default_template(selected_template)
    return UsageQueryConfig(
        account_id=account_id,
        enabled=True,
        template_type=template_type,
        code=submitted_code or default_template(template_type),
        base_url=existing.base_url,
        api_key=existing.api_key,
        access_token=access_token or (existing.access_token if existing.template_type == selected_template else ""),
        user_id=str(raw.get("user_id") or "").strip(),
        use_account_credentials=True,
        timeout_seconds=int_param(str(raw.get("timeout_seconds")), 10, 2, 30),
        upstream_multiplier=float_param(raw.get("upstream_multiplier"), 1.0, 0.0001, 1_000_000.0),
        guard_disable_on_zero=True,
        auto_query_interval_minutes=existing.auto_query_interval_minutes,
        updated_at=datetime.now(timezone.utc).isoformat(),
    )


def float_param(value: Any, default: float, minimum: float, maximum: float) -> float:
    try:
        parsed = float(str(value).strip())
    except (TypeError, ValueError):
        return default
    if not math.isfinite(parsed):
        return default
    return max(minimum, min(maximum, parsed))


def scheduled_test_capability() -> dict[str, Any]:
    row = db.fetch_one(SCHEDULED_TEST_CAPABILITY_SQL) or {}
    available = bool(
        row.get("plans_table_exists")
        and row.get("results_table_exists")
        and row.get("auto_recover_column_exists")
    )
    return {
        **row,
        "available": available,
        "message": ""
        if available
        else "当前 Sub2API 数据库缺少 scheduled_test_plans/results 或 auto_recover 字段，请先确认上游迁移已执行。",
    }


def scheduled_test_reasons(row: dict[str, Any]) -> list[str]:
    reasons: list[str] = []
    if row.get("status") and row.get("status") != "active":
        reasons.append(f"状态 {row.get('status')}")
    if not row.get("schedulable", True):
        reasons.append("已停调度")
    if account_ops.is_cooling(row):
        reasons.append("临时冷却")
    if row.get("rate_limited_at") or row.get("rate_limit_reset_at"):
        reasons.append("限流状态")
    if row.get("overload_until"):
        reasons.append("过载状态")
    if row.get("error_message"):
        reasons.append("错误状态")
    return reasons


def load_scheduled_test_accounts(group_names: list[str], platform: str, include_all: bool) -> list[dict[str, Any]]:
    capability = account_routing_capability()
    sql = SCHEDULED_TEST_ACCOUNTS_SQL if capability["load_factor"] else SCHEDULED_TEST_ACCOUNTS_SQL_COMPAT_NO_LOAD_FACTOR
    rows = db.fetch_all(
        sql,
        {"group_names": group_names, "platform": platform, "include_all": include_all},
    )
    for row in rows:
        row["plan_interval_minutes"] = interval_from_cron(row.get("plan_cron_expression") or "")
        row["state_label"] = account_ops.account_state(row)
        row["recovery_reasons"] = scheduled_test_reasons(row)
        row["has_plan"] = bool(row.get("plan_id"))
    return rows


def load_scheduled_test_results(plan_id: int | None = None, limit: int = 30) -> list[dict[str, Any]]:
    return db.fetch_all(
        SCHEDULED_TEST_RESULTS_SQL,
        {"plan_id": plan_id, "limit": int_param(str(limit), 30, 1, 100)},
    )


def scheduled_test_dashboard(rows: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "shown_count": len(rows),
        "recoverable_count": sum(1 for row in rows if row.get("has_recoverable_signal")),
        "plan_count": sum(1 for row in rows if row.get("plan_id")),
        "enabled_count": sum(1 for row in rows if row.get("plan_enabled")),
        "auto_recover_count": sum(1 for row in rows if row.get("plan_auto_recover")),
    }


def scheduled_tests_url(group_values: list[str], platform: str, include_all: bool, msg: str = "") -> str:
    query: list[tuple[str, str]] = [("platform", platform)]
    for value in unique_group_values(group_values) or [DEFAULT_GROUP_NAME]:
        query.append(("group", value))
    if include_all:
        query.append(("include_all", "1"))
    if msg:
        query.append(("msg", msg))
    return f"{settings.base_path}/scheduled-tests?{urlencode(query)}"


def guard_config(policy: GuardPolicy | None = None, *, include_recent_events: bool = True) -> dict[str, Any]:
    policy = policy or guard_policy_from_store()
    policy_payload = asdict(policy)
    policy_payload["whitelist_account_ids_text"] = ", ".join(str(item) for item in policy.whitelist_account_ids)
    return {
        "enabled": settings.guard_enabled,
        "interval_seconds": settings.guard_interval_seconds,
        "scope": "all_accounts",
        "quality_hours": getattr(settings, "guard_quality_hours", 24),
        "threshold": settings.guard_balance_error_threshold,
        "balance_max_age_hours": settings.guard_balance_error_max_age_hours,
        "action": "pause",
        "state": guard_state,
        "policy": policy_payload,
        "account_routing": account_routing_capability(),
        "recent_events": read_audit(settings.audit_path, limit=12, event_prefix="guard_")
        if include_recent_events
        else [],
    }


def guard_policy_from_store() -> GuardPolicy:
    raw = GuardStore(settings.guard_state_path).policy_config()
    return GuardPolicy(
        hard_pause_enabled=form_truthy(raw.get("hard_pause_enabled")) if "hard_pause_enabled" in raw else True,
        rate_limit_enabled=form_truthy(raw.get("rate_limit_enabled")) if "rate_limit_enabled" in raw else True,
        unstable_enabled=form_truthy(raw.get("unstable_enabled")) if "unstable_enabled" in raw else True,
        failure_threshold=int_param(str(raw.get("failure_threshold")), 4, 1, 50),
        success_threshold=int_param(str(raw.get("success_threshold")), 2, 1, 20),
        circuit_timeout_seconds=int_param(str(raw.get("circuit_timeout_seconds")), 60, 5, 3600),
        blocked_403_threshold=int_param(str(raw.get("blocked_403_threshold")), 1, 1, 20),
        balance_pause_threshold=int_param(str(raw.get("balance_pause_threshold")), 1, 1, 20),
        whitelist_account_ids=parse_int_csv(raw.get("whitelist_account_ids")),
        whitelist_balance_pause_threshold=int_param(str(raw.get("whitelist_balance_pause_threshold")), 10, 1, 100),
    )


def guard_store() -> GuardStore:
    return GuardStore(settings.guard_state_path)


def guard_engine(store: GuardStore | None = None) -> GuardEngine:
    capability = account_routing_capability()
    return GuardEngine(
        db=db,
        store=store or guard_store(),
        audit_path=settings.audit_path,
        policy=guard_policy_from_store(),
        batch_size=settings.guard_event_batch_size,
        load_factor_supported=capability["load_factor"],
    )


def enrich_guard_rows(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    store = guard_store()
    for row in rows:
        row["guard_circuit"] = asdict(store.circuit(int(row.get("id") or 0)))
        row["problem"] = account_problem(row)
        row["queue_position"] = queue_position(row)
        row["membership_key"] = membership_key(row)
    return rows


def guard_whitelist_options(rows: list[dict[str, Any]], policy: GuardPolicy) -> list[dict[str, Any]]:
    selected = set(policy.whitelist_account_ids)
    seen: set[int] = set()
    options: list[dict[str, Any]] = []
    for row in rows:
        try:
            account_id = int(row.get("id") or 0)
        except (TypeError, ValueError):
            continue
        if account_id <= 0 or account_id in seen:
            continue
        seen.add(account_id)
        name = str(row.get("name") or f"Account {account_id}").strip()
        meta_parts = [str(value).strip() for value in (row.get("type"), row.get("platform")) if str(value or "").strip()]
        options.append(
            {
                "id": account_id,
                "label": f"#{account_id} {name}",
                "meta": " / ".join(meta_parts),
                "checked": account_id in selected,
            }
        )
    for account_id in sorted(selected - seen):
        options.append(
            {
                "id": account_id,
                "label": f"#{account_id} 当前列表未返回",
                "meta": "已保存",
                "checked": True,
            }
        )
    return options


def guard_whitelist_account_options(accounts: list[dict[str, Any]], policy: GuardPolicy) -> list[dict[str, Any]]:
    selected = set(policy.whitelist_account_ids)
    seen: set[int] = set()
    options: list[dict[str, Any]] = []
    for row in accounts:
        try:
            account_id = int(row.get("id") or 0)
        except (TypeError, ValueError):
            continue
        if account_id <= 0 or account_id in seen:
            continue
        seen.add(account_id)
        name = str(row.get("name") or f"Account {account_id}").strip()
        meta_parts = [str(value).strip() for value in (row.get("type"), row.get("platform")) if str(value or "").strip()]
        options.append(
            {
                "id": account_id,
                "label": f"#{account_id} {name}",
                "meta": " / ".join(meta_parts),
                "checked": account_id in selected,
            }
        )
    for account_id in sorted(selected - seen):
        options.append(
            {
                "id": account_id,
                "label": f"#{account_id} 当前列表未返回",
                "meta": "已保存",
                "checked": True,
            }
        )
    return options


def guard_section_cards(queue_query: str = "") -> list[dict[str, str]]:
    queue_suffix = f"?{queue_query}" if queue_query else ""
    return [
        {
            "key": "queue",
            "title": "分组队列",
            "description": "按需加载各分组调度队列、拖拽排序和队列保存。",
            "url": f"{settings.base_path}/guard/sections/queue{queue_suffix}",
        },
        {
            "key": "suggestions",
            "title": "自动动作与人工建议",
            "description": "按需加载当前 Guard 建议和人工执行入口。",
            "url": f"{settings.base_path}/guard/sections/suggestions",
        },
        {
            "key": "routing",
            "title": "账号路由",
            "description": "按需加载账号优先级、负载因子和最近信号。",
            "url": f"{settings.base_path}/guard/sections/routing",
        },
        {
            "key": "audit",
            "title": "最近 Guard 记录",
            "description": "按需加载最近 Guard 审计记录。",
            "url": f"{settings.base_path}/guard/sections/audit",
        },
    ]


def guard_queue_section_query(request: Request) -> str:
    query_params = getattr(request, "query_params", None)
    if not hasattr(query_params, "getlist"):
        return ""
    pairs = [("queue_group", value) for value in query_params.getlist("queue_group")]
    return urlencode(pairs)


def guard_queue_context(request: Request) -> dict[str, Any]:
    groups = load_groups()
    query_params = getattr(request, "query_params", None)
    queue_group_values = query_params.getlist("queue_group") if hasattr(query_params, "getlist") else []
    queue_group_selection = build_group_selection(queue_group_values, groups)
    queue_rows = filter_guard_queue_rows(enrich_guard_rows(load_guard_queue_quality()), queue_group_selection)
    return {
        "active": "guard",
        "queue_rows": queue_rows,
        "queue_groups": group_queue_rows(queue_rows),
        "groups": groups,
        "queue_group_selection": queue_group_selection,
    }


def guard_quality_context(*, include_suggestions: bool = False, include_routing: bool = False) -> dict[str, Any]:
    rows = enrich_guard_rows(load_guard_quality())
    context: dict[str, Any] = {
        "active": "guard",
        "rows": rows,
        "guard": guard_config(include_recent_events=False),
    }
    if include_suggestions:
        context["suggestions"] = [s for row in rows if (s := guard_suggestion(row))]
    if include_routing:
        context["guard"] = guard_config(include_recent_events=False)
    return context


def guard_audit_context() -> dict[str, Any]:
    return {
        "active": "guard",
        "guard": {
            "recent_events": read_audit(settings.audit_path, limit=12, event_prefix="guard_"),
        },
    }


def filter_guard_queue_rows(rows: list[dict[str, Any]], group_selection: dict[str, Any]) -> list[dict[str, Any]]:
    if group_selection.get("all_selected") or not group_selection.get("options"):
        return list(rows)
    selected = set(group_selection.get("selected") or [])
    return [row for row in rows if row.get("group_name") in selected]


def guard_queue_url(queue_group_values: list[Any], msg: str = "") -> str:
    query: list[tuple[str, str]] = []
    values = unique_group_values(queue_group_values) or [ALL_GROUP_VALUE]
    for value in values:
        query.append(("queue_group", value))
    if msg:
        query.append(("msg", msg))
    suffix = urlencode(query)
    return f"{settings.base_path}/guard?{suffix}" if suffix else f"{settings.base_path}/guard"


def request_scan_limit(limit: int, account_id: str = "", q: str = "") -> int:
    multiplier = 50 if str(account_id or "").strip() else 20
    if str(q or "").strip():
        multiplier = max(multiplier, 30)
    return min(max(limit, limit * multiplier), 50000)


def parse_int_csv(value: Any) -> tuple[int, ...]:
    return normalize_account_ids(value)


def mask_secret(value: str) -> str:
    raw = value.strip()
    if not raw:
        return ""
    if len(raw) <= 10:
        return "已设置"
    return f"{raw[:6]}...{raw[-4:]}"


def generate_telegram_pairing_code() -> str:
    raw = "".join(secrets.choice(TELEGRAM_PAIRING_ALPHABET) for _ in range(8))
    return f"{raw[:4]}-{raw[4:]}"


def telegram_state() -> dict[str, Any]:
    try:
        state = json.loads(Path(settings.telegram_state_path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        state = {}
    state.setdefault("paired_chat_ids", [])
    state.setdefault("paired_user_ids", [])
    return state


def telegram_config_file() -> dict[str, Any]:
    try:
        data = json.loads(Path(settings.telegram_config_path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return data if isinstance(data, dict) else {}


def ensure_telegram_pairing_code() -> str:
    config_file = telegram_config_file()
    pairing_code = str(settings.telegram_pairing_code or config_file.get("pairing_code") or "").strip()
    if pairing_code or not settings.telegram_bot_token.strip():
        return pairing_code

    pairing_code = generate_telegram_pairing_code()
    payload = {
        **config_file,
        "pairing_enabled": True,
        "pairing_code": pairing_code,
        "updated_at": datetime.now(timezone.utc).isoformat(),
        "updated_by": config_file.get("updated_by") or "system",
    }
    save_telegram_runtime_config(payload)
    apply_telegram_runtime_config(payload)
    return pairing_code


def build_telegram_config() -> dict[str, Any]:
    state = telegram_state()
    config_file = telegram_config_file()
    pairing_code = ensure_telegram_pairing_code()
    config_file = telegram_config_file()
    paired_chat_ids = sorted({int(item) for item in state.get("paired_chat_ids", []) if str(item).lstrip("-").isdigit()})
    paired_user_ids = sorted({int(item) for item in state.get("paired_user_ids", []) if str(item).lstrip("-").isdigit()})
    push_target_count = len(set(paired_chat_ids))
    control_user_count = len(set(paired_user_ids))
    return {
        "enabled": settings.telegram_enabled,
        "bot_token_set": bool(settings.telegram_bot_token.strip()),
        "bot_token_preview": mask_secret(settings.telegram_bot_token),
        "configured": settings.telegram_enabled and bool(settings.telegram_bot_token.strip()),
        "pairing_enabled": settings.telegram_pairing_enabled,
        "pairing_code": pairing_code,
        "config_updated_at": config_file.get("updated_at"),
        "state_updated_at": state.get("updated_at"),
        "paired_chat_ids": paired_chat_ids,
        "paired_user_ids": paired_user_ids,
        "push_target_count": push_target_count,
        "control_user_count": control_user_count,
        "oauth_usage_refresh_enabled": telegram_oauth_usage_refresh_enabled(),
        "oauth_recovery_monitor_enabled": telegram_oauth_recovery_monitor_enabled(),
        "oauth_recovery_push_enabled": telegram_oauth_recovery_push_enabled(),
        "binding_status": "未启用"
        if not settings.telegram_bot_token.strip()
        else ("已绑定" if push_target_count or control_user_count else "待配对"),
    }


def save_telegram_runtime_config(payload: dict[str, Any]) -> None:
    path = Path(settings.telegram_config_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    path.chmod(0o600)


def apply_telegram_runtime_config(payload: dict[str, Any]) -> None:
    if "enabled" in payload:
        settings.telegram_enabled = bool(payload.get("enabled"))
    if "bot_token" in payload:
        settings.telegram_bot_token = str(payload.get("bot_token") or "")
    if "pairing_enabled" in payload:
        settings.telegram_pairing_enabled = bool(payload.get("pairing_enabled", True))
    if "pairing_code" in payload:
        settings.telegram_pairing_code = str(payload.get("pairing_code", "") or "")
    if "allowed_chat_ids" in payload:
        settings.telegram_allowed_chat_ids = parse_int_csv(",".join(str(v) for v in payload.get("allowed_chat_ids", [])))
    if "allowed_user_ids" in payload:
        settings.telegram_allowed_user_ids = parse_int_csv(",".join(str(v) for v in payload.get("allowed_user_ids", [])))
    if "default_platform" in payload:
        settings.telegram_default_platform = str(payload.get("default_platform") or "openai").strip() or "openai"
    if "default_group" in payload:
        settings.telegram_default_group = str(payload.get("default_group") or "openai-default").strip() or "openai-default"
    if "quality_hours" in payload:
        settings.telegram_quality_hours = int_param(str(payload.get("quality_hours")), 24, 1, 168)
    if "poll_timeout_seconds" in payload:
        settings.telegram_poll_timeout_seconds = int_param(str(payload.get("poll_timeout_seconds")), 25, 5, 50)
    if "error_alert_enabled" in payload:
        settings.telegram_error_alert_enabled = bool(payload.get("error_alert_enabled"))
    if "error_alert_interval_seconds" in payload:
        settings.telegram_error_alert_interval_seconds = int_param(str(payload.get("error_alert_interval_seconds")), 2, 1, 60)
    if "error_alert_batch_size" in payload:
        settings.telegram_error_alert_batch_size = int_param(str(payload.get("error_alert_batch_size")), 50, 1, 100)
    if "oauth_usage_refresh_enabled" in payload:
        settings.telegram_oauth_usage_refresh_enabled = bool(payload.get("oauth_usage_refresh_enabled", True))
    if "oauth_recovery_monitor_enabled" in payload:
        settings.telegram_oauth_recovery_monitor_enabled = bool(payload.get("oauth_recovery_monitor_enabled", True))
    if "oauth_recovery_push_enabled" in payload:
        settings.telegram_oauth_recovery_push_enabled = bool(payload.get("oauth_recovery_push_enabled", True))


def telegram_oauth_usage_refresh_enabled() -> bool:
    return bool(getattr(settings, "telegram_oauth_usage_refresh_enabled", True))


def telegram_oauth_recovery_monitor_enabled() -> bool:
    return bool(getattr(settings, "telegram_oauth_recovery_monitor_enabled", True))


def telegram_oauth_recovery_push_enabled() -> bool:
    return bool(getattr(settings, "telegram_oauth_recovery_push_enabled", True))


async def restart_telegram_bot() -> None:
    global telegram_bot, telegram_task
    if telegram_task:
        telegram_task.cancel()
        with suppress(asyncio.CancelledError):
            await telegram_task
        telegram_task = None
    telegram_bot = TelegramOpsBot(settings, db, run_auto_guard_threaded, guard_config)
    if telegram_bot.enabled:
        telegram_task = asyncio.create_task(telegram_bot.run())


def pause_guard_candidate(row: dict[str, Any], actor: str) -> dict[str, Any]:
    reason = (
        "auto guard: permanent pause for balance/quota error; manual resume required; "
        f"{row['balance_error_count']} historical balance/quota errors; "
        f"last={row.get('last_message') or 'n/a'}"
    )
    updated = db.fetch_one(
        """
        UPDATE accounts
        SET schedulable = false,
            temp_unschedulable_until = NULL,
            temp_unschedulable_reason = %(reason)s,
            rate_limited_at = NULL,
            rate_limit_reset_at = NULL,
            overload_until = NULL,
            error_message = NULL,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        RETURNING
          id,
          name,
          schedulable,
          temp_unschedulable_until,
          temp_unschedulable_reason,
          rate_limited_at,
          rate_limit_reset_at,
          overload_until,
          error_message
        """,
        {"account_id": row["id"], "reason": reason},
    )

    result = {
        "account_id": row["id"],
        "name": row["name"],
        "action": "pause",
        "balance_error_count": row["balance_error_count"],
        "last_error_at": row.get("last_error_at"),
        "last_message": row.get("last_message"),
        "updated": updated,
        "actor": actor,
        "reason": reason,
    }
    if updated:
        write_audit(settings.audit_path, "guard_auto_pause_account", result)
    return result


def run_guard_balance_fallback(actor: str) -> list[dict[str, Any]]:
    policy = guard_policy_from_store()
    if not policy.hard_pause_enabled:
        return []
    candidates = db.fetch_all(
        GUARD_BALANCE_CANDIDATES_SQL,
        {
            "threshold": settings.guard_balance_error_threshold,
            "max_age_hours": settings.guard_balance_error_max_age_hours,
        },
    )
    actionable: list[dict[str, Any]] = []
    whitelist_skipped_count = 0
    for row in candidates:
        if not row.get("id"):
            continue
        if is_whitelisted_account(policy, row.get("id")):
            whitelist_skipped_count += 1
            continue
        actionable.append(row)
    actions = [pause_guard_candidate(row, actor) for row in actionable]
    applied = [item for item in actions if item.get("updated")]
    write_audit(
        settings.audit_path,
        "guard_auto_fallback_balance_scan",
        {
            "actor": actor,
            "candidate_count": len(candidates),
            "whitelist_skipped_count": whitelist_skipped_count,
            "action_count": len(applied),
        },
    )
    return applied


def run_usage_query_guard(actor: str) -> list[dict[str, Any]]:
    store = usage_query_store()
    usage_enabled = store.usage_query_enabled()
    hard_stop_enabled = store.guard_disable_on_zero()
    auto_query_interval_seconds = store.auto_query_interval_seconds()
    actions: list[dict[str, Any]] = []
    checked_count = 0
    queried_count = 0
    oauth_checked_count = 0
    oauth_queried_count = 0
    oauth_failed_count = 0
    oauth_skipped_disabled_count = 0
    if not usage_enabled:
        write_audit(
            settings.audit_path,
            "guard_auto_usage_query_scan",
            {
                "actor": actor,
                "checked_count": 0,
                "queried_count": 0,
                "oauth_checked_count": 0,
                "oauth_queried_count": 0,
                "oauth_failed_count": 0,
                "oauth_skipped_disabled_count": 0,
                "action_count": 0,
                "usage_query_enabled": False,
                "guard_disable_on_zero": hard_stop_enabled,
            },
        )
        return []
    for config in store.configs():
        if not usage_query_configured(config):
            continue
        row = usage_query_account_row(config.account_id)
        if not row:
            continue
        if is_oauth_account(row):
            oauth_checked_count += 1
            if not telegram_oauth_usage_refresh_enabled():
                oauth_skipped_disabled_count += 1
                continue
            result = store.result(config.account_id)
            if is_query_due(config, result, interval_seconds=auto_query_interval_seconds):
                result = run_oauth_usage_query(config.account_id, row, store, timeout_seconds=config.timeout_seconds)
                store.save_result(config.account_id, result)
                oauth_queried_count += 1
                if not result.get("success"):
                    oauth_failed_count += 1
            continue
        if not row.get("schedulable", True):
            continue
        checked_count += 1
        result = store.result(config.account_id)
        if is_query_due(config, result, interval_seconds=auto_query_interval_seconds):
            result = execute_usage_query(hydrate_usage_query_config(config, row))
            store.save_result(config.account_id, result)
            queried_count += 1
        if not hard_stop_enabled:
            continue
        if not should_pause_for_depleted(result):
            continue
        reason = (
            "auto guard: usage query depleted; manual resume required; "
            f"remaining={result.get('remaining')}; "
            f"multiplier={result.get('upstream_multiplier')}; "
            f"available={result.get('actual_available')} {result.get('unit') or ''}".strip()
        )
        updated = account_ops.guard_pause_account(db, config.account_id, reason)
        if not updated:
            continue
        action = {
            "account_id": config.account_id,
            "name": updated.get("name") or row.get("name"),
            "action": "pause",
            "reason": reason,
            "updated": updated,
            "actor": actor,
            "remaining": result.get("remaining"),
            "actual_available": result.get("actual_available"),
            "unit": result.get("unit"),
            "upstream_multiplier": result.get("upstream_multiplier"),
        }
        write_audit(settings.audit_path, "guard_auto_usage_query_pause_account", action)
        actions.append(action)
    configured_account_ids = {config.account_id for config in store.configs()}
    try:
        oauth_rows = usage_query_oauth_account_rows()
    except Exception:
        oauth_rows = []
    for row in oauth_rows:
        account_id = int(row.get("id") or 0)
        if account_id <= 0 or account_id in configured_account_ids or not is_oauth_account(row):
            continue
        oauth_checked_count += 1
        if not telegram_oauth_usage_refresh_enabled():
            oauth_skipped_disabled_count += 1
            continue
        config = usage_query_oauth_config(account_id, store)
        result = store.result(account_id)
        if not is_query_due(config, result, interval_seconds=auto_query_interval_seconds):
            continue
        result = run_oauth_usage_query(account_id, row, store, timeout_seconds=config.timeout_seconds)
        store.save_result(account_id, result)
        oauth_queried_count += 1
        if not result.get("success"):
            oauth_failed_count += 1
    write_audit(
        settings.audit_path,
        "guard_auto_usage_query_scan",
        {
            "actor": actor,
            "checked_count": checked_count,
            "queried_count": queried_count,
            "oauth_checked_count": oauth_checked_count,
            "oauth_queried_count": oauth_queried_count,
            "oauth_failed_count": oauth_failed_count,
            "oauth_skipped_disabled_count": oauth_skipped_disabled_count,
            "action_count": len(actions),
            "usage_query_enabled": usage_enabled,
            "guard_disable_on_zero": hard_stop_enabled,
            "auto_query_interval_seconds": auto_query_interval_seconds,
        },
    )
    return actions


def scan_oauth_quota_recovery_alerts(
    *,
    state: dict[str, Any],
    now: datetime | None = None,
) -> list[dict[str, Any]]:
    if not telegram_oauth_recovery_monitor_enabled():
        write_audit(
            settings.audit_path,
            "telegram_oauth_quota_recovery_scan",
            {"skipped": "oauth_recovery_monitor_disabled"},
        )
        return []
    active_store = usage_query_store()
    if not active_store.usage_query_enabled():
        return []
    base_url = oauth_usage_query_base_url()
    admin_token = active_store.sub2api_admin_token()
    if not base_url or not admin_token:
        write_audit(
            settings.audit_path,
            "telegram_oauth_quota_recovery_scan",
            {
                "skipped": "missing_sub2api_admin_credentials",
                "base_url_set": bool(base_url),
                "admin_token_set": bool(admin_token),
            },
        )
        return []
    try:
        rows = usage_query_oauth_account_rows()
    except Exception as exc:
        write_audit(settings.audit_path, "telegram_oauth_quota_recovery_error", {"stage": "load_accounts", "error": str(exc)})
        return []

    current = (now or datetime.now(timezone.utc)).astimezone(timezone.utc)
    dedupe = state.setdefault("oauth_account_recovery_alerts", {})
    if not isinstance(dedupe, dict):
        dedupe = {}
        state["oauth_account_recovery_alerts"] = dedupe
    pending = state.setdefault("oauth_account_recovery_pending", {})
    if not isinstance(pending, dict):
        pending = {}
        state["oauth_account_recovery_pending"] = pending

    events: list[dict[str, Any]] = []
    checked_count = 0
    queried_count = 0
    early_probe_count = 0
    tested_count = 0
    test_failed_count = 0
    for row in rows:
        account_id = int(row.get("id") or 0)
        if account_id <= 0 or not is_oauth_account(row):
            continue
        cached_result = active_store.result(account_id)
        summary = oauth_quota_for_row(row, cached_result)
        config = usage_query_oauth_config(account_id, active_store)
        probe_interval = min(active_store.auto_query_interval_seconds() or 60, 60)
        if probe_interval <= 0:
            probe_interval = 60
        cached_probe = cached_result.get("oauth_recovery_probe") if isinstance(cached_result, dict) else None
        due_candidate = oauth_account_recovery_probe_due(summary, now=current)
        early_probe = False
        throttle_probe = False
        if (
            due_candidate
            and isinstance(cached_probe, dict)
            and not bool(cached_result.get("success"))
            and str(cached_probe.get("fingerprint") or "")
            == str(due_candidate.get("fingerprint") or "")
        ):
            throttle_probe = True
        if not due_candidate:
            due_candidate = cached_probe if isinstance(cached_probe, dict) else None
            if due_candidate:
                early_probe = bool(due_candidate.get("early_probe"))
                throttle_probe = True
            if not due_candidate:
                due_candidate = oauth_account_recovery_early_probe_due(
                    summary,
                    cached_result,
                    now=current,
                    interval_seconds=probe_interval,
                )
                if not due_candidate:
                    continue
                early_probe = True
                throttle_probe = True
        if throttle_probe and not is_query_due(config, cached_result, now=current, interval_seconds=probe_interval):
            continue
        if oauth_recovery_success_dedupe_key(account_id, due_candidate) in dedupe:
            continue
        checked_count += 1
        result = run_oauth_usage_query(account_id, row, active_store, timeout_seconds=config.timeout_seconds)
        result = dict(result)
        result["oauth_recovery_probe"] = due_candidate
        active_store.save_result(account_id, result)
        queried_count += 1
        if early_probe:
            early_probe_count += 1
        if not result.get("success"):
            write_audit(
                settings.audit_path,
                "telegram_oauth_quota_recovery_error",
                {
                    "stage": "active_usage",
                    "account_id": account_id,
                    "error": result.get("error") or "",
                    "error_code": result.get("error_code") or "",
                },
            )
            continue
        refreshed_summary = result.get("oauth_quota") if isinstance(result.get("oauth_quota"), dict) else {}
        candidate = oauth_account_recovery_candidate_from_probe(refreshed_summary, due_candidate, now=current)
        if not candidate:
            continue
        success_dedupe_key = oauth_recovery_success_dedupe_key(account_id, candidate)
        if success_dedupe_key in dedupe:
            continue
        model_id = scheduled_test_model_for_account(account_id)
        test_result = execute_sub2api_account_test(
            account_id,
            model_id,
            base_url=base_url,
            admin_token=admin_token,
            timeout_seconds=max(10, config.timeout_seconds),
        )
        tested_count += 1
        if not test_result.get("success"):
            test_failed_count += 1
            failure_dedupe_key = oauth_recovery_failure_dedupe_key(account_id, candidate, test_result)
            write_audit(
                settings.audit_path,
                "telegram_oauth_quota_recovery_error",
                {
                    "stage": "account_test",
                    "account_id": account_id,
                    "error": test_result.get("error") or "",
                    "error_code": test_result.get("error_code") or "",
                    "model_id": model_id,
                },
            )
            if failure_dedupe_key not in dedupe:
                event = oauth_recovery_event(
                    row,
                    candidate,
                    test_result,
                    model_id,
                    failure_dedupe_key,
                    status="test_failed",
                )
                events.append(event)
                pending[failure_dedupe_key] = {
                    "account_id": account_id,
                    "fingerprint": candidate.get("fingerprint"),
                    "error_code": test_result.get("error_code") or "",
                    "checked_at": current.isoformat(),
                }
            continue
        event = oauth_recovery_event(row, candidate, test_result, model_id, success_dedupe_key, status="recovered")
        events.append(event)
        pending[success_dedupe_key] = {
            "account_id": account_id,
            "fingerprint": candidate.get("fingerprint"),
            "checked_at": current.isoformat(),
        }
    write_audit(
        settings.audit_path,
        "telegram_oauth_quota_recovery_scan",
        {
            "checked_count": checked_count,
            "queried_count": queried_count,
            "early_probe_count": early_probe_count,
            "tested_count": tested_count,
            "test_failed_count": test_failed_count,
            "push_count": len(events),
        },
    )
    return events


def oauth_recovery_dedupe_key(account_id: int, candidate: dict[str, Any]) -> str:
    return f"{int(account_id)}:{candidate.get('fingerprint') or ''}"


def oauth_recovery_success_dedupe_key(account_id: int, candidate: dict[str, Any]) -> str:
    return f"success:{oauth_recovery_dedupe_key(account_id, candidate)}"


def oauth_recovery_failure_dedupe_key(account_id: int, candidate: dict[str, Any], test_result: dict[str, Any]) -> str:
    code = str(test_result.get("error_code") or "unknown_test_error").strip() or "unknown_test_error"
    return f"failure:{oauth_recovery_dedupe_key(account_id, candidate)}:{code}"


def oauth_recovery_event(
    row: dict[str, Any],
    candidate: dict[str, Any],
    test_result: dict[str, Any],
    model_id: str,
    dedupe_key: str = "",
    status: str = "recovered",
) -> dict[str, Any]:
    return {
        "account_id": int(row.get("id") or 0),
        "account_name": row.get("name") or "-",
        "platform": row.get("platform") or "openai",
        "type": row.get("type") or "oauth",
        "plan_type": candidate.get("plan_type") or "oauth",
        "window_labels": candidate.get("window_labels") or [],
        "trigger_window_labels": candidate.get("trigger_window_labels") or [],
        "reset_at": candidate.get("reset_at") or "",
        "remaining_summary": candidate.get("remaining_summary") or "",
        "status": status,
        "error": test_result.get("error") or "",
        "error_code": test_result.get("error_code") or "",
        "test_latency_ms": test_result.get("latency_ms"),
        "test_model_id": model_id,
        "test_response_text": test_result.get("response_text") or "",
        "dedupe_key": dedupe_key,
        "fingerprint": candidate.get("fingerprint") or "",
    }


def mark_oauth_recovery_alerts_notified(
    state: dict[str, Any],
    rows: list[dict[str, Any]],
    *,
    now: datetime | None = None,
) -> None:
    if not rows:
        return
    current = (now or datetime.now(timezone.utc)).astimezone(timezone.utc).isoformat()
    dedupe = state.setdefault("oauth_account_recovery_alerts", {})
    if not isinstance(dedupe, dict):
        dedupe = {}
        state["oauth_account_recovery_alerts"] = dedupe
    pending = state.get("oauth_account_recovery_pending")
    if not isinstance(pending, dict):
        pending = {}
    for row in rows:
        key = str(row.get("dedupe_key") or "")
        if not key:
            continue
        dedupe[key] = {
            "account_id": int(row.get("account_id") or 0),
            "fingerprint": row.get("fingerprint") or "",
            "notified_at": current,
        }
        pending.pop(key, None)
    state["oauth_account_recovery_pending"] = pending


def suppress_oauth_recovery_alerts(
    state: dict[str, Any],
    rows: list[dict[str, Any]],
    *,
    now: datetime | None = None,
) -> None:
    if not rows:
        return
    current = (now or datetime.now(timezone.utc)).astimezone(timezone.utc).isoformat()
    dedupe = state.setdefault("oauth_account_recovery_alerts", {})
    if not isinstance(dedupe, dict):
        dedupe = {}
        state["oauth_account_recovery_alerts"] = dedupe
    pending = state.get("oauth_account_recovery_pending")
    if not isinstance(pending, dict):
        pending = {}
    for row in rows:
        key = str(row.get("dedupe_key") or "")
        if not key:
            continue
        dedupe[key] = {
            "account_id": int(row.get("account_id") or 0),
            "fingerprint": row.get("fingerprint") or "",
            "suppressed_at": current,
            "reason": "oauth_recovery_push_disabled",
        }
        pending.pop(key, None)
    state["oauth_account_recovery_pending"] = pending


def run_auto_guard_once(actor: str = "auto_guard") -> list[dict[str, Any]]:
    guard_state["running"] = True
    guard_state["last_error"] = ""
    guard_state["last_error_at"] = ""
    try:
        incremental_actions = guard_engine().run_once(actor)
        balance_actions = run_guard_balance_fallback(actor)
        usage_actions = run_usage_query_guard(actor)
        actions = [*incremental_actions, *balance_actions, *usage_actions]
        guard_state.update(
            {
                "last_run_at": datetime.now(timezone.utc).isoformat(),
                "last_actions": actions[:10],
                "last_error_notified": "",
            }
        )
        if not actions:
            write_audit(
                settings.audit_path,
                "guard_auto_noop",
                {"actor": actor, "reason": "no rows updated"},
            )
        return actions
    except Exception as exc:
        fallback_actions = run_guard_balance_fallback(actor)
        error = f"incremental guard failed; fallback balance scan applied {len(fallback_actions)} action(s): {exc}"
        guard_state["last_error"] = error
        guard_state["last_error_at"] = datetime.now(timezone.utc).isoformat()
        write_audit(
            settings.audit_path,
            "guard_auto_error",
            {
                "actor": actor,
                "error": str(exc),
                "fallback_action_count": len(fallback_actions),
            },
        )
        guard_state.update(
            {
                "last_run_at": datetime.now(timezone.utc).isoformat(),
                "last_actions": fallback_actions[:10],
            }
        )
        return fallback_actions
    finally:
        guard_state["running"] = False


async def run_auto_guard_threaded(actor: str = "auto_guard") -> list[dict[str, Any]]:
    async with guard_lock:
        return await asyncio.to_thread(run_auto_guard_once, actor)


def current_error_log_id() -> int:
    row = db.fetch_one("SELECT COALESCE(max(id), 0) AS cursor_id FROM ops_error_logs")
    return int((row or {}).get("cursor_id") or 0)


def load_telegram_error_alert_rows(cursor_id: int) -> list[dict[str, Any]]:
    return db.fetch_all(
        TELEGRAM_ERROR_ALERTS_SQL,
        {
            "cursor_id": max(0, int(cursor_id or 0)),
            "limit": settings.telegram_error_alert_batch_size,
        },
    )


def filter_telegram_error_alert_rows(
    rows: list[dict[str, Any]],
    policy: GuardPolicy | None = None,
) -> list[dict[str, Any]]:
    active_policy = policy or guard_policy_from_store()
    filtered: list[dict[str, Any]] = []
    for row in rows:
        account_id = row.get("account_id")
        if not account_id:
            continue
        if not is_whitelisted_account(active_policy, account_id):
            filtered.append(row)
    return filtered


def current_scheduled_test_result_id() -> int:
    capability = scheduled_test_capability()
    if not capability["available"]:
        return 0
    row = db.fetch_one("SELECT COALESCE(max(id), 0) AS cursor_id FROM scheduled_test_results")
    return int((row or {}).get("cursor_id") or 0)


def load_scheduled_test_recovery_alert_rows(cursor_id: int) -> list[dict[str, Any]]:
    return db.fetch_all(
        SCHEDULED_TEST_RECOVERY_ALERTS_SQL,
        {
            "cursor_id": max(0, int(cursor_id or 0)),
            "limit": settings.telegram_error_alert_batch_size,
        },
    )


def scheduled_test_needs_recovery(row: dict[str, Any]) -> bool:
    if is_oauth_account(row):
        return False
    account_status = row.get("account_status", row.get("status"))
    return (
        bool(account_status and account_status != "active")
        or not row.get("schedulable", True)
        or bool(row.get("rate_limited_at"))
        or bool(row.get("rate_limit_reset_at"))
        or bool(row.get("overload_until"))
        or account_ops.is_cooling(row)
        or bool(str(row.get("error_message") or "").strip())
    )


async def notify_telegram(text: str) -> None:
    if telegram_bot is None:
        return
    try:
        await telegram_bot.notify(text)
    except Exception:
        return


async def notify_telegram_account_alerts(title: str, actions: list[dict[str, Any]]) -> None:
    if telegram_bot is None:
        return
    try:
        await telegram_bot.notify_account_alerts(title, actions)
    except Exception:
        return


def process_guard_recovery_circuits() -> None:
    if not scheduled_test_capability()["available"]:
        return
    store = guard_store()
    cursor_id = store.recovery_cursor()
    if cursor_id <= 0:
        current_id = current_scheduled_test_result_id()
        if current_id > 0:
            store.set_recovery_cursor(current_id)
        return
    rows = load_scheduled_test_recovery_alert_rows(cursor_id)
    if not rows:
        return
    engine = guard_engine(store)
    next_cursor = cursor_id
    for row in rows:
        next_cursor = max(next_cursor, int(row.get("result_id") or cursor_id))
        if not scheduled_test_needs_recovery(row):
            continue
        engine.record_recovery_success(
            int(row["account_id"]),
            int(row["result_id"]),
            f"scheduled test success: {row.get('model_id') or ''}",
        )
    engine.store.set_recovery_cursor(next_cursor)


async def telegram_recovery_alert_loop() -> None:
    while True:
        try:
            bot = telegram_bot
            if bot is not None and bot.enabled:
                try:
                    state = await bot.oauth_recovery_state()
                    oauth_recovery_rows = await asyncio.to_thread(scan_oauth_quota_recovery_alerts, state=state)
                    delivered_oauth_recovery_rows: list[dict[str, Any]] = []
                    if oauth_recovery_rows:
                        if telegram_oauth_recovery_push_enabled():
                            delivered_oauth_recovery_rows = await bot.notify_oauth_quota_recovery_alerts(oauth_recovery_rows)
                        else:
                            suppress_oauth_recovery_alerts(state, oauth_recovery_rows)
                            await bot.save_oauth_recovery_state(state)
                        if delivered_oauth_recovery_rows:
                            mark_oauth_recovery_alerts_notified(state, delivered_oauth_recovery_rows)
                            await bot.save_oauth_recovery_state(state)
                        delivered_ids = {
                            int(row.get("account_id") or 0)
                            for row in delivered_oauth_recovery_rows
                        }
                        write_audit(
                            settings.audit_path,
                            "telegram_oauth_quota_recovery_push",
                            {
                                "row_count": len(oauth_recovery_rows),
                                "delivered_count": len(delivered_oauth_recovery_rows),
                                "delivered_account_ids": [
                                    int(row.get("account_id") or 0) for row in delivered_oauth_recovery_rows
                                ],
                                "undelivered_account_ids": [
                                    int(row.get("account_id") or 0)
                                    for row in oauth_recovery_rows
                                    if int(row.get("account_id") or 0) not in delivered_ids
                                ],
                                "suppressed": not telegram_oauth_recovery_push_enabled(),
                            },
                        )
                except Exception as exc:
                    write_audit(settings.audit_path, "telegram_oauth_quota_recovery_error", {"stage": "loop", "error": str(exc)})
                cursor_id = await bot.recovery_alert_cursor_id()
                if cursor_id <= 0:
                    current_id = await asyncio.to_thread(current_scheduled_test_result_id)
                    await bot.set_recovery_alert_cursor_id(current_id)
                    guard_store().set_recovery_cursor(current_id)
                else:
                    rows = await asyncio.to_thread(load_scheduled_test_recovery_alert_rows, cursor_id)
                    if rows:
                        engine = guard_engine()
                        recovery_rows = [row for row in rows if scheduled_test_needs_recovery(row)]
                        for row in recovery_rows:
                            engine.record_recovery_success(
                                int(row["account_id"]),
                                int(row["result_id"]),
                                f"scheduled test success: {row.get('model_id') or ''}",
                            )
                        next_cursor = max(int(row.get("result_id") or cursor_id) for row in rows)
                        engine.store.set_recovery_cursor(next_cursor)
                        if recovery_rows:
                            await bot.notify_recovery_alerts(recovery_rows)
                            write_audit(
                                settings.audit_path,
                                "telegram_scheduled_test_recovery_push",
                                {
                                    "row_count": len(recovery_rows),
                                    "from_cursor_id": cursor_id,
                                    "to_cursor_id": next_cursor,
                                },
                            )
                        await bot.set_recovery_alert_cursor_id(next_cursor)
            else:
                await asyncio.to_thread(process_guard_recovery_circuits)
        except Exception as exc:
            write_audit(settings.audit_path, "telegram_scheduled_test_recovery_error", {"error": str(exc)})
        await asyncio.sleep(settings.telegram_error_alert_interval_seconds)


async def telegram_error_alert_loop() -> None:
    while True:
        try:
            bot = telegram_bot
            if bot is not None and bot.enabled and settings.telegram_error_alert_enabled:
                cursor_id = await bot.error_alert_cursor_id()
                if cursor_id <= 0:
                    await bot.set_error_alert_cursor_id(await asyncio.to_thread(current_error_log_id))
                else:
                    rows = await asyncio.to_thread(load_telegram_error_alert_rows, cursor_id)
                    if rows:
                        next_cursor = max(int(row.get("error_log_id") or cursor_id) for row in rows)
                        policy = await asyncio.to_thread(guard_policy_from_store)
                        account_rows = filter_telegram_error_alert_rows(rows, policy)
                        if account_rows:
                            await bot.notify_error_chain_alerts(account_rows)
                            write_audit(
                                settings.audit_path,
                                "telegram_error_alert_push",
                                {
                                    "row_count": len(account_rows),
                                    "from_cursor_id": cursor_id,
                                    "to_cursor_id": next_cursor,
                                },
                            )
                        await bot.set_error_alert_cursor_id(next_cursor)
        except Exception as exc:
            write_audit(settings.audit_path, "telegram_error_alert_error", {"error": str(exc)})
        await asyncio.sleep(settings.telegram_error_alert_interval_seconds)


async def auto_guard_loop() -> None:
    while True:
        try:
            actions = await run_auto_guard_threaded()
            error = str(guard_state.get("last_error") or "")
            if error and guard_state.get("last_error_notified") != error:
                await notify_telegram(
                    "自动 Guard 增量扫描失败，已执行余额/额度兜底扫描\n"
                    f"兜底动作：{len(actions)}\n"
                    f"错误：{error}"
                )
                guard_state["last_error_notified"] = error
            if actions:
                await notify_telegram_account_alerts("自动 Guard 已处理账号异常", actions)
        except Exception as exc:
            await notify_telegram(f"自动 Guard 执行失败\n{exc}")
        await asyncio.sleep(settings.guard_interval_seconds)


def guard_suggestion(row: dict[str, Any]) -> dict[str, Any] | None:
    if is_oauth_account(row):
        return None
    if not row.get("schedulable"):
        return None
    account_id = row["id"]
    name = row["name"]
    blocked = int(row.get("blocked_403_window") or 0)
    balance = int(row.get("balance_or_quota_window") or 0)
    unstable = int(row.get("unstable_5xx_stream_window") or 0)
    rate_limit = int(row.get("rate_limit_window") or 0)
    errors = int(row.get("account_quality_errors_window") or 0)
    error_rate = row.get("error_rate_window_pct")
    error_rate_float = float(error_rate) if error_rate is not None else 0.0

    if blocked > 0:
        return {
            "account_id": account_id,
            "name": name,
            "action": "pause",
            "minutes": None,
            "reason": f"auto guard suggestion: {blocked} blocked 403 errors in window",
        }
    if balance > 0:
        return {
            "account_id": account_id,
            "name": name,
            "action": "pause",
            "minutes": None,
            "reason": f"auto guard suggestion: {balance} balance/quota errors in window",
        }
    if errors >= 5 and error_rate_float >= 50:
        return {
            "account_id": account_id,
            "name": name,
            "action": "cooldown",
            "minutes": 30,
            "reason": f"auto guard suggestion: {errors} quality errors, {error_rate_float:.1f}% error rate",
        }
    if unstable >= 5:
        return {
            "account_id": account_id,
            "name": name,
            "action": "cooldown",
            "minutes": 20,
            "reason": f"auto guard suggestion: {unstable} upstream 5xx/stream errors in window",
        }
    if rate_limit >= 3:
        return {
            "account_id": account_id,
            "name": name,
            "action": "cooldown",
            "minutes": 10,
            "reason": f"auto guard suggestion: {rate_limit} rate-limit errors in window",
        }
    return None


def account_problem(row: dict[str, Any]) -> dict[str, str]:
    if not row.get("schedulable"):
        return {"level": "muted", "title": "已停调度", "detail": "当前不会被 Sub2API 选中。"}
    if int(row.get("blocked_403_window") or 0) > 0:
        return {"level": "bad", "title": "403 blocked", "detail": "供应商明确拦截，建议暂停。"}
    if int(row.get("balance_or_quota_window") or 0) > 0:
        return {"level": "bad", "title": "余额/额度不足", "detail": "继续调度只会制造失败，建议暂停。"}
    if int(row.get("unstable_5xx_stream_window") or 0) >= 5:
        return {"level": "warn", "title": "上游不稳定", "detail": "5xx 或流式终止频繁，建议冷却。"}
    if int(row.get("rate_limit_window") or 0) >= 3:
        return {"level": "warn", "title": "限流偏高", "detail": "建议短冷却或降低并发。"}
    if int(row.get("account_quality_errors_window") or 0) > 0:
        return {"level": "warn", "title": "有错误", "detail": "样本不多，先观察链路。"}
    return {"level": "good", "title": "正常", "detail": "当前窗口没有账号质量错误。"}


def build_dashboard(
    rows: list[dict[str, Any]],
) -> dict[str, Any]:
    active_rows = [row for row in rows if row.get("schedulable")]

    for row in rows:
        row["problem"] = account_problem(row)

    return {
        "active_count": len(active_rows),
        "success_count": sum(int(row.get("success_window") or 0) for row in rows),
        "quality_error_count": sum(int(row.get("account_quality_errors_window") or 0) for row in rows),
        "suggestion_count": sum(1 for row in rows if guard_suggestion(row)),
    }


def weighted_average(
    rows: list[dict[str, Any]],
    value_key: str,
    weight_key: str = "success_window",
) -> float | None:
    total_weight = 0
    weighted_total = 0.0
    for row in rows:
        value = row.get(value_key)
        if value in (None, ""):
            continue
        weight = int(row.get(weight_key) or 0)
        if weight <= 0:
            continue
        weighted_total += float(value) * weight
        total_weight += weight
    if total_weight <= 0:
        return None
    return round(weighted_total / total_weight, 2)


def build_speed_dashboard(rows: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "success_count": sum(int(row.get("success_window") or 0) for row in rows),
        "output_tokens": sum(int(row.get("output_tokens_window") or 0) for row in rows),
        "avg_first_token_ms": weighted_average(rows, "avg_first_token_ms"),
        "avg_duration_ms": weighted_average(rows, "avg_duration_ms"),
        "avg_ms_per_output_token": weighted_average(rows, "avg_ms_per_output_token"),
        "usage_total_cost": sum(float(row.get("usage_total_cost") or 0) for row in rows),
        "usage_total_tokens": sum(int(row.get("usage_total_tokens") or 0) for row in rows),
    }


@app.get("/sso/start")
def sub2api_sso_start(
    request: Request,
    token: str = "",
    user_id: str = "",
    next: str = "/",
) -> Response:
    next_path = safe_next(next)
    client_host = request.client.host if request.client else ""
    sso_config = current_sso_config()

    if not sso_config.enabled:
        write_audit(settings.audit_path, "sso_login_reject", {"reason": "disabled", "client": client_host})
        return sso_required_response()

    verify_base_url = sso_config.verify_base_url or sso_config.base_url
    try:
        principal = validate_sub2api_token(
            verify_base_url,
            token=token,
            expected_user_id=user_id or None,
            required_role=sso_config.required_role,
            timeout_seconds=sso_config.verify_timeout_seconds,
        )
    except Sub2APISSOError as exc:
        write_audit(
            settings.audit_path,
            "sso_login_reject",
            {
                "reason": exc.reason,
                "message": exc.message,
                "user_id": user_id,
                "client": client_host,
                "src_host": request.query_params.get("src_host", ""),
                "verify_base_url_set": bool(sso_config.verify_base_url),
            },
        )
        return sso_required_response()

    session_user = f"sub2api:{principal.id}:{principal.username}"
    issued_at = int(time.time())
    response = no_store_redirect(f"{settings.base_path}{next_path}")
    response.set_cookie(
        SESSION_COOKIE,
        sign_session(
            session_user,
            issued_at,
            ttl_seconds=sso_config.session_ttl_seconds,
            source="sub2api_sso",
        ),
        max_age=sso_config.session_ttl_seconds,
        httponly=True,
        secure=True,
        samesite="lax",
        path=cookie_path(),
    )
    write_audit(
        settings.audit_path,
        "sso_login",
        {
            "user": session_user,
            "sub2api_user_id": principal.id,
            "sub2api_role": principal.role,
            "client": client_host,
            "src_host": request.query_params.get("src_host", ""),
        },
    )
    return response


@app.post("/logout")
def logout(user: AuthUser) -> Response:
    response = no_store_redirect(f"{settings.base_path}/sso/start")
    response.delete_cookie(SESSION_COOKIE, path=cookie_path())
    write_audit(settings.audit_path, "logout", {"user": user})
    return response


@app.get("/", response_class=HTMLResponse)
def index(
    request: Request,
    _: AuthUser,
    platform: str = "openai",
    time_range: str = "",
    start_date: str = "",
    end_date: str = "",
    hours: int | None = None,
    sort: str = "default",
    msg: str = "",
) -> HTMLResponse:
    groups = load_groups()
    group_selection = build_group_selection(request.query_params.getlist("group"), groups)
    selected_range = build_time_range(time_range, start_date, end_date, hours)
    selected_sort = normalize_stability_sort(sort)
    rows = sort_stability_rows(
        load_quality(group_selection["selected"], platform, selected_range["start_at"], selected_range["end_at"]),
        selected_sort,
    )
    suggestions = [s for row in rows if (s := guard_suggestion(row))]
    dashboard = build_dashboard(rows)
    requests_query = clean_query_string({"platform": platform, **selected_range["query_args"]})
    return render(
        request,
        "index.html",
        {
            "active": "stability",
            "rows": rows,
            "groups": groups,
            "group": group_selection["selected"][0],
            "group_selection": group_selection,
            "platform": platform,
            "time_range": selected_range,
            "sort": selected_sort,
            "sort_options": STABILITY_SORT_OPTIONS,
            "requests_query": requests_query,
            "suggestions": suggestions,
            "dashboard": dashboard,
            "guard": guard_config(),
            "msg": msg,
        },
    )


@app.get("/speed", response_class=HTMLResponse)
def speed_view(
    request: Request,
    _: AuthUser,
    platform: str = "openai",
    time_range: str = "",
    start_date: str = "",
    end_date: str = "",
    hours: int | None = None,
    msg: str = "",
) -> HTMLResponse:
    groups = load_groups()
    group_selection = build_group_selection(request.query_params.getlist("group"), groups)
    selected_range = build_time_range(time_range, start_date, end_date, hours)
    store = usage_query_store()
    rows = enrich_usage_query_rows(
        sort_speed_rows(
            load_speed_quality(group_selection["selected"], platform, selected_range["start_at"], selected_range["end_at"])
        ),
        store,
    )
    dashboard = build_speed_dashboard(rows)
    dashboard.update(usage_query_dashboard(rows, store))
    return render(
        request,
        "speed.html",
        {
            "active": "speed",
            "rows": rows,
            "groups": groups,
            "group": group_selection["selected"][0],
            "group_selection": group_selection,
            "platform": platform,
            "time_range": selected_range,
            "usage_query_settings": usage_query_settings(store),
            "dashboard": dashboard,
            "return_to": f"{settings.base_path}/speed"
            + (f"?{request.url.query}" if request.url.query else ""),
            "msg": msg,
        },
    )


@app.post("/usage-query/accounts/{account_id}")
async def usage_query_config_save(request: Request, user: AuthUser, account_id: int) -> Response:
    form = await request.form()
    raw = dict(form)
    return_to = str(raw.get("return_to") or f"{settings.base_path}/speed")
    store = usage_query_store()
    config = usage_query_config_from_form(account_id, raw, store.config(account_id), user)
    store.save_config(config)
    row = usage_query_account_row(account_id)
    hydrated = hydrate_usage_query_config(config, row)
    write_audit(
        settings.audit_path,
        "usage_query_config_save",
        usage_query_audit_payload(hydrated, user),
    )
    return redirect_with_msg(return_to, f"已保存账号 #{account_id} 的额度查询配置")


@app.get("/usage-query/accounts/{account_id}/editor", response_class=HTMLResponse)
def usage_query_account_editor(
    request: Request,
    _: AuthUser,
    account_id: int,
    return_to: str = "",
) -> HTMLResponse:
    row = usage_query_account_row(account_id)
    if row is None:
        raise HTTPException(status_code=404, detail="account not found")
    usage = usage_query_editor_view(account_id)
    html = templates.get_template("usage_query_editor.html").render(
        {
            "base_path": settings.base_path,
            "account_id": int(account_id),
            "account": row,
            "usage": usage,
            "return_to": return_to or f"{settings.base_path}/speed#usage-query-{int(account_id)}",
        }
    )
    return HTMLResponse(html)


@app.post("/usage-query/settings")
async def usage_query_settings_save(
    request: Request,
    user: AuthUser,
) -> Response:
    form = await request.form()
    raw = dict(form)
    return_to = str(raw.get("return_to") or "/speed")
    store = usage_query_store()

    def any_form_truthy(name: str) -> bool:
        values = form.getlist(name)
        return any(form_truthy(value) for value in values)

    if "usage_query_enabled" in form:
        query_enabled = any_form_truthy("usage_query_enabled")
    else:
        query_enabled = store.usage_query_enabled()
    if "guard_disable_on_zero" in form:
        hard_stop_enabled = any_form_truthy("guard_disable_on_zero")
    else:
        hard_stop_enabled = store.guard_disable_on_zero()
    if raw.get("auto_query_interval_seconds") not in {None, ""}:
        interval = int_param(str(raw.get("auto_query_interval_seconds")), 3600, 0, 86400)
    elif raw.get("auto_query_interval_minutes") not in {None, ""}:
        interval = int_param(str(raw.get("auto_query_interval_minutes")), 60, 0, 1440) * 60
    else:
        interval = store.auto_query_interval_seconds()
    store.save_usage_query_settings(
        usage_query_enabled=query_enabled,
        guard_disable_on_zero=hard_stop_enabled,
        auto_query_interval_seconds=interval,
        sub2api_admin_token=raw.get("sub2api_admin_token"),
    )
    admin_token_set = bool(str(raw.get("sub2api_admin_token") or "").strip())
    write_audit(
        settings.audit_path,
        "usage_query_settings_save",
        {
            "user": user,
            "usage_query_enabled": query_enabled,
            "guard_disable_on_zero": hard_stop_enabled,
            "auto_query_interval_seconds": interval,
            "sub2api_admin_token_set": admin_token_set,
            "sub2api_admin_token_saved": bool(store.sub2api_admin_token()),
        },
    )
    return redirect_with_msg(return_to, f"已保存全局额度查询设置：自动间隔 {interval} 秒")


@app.post("/usage-query/accounts/{account_id}/fill-credentials")
async def usage_query_fill_credentials(request: Request, user: AuthUser, account_id: int) -> Response:
    form = await request.form()
    raw = dict(form)
    return_to = str(raw.get("return_to") or f"{settings.base_path}/speed")
    store = usage_query_store()
    base_config = usage_query_config_from_form(account_id, raw, store.config(account_id), user)
    row = usage_query_account_row(account_id)
    filled = fill_account_credentials(base_config, row)
    store.save_config(filled)
    hydrated = hydrate_usage_query_config(filled, row)
    credentials = account_credentials(row or {})
    credentials_found = bool(credentials.get("base_url") or credentials.get("api_key"))
    write_audit(
        settings.audit_path,
        "usage_query_fill_credentials",
        {
            **usage_query_audit_payload(hydrated, user),
            "credentials_found": credentials_found,
            "base_url_filled": bool(hydrated.base_url),
            "api_key_filled": bool(hydrated.api_key),
            "access_token_filled": bool(hydrated.access_token),
        },
    )
    if credentials_found:
        return redirect_with_msg(return_to, f"账号 #{account_id} 已改为实时读取 Base URL / API Key")
    return redirect_with_msg(return_to, f"账号 #{account_id} 没有可读取的 Base URL / API Key")


@app.post("/usage-query/accounts/{account_id}/query")
async def usage_query_account_query(request: Request, user: AuthUser, account_id: int) -> Response:
    form = await request.form()
    raw = dict(form)
    return_to = str(raw.get("return_to") or f"{settings.base_path}/speed")
    store = usage_query_store()
    config = usage_query_config_from_form(account_id, raw, store.config(account_id), user)
    store.save_config(config)
    row = usage_query_account_row(account_id)
    if row and is_oauth_account(row):
        result = run_oauth_usage_query(account_id, row, store, timeout_seconds=config.timeout_seconds)
        store.save_result(account_id, result)
        write_audit(
            settings.audit_path,
            "usage_query_account_query",
            {
                **usage_query_audit_payload(config, user),
                "success": result.get("success"),
                "oauth_query": True,
                "error": result.get("error") if not result.get("success") else "",
            },
        )
        if result.get("success"):
            return redirect_with_msg(return_to, f"账号 #{account_id} OAuth 额度查询成功")
        return redirect_with_msg(return_to, f"账号 #{account_id} OAuth 额度查询失败：{result.get('error') or '未知错误'}")
    hydrated = hydrate_usage_query_config(config, row)
    result = execute_usage_query(hydrated)
    store.save_result(account_id, result)
    write_audit(
        settings.audit_path,
        "usage_query_account_query",
        {
            **usage_query_audit_payload(hydrated, user),
            "success": result.get("success"),
            "remaining": result.get("remaining"),
            "actual_available": result.get("actual_available"),
            "error": result.get("error") if not result.get("success") else "",
        },
    )
    if result.get("success"):
        return redirect_with_msg(return_to, f"账号 #{account_id} 额度查询成功")
    return redirect_with_msg(return_to, f"账号 #{account_id} 额度查询失败：{result.get('error') or '未知错误'}")


@app.post("/usage-query/accounts/{account_id}/delete")
async def usage_query_config_delete(
    user: AuthUser,
    account_id: int,
    return_to: str = Form("/speed"),
) -> Response:
    usage_query_store().delete_config(account_id)
    write_audit(settings.audit_path, "usage_query_config_delete", {"user": user, "account_id": account_id})
    return redirect_with_msg(return_to, f"已移除账号 #{account_id} 的额度查询配置")


@app.post("/usage-query/query-enabled")
async def usage_query_query_enabled(user: AuthUser, return_to: str = Form("/speed")) -> Response:
    store = usage_query_store()
    queried = 0
    failed = 0
    skipped_missing = 0
    oauth_queried = 0
    oauth_failed = 0
    if not store.usage_query_enabled():
        write_audit(
            settings.audit_path,
            "usage_query_batch_query",
            {
                "user": user,
                "queried": 0,
                "failed": 0,
                "skipped_missing": 0,
                "oauth_queried": 0,
                "oauth_failed": 0,
                "usage_query_enabled": False,
            },
        )
        return redirect_with_msg(return_to, "全局额度查询已关闭，未查询账号")
    seen_account_ids: set[int] = set()
    for config in store.configs():
        if not usage_query_configured(config):
            continue
        seen_account_ids.add(config.account_id)
        row = usage_query_account_row(config.account_id)
        if not row:
            skipped_missing += 1
            continue
        if row and is_oauth_account(row):
            result = run_oauth_usage_query(config.account_id, row, store, timeout_seconds=config.timeout_seconds)
            oauth_queried += 1
        else:
            result = execute_usage_query(hydrate_usage_query_config(config, row))
        store.save_result(config.account_id, result)
        queried += 1
        if not result.get("success"):
            failed += 1
            if row and is_oauth_account(row):
                oauth_failed += 1
    try:
        oauth_rows = usage_query_oauth_account_rows()
    except Exception:
        oauth_rows = []
    for row in oauth_rows:
        account_id = int(row.get("id") or 0)
        if account_id <= 0 or account_id in seen_account_ids or not is_oauth_account(row):
            continue
        config = usage_query_oauth_config(account_id, store)
        result = run_oauth_usage_query(account_id, row, store, timeout_seconds=config.timeout_seconds)
        store.save_result(account_id, result)
        queried += 1
        oauth_queried += 1
        if not result.get("success"):
            failed += 1
            oauth_failed += 1
    write_audit(
        settings.audit_path,
        "usage_query_batch_query",
        {
            "user": user,
            "queried": queried,
            "failed": failed,
            "skipped_missing": skipped_missing,
            "oauth_queried": oauth_queried,
            "oauth_failed": oauth_failed,
            "usage_query_enabled": True,
        },
    )
    return redirect_with_msg(
        return_to,
        f"已查询 {queried} 个已配置账号，失败 {failed} 个",
    )


@app.get("/scheduled-tests", response_class=HTMLResponse)
def scheduled_tests_view(
    request: Request,
    _: AuthUser,
    platform: str = "openai",
    include_all: str = "",
    msg: str = "",
) -> HTMLResponse:
    groups = load_groups()
    group_selection = build_group_selection(request.query_params.getlist("group"), groups)
    capability = scheduled_test_capability()
    include_all_flag = form_truthy(include_all)
    rows: list[dict[str, Any]] = []
    results: list[dict[str, Any]] = []
    if capability["available"]:
        rows = load_scheduled_test_accounts(group_selection["selected"], platform, include_all_flag)
        results = load_scheduled_test_results(limit=30)
    return render(
        request,
        "scheduled_tests.html",
        {
            "active": "scheduled_tests",
            "group": group_selection["selected"][0],
            "platform": platform,
            "include_all": include_all_flag,
            "groups": groups,
            "group_selection": group_selection,
            "platform_options": load_platform_options(),
            "capability": capability,
            "rows": rows,
            "results": results,
            "dashboard": scheduled_test_dashboard(rows),
            "interval_options": interval_options(),
            "msg": msg,
        },
    )


@app.post("/scheduled-tests")
async def scheduled_test_save(
    request: Request,
    user: AuthUser,
    account_id: int = Form(...),
    model_id: str = Form(""),
    interval_minutes: str = Form("30"),
    enabled: str | None = Form(None),
    auto_recover: str | None = Form(None),
    max_results: int = Form(50),
    platform: str = Form("openai"),
    include_all: str = Form(""),
) -> Response:
    form = await request.form()
    group_values = [str(value) for value in form.getlist("group")]
    capability = scheduled_test_capability()
    include_all_flag = form_truthy(include_all)
    if not capability["available"]:
        return RedirectResponse(
            scheduled_tests_url(group_values, platform, include_all_flag, "上游定时测试表未就绪"),
            status_code=303,
        )

    interval = normalize_interval_minutes(interval_minutes)
    next_run_at = next_aligned_run(datetime.now(timezone.utc), interval)
    row = db.fetch_one(
        SCHEDULED_TEST_UPSERT_SQL,
        {
            "account_id": account_id,
            "model_id": model_id.strip(),
            "cron_expression": schedule_cron(interval),
            "enabled": form_truthy(enabled),
            "max_results": int_param(str(max_results), 50, 1, 500),
            "auto_recover": form_truthy(auto_recover),
            "next_run_at": next_run_at,
        },
    )
    write_audit(
        settings.audit_path,
        "scheduled_test_plan_save",
        {"user": user, "account_id": account_id, "plan": row, "interval_minutes": interval},
    )
    return RedirectResponse(
        scheduled_tests_url(group_values, platform, include_all_flag, f"已保存账号 #{account_id} 的定时恢复计划"),
        status_code=303,
    )


@app.post("/scheduled-tests/{plan_id}/delete")
async def scheduled_test_delete(
    request: Request,
    user: AuthUser,
    plan_id: int,
    platform: str = Form("openai"),
    include_all: str = Form(""),
) -> Response:
    form = await request.form()
    group_values = [str(value) for value in form.getlist("group")]
    row = db.fetch_one(SCHEDULED_TEST_DELETE_SQL, {"plan_id": plan_id})
    write_audit(settings.audit_path, "scheduled_test_plan_delete", {"user": user, "plan_id": plan_id, "plan": row})
    return RedirectResponse(
        scheduled_tests_url(group_values, platform, form_truthy(include_all), f"已删除定时恢复计划 #{plan_id}"),
        status_code=303,
    )


@app.get("/requests", response_class=HTMLResponse)
def requests_view(
    request: Request,
    _: AuthUser,
    q: str = "",
    platform: str = "openai",
    account_id: str = "",
    time_range: str = "",
    start_date: str = "",
    end_date: str = "",
    hours: int | None = None,
    limit: int = 200,
) -> HTMLResponse:
    platform = platform.strip()
    account_id = account_id.strip()
    selected_range = build_time_range(time_range, start_date, end_date, hours)
    parsed_limit = int_param(str(limit), 200, 1, 1000)
    scan_limit = request_scan_limit(parsed_limit, account_id, q)
    rows = db.fetch_all(
        REQUESTS_SQL,
        {
            "q": q.strip(),
            "platform": platform,
            "account_id": nullable_int(account_id),
            "range_start": selected_range["start_at"],
            "range_end": selected_range["end_at"],
            "limit": parsed_limit,
            "scan_limit": scan_limit,
        },
    )
    return render(
        request,
        "requests.html",
        {
            "active": "requests",
            "rows": rows,
            "q": q.strip(),
            "platform": platform,
            "account_id": account_id,
            "time_range": selected_range,
            "limit": parsed_limit,
            "scan_limit": scan_limit,
            "platform_options": load_platform_options(),
            "account_options": load_account_options(platform),
        },
    )


@app.get("/requests/{request_id}", response_class=HTMLResponse)
def request_detail(request: Request, _: AuthUser, request_id: str) -> HTMLResponse:
    detail_range = rolling_hours_range(168)
    rows = db.fetch_all(
        REQUESTS_SQL,
        {
            "q": request_id,
            "platform": "",
            "account_id": None,
            "range_start": detail_range["start_at"],
            "range_end": detail_range["end_at"],
            "limit": 200,
            "scan_limit": request_scan_limit(200, "", request_id),
        },
    )
    return render(
        request,
        "request_detail.html",
        {
            "active": "requests",
            "request_id": request_id,
            "rows": rows,
        },
    )


@app.get("/guard", response_class=HTMLResponse)
def guard_view(
    request: Request,
    _: AuthUser,
    msg: str = "",
) -> HTMLResponse:
    policy = guard_policy_from_store()
    return render(
        request,
        "guard.html",
        {
            "active": "guard",
            "guard": guard_config(policy, include_recent_events=False),
            "whitelist_options": guard_whitelist_account_options(load_guard_account_options(), policy),
            "guard_section_cards": guard_section_cards(guard_queue_section_query(request)),
            "msg": msg,
        },
    )


@app.get("/guard/sections/queue", response_class=HTMLResponse)
def guard_section_queue(request: Request, _: AuthUser) -> HTMLResponse:
    return render(request, "guard_queue_section.html", guard_queue_context(request))


@app.get("/guard/sections/suggestions", response_class=HTMLResponse)
def guard_section_suggestions(request: Request, _: AuthUser) -> HTMLResponse:
    return render(request, "guard_suggestions_section.html", guard_quality_context(include_suggestions=True))


@app.get("/guard/sections/routing", response_class=HTMLResponse)
def guard_section_routing(request: Request, _: AuthUser) -> HTMLResponse:
    return render(request, "guard_routing_section.html", guard_quality_context(include_routing=True))


@app.get("/guard/sections/audit", response_class=HTMLResponse)
def guard_section_audit(request: Request, _: AuthUser) -> HTMLResponse:
    return render(request, "guard_audit_section.html", guard_audit_context())


@app.get("/telegram", response_class=HTMLResponse)
def telegram_view(request: Request, _: AuthUser, msg: str = "") -> HTMLResponse:
    return render(
        request,
        "telegram.html",
        {
            "active": "telegram",
            "telegram": build_telegram_config(),
            "guard": guard_config(),
            "msg": msg,
        },
    )


@app.get("/sso", response_class=HTMLResponse)
def sso_view(request: Request, _: AuthUser, msg: str = "") -> HTMLResponse:
    sso_runtime = current_sso_config()
    usage_store = usage_query_store()
    sso_panel = build_sso_panel_config(sso_runtime, base_path=settings.base_path)
    sso_panel["sub2api_admin_token_saved"] = bool(usage_store.sub2api_admin_token())
    return render(
        request,
        "sso.html",
        {
            "active": "sso",
            "sso": sso_panel,
            "sso_config_path": settings.sso_config_path,
            "msg": msg,
        },
    )


@app.post("/guard/run")
async def run_guard_now(user: AuthUser) -> Response:
    actions = await run_auto_guard_threaded(user)
    if guard_state.get("last_error"):
        await notify_telegram(
            f"用户 {user} 手动执行 Guard 时增量扫描失败，已执行余额/额度兜底扫描\n"
            f"兜底动作：{len(actions)}\n"
            f"错误：{guard_state.get('last_error')}"
        )
    if actions:
        await notify_telegram_account_alerts(f"账号异常：用户 {user} 手动执行 Guard", actions)
    if guard_state.get("last_error"):
        return RedirectResponse(
            f"{settings.base_path}/guard?msg={quote(f'Guard 增量扫描失败，已执行余额兜底 {len(actions)} 个动作')}",
            status_code=303,
        )
    return RedirectResponse(f"{settings.base_path}/guard?msg=guard+applied+{len(actions)}+actions", status_code=303)


@app.post("/guard/policy")
def guard_policy_save(
    user: AuthUser,
    hard_pause_enabled: str | None = Form(None),
    rate_limit_enabled: str | None = Form(None),
    unstable_enabled: str | None = Form(None),
    failure_threshold: int = Form(4),
    success_threshold: int = Form(2),
    circuit_timeout_seconds: int = Form(60),
    blocked_403_threshold: int = Form(1),
    balance_pause_threshold: int = Form(1),
    whitelist_account_ids: list[str] | None = Form(None),
    whitelist_balance_pause_threshold: int = Form(10),
) -> Response:
    parsed_whitelist = parse_int_csv(whitelist_account_ids)
    payload = {
        "hard_pause_enabled": form_truthy(hard_pause_enabled),
        "rate_limit_enabled": form_truthy(rate_limit_enabled),
        "unstable_enabled": form_truthy(unstable_enabled),
        "failure_threshold": int_param(str(failure_threshold), 4, 1, 50),
        "success_threshold": int_param(str(success_threshold), 2, 1, 20),
        "circuit_timeout_seconds": int_param(str(circuit_timeout_seconds), 60, 5, 3600),
        "blocked_403_threshold": int_param(str(blocked_403_threshold), 1, 1, 20),
        "balance_pause_threshold": int_param(str(balance_pause_threshold), 1, 1, 20),
        "whitelist_account_ids": list(parsed_whitelist),
        "whitelist_balance_pause_threshold": int_param(str(whitelist_balance_pause_threshold), 10, 1, 100),
        "updated_by": user,
        "updated_at": datetime.now(timezone.utc).isoformat(),
    }
    guard_store().save_policy(payload)
    write_audit(settings.audit_path, "guard_policy_update", payload)
    return RedirectResponse(f"{settings.base_path}/guard?msg={quote('Guard 策略已保存')}", status_code=303)


@app.post("/guard/queue/reorder")
async def guard_queue_reorder(request: Request, user: AuthUser) -> Response:
    form = await request.form()
    queue_group_values = [str(value) for value in form.getlist("queue_group")]
    groups = load_groups()
    group_selection = build_group_selection(queue_group_values, groups)
    rows = filter_guard_queue_rows(enrich_guard_rows(load_guard_queue_quality()), group_selection)
    capability = account_routing_capability()
    if not capability["group_priority"]:
        raise HTTPException(status_code=400, detail="account_groups.priority column is not available")

    ordered_keys: list[str] = []
    for value in form.getlist("account_order"):
        item = str(value or "").strip()
        if item:
            ordered_keys.append(item)

    plan = reorder_queue_plan(rows, ordered_keys, load_factor_supported=capability["load_factor"])
    applied: list[dict[str, Any]] = []
    for item in plan:
        updated = account_ops.guard_update_account_group_priority(
            db,
            settings.audit_path,
            int(item["account_id"]),
            item.get("group_id"),
            str(item.get("group_name") or ""),
            user,
            int(item["group_priority"]),
            f"guard queue reorder: {item['group_name'] or '-'} P{item['position']}",
        )
        if updated:
            applied.append({**item, "updated": updated})
    write_audit(
        settings.audit_path,
        "guard_queue_reorder",
        {
            "user": user,
            "selected_groups": group_selection.get("selected"),
            "submitted": len(ordered_keys),
            "planned": len(plan),
            "applied": len(applied),
            "load_factor_supported": capability["load_factor"],
        },
    )
    return RedirectResponse(
        guard_queue_url(queue_group_values, f"已保存 {len(applied)} 个账号的队列顺序"),
        status_code=303,
    )


@app.post("/guard/queue/auto")
async def guard_queue_auto_apply(request: Request, user: AuthUser) -> Response:
    form = await request.form()
    queue_group_values = [str(value) for value in form.getlist("queue_group")]
    groups = load_groups()
    group_selection = build_group_selection(queue_group_values, groups)
    rows = filter_guard_queue_rows(enrich_guard_rows(load_guard_queue_quality()), group_selection)
    capability = account_routing_capability()
    if not capability["group_priority"]:
        raise HTTPException(status_code=400, detail="account_groups.priority column is not available")

    plan = auto_queue_plan(rows, load_factor_supported=capability["load_factor"])
    applied: list[dict[str, Any]] = []
    for item in plan:
        updated = account_ops.guard_update_account_group_priority(
            db,
            settings.audit_path,
            int(item["account_id"]),
            item.get("group_id"),
            str(item.get("group_name") or ""),
            user,
            int(item["group_priority"]),
            f"guard auto queue adjustment: {item['group_name'] or '-'} P{item['position']}",
        )
        if updated:
            load_factor_update = None
            if capability["load_factor"]:
                load_factor_update = account_ops.guard_update_account_load_factor(
                    db,
                    settings.audit_path,
                    int(item["account_id"]),
                    user,
                    item.get("load_factor"),
                    f"guard auto queue load factor: {item['group_name'] or '-'} P{item['position']}",
                )
            applied.append({**item, "updated": updated})
            if load_factor_update:
                applied[-1]["load_factor_updated"] = load_factor_update
    write_audit(
        settings.audit_path,
        "guard_auto_queue_adjustment",
        {
            "user": user,
            "selected_groups": group_selection.get("selected"),
            "planned": len(plan),
            "applied": len(applied),
            "load_factor_supported": capability["load_factor"],
        },
    )
    return RedirectResponse(
        guard_queue_url(queue_group_values, f"已按健康度重排 {len(applied)} 个账号"),
        status_code=303,
    )


@app.post("/guard/account-routing")
def guard_account_routing_save(
    user: AuthUser,
    account_id: int = Form(...),
    priority: int = Form(50),
    load_factor: str = Form(""),
    reason: str = Form("manual guard routing update"),
) -> Response:
    capability = account_routing_capability()
    if not capability["priority"]:
        raise HTTPException(status_code=400, detail="accounts.priority column is not available")
    updated = account_ops.guard_update_account_routing(
        db,
        settings.audit_path,
        account_id,
        user,
        account_ops.normalize_priority_value(priority),
        account_ops.normalize_load_factor_value(load_factor),
        capability["load_factor"],
        reason,
    )
    if not updated:
        raise HTTPException(status_code=404, detail="account not found")
    return RedirectResponse(f"{settings.base_path}/guard?msg={quote('账号优先级/负载因子已保存')}", status_code=303)


@app.post("/telegram/config")
async def telegram_config_save(
    user: AuthUser,
    telegram_bot_token: str = Form(""),
) -> Response:
    token = telegram_bot_token.strip() or settings.telegram_bot_token
    existing = telegram_config_file()
    pairing_code = str(existing.get("pairing_code") or settings.telegram_pairing_code or "").strip()
    if token and not pairing_code:
        pairing_code = generate_telegram_pairing_code()
    payload = {
        **existing,
        "enabled": bool(token),
        "bot_token": token,
        "pairing_enabled": True,
        "pairing_code": pairing_code,
        "updated_at": datetime.now(timezone.utc).isoformat(),
        "updated_by": user,
    }
    save_telegram_runtime_config(payload)
    apply_telegram_runtime_config(payload)
    await restart_telegram_bot()
    write_audit(
        settings.audit_path,
        "telegram_config_update",
        {
            "user": user,
            "enabled": payload["enabled"],
            "bot_token_set": bool(token),
            "pairing_code_set": bool(pairing_code),
            "mode": "token_with_pairing_code",
        },
    )
    return RedirectResponse(f"{settings.base_path}/telegram?msg={quote('Telegram 配置已保存，配对码已生成')}", status_code=303)


@app.post("/telegram/oauth-settings")
async def telegram_oauth_settings_save(request: Request, user: AuthUser) -> Response:
    form = await request.form()
    oauth_usage_refresh_enabled = bool(form.getlist("oauth_usage_refresh_enabled"))
    oauth_recovery_monitor_enabled = bool(form.getlist("oauth_recovery_monitor_enabled"))
    oauth_recovery_push_enabled = bool(form.getlist("oauth_recovery_push_enabled"))
    existing = telegram_config_file()
    payload = {
        **existing,
        "oauth_usage_refresh_enabled": oauth_usage_refresh_enabled,
        "oauth_recovery_monitor_enabled": oauth_recovery_monitor_enabled,
        "oauth_recovery_push_enabled": oauth_recovery_push_enabled,
        "updated_at": datetime.now(timezone.utc).isoformat(),
        "updated_by": user,
    }
    save_telegram_runtime_config(payload)
    apply_telegram_runtime_config(payload)
    await restart_telegram_bot()
    write_audit(
        settings.audit_path,
        "telegram_oauth_settings_update",
        {
            "user": user,
            "oauth_usage_refresh_enabled": oauth_usage_refresh_enabled,
            "oauth_recovery_monitor_enabled": oauth_recovery_monitor_enabled,
            "oauth_recovery_push_enabled": oauth_recovery_push_enabled,
        },
    )
    return RedirectResponse(f"{settings.base_path}/telegram?msg={quote('OAuth 账号监控开关已保存')}", status_code=303)


@app.post("/sso-config")
def sso_config_save(
    user: AuthUser,
    enabled: str | None = Form(None),
    base_url: str = Form(""),
    verify_base_url: str = Form(""),
    required_role: str = Form("admin"),
    session_ttl_seconds: int = Form(86400),
    verify_timeout_seconds: int = Form(5),
    sub2api_admin_token: str = Form(""),
) -> Response:
    clean_base_url = base_url.strip().rstrip("/")
    clean_verify_base_url = verify_base_url.strip().rstrip("/")
    if form_truthy(enabled) and not clean_base_url:
        return RedirectResponse(
            f"{settings.base_path}/sso?msg={quote('启用 Sub2API 免登录前需要填写 Sub2API 地址')}",
            status_code=303,
        )
    if clean_base_url:
        try:
            clean_base_url = normalize_base_url(clean_base_url)
        except Sub2APISSOError:
            return RedirectResponse(
                f"{settings.base_path}/sso?msg={quote('Sub2API 地址必须是完整的 http(s) 地址')}",
                status_code=303,
            )
    if clean_verify_base_url:
        try:
            clean_verify_base_url = normalize_base_url(clean_verify_base_url)
        except Sub2APISSOError:
            return RedirectResponse(
                f"{settings.base_path}/sso?msg={quote('服务端校验地址必须是完整的 http(s) 地址')}",
                status_code=303,
            )
    save_sso_config(
        settings.sso_config_path,
        enabled=form_truthy(enabled),
        base_url=clean_base_url,
        verify_base_url=clean_verify_base_url,
        required_role=required_role.strip() or "admin",
        session_ttl_seconds=session_ttl_seconds,
        verify_timeout_seconds=verify_timeout_seconds,
        updated_by=user,
    )
    usage_store = usage_query_store()
    usage_store.save_usage_query_settings(sub2api_admin_token=sub2api_admin_token)
    admin_token_set = bool(str(sub2api_admin_token or "").strip())
    write_audit(
        settings.audit_path,
        "ops_sso_config_update",
        {
            "user": user,
            "enabled": form_truthy(enabled),
            "base_url_set": bool(clean_base_url),
            "verify_base_url_set": bool(clean_verify_base_url),
            "required_role": required_role.strip() or "admin",
            "sub2api_admin_token_set": admin_token_set,
            "sub2api_admin_token_saved": bool(usage_store.sub2api_admin_token()),
        },
    )
    return RedirectResponse(
        f"{settings.base_path}/sso?msg={quote('Sub2API 免二次登录配置已保存，下一次从自定义菜单进入立即生效')}",
        status_code=303,
    )


@app.post("/telegram/pairing-code/regenerate")
async def telegram_pairing_code_regenerate(user: AuthUser) -> Response:
    existing = telegram_config_file()
    pairing_code = generate_telegram_pairing_code()
    payload = {
        **existing,
        "pairing_enabled": True,
        "pairing_code": pairing_code,
        "updated_at": datetime.now(timezone.utc).isoformat(),
        "updated_by": user,
    }
    save_telegram_runtime_config(payload)
    apply_telegram_runtime_config(payload)
    await restart_telegram_bot()
    write_audit(
        settings.audit_path,
        "telegram_pairing_code_regenerate",
        {"user": user, "pairing_code_set": True},
    )
    return RedirectResponse(f"{settings.base_path}/telegram?msg={quote('已重新生成 Telegram 配对码')}", status_code=303)


@app.post("/telegram/push-test")
async def telegram_push_test(user: AuthUser) -> Response:
    if telegram_bot is None or not telegram_bot.enabled:
        return RedirectResponse(
            f"{settings.base_path}/telegram?msg={quote('Telegram 未启用或未配置 Bot Token')}",
            status_code=303,
        )
    chat_ids = await telegram_bot.allowed_chat_ids()
    if not chat_ids:
        return RedirectResponse(
            f"{settings.base_path}/telegram?msg={quote('没有可推送 chat，请先在 Telegram 私聊发送 /pair 配对码完成绑定')}",
            status_code=303,
        )
    await telegram_bot.notify(f"Sub2API Ops 面板推送测试\n用户：{user}\n时间：{beijing_time(datetime.now(timezone.utc))}")
    return RedirectResponse(
        f"{settings.base_path}/telegram?msg={quote(f'已发送测试推送到 {len(chat_ids)} 个 chat')}",
        status_code=303,
    )


@app.post("/telegram/guard-run")
async def telegram_guard_run(user: AuthUser) -> Response:
    actions = await run_auto_guard_threaded(f"{user}:telegram_panel")
    if actions:
        await notify_telegram_account_alerts(f"账号异常：用户 {user} 从 Telegram 面板执行 Guard", actions)
    return RedirectResponse(
        f"{settings.base_path}/telegram?msg={quote(f'Guard 已执行，动作 {len(actions)} 个')}",
        status_code=303,
    )


@app.post("/settings/openai-advanced-scheduler/enable")
def enable_openai_advanced_scheduler(user: AuthUser) -> Response:
    row = db.fetch_one(
        """
        INSERT INTO settings (key, value, updated_at)
        VALUES ('openai_advanced_scheduler_enabled', 'true', now())
        ON CONFLICT (key)
        DO UPDATE SET value = EXCLUDED.value, updated_at = now()
        RETURNING key, value, updated_at
        """,
    )
    write_audit(settings.audit_path, "enable_openai_advanced_scheduler", {"user": user, "setting": row})
    return RedirectResponse(f"{settings.base_path}/?msg=openai+advanced+scheduler+enabled", status_code=303)


@app.post("/accounts/{account_id}/pause")
def pause_account(
    request: Request,
    account_id: int,
    user: AuthUser,
    reason: str = Form("manual pause from ops companion"),
    return_to: str = Form(""),
) -> Response:
    account_ops.pause_account(db, settings.audit_path, account_id, user, reason)
    if return_to == "guard":
        return RedirectResponse(f"{settings.base_path}/guard?msg=paused+account+{account_id}", status_code=303)
    return RedirectResponse(f"{settings.base_path}/?msg=paused+account+{account_id}", status_code=303)


@app.post("/accounts/{account_id}/cooldown")
def cooldown_account(
    request: Request,
    account_id: int,
    user: AuthUser,
    minutes: int = Form(15),
    reason: str = Form("temporary cooldown from ops companion"),
) -> Response:
    minutes = int_param(str(minutes), 15, 1, 1440)
    account_ops.cooldown_account(db, settings.audit_path, account_id, user, minutes, reason)
    return RedirectResponse(f"{settings.base_path}/?msg=cooldown+account+{account_id}", status_code=303)


@app.post("/accounts/{account_id}/resume")
def resume_account(request: Request, account_id: int, user: AuthUser, return_to: str = Form("")) -> Response:
    account_ops.resume_account(db, settings.audit_path, account_id, user)
    if return_to == "guard":
        return RedirectResponse(f"{settings.base_path}/guard?msg=resumed+account+{account_id}", status_code=303)
    return RedirectResponse(f"{settings.base_path}/?msg=resumed+account+{account_id}", status_code=303)


@app.post("/guard/apply")
def apply_guard(
    request: Request,
    user: AuthUser,
    account_id: int = Form(...),
    action: str = Form(...),
    minutes: int = Form(15),
    reason: str = Form(...),
) -> Response:
    if action == "pause":
        return pause_account(request, account_id, user, reason)
    if action == "cooldown":
        return cooldown_account(request, account_id, user, minutes, reason)
    raise HTTPException(status_code=400, detail="Unknown guard action")


@app.get("/system/version")
def system_version(_: AuthUser) -> dict[str, Any]:
    info = version_info(settings)
    return {"version": info["current_version"], **info}


@app.get("/system/check-updates")
def system_check_updates(_: AuthUser, force: bool = False) -> dict[str, Any]:
    return version_info(settings, force=force)


@app.post("/system/update")
def system_update(user: AuthUser) -> dict[str, Any]:
    try:
        result = perform_update(settings)
    except UpdateError as exc:
        raise HTTPException(status_code=409, detail=str(exc)) from exc
    write_audit(settings.audit_path, "system_update", {"user": user, **result})
    if result.get("need_restart"):
        restart_process_soon()
    return result


@app.post("/system/restart")
def system_restart(user: AuthUser) -> dict[str, Any]:
    write_audit(settings.audit_path, "system_restart", {"user": user})
    restart_process_soon()
    return {"message": "服务正在重启", "need_restart": True}


@app.get("/healthz")
def healthz() -> dict[str, str]:
    db.fetch_one("SELECT 1 AS ok")
    return {"status": "ok"}
