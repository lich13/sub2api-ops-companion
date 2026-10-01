from __future__ import annotations

import asyncio
import json
import os
import sys
import tempfile
import threading
from contextlib import asynccontextmanager, suppress
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from fastapi import FastAPI

from .audit import write_audit
from .bark import BarkNotifier, DEFAULT_BARK_SERVER_URL, normalize_bark_server_url
from .connection_config import connection_config
from .db import Database
from .key_fallback import EVAL_INTERVAL_SECONDS, KeyFallbackController
from .oauth_monitor import OAuthMonitor, OAuthStateStore, migrate_legacy_recovery_state
from .settings import load_settings
from .versioning import APP_VERSION
from .capacity_alerts import CapacityAlerts
from .fingerprint_bank import FINGERPRINT_SYNC_INTERVAL_SECONDS, FingerprintBankService
from .modeltrace import configure_bank_provider

settings = load_settings()
db = Database(settings.database_url)
oauth_monitor: OAuthMonitor | None = None
oauth_monitor_task: asyncio.Task[None] | None = None
key_fallback_controller: KeyFallbackController | None = None
key_fallback_task: asyncio.Task[None] | None = None
bark_notifier = BarkNotifier(settings)
BARK_CONFIG_LOCK = threading.RLock()
capacity_alerts: CapacityAlerts | None = None
fingerprint_bank_service = FingerprintBankService(
    Path(settings.usage_query_state_path).with_name("modeltrace-bank-state.json"),
    settings.audit_path,
)
# Runtime services expose a short, stable attribute for background consumers.
fingerprint_bank = fingerprint_bank_service
configure_bank_provider(fingerprint_bank_service)
fingerprint_bank_task: asyncio.Task[None] | None = None


def oauth_state_store() -> OAuthStateStore:
    return OAuthStateStore(settings.usage_query_state_path)


def legacy_recovery_state_path() -> str:
    return str(Path(settings.usage_query_state_path).with_name("guard-state.json"))


