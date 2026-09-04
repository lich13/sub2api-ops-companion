from __future__ import annotations

import asyncio
import hashlib
import hmac
import json
import os
import secrets
import tempfile
import threading
import time
from contextlib import asynccontextmanager, suppress
from datetime import datetime, timezone
from pathlib import Path
from typing import Annotated, Any
from urllib.parse import quote, urlsplit
from zoneinfo import ZoneInfo

from fastapi import Depends, FastAPI, Form, HTTPException, Request, Response
from fastapi.responses import HTMLResponse, RedirectResponse
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates

from .audit import write_audit
from .bark import BarkNotifier, DEFAULT_BARK_SERVER_URL, normalize_bark_server_url
from .db import Database
from .oauth_monitor import OAuthMonitor, OAuthStateStore, migrate_legacy_recovery_state
from .secure_session import create_session_cookie, read_session_cookie
from .settings import load_settings
from .sso_config import (
    SSORuntimeConfig,
    build_sso_panel_config,
    load_sso_runtime_config,
    save_sso_config,
)
from .sub2api_sso import Sub2APISSOError, normalize_base_url, validate_sub2api_token
from .telegram_bot import TelegramOpsBot
from .versioning import (
    APP_VERSION,
    UpdateError,
    perform_update,
    restart_process_soon,
    version_info,
)

settings = load_settings()
db = Database(settings.database_url)
templates = Jinja2Templates(directory="app/templates")
SESSION_COOKIE = "sub2ops_session"
BEIJING_TZ = ZoneInfo("Asia/Shanghai")
TELEGRAM_PAIRING_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"
telegram_bot: TelegramOpsBot | None = None
telegram_task: asyncio.Task[None] | None = None
oauth_monitor: OAuthMonitor | None = None
oauth_monitor_task: asyncio.Task[None] | None = None
bark_notifier = BarkNotifier(settings)
BARK_CONFIG_LOCK = threading.RLock()


def beijing_time(value: Any) -> str:
    if value in (None, ""):
        return "-"
    if isinstance(value, datetime):
        parsed = value
    else:
        try:
            parsed = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
        except ValueError:
            return str(value)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(BEIJING_TZ).strftime("%Y-%m-%d %H:%M")


templates.env.filters["bj_time"] = beijing_time


def oauth_state_store() -> OAuthStateStore:
    return OAuthStateStore(settings.usage_query_state_path)


def legacy_recovery_state_path() -> str:
    return str(Path(settings.usage_query_state_path).with_name("guard-state.json"))


def oauth_base_url() -> str:
    config = current_sso_config()
    return str(
        config.verify_base_url
        or config.base_url
        or settings.sub2api_verify_base_url
        or settings.sub2api_base_url
        or ""
    ).strip().rstrip("/")


async def deliver_oauth_monitor_events(events: list[dict[str, Any]]) -> None:
    if not events or oauth_monitor is None:
        return
    runtime = bark_notifier.runtime_config()
    if not runtime.config_valid:
        return
    if not runtime.enabled:
        await asyncio.to_thread(
            oauth_monitor.store.mark_events_delivered,
            events,
            suppressed=True,
        )
        return
    delivered = await asyncio.to_thread(
        bark_notifier.notify_oauth_monitor_events,
        events,
        config=runtime,
    )
    if delivered:
        await asyncio.to_thread(oauth_monitor.store.mark_events_delivered, delivered)


async def oauth_monitor_loop() -> None:
    while True:
        try:
            if oauth_monitor is not None:
                events = await asyncio.to_thread(oauth_monitor.run_once)
                await deliver_oauth_monitor_events(events)
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            write_audit(settings.audit_path, "oauth_monitor_loop_error", {"error": str(exc)})
        await asyncio.sleep(2)


