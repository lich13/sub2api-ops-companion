from __future__ import annotations

import secrets
import hashlib
import hmac
import time
import asyncio
import json
from contextlib import asynccontextmanager, suppress
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Annotated, Any
from urllib.parse import quote
from zoneinfo import ZoneInfo

from fastapi import Depends, FastAPI, Form, HTTPException, Request, Response
from fastapi.responses import HTMLResponse, RedirectResponse
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates

from . import account_ops
from .audit import read_audit, write_audit
from .db import Database
from .quality_sort import STABILITY_SORT_OPTIONS, normalize_stability_sort, sort_stability_rows
from .settings import load_settings
from .sql import (
    ACCOUNT_OPTIONS_SQL,
    GROUPS_SQL,
    GUARD_BALANCE_CANDIDATES_SQL,
    PLATFORM_OPTIONS_SQL,
    QUALITY_SQL,
    REQUESTS_SQL,
)
from .telegram_bot import TelegramOpsBot, format_guard_actions
from .time_range import build_time_range, clean_query_string, rolling_hours_range
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
    "last_actions": [],
}
telegram_bot: TelegramOpsBot | None = None
telegram_task: asyncio.Task[None] | None = None


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


templates.env.filters["bj_time"] = beijing_time
templates.env.filters["bj_after"] = beijing_time_after_seconds
templates.env.filters["int_commas"] = integer_with_commas


@asynccontextmanager
async def lifespan(_: FastAPI):
    global telegram_bot, telegram_task
    db.open()
    guard_task: asyncio.Task[None] | None = None
    await restart_telegram_bot()
    if settings.guard_enabled:
        guard_task = asyncio.create_task(auto_guard_loop())
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
        telegram_bot = None
        db.close()


app = FastAPI(title=settings.app_name, lifespan=lifespan)
app.mount("/static", StaticFiles(directory="app/static"), name="static")


def cookie_path() -> str:
    return settings.base_path or "/"