def oauth_base_url() -> str:
    config = connection_config(settings)
    return config["verify_base_url"] or config["base_url"]


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
    for event in events:
        # A manual scheduling change can revoke events after the monitor returns,
        # or while another notification is in flight. Recheck at each dispatch.
        pending = await asyncio.to_thread(oauth_monitor.store.snapshot)
        if pending["pending_events"].get(str(event.get("dedupe_key"))) != event:
            continue
        delivered = await asyncio.to_thread(
            bark_notifier.notify_oauth_monitor_events, [event], config=runtime,
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


async def daily_schedule_loop() -> None:
    # This clock keeps ticking while a monitor cycle holds its lock during network I/O.
    while True:
        try:
            if oauth_monitor is not None:
                await asyncio.to_thread(oauth_monitor.daily_schedule.tick, datetime.now(timezone.utc))
        except asyncio.CancelledError:
            raise
        except Exception:
            write_audit(settings.audit_path, "oauth_daily_schedule_error", {"error": "每日测活排程失败"})
        await asyncio.sleep(1)


async def key_fallback_loop() -> None:
    while True:
        try:
            if key_fallback_controller is not None:
                await asyncio.to_thread(key_fallback_controller.run_once)
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            write_audit(settings.audit_path, "key_fallback_loop_error", {"error": str(exc)})
        await asyncio.sleep(EVAL_INTERVAL_SECONDS)


async def fingerprint_bank_loop() -> None:
    while True:
        try:
            await asyncio.to_thread(fingerprint_bank_service.sync)
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            write_audit(settings.audit_path, "fingerprint_bank_loop_error", {"error": type(exc).__name__})
        await asyncio.sleep(FINGERPRINT_SYNC_INTERVAL_SECONDS)


@asynccontextmanager
async def lifespan(_: FastAPI):
    global oauth_monitor, oauth_monitor_task
    global key_fallback_controller, key_fallback_task
    global capacity_alerts
    global fingerprint_bank_task
    db.open()
    old_oauth_config = oauth_config_file()
    if "oauth_7d_probe_interval_seconds" in old_oauth_config:
        await asyncio.to_thread(save_oauth_runtime_config, old_oauth_config)
    store = oauth_state_store()
    await asyncio.to_thread(store.commit)
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
    key_fallback_controller = KeyFallbackController(
        settings,
        db,
        oauth_monitor=oauth_monitor,
        base_url_provider=oauth_base_url,
        admin_token_provider=lambda: oauth_state_store().admin_token(),
    )
    await asyncio.to_thread(key_fallback_controller.migrate_legacy_config)
    oauth_monitor_task = asyncio.create_task(oauth_monitor_loop())
    daily_schedule_task = asyncio.create_task(daily_schedule_loop())
    key_fallback_task = asyncio.create_task(key_fallback_loop())
    capacity_alerts = CapacityAlerts(settings, db, bark_notifier)
    capacity_tasks = [asyncio.create_task(capacity_alerts.collect_loop()), asyncio.create_task(capacity_alerts.delivery_loop())]
    profile_task = asyncio.create_task(desktop_service.account_model_profiles.loop())
    operation_task = asyncio.create_task(desktop_service.operations.loop())
    desktop_service.model_tests.resume()
    fingerprint_bank_task = asyncio.create_task(fingerprint_bank_loop())
    try:
        yield
    finally:
        operation_task.cancel()
        with suppress(asyncio.CancelledError):
            await operation_task
        profile_task.cancel()
        with suppress(asyncio.CancelledError):
            await profile_task
        for task in capacity_tasks:
            task.cancel()
        for task in capacity_tasks:
            with suppress(asyncio.CancelledError):
                await task
        capacity_alerts = None
        if fingerprint_bank_task:
            fingerprint_bank_task.cancel()
            with suppress(asyncio.CancelledError):
                await fingerprint_bank_task
            fingerprint_bank_task = None
        daily_schedule_task.cancel()
        with suppress(asyncio.CancelledError):
            await daily_schedule_task
        if key_fallback_task:
            key_fallback_task.cancel()
            with suppress(asyncio.CancelledError):
                await key_fallback_task
            key_fallback_task = None
        if oauth_monitor_task:
            oauth_monitor_task.cancel()
            with suppress(asyncio.CancelledError):
                await oauth_monitor_task
            oauth_monitor_task = None
        await desktop_service.close()
        oauth_monitor = None
        key_fallback_controller = None
        model_service.close()
        db.close()


app = FastAPI(title=settings.app_name, lifespan=lifespan, docs_url=None, redoc_url=None, openapi_url=None)


def oauth_config_file() -> dict[str, Any]:
    try:
        raw = json.loads(Path(settings.oauth_config_path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        raw = {}
    return raw if isinstance(raw, dict) else {}


def bark_config_file() -> dict[str, Any]:
    try:
        raw = json.loads(Path(settings.bark_config_path).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        raw = {}
    return raw if isinstance(raw, dict) else {}


def save_oauth_runtime_config(payload: dict[str, Any]) -> None:
    path = Path(settings.oauth_config_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    from .config_service import OAUTH_FIELDS
    payload = {key: value for key, value in payload.items() if key in OAUTH_FIELDS or key in {"updated_at", "updated_by"}}
    payload.pop("oauth_night_recovery_cooldown_enabled", None)
    payload.pop("oauth_usage_refresh_enabled", None)
    payload.pop("oauth_regular_refresh_interval_seconds", None)
    fd, name = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    temporary = Path(name)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        temporary.replace(path)
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        temporary.unlink(missing_ok=True)


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


def apply_oauth_runtime_config(payload: dict[str, Any]) -> None:
    from .config_service import OAUTH_FIELDS
    for key in OAUTH_FIELDS:
        if key in payload:
            setattr(settings, key, payload[key])
    if oauth_monitor is not None:
        oauth_monitor.daily_schedule.tick(datetime.now(timezone.utc))


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


def build_bark_config() -> dict[str, Any]:
    existing = bark_config_file()
    runtime = bark_notifier.runtime_config()
    try:
        normalize_bark_server_url(runtime.server_url)
        server_url_valid = True
    except ValueError:
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
        "server_url_valid": server_url_valid,
        "config_updated_at": existing.get("updated_at"),
    }


@app.get("/healthz")
def healthz() -> dict[str, str]:
    db.fetch_one("SELECT 1 AS ok")
    return {"status": "ok"}


from .desktop_api import install_desktop_api

desktop_service = install_desktop_api(app, sys.modules[__name__])

from .model_api import install_model_api

model_service = install_model_api(app, sys.modules[__name__], desktop_service)