@asynccontextmanager
async def lifespan(_: FastAPI):
    global telegram_bot, telegram_task, oauth_monitor, oauth_monitor_task
    db.open()
    store = oauth_state_store()
    await asyncio.to_thread(
        migrate_legacy_recovery_state,
        db,
        store,
        legacy_recovery_state_path(),
        settings.audit_path,
    )
    oauth_monitor = OAuthMonitor(
        settings,
        db,
        base_url_provider=oauth_base_url,
    )
    await restart_telegram_bot()
    oauth_monitor_task = asyncio.create_task(oauth_monitor_loop())
    try:
        yield
    finally:
        if oauth_monitor_task:
            oauth_monitor_task.cancel()
            with suppress(asyncio.CancelledError):
                await oauth_monitor_task
            oauth_monitor_task = None
        if telegram_task:
            telegram_task.cancel()
            with suppress(asyncio.CancelledError):
                await telegram_task
            telegram_task = None
        telegram_bot = None
        oauth_monitor = None
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
    return username if secrets.compare_digest(signature, expected) else None


def safe_next(value: str | None) -> str:
    if value in {"/", "/telegram", "/sso"}:
        return "/telegram" if value == "/" else str(value)
    return "/telegram"


def origin_from_url(value: str) -> str:
    parsed = urlsplit(value.strip())
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        return ""
    return f"{parsed.scheme}://{parsed.netloc}"


def frame_ancestors_value(sso_config: SSORuntimeConfig | None = None) -> str:
    config = sso_config or current_sso_config()
    ancestors = ["'self'"]
    origin = origin_from_url(config.base_url)
    if origin and origin not in ancestors:
        ancestors.append(origin)
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
    return apply_sso_security_headers(
        RedirectResponse(location, status_code=status_code), no_store=True
    )  # type: ignore[return-value]


def sso_required_response() -> Response:
    return apply_sso_security_headers(
        Response("Sub2API SSO required", status_code=403, media_type="text/plain"),
        no_store=True,
    )


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
    return apply_sso_security_headers(await call_next(request))


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
    context.setdefault("current_user", verify_session(request.cookies.get(SESSION_COOKIE)))
    context.setdefault("version", {"current_version": APP_VERSION})
    return templates.TemplateResponse(request, template, context)


def int_param(value: object, default: int, minimum: int, maximum: int) -> int:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        parsed = default
    return max(minimum, min(maximum, parsed))


def form_truthy(value: Any) -> bool:
    return str(value or "").strip().lower() in {"1", "true", "yes", "on", "checked"}


def mask_secret(value: str) -> str:
    raw = value.strip()
    if not raw:
        return ""
    return "已设置" if len(raw) <= 10 else f"{raw[:6]}...{raw[-4:]}"


def parse_int_csv(value: Any) -> tuple[int, ...]:
    values: list[int] = []
    for item in str(value or "").replace(";", ",").split(","):
        try:
            values.append(int(item.strip()))
        except (TypeError, ValueError):
            continue
    return tuple(dict.fromkeys(values))


def generate_telegram_pairing_code() -> str:
    raw = "".join(secrets.choice(TELEGRAM_PAIRING_ALPHABET) for _ in range(8))
    return f"{raw[:4]}-{raw[4:]}"