def sign_session(username: str, issued_at: int) -> str:
    payload = f"{username}:{issued_at}"
    signature = hmac.new(
        settings.session_secret.encode("utf-8"),
        payload.encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()
    return f"{payload}:{signature}"


def verify_session(value: str | None) -> str | None:
    if not value:
        return None
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
    expected = sign_session(username, issued_at).rsplit(":", 1)[1]
    if not secrets.compare_digest(signature, expected):
        return None
    return username


def login_redirect(request: Request) -> HTTPException:
    next_path = request.url.path
    if request.url.query:
        next_path += f"?{request.url.query}"
    location = f"{settings.base_path}/login?next={quote(next_path, safe='/')}"
    return HTTPException(status_code=303, headers={"Location": location}, detail="Login required")


def safe_next(value: str | None) -> str:
    if not value or not value.startswith("/") or value.startswith("//"):
        return "/"
    return value


def require_auth(request: Request) -> str:
    username = verify_session(request.cookies.get(SESSION_COOKIE))
    if username is None:
        raise login_redirect(request)
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


def load_quality(
    group_name: str,
    platform: str,
    range_start: datetime | None,
    range_end: datetime | None,
) -> list[dict[str, Any]]:
    return db.fetch_all(
        QUALITY_SQL,
        {"group_name": group_name, "platform": platform, "range_start": range_start, "range_end": range_end},
    )


def guard_config() -> dict[str, Any]:
    return {
        "enabled": settings.guard_enabled,
        "interval_seconds": settings.guard_interval_seconds,
        "lookback_minutes": settings.guard_lookback_minutes,
        "threshold": settings.guard_balance_error_threshold,
        "action": "pause",
        "state": guard_state,
        "recent_events": read_audit(settings.audit_path, limit=12, event_prefix="guard_"),
    }


def parse_int_csv(value: str) -> tuple[int, ...]:
    values: list[int] = []
    for part in str(value or "").replace(";", ",").split(","):
        item = part.strip()
        if not item:
            continue
        try:
            parsed = int(item)
        except ValueError:
            continue
        if parsed and parsed not in values:
            values.append(parsed)
    return tuple(values)


def mask_secret(value: str) -> str:
    raw = value.strip()
    if not raw:
        return ""
    if len(raw) <= 10:
        return "已设置"
    return f"{raw[:6]}...{raw[-4:]}"


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


def build_telegram_config() -> dict[str, Any]:
    state = telegram_state()
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
        "config_updated_at": config_file.get("updated_at"),
        "state_updated_at": state.get("updated_at"),
        "paired_chat_ids": paired_chat_ids,
        "paired_user_ids": paired_user_ids,
        "push_target_count": push_target_count,
        "control_user_count": control_user_count,
        "binding_status": "未启用"
        if not settings.telegram_bot_token.strip()
        else ("已绑定" if push_target_count or control_user_count else "待首次绑定"),
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
    settings.telegram_pairing_enabled = bool(payload.get("pairing_enabled", True))
    settings.telegram_pairing_code = str(payload.get("pairing_code", "") or "")
    if "allowed_chat_ids" in payload:
        settings.telegram_allowed_chat_ids = parse_int_csv(",".join(str(v) for v in payload.get("allowed_chat_ids", [])))
    else:
        settings.telegram_allowed_chat_ids = ()
    if "allowed_user_ids" in payload:
        settings.telegram_allowed_user_ids = parse_int_csv(",".join(str(v) for v in payload.get("allowed_user_ids", [])))
    else:
        settings.telegram_allowed_user_ids = ()
    if "default_platform" in payload:
        settings.telegram_default_platform = str(payload.get("default_platform") or "openai").strip() or "openai"
    if "default_group" in payload:
        settings.telegram_default_group = str(payload.get("default_group") or "openai-default").strip() or "openai-default"
    if "quality_hours" in payload:
        settings.telegram_quality_hours = int_param(str(payload.get("quality_hours")), 24, 1, 168)
    if "poll_timeout_seconds" in payload:
        settings.telegram_poll_timeout_seconds = int_param(str(payload.get("poll_timeout_seconds")), 25, 5, 50)


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
        f"{row['balance_error_count']} balance/quota errors in last {settings.guard_lookback_minutes} minutes; "
        f"last={row.get('last_message') or 'n/a'}"
    )
    updated = db.fetch_one(
        """
        UPDATE accounts
        SET schedulable = false,
            temp_unschedulable_until = NULL,
            temp_unschedulable_reason = %(reason)s,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
          AND (
              schedulable = true
              OR temp_unschedulable_until IS NOT NULL
          )
        RETURNING id, name, schedulable, temp_unschedulable_until, temp_unschedulable_reason
        """,
        {"account_id": row["id"], "reason": reason},
    )

    result = {
        "account_id": row["id"],
        "name": row["name"],
        "action": "pause",
        "balance_error_count": row["balance_error_count"],
        "last_error_at": row.get("last_error_at"),
        "updated": updated,
        "actor": actor,
        "reason": reason,
    }
    if updated:
        write_audit(settings.audit_path, "guard_auto_pause_account", result)
    return result


def run_auto_guard_once(actor: str = "auto_guard") -> list[dict[str, Any]]:
    guard_state["running"] = True
    guard_state["last_error"] = ""
    try:
        candidates = db.fetch_all(
            GUARD_BALANCE_CANDIDATES_SQL,
            {
                "lookback_minutes": settings.guard_lookback_minutes,
                "threshold": settings.guard_balance_error_threshold,
            },
        )
        actions = [pause_guard_candidate(row, actor) for row in candidates]
        applied = [item for item in actions if item.get("updated")]
        guard_state.update(
            {
                "last_run_at": datetime.now(timezone.utc).isoformat(),
                "last_actions": applied[:10],
            }
        )
        if candidates and not applied:
            write_audit(
                settings.audit_path,
                "guard_auto_noop",
                {"actor": actor, "candidate_count": len(candidates), "reason": "no rows updated"},
            )
        return applied
    except Exception as exc:
        guard_state["last_error"] = str(exc)
        write_audit(settings.audit_path, "guard_auto_error", {"actor": actor, "error": str(exc)})
        raise
    finally:
        guard_state["running"] = False


async def run_auto_guard_threaded(actor: str = "auto_guard") -> list[dict[str, Any]]:
    async with guard_lock:
        return await asyncio.to_thread(run_auto_guard_once, actor)


async def notify_telegram(text: str) -> None:
    if telegram_bot is None:
        return
    try:
        await telegram_bot.notify(text)
    except Exception:
        return


async def auto_guard_loop() -> None:
    while True:
        try:
            actions = await run_auto_guard_threaded()
            if actions:
                await notify_telegram(format_guard_actions("自动 Guard 已暂停账号", actions))
        except Exception as exc:
            await notify_telegram(f"自动 Guard 执行失败\n{exc}")
        await asyncio.sleep(settings.guard_interval_seconds)


def guard_suggestion(row: dict[str, Any]) -> dict[str, Any] | None:
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
        "avg_ms_per_output_token": weighted_average(rows, "avg_ms_per_output_token"),
        "usage_total_cost": sum(float(row.get("usage_total_cost") or 0) for row in rows),
        "usage_total_tokens": sum(int(row.get("usage_total_tokens") or 0) for row in rows),
    }


@app.get("/login", response_class=HTMLResponse)
def login_page(request: Request, next: str = "/", error: str = "") -> HTMLResponse:
    next_path = safe_next(next)
    if verify_session(request.cookies.get(SESSION_COOKIE)):
        return RedirectResponse(f"{settings.base_path}{next_path}", status_code=303)
    return render(request, "login.html", {"active": "login", "next": next_path, "error": error})


@app.post("/login")
def login_submit(
    request: Request,
    username: str = Form(...),
    password: str = Form(...),
    next: str = Form("/"),
) -> Response:
    next_path = safe_next(next)
    user_ok = secrets.compare_digest(username, settings.basic_user)
    password_ok = secrets.compare_digest(password, settings.basic_password)
    if not (user_ok and password_ok):
        return RedirectResponse(f"{settings.base_path}/login?error=1&next={quote(next_path, safe='/')}", status_code=303)

    response = RedirectResponse(f"{settings.base_path}{next_path}", status_code=303)
    response.set_cookie(
        SESSION_COOKIE,
        sign_session(username, int(time.time())),
        max_age=settings.session_ttl_seconds,
        httponly=True,
        secure=True,
        samesite="lax",
        path=cookie_path(),
    )
    write_audit(settings.audit_path, "login", {"user": username, "client": request.client.host if request.client else ""})
    return response


@app.post("/logout")
def logout(user: AuthUser) -> Response:
    response = RedirectResponse(f"{settings.base_path}/login", status_code=303)
    response.delete_cookie(SESSION_COOKIE, path=cookie_path())
    write_audit(settings.audit_path, "logout", {"user": user})
    return response