def telegram_state() -> dict[str, Any]:
    try:
        raw = json.loads(Path(settings.telegram_state_path).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        raw = {}
    return raw if isinstance(raw, dict) else {}


def telegram_config_file() -> dict[str, Any]:
    try:
        raw = json.loads(Path(settings.telegram_config_path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        raw = {}
    return raw if isinstance(raw, dict) else {}


def bark_config_file() -> dict[str, Any]:
    try:
        raw = json.loads(Path(settings.bark_config_path).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        raw = {}
    return raw if isinstance(raw, dict) else {}


def save_telegram_runtime_config(payload: dict[str, Any]) -> None:
    path = Path(settings.telegram_config_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    temporary.chmod(0o600)
    temporary.replace(path)
    path.chmod(0o600)


def save_bark_runtime_config(payload: dict[str, Any]) -> None:
    path = Path(settings.bark_config_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    file_descriptor, temporary_name = tempfile.mkstemp(
        dir=path.parent, prefix=f".{path.name}.", suffix=".tmp"
    )
    temporary = Path(temporary_name)
    try:
        os.fchmod(file_descriptor, 0o600)
        with os.fdopen(file_descriptor, "w", encoding="utf-8") as handle:
            file_descriptor = -1
            json.dump(payload, handle, ensure_ascii=False, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        temporary.replace(path)
        path.chmod(0o600)
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if file_descriptor >= 0:
            os.close(file_descriptor)
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def apply_telegram_runtime_config(payload: dict[str, Any]) -> None:
    bool_fields = (
        "enabled",
        "pairing_enabled",
        "oauth_usage_refresh_enabled",
        "oauth_recovery_monitor_enabled",
        "oauth_night_recovery_cooldown_enabled",
    )
    for key in bool_fields:
        if key in payload:
            setattr(settings, f"telegram_{key}", bool(payload.get(key)))
    if "bot_token" in payload:
        settings.telegram_bot_token = str(payload.get("bot_token") or "")
    if "pairing_code" in payload:
        settings.telegram_pairing_code = str(payload.get("pairing_code") or "")
    if "allowed_chat_ids" in payload:
        settings.telegram_allowed_chat_ids = parse_int_csv(
            ",".join(str(value) for value in payload.get("allowed_chat_ids") or [])
        )
    if "allowed_user_ids" in payload:
        settings.telegram_allowed_user_ids = parse_int_csv(
            ",".join(str(value) for value in payload.get("allowed_user_ids") or [])
        )
    integer_fields = {
        "oauth_usage_refresh_concurrency": (4, 1, 16),
        "oauth_recovery_test_concurrency": (2, 1, 8),
        "oauth_early_probe_batch_size": (8, 1, 50),
        "oauth_regular_refresh_interval_seconds": (3600, 60, 86400),
        "oauth_7d_probe_interval_seconds": (3600, 60, 86400),
    }
    for key, (default, minimum, maximum) in integer_fields.items():
        if key in payload:
            setattr(
                settings,
                f"telegram_{key}",
                int_param(payload.get(key), default, minimum, maximum),
            )
    if "oauth_recovery_test_model_id" in payload:
        settings.telegram_oauth_recovery_test_model_id = (
            str(payload.get("oauth_recovery_test_model_id") or "gpt-5.6-luna").strip()
            or "gpt-5.6-luna"
        )


def apply_bark_runtime_config(payload: dict[str, Any]) -> None:
    with BARK_CONFIG_LOCK:
        settings.bark_config_valid = True
        if "enabled" in payload:
            settings.bark_enabled = bool(payload.get("enabled"))
        if "device_key" in payload:
            settings.bark_device_key = str(payload.get("device_key") or "")
        if "server_url" in payload:
            settings.bark_server_url = str(payload.get("server_url") or DEFAULT_BARK_SERVER_URL)
        bark_notifier.configure_from_settings(settings)


def ensure_telegram_pairing_code() -> str:
    existing = telegram_config_file()
    code = str(settings.telegram_pairing_code or existing.get("pairing_code") or "").strip()
    if code or not settings.telegram_bot_token.strip():
        return code
    code = generate_telegram_pairing_code()
    payload = {
        **existing,
        "pairing_enabled": True,
        "pairing_code": code,
        "updated_at": datetime.now(timezone.utc).isoformat(),
        "updated_by": existing.get("updated_by") or "system",
    }
    save_telegram_runtime_config(payload)
    apply_telegram_runtime_config(payload)
    return code


def build_telegram_config() -> dict[str, Any]:
    state = telegram_state()
    existing = telegram_config_file()
    paired_chats = sorted(
        {int(item) for item in state.get("paired_chat_ids") or [] if str(item).lstrip("-").isdigit()}
    )
    paired_users = sorted(
        {int(item) for item in state.get("paired_user_ids") or [] if str(item).lstrip("-").isdigit()}
    )
    return {
        "configured": settings.telegram_enabled and bool(settings.telegram_bot_token.strip()),
        "bot_token_set": bool(settings.telegram_bot_token.strip()),
        "bot_token_preview": mask_secret(settings.telegram_bot_token),
        "pairing_code": ensure_telegram_pairing_code(),
        "binding_status": "未启用"
        if not settings.telegram_bot_token.strip()
        else ("已绑定" if paired_chats or paired_users else "待配对"),
        "paired_chat_ids": paired_chats,
        "paired_user_ids": paired_users,
        "push_target_count": len(paired_chats),
        "control_user_count": len(paired_users),
        "config_updated_at": existing.get("updated_at"),
        "oauth_usage_refresh_enabled": settings.telegram_oauth_usage_refresh_enabled,
        "oauth_recovery_monitor_enabled": settings.telegram_oauth_recovery_monitor_enabled,
        "oauth_night_recovery_cooldown_enabled": (
            settings.telegram_oauth_night_recovery_cooldown_enabled
        ),
        "oauth_usage_refresh_concurrency": settings.telegram_oauth_usage_refresh_concurrency,
        "oauth_recovery_test_concurrency": settings.telegram_oauth_recovery_test_concurrency,
        "oauth_early_probe_batch_size": settings.telegram_oauth_early_probe_batch_size,
        "oauth_regular_refresh_interval_seconds": settings.telegram_oauth_regular_refresh_interval_seconds,
        "oauth_7d_probe_interval_seconds": settings.telegram_oauth_7d_probe_interval_seconds,
        "oauth_recovery_test_model_id": settings.telegram_oauth_recovery_test_model_id
        or "gpt-5.6-luna",
    }


def build_bark_config() -> dict[str, Any]:
    existing = bark_config_file()
    runtime = bark_notifier.runtime_config()
    try:
        server_url = normalize_bark_server_url(runtime.server_url)
        server_url_valid = True
    except ValueError:
        server_url = str(runtime.server_url or DEFAULT_BARK_SERVER_URL)
        server_url_valid = False
    key_set = bool(runtime.device_key.strip())
    return {
        "configured": bool(
            runtime.config_valid
            and runtime.enabled
            and key_set
            and server_url_valid
        ),
        "config_valid": runtime.config_valid,
        "enabled": runtime.enabled,
        "device_key_set": key_set,
        "device_key_status": "已设置" if key_set else "未设置",
        "server_url": server_url,
        "server_url_valid": server_url_valid,
        "config_updated_at": existing.get("updated_at"),
    }


async def restart_telegram_bot() -> None:
    global telegram_bot, telegram_task
    if telegram_task:
        telegram_task.cancel()
        with suppress(asyncio.CancelledError):
            await telegram_task
        telegram_task = None
    telegram_bot = TelegramOpsBot(settings, db, oauth_monitor=oauth_monitor)
    if telegram_bot.enabled:
        telegram_task = asyncio.create_task(telegram_bot.run())


@app.get("/sso/start")
def sub2api_sso_start(
    request: Request,
    token: str = "",
    user_id: str = "",
    next: str = "/telegram",
) -> Response:
    next_path = safe_next(next)
    client_host = request.client.host if request.client else ""
    sso_config = current_sso_config()
    if not sso_config.enabled:
        write_audit(settings.audit_path, "sso_login_reject", {"reason": "disabled", "client": client_host})
        return sso_required_response()
    try:
        principal = validate_sub2api_token(
            sso_config.verify_base_url or sso_config.base_url,
            token=token,
            expected_user_id=user_id or None,
            required_role=sso_config.required_role,
            timeout_seconds=sso_config.verify_timeout_seconds,
        )
    except Sub2APISSOError as exc:
        write_audit(
            settings.audit_path,
            "sso_login_reject",
            {"reason": exc.reason, "message": exc.message, "user_id": user_id, "client": client_host},
        )
        return sso_required_response()
    session_user = f"sub2api:{principal.id}:{principal.username}"
    response = no_store_redirect(f"{settings.base_path}{next_path}")
    response.set_cookie(
        SESSION_COOKIE,
        sign_session(
            session_user,
            int(time.time()),
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
        {"user": session_user, "sub2api_user_id": principal.id, "sub2api_role": principal.role},
    )
    return response


@app.post("/logout")
def logout(user: AuthUser) -> Response:
    response = no_store_redirect(f"{settings.base_path}/sso/start")
    response.delete_cookie(SESSION_COOKIE, path=cookie_path())
    write_audit(settings.audit_path, "logout", {"user": user})
    return response


@app.get("/")
def index(request: Request, _: AuthUser, msg: str = "") -> Response:
    destination = f"{settings.base_path}/telegram"
    if msg:
        destination += f"?msg={quote(msg)}"
    return RedirectResponse(destination, status_code=303)


@app.get("/telegram", response_class=HTMLResponse)
def telegram_view(request: Request, _: AuthUser, msg: str = "") -> HTMLResponse:
    return render(
        request,
        "telegram.html",
        {
            "active": "telegram",
            "telegram": build_telegram_config(),
            "bark": build_bark_config(),
            "msg": msg,
        },
    )


@app.get("/sso", response_class=HTMLResponse)
def sso_view(request: Request, _: AuthUser, msg: str = "") -> HTMLResponse:
    panel = build_sso_panel_config(current_sso_config(), base_path=settings.base_path)
    panel["sub2api_admin_token_saved"] = bool(oauth_state_store().admin_token())
    return render(
        request,
        "sso.html",
        {"active": "sso", "sso": panel, "sso_config_path": settings.sso_config_path, "msg": msg},
    )


@app.post("/telegram/config")
async def telegram_config_save(user: AuthUser, telegram_bot_token: str = Form("")) -> Response:
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
        {"user": user, "enabled": bool(token), "bot_token_set": bool(token)},
    )
    return RedirectResponse(
        f"{settings.base_path}/telegram?msg={quote('Telegram 配置已保存')}", status_code=303
    )


@app.post("/telegram/oauth-settings")
async def telegram_oauth_settings_save(request: Request, user: AuthUser) -> Response:
    form = await request.form()
    existing = telegram_config_file()
    existing.pop("oauth_early_probe_interval_seconds", None)
    existing.pop("oauth_recovery_push_enabled", None)
    payload = {
        **existing,
        "oauth_usage_refresh_enabled": bool(form.getlist("oauth_usage_refresh_enabled")),
        "oauth_recovery_monitor_enabled": bool(form.getlist("oauth_recovery_monitor_enabled")),
        "oauth_night_recovery_cooldown_enabled": bool(
            form.getlist("oauth_night_recovery_cooldown_enabled")
        ),
        "oauth_usage_refresh_concurrency": int_param(form.get("oauth_usage_refresh_concurrency"), 4, 1, 16),
        "oauth_recovery_test_concurrency": int_param(form.get("oauth_recovery_test_concurrency"), 2, 1, 8),
        "oauth_early_probe_batch_size": int_param(form.get("oauth_early_probe_batch_size"), 8, 1, 50),
        "oauth_regular_refresh_interval_seconds": int_param(
            form.get("oauth_regular_refresh_interval_seconds"), 3600, 60, 86400
        ),
        "oauth_7d_probe_interval_seconds": int_param(
            form.get("oauth_7d_probe_interval_seconds"), 3600, 60, 86400
        ),
        "oauth_recovery_test_model_id": str(
            form.get("oauth_recovery_test_model_id") or "gpt-5.6-luna"
        ).strip()
        or "gpt-5.6-luna",
        "updated_at": datetime.now(timezone.utc).isoformat(),
        "updated_by": user,
    }
    save_telegram_runtime_config(payload)
    apply_telegram_runtime_config(payload)
    write_audit(
        settings.audit_path,
        "telegram_oauth_settings_update",
        {key: value for key, value in payload.items() if key.startswith("oauth_")},
    )
    return RedirectResponse(
        f"{settings.base_path}/telegram?msg={quote('OAuth 监控设置已保存')}", status_code=303
    )


@app.post("/bark/config")
async def bark_config_save(
    user: AuthUser,
    enabled: str | None = Form(None),
    bark_device_key: str = Form(""),
    bark_server_url: str = Form(DEFAULT_BARK_SERVER_URL),
) -> Response:
    with BARK_CONFIG_LOCK:
        existing = bark_config_file()
        device_key = bark_device_key.strip() or bark_notifier.runtime_config().device_key.strip()
        try:
            server_url = normalize_bark_server_url(
                bark_server_url.strip() or DEFAULT_BARK_SERVER_URL
            )
        except ValueError:
            return RedirectResponse(
                f"{settings.base_path}/telegram?msg={quote('Bark 服务 URL 无效；HTTP 仅允许 loopback')}",
                status_code=303,
            )
        is_enabled = form_truthy(enabled)
        if is_enabled and not device_key:
            return RedirectResponse(
                f"{settings.base_path}/telegram?msg={quote('启用 Bark 前需要填写 Device Key')}",
                status_code=303,
            )
        payload = {
            **existing,
            "enabled": is_enabled,
            "device_key": device_key,
            "server_url": server_url,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "updated_by": user,
        }
        save_bark_runtime_config(payload)
        apply_bark_runtime_config(payload)
    write_audit(
        settings.audit_path,
        "bark_config_update",
        {
            "user": user,
            "enabled": is_enabled,
            "device_key_set": bool(device_key),
            "server_url": server_url,
        },
    )
    return RedirectResponse(
        f"{settings.base_path}/telegram?msg={quote('Bark 配置已保存')}", status_code=303
    )


@app.post("/bark/push-test")
async def bark_push_test(user: AuthUser) -> Response:
    result = await asyncio.to_thread(bark_notifier.push_test)
    result_code = result.error_code or "ok"
    message = "Bark 测试推送已发送" if result.success else f"Bark 测试推送失败：{result_code}"
    write_audit(
        settings.audit_path,
        "bark_push_test",
        {"user": user, "success": result_code == "ok", "result": result_code},
    )
    return RedirectResponse(
        f"{settings.base_path}/telegram?msg={quote(message)}", status_code=303
    )


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
            f"{settings.base_path}/sso?msg={quote('启用前需要填写 Sub2API 地址')}", status_code=303
        )
    try:
        if clean_base_url:
            clean_base_url = normalize_base_url(clean_base_url)
        if clean_verify_base_url:
            clean_verify_base_url = normalize_base_url(clean_verify_base_url)
    except Sub2APISSOError:
        return RedirectResponse(
            f"{settings.base_path}/sso?msg={quote('Sub2API 地址必须是完整的 http(s) 地址')}",
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
    oauth_state_store().save_admin_token(sub2api_admin_token)
    write_audit(
        settings.audit_path,
        "ops_sso_config_update",
        {
            "user": user,
            "enabled": form_truthy(enabled),
            "base_url_set": bool(clean_base_url),
            "verify_base_url_set": bool(clean_verify_base_url),
            "sub2api_admin_token_set": bool(str(sub2api_admin_token).strip()),
        },
    )
    return RedirectResponse(
        f"{settings.base_path}/sso?msg={quote('Sub2API 接入配置已保存')}", status_code=303
    )


@app.post("/telegram/pairing-code/regenerate")
async def telegram_pairing_code_regenerate(user: AuthUser) -> Response:
    payload = {
        **telegram_config_file(),
        "pairing_enabled": True,
        "pairing_code": generate_telegram_pairing_code(),
        "updated_at": datetime.now(timezone.utc).isoformat(),
        "updated_by": user,
    }
    save_telegram_runtime_config(payload)
    apply_telegram_runtime_config(payload)
    await restart_telegram_bot()
    write_audit(settings.audit_path, "telegram_pairing_code_regenerate", {"user": user})
    return RedirectResponse(
        f"{settings.base_path}/telegram?msg={quote('已重新生成 Telegram 配对码')}", status_code=303
    )


@app.post("/telegram/push-test")
async def telegram_push_test(user: AuthUser) -> Response:
    if telegram_bot is None or not telegram_bot.enabled:
        message = "Telegram 未启用或未配置 Bot Token"
    else:
        chat_ids = await telegram_bot.allowed_chat_ids()
        if not chat_ids:
            message = "没有可推送 chat，请先完成配对"
        else:
            await telegram_bot.notify(
                f"Sub2API Ops 面板推送测试\n用户：{user}\n时间：{beijing_time(datetime.now(timezone.utc))}"
            )
            message = f"已发送测试推送到 {len(chat_ids)} 个 chat"
    return RedirectResponse(
        f"{settings.base_path}/telegram?msg={quote(message)}", status_code=303
    )


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