@app.get("/", response_class=HTMLResponse)
def index(
    request: Request,
    _: AuthUser,
    group: str = "openai-default",
    platform: str = "openai",
    time_range: str = "",
    start_date: str = "",
    end_date: str = "",
    hours: int | None = None,
    sort: str = "default",
    msg: str = "",
) -> HTMLResponse:
    selected_range = build_time_range(time_range, start_date, end_date, hours)
    selected_sort = normalize_stability_sort(sort)
    rows = sort_stability_rows(
        load_quality(group, platform, selected_range["start_at"], selected_range["end_at"]),
        selected_sort,
    )
    groups = load_groups()
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
            "group": group,
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
    group: str = "openai-default",
    platform: str = "openai",
    time_range: str = "",
    start_date: str = "",
    end_date: str = "",
    hours: int | None = None,
    msg: str = "",
) -> HTMLResponse:
    selected_range = build_time_range(time_range, start_date, end_date, hours)
    rows = load_quality(group, platform, selected_range["start_at"], selected_range["end_at"])
    return render(
        request,
        "speed.html",
        {
            "active": "speed",
            "rows": rows,
            "groups": load_groups(),
            "group": group,
            "platform": platform,
            "time_range": selected_range,
            "dashboard": build_speed_dashboard(rows),
            "msg": msg,
        },
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
    rows = db.fetch_all(
        REQUESTS_SQL,
        {
            "q": q.strip(),
            "platform": platform,
            "account_id": nullable_int(account_id),
            "range_start": selected_range["start_at"],
            "range_end": selected_range["end_at"],
            "limit": parsed_limit,
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
    group: str = "openai-default",
    platform: str = "openai",
    hours: int = 1,
    msg: str = "",
) -> HTMLResponse:
    hours = int_param(str(hours), 1, 1, 168)
    guard_range = rolling_hours_range(hours)
    rows = load_quality(group, platform, guard_range["start_at"], guard_range["end_at"])
    suggestions = [s for row in rows if (s := guard_suggestion(row))]
    return render(
        request,
        "guard.html",
        {
            "active": "guard",
            "group": group,
            "platform": platform,
            "hours": hours,
            "rows": rows,
            "suggestions": suggestions,
            "guard": guard_config(),
            "msg": msg,
        },
    )


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


@app.post("/guard/run")
async def run_guard_now(user: AuthUser) -> Response:
    actions = await run_auto_guard_threaded(user)
    if actions:
        await notify_telegram(format_guard_actions(f"用户 {user} 手动执行 Guard", actions))
    return RedirectResponse(f"{settings.base_path}/guard?msg=guard+applied+{len(actions)}+actions", status_code=303)


@app.post("/telegram/config")
async def telegram_config_save(
    user: AuthUser,
    telegram_bot_token: str = Form(""),
) -> Response:
    token = telegram_bot_token.strip() or settings.telegram_bot_token
    payload = {
        "enabled": bool(token),
        "bot_token": token,
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
            "mode": "token_only_auto_bind",
        },
    )
    return RedirectResponse(f"{settings.base_path}/telegram?msg={quote('Telegram 配置已保存')}", status_code=303)


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
            f"{settings.base_path}/telegram?msg={quote('没有可推送 chat，请先在 Telegram 给 Bot 发送 /start 或 /menu 完成首次绑定')}",
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
        await notify_telegram(format_guard_actions(f"用户 {user} 从 Telegram 面板执行 Guard", actions))
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
) -> Response:
    account_ops.pause_account(db, settings.audit_path, account_id, user, reason)
    return RedirectResponse(f"{settings.base_path}/?msg=paused+account+{account_id}", status_code=303)


@app.post("/accounts/{account_id}/cooldown")
def cooldown_account(
    request: Request,
    account_id: int,
    user: AuthUser,
    minutes: int = Form(30),
    reason: str = Form("temporary cooldown from ops companion"),
) -> Response:
    minutes = int_param(str(minutes), 30, 1, 1440)
    account_ops.cooldown_account(db, settings.audit_path, account_id, user, minutes, reason)
    return RedirectResponse(f"{settings.base_path}/?msg=cooldown+account+{account_id}", status_code=303)


@app.post("/accounts/{account_id}/resume")
def resume_account(request: Request, account_id: int, user: AuthUser) -> Response:
    account_ops.resume_account(db, settings.audit_path, account_id, user)
    return RedirectResponse(f"{settings.base_path}/?msg=resumed+account+{account_id}", status_code=303)


@app.post("/guard/apply")
def apply_guard(
    request: Request,
    user: AuthUser,
    account_id: int = Form(...),
    action: str = Form(...),
    minutes: int = Form(30),
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
