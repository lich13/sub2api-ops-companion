from __future__ import annotations

import json
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

from . import account_ops
from .audit import write_audit
from .sql import LEGACY_RECOVERY_PLAN_CLEANUP_SQL
from .usage_query import (
    execute_oauth_usage_query,
    oauth_plan_type,
    oauth_quota_summary_from_result,
    oauth_recovery_transition,
    oauth_windows_by_key,
    parse_iso_datetime,
    percent_or_none,
    required_oauth_window_keys,
)


STATE_VERSION = 2
INVENTORY_REFRESH_SECONDS = 60
EXACT_RESET_RETRY_SECONDS = 60
DEFAULT_REGULAR_REFRESH_SECONDS = 3600
DEFAULT_SEVEN_DAY_PROBE_SECONDS = 3600
DEFAULT_TEST_MODEL_ID = "gpt-5.6-luna"
_STORE_LOCK = threading.RLock()


def _utc(value: datetime | None = None) -> datetime:
    current = value or datetime.now(timezone.utc)
    if current.tzinfo is None:
        current = current.replace(tzinfo=timezone.utc)
    return current.astimezone(timezone.utc)


def _positive_int(value: object, default: int, minimum: int, maximum: int) -> int:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        parsed = default
    return max(minimum, min(maximum, parsed))


def _json_copy(value: Any) -> Any:
    return json.loads(json.dumps(value, ensure_ascii=False))


def _oauth_result(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    return bool(
        str(value.get("template_type") or "").lower() == "oauth"
        or value.get("source") == "sub2api_admin_usage"
        or isinstance(value.get("oauth_quota"), dict)
    )


class OAuthStateStore:
    def __init__(self, path: str) -> None:
        self.path = Path(path)
        self._data = self._normalize(self._read_raw())

    def _read_raw(self) -> dict[str, Any]:
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return {}
        return data if isinstance(data, dict) else {}

    def _normalize(self, raw: dict[str, Any]) -> dict[str, Any]:
        raw_settings = raw.get("settings") if isinstance(raw.get("settings"), dict) else {}
        settings: dict[str, Any] = {}
        admin_token = str(raw_settings.get("sub2api_admin_token") or "").strip()
        if admin_token:
            settings["sub2api_admin_token"] = admin_token
        if raw_settings.get("legacy_recovery_cleanup_completed") is True:
            settings["legacy_recovery_cleanup_completed"] = True
            if raw_settings.get("legacy_recovery_cleanup_completed_at"):
                settings["legacy_recovery_cleanup_completed_at"] = str(
                    raw_settings.get("legacy_recovery_cleanup_completed_at")
                )

        results: dict[str, dict[str, Any]] = {}
        for source_name in ("results", "oauth_results"):
            source = raw.get(source_name)
            if not isinstance(source, dict):
                continue
            for key, value in source.items():
                try:
                    account_id = int(key)
                except (TypeError, ValueError):
                    continue
                if account_id > 0 and _oauth_result(value):
                    results[str(account_id)] = dict(value)

        scheduler: dict[str, dict[str, Any]] = {}
        raw_scheduler = raw.get("scheduler")
        if isinstance(raw_scheduler, dict):
            for key, value in raw_scheduler.items():
                try:
                    account_id = int(key)
                except (TypeError, ValueError):
                    continue
                if account_id > 0 and isinstance(value, dict):
                    scheduler[str(account_id)] = dict(value)

        pending_events: dict[str, dict[str, Any]] = {}
        raw_pending = raw.get("pending_events")
        if isinstance(raw_pending, dict):
            for key, value in raw_pending.items():
                if str(key).strip() and isinstance(value, dict):
                    pending_events[str(key)] = dict(value)

        return {
            "version": STATE_VERSION,
            "settings": settings,
            "oauth_results": results,
            "scheduler": scheduler,
            "pending_events": pending_events,
        }

    def _write(self, data: dict[str, Any]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        temporary = self.path.with_suffix(self.path.suffix + ".tmp")
        temporary.write_text(
            json.dumps(data, ensure_ascii=False, indent=2, sort_keys=True),
            encoding="utf-8",
        )
        temporary.chmod(0o600)
        temporary.replace(self.path)
        self.path.chmod(0o600)

    def reload(self) -> None:
        with _STORE_LOCK:
            self._data = self._normalize(self._read_raw())

    def migrate(self) -> bool:
        with _STORE_LOCK:
            current = self._read_raw()
            normalized = self._normalize(current)
            changed = current != normalized
            if changed:
                self._write(normalized)
            self._data = normalized
            return changed

    def snapshot(self) -> dict[str, Any]:
        with _STORE_LOCK:
            self.reload()
            return _json_copy(self._data)

    def cached_snapshot(self) -> dict[str, Any]:
        with _STORE_LOCK:
            return _json_copy(self._data)

    def admin_token(self) -> str:
        with _STORE_LOCK:
            self.reload()
            return str(self._data["settings"].get("sub2api_admin_token") or "").strip()

    def save_admin_token(self, value: object) -> None:
        token = str(value or "").strip()
        if not token:
            return
        with _STORE_LOCK:
            data = self._normalize(self._read_raw())
            data["settings"]["sub2api_admin_token"] = token
            self._write(data)
            self._data = data

    def results(self) -> dict[int, dict[str, Any]]:
        data = self.snapshot()["oauth_results"]
        return {int(key): value for key, value in data.items()}

    def result(self, account_id: int) -> dict[str, Any]:
        return self.results().get(int(account_id), {})

    def scheduler(self) -> dict[int, dict[str, Any]]:
        data = self.snapshot()["scheduler"]
        return {int(key): value for key, value in data.items()}

    def pending_events(self) -> list[dict[str, Any]]:
        data = self.snapshot()["pending_events"]
        return [dict(data[key]) for key in sorted(data)]

    def cached_pending_events(self) -> list[dict[str, Any]]:
        data = self.cached_snapshot()["pending_events"]
        return [dict(data[key]) for key in sorted(data)]

    def update_scheduler(self, updates: dict[int, dict[str, Any]]) -> None:
        self.commit(scheduler_updates=updates)

    def commit(
        self,
        *,
        results: dict[int, dict[str, Any]] | None = None,
        scheduler_updates: dict[int, dict[str, Any]] | None = None,
        pending_events: dict[str, dict[str, Any]] | None = None,
        remove_pending_keys: set[str] | None = None,
    ) -> None:
        with _STORE_LOCK:
            data = self._normalize(self._read_raw())
            for account_id, value in (results or {}).items():
                data["oauth_results"][str(int(account_id))] = dict(value)
            for account_id, update in (scheduler_updates or {}).items():
                key = str(int(account_id))
                current = data["scheduler"].get(key)
                merged = dict(current) if isinstance(current, dict) else {}
                merged.update(dict(update))
                data["scheduler"][key] = merged
            for key, event in (pending_events or {}).items():
                data["pending_events"][str(key)] = dict(event)
            for key in remove_pending_keys or set():
                data["pending_events"].pop(str(key), None)
            self._write(data)
            self._data = data

    def mark_events_delivered(self, events: list[dict[str, Any]], *, suppressed: bool = False) -> None:
        if not events:
            return
        snapshot = self.snapshot()
        scheduler_updates: dict[int, dict[str, Any]] = {}
        remove_keys: set[str] = set()
        now = datetime.now(timezone.utc).isoformat()
        for event in events:
            key = str(event.get("dedupe_key") or "").strip()
            account_id = int(event.get("account_id") or 0)
            if not key or account_id <= 0:
                continue
            remove_keys.add(key)
            current = dict((snapshot.get("scheduler") or {}).get(str(account_id)) or {})
            notified = [str(item) for item in current.get("notified_keys") or [] if str(item)]
            if key not in notified:
                notified.append(key)
            scheduler_updates[account_id] = {
                "notified_keys": notified[-40:],
                "last_notification_at": now,
                "last_notification_suppressed": bool(suppressed),
            }
        self.commit(scheduler_updates=scheduler_updates, remove_pending_keys=remove_keys)

    def legacy_recovery_cleanup_completed(self) -> bool:
        return bool(self.snapshot()["settings"].get("legacy_recovery_cleanup_completed"))

    def mark_legacy_recovery_cleanup_completed(self) -> None:
        with _STORE_LOCK:
            data = self._normalize(self._read_raw())
            data["settings"]["legacy_recovery_cleanup_completed"] = True
            data["settings"]["legacy_recovery_cleanup_completed_at"] = datetime.now(timezone.utc).isoformat()
            self._write(data)
            self._data = data


def migrate_legacy_recovery_state(
    db: Any,
    store: OAuthStateStore,
    legacy_state_path: str,
    audit_path: str,
) -> dict[str, Any]:
    store.migrate()
    if store.legacy_recovery_cleanup_completed():
        return {"success": True, "skipped": True, "deleted_count": 0}
    try:
        deleted = db.fetch_all(LEGACY_RECOVERY_PLAN_CLEANUP_SQL)
    except Exception as exc:
        outcome = {"success": False, "skipped": False, "deleted_count": 0, "error": str(exc)}
        write_audit(audit_path, "oauth_legacy_recovery_cleanup_failed", outcome)
        return outcome
    path = Path(legacy_state_path)
    try:
        path.unlink(missing_ok=True)
    except OSError as exc:
        outcome = {"success": False, "skipped": False, "deleted_count": len(deleted), "error": str(exc)}
        write_audit(audit_path, "oauth_legacy_recovery_cleanup_failed", outcome)
        return outcome
    store.mark_legacy_recovery_cleanup_completed()
    outcome = {"success": True, "skipped": False, "deleted_count": len(deleted)}
    write_audit(audit_path, "oauth_legacy_recovery_cleanup", outcome)
    return outcome


def _scheduler_row(scheduler: dict[int, dict[str, Any]] | dict[str, dict[str, Any]], account_id: int) -> dict[str, Any]:
    value = scheduler.get(account_id) or scheduler.get(str(account_id))  # type: ignore[arg-type]
    return dict(value) if isinstance(value, dict) else {}


def _seconds_since(value: object, now: datetime) -> float | None:
    parsed = parse_iso_datetime(value)
    return None if parsed is None else max(0.0, (now - parsed).total_seconds())


def _due(value: object, interval_seconds: int, now: datetime) -> bool:
    elapsed = _seconds_since(value, now)
    return elapsed is None or elapsed >= interval_seconds


def build_monitor_candidates(
    accounts: list[dict[str, Any]],
    results: dict[int, dict[str, Any]],
    scheduler: dict[int, dict[str, Any]] | dict[str, dict[str, Any]],
    now: datetime,
    *,
    regular_interval_seconds: int = DEFAULT_REGULAR_REFRESH_SECONDS,
    seven_day_probe_interval_seconds: int = DEFAULT_SEVEN_DAY_PROBE_SECONDS,
    usage_refresh_enabled: bool = True,
    recovery_monitor_enabled: bool = True,
) -> list[dict[str, Any]]:
    current = _utc(now)
    regular_interval = _positive_int(regular_interval_seconds, 3600, 60, 86400)
    probe_interval = _positive_int(seven_day_probe_interval_seconds, 3600, 60, 86400)
    candidates: list[dict[str, Any]] = []
    for row in accounts:
        account_id = int(row.get("id") or row.get("account_id") or 0)
        if account_id <= 0:
            continue
        result = results.get(account_id) or {}
        metadata = _scheduler_row(scheduler, account_id)
        summary = oauth_quota_summary_from_result(row, result)
        windows = oauth_windows_by_key(summary.get("ui_windows"))
        required_keys = required_oauth_window_keys(summary.get("plan_type"))
        full_windows: list[dict[str, Any]] = []
        for key in required_keys:
            item = windows.get(key)
            used = percent_or_none((item or {}).get("used_percent"))
            if item and used is not None and used >= 100:
                full_windows.append(item)

        reason = ""
        priority = 99
        fingerprint = ""
        if recovery_monitor_enabled and full_windows:
            reset_times = [parse_iso_datetime(item.get("reset_at")) for item in full_windows]
            if all(reset_times):
                latest = max(item for item in reset_times if item is not None)
                fingerprint = "|".join(str(item.get("reset_at") or "") for item in full_windows)
                same_exact = metadata.get("last_exact_fingerprint") == fingerprint
                retry_due = _due(metadata.get("last_exact_at"), EXACT_RESET_RETRY_SECONDS, current)
                if current >= latest and (not same_exact or retry_due):
                    reason, priority = "exact_reset", 0

        if not reason and not result.get("success") and (usage_refresh_enabled or recovery_monitor_enabled):
            if _due(metadata.get("last_attempt_at"), EXACT_RESET_RETRY_SECONDS, current):
                reason, priority = "bootstrap", 1

        seven_day = windows.get("codex_7d")
        seven_used = percent_or_none((seven_day or {}).get("used_percent"))
        seven_reset = parse_iso_datetime((seven_day or {}).get("reset_at"))
        seven_full_before_reset = bool(
            seven_day and seven_used is not None and seven_used >= 100 and (seven_reset is None or current < seven_reset)
        )
        if not reason and recovery_monitor_enabled and seven_full_before_reset:
            last_probe = metadata.get("last_7d_probe_at") or result.get("queried_at")
            if _due(last_probe, probe_interval, current):
                reason, priority = "seven_day_probe", 2

        if not reason and usage_refresh_enabled:
            last_regular = metadata.get("last_regular_at") or result.get("queried_at")
            if _due(last_regular, regular_interval, current):
                reason, priority = "regular_refresh", 3

        if reason:
            candidates.append(
                {
                    "account_id": account_id,
                    "row": row,
                    "reason": reason,
                    "priority": priority,
                    "exact_fingerprint": fingerprint,
                    "previous_summary": summary,
                }
            )
    candidates.sort(key=lambda item: (int(item["priority"]), int(item["account_id"])))
    return candidates


def _auth_error_code(value: object) -> str:
    code = str(value or "").strip().lower()
    if code in {"401", "http_401"}:
        return "http_401"
    if code in {"402", "http_402"}:
        return "http_402"
    return ""


def execute_sub2api_account_test(
    account_id: int,
    model_id: str,
    *,
    base_url: str,
    admin_token: str,
    timeout_seconds: int = 30,
) -> dict[str, Any]:
    base = str(base_url or "").strip().rstrip("/")
    token = str(admin_token or "").strip()
    if not base or not token:
        return {
            "success": False,
            "error": "缺少 Sub2API 地址或 Admin API Key",
            "error_code": "missing_sub2api_admin_credentials",
        }
    request = urllib.request.Request(
        f"{base}/api/v1/admin/accounts/{int(account_id)}/test",
        data=json.dumps({"model_id": str(model_id or ""), "prompt": "", "mode": ""}).encode("utf-8"),
        headers={
            "Accept": "text/event-stream",
            "Content-Type": "application/json",
            "x-api-key": token,
        },
        method="POST",
    )
    started = time.monotonic()
    try:
        with urllib.request.urlopen(request, timeout=_positive_int(timeout_seconds, 30, 2, 60)) as response:
            payload = response.read().decode("utf-8", errors="replace")
        parsed = parse_sub2api_account_test_sse(payload)
        parsed["duration_ms"] = int((time.monotonic() - started) * 1000)
        parsed["model_id"] = str(model_id or "")
        return parsed
    except TimeoutError as exc:
        return {"success": False, "error": str(exc) or "请求超时", "error_code": "timeout", "model_id": model_id}
    except urllib.error.HTTPError as exc:
        preview = exc.read(200).decode("utf-8", errors="replace")
        return {
            "success": False,
            "error": preview or f"HTTP {exc.code}",
            "error_code": f"http_{exc.code}",
            "model_id": model_id,
        }
    except urllib.error.URLError as exc:
        return {"success": False, "error": str(exc.reason), "error_code": "network_error", "model_id": model_id}
    except Exception as exc:
        return {"success": False, "error": str(exc), "error_code": "account_test_error", "model_id": model_id}


def parse_sub2api_account_test_sse(payload: str) -> dict[str, Any]:
    texts: list[str] = []
    success = False
    for line in str(payload or "").splitlines():
        line = line.strip()
        if not line.startswith("data:"):
            continue
        try:
            event = json.loads(line.removeprefix("data:").strip())
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        if event.get("type") == "error":
            nested = event.get("error") if isinstance(event.get("error"), dict) else {}
            error_code = next(
                (
                    str(value).strip()
                    for value in (
                        event.get("code"),
                        event.get("error_code"),
                        event.get("status_code"),
                        nested.get("code"),
                    )
                    if str(value or "").strip()
                ),
                "unknown_test_error",
            )
            error = next(
                (
                    str(value).strip()
                    for value in (
                        nested.get("message"),
                        nested.get("error"),
                        nested.get("detail"),
                        event.get("message"),
                        event.get("detail"),
                        event.get("error"),
                    )
                    if str(value or "").strip()
                ),
                "账号测试失败",
            )
            return {"success": False, "error": error, "error_code": error_code}
        if event.get("type") == "content" and event.get("text"):
            texts.append(str(event.get("text")))
        if event.get("type") == "test_complete" and event.get("success") is True:
            success = True
    if not success:
        return {"success": False, "error": "账号测试未返回成功事件", "error_code": "missing_success_event"}
    return {"success": True, "response_text": "".join(texts).strip()}


class OAuthMonitor:
    def __init__(
        self,
        settings: Any,
        db: Any,
        *,
        base_url_provider: Callable[[], str],
        inventory_loader: Callable[[Any], list[dict[str, Any]]] = account_ops.current_oauth_accounts,
        usage_runner: Callable[..., dict[str, Any]] = execute_oauth_usage_query,
        test_runner: Callable[..., dict[str, Any]] = execute_sub2api_account_test,
    ) -> None:
        self.settings = settings
        self.db = db
        self.store = OAuthStateStore(settings.usage_query_state_path)
        self.base_url_provider = base_url_provider
        self.inventory_loader = inventory_loader
        self.usage_runner = usage_runner
        self.test_runner = test_runner
        self._accounts: list[dict[str, Any]] = []
        self._inventory_loaded_at: datetime | None = None
        self._run_lock = threading.Lock()

    def _refresh_inventory(self, now: datetime) -> None:
        if self._inventory_loaded_at and (now - self._inventory_loaded_at).total_seconds() < INVENTORY_REFRESH_SECONDS:
            return
        self._accounts = list(self.inventory_loader(self.db))
        self._inventory_loaded_at = now
        self.store.reload()

    def _known_event(self, key: str, account_id: int, state: dict[str, Any]) -> bool:
        if key in (state.get("pending_events") or {}):
            return True
        metadata = (state.get("scheduler") or {}).get(str(account_id)) or {}
        return key in [str(item) for item in metadata.get("notified_keys") or []]

    def run_once(self, now: datetime | None = None) -> list[dict[str, Any]]:
        if not self._run_lock.acquire(blocking=False):
            return self.store.cached_pending_events()
        started = time.monotonic()
        current = _utc(now)
        try:
            try:
                self._refresh_inventory(current)
            except Exception as exc:
                write_audit(self.settings.audit_path, "oauth_monitor_inventory_failed", {"error": str(exc)})
                return self.store.cached_pending_events()

            state = self.store.cached_snapshot()
            results = {int(key): value for key, value in (state.get("oauth_results") or {}).items()}
            scheduler = {int(key): value for key, value in (state.get("scheduler") or {}).items()}
            candidates = build_monitor_candidates(
                self._accounts,
                results,
                scheduler,
                current,
                regular_interval_seconds=getattr(
                    self.settings,
                    "telegram_oauth_regular_refresh_interval_seconds",
                    DEFAULT_REGULAR_REFRESH_SECONDS,
                ),
                seven_day_probe_interval_seconds=getattr(
                    self.settings,
                    "telegram_oauth_7d_probe_interval_seconds",
                    DEFAULT_SEVEN_DAY_PROBE_SECONDS,
                ),
                usage_refresh_enabled=bool(getattr(self.settings, "telegram_oauth_usage_refresh_enabled", True)),
                recovery_monitor_enabled=bool(getattr(self.settings, "telegram_oauth_recovery_monitor_enabled", True)),
            )
            batch_size = _positive_int(
                getattr(self.settings, "telegram_oauth_early_probe_batch_size", 8), 8, 1, 50
            )
            selected = candidates[:batch_size]
            if not selected:
                return [dict(value) for _, value in sorted((state.get("pending_events") or {}).items())]

            token = str((state.get("settings") or {}).get("sub2api_admin_token") or "").strip()
            base_url = str(self.base_url_provider() or "").strip().rstrip("/")
            workers = min(
                len(selected),
                _positive_int(getattr(self.settings, "telegram_oauth_usage_refresh_concurrency", 4), 4, 1, 16),
            )
            usage_results: dict[int, dict[str, Any]] = {}
            with ThreadPoolExecutor(max_workers=max(1, workers)) as executor:
                futures = {
                    executor.submit(
                        self.usage_runner,
                        int(item["account_id"]),
                        base_url,
                        token,
                        account_row=item["row"],
                        timeout_seconds=10,
                        now=current,
                    ): item
                    for item in selected
                }
                for future in as_completed(futures):
                    item = futures[future]
                    account_id = int(item["account_id"])
                    try:
                        usage_results[account_id] = future.result()
                    except Exception as exc:
                        usage_results[account_id] = {
                            "account_id": account_id,
                            "template_type": "oauth",
                            "success": False,
                            "queried_at": current.isoformat(),
                            "error": str(exc),
                            "error_code": "oauth_usage_query_error",
                        }

            successful_results: dict[int, dict[str, Any]] = {}
            scheduler_updates: dict[int, dict[str, Any]] = {}
            pending_updates: dict[str, dict[str, Any]] = {}
            remove_pending: set[str] = set()
            test_jobs: list[tuple[dict[str, Any], dict[str, Any]]] = []
            selected_by_id = {int(item["account_id"]): item for item in selected}
            for account_id, refreshed_result in usage_results.items():
                selected_item = selected_by_id[account_id]
                reason = str(selected_item["reason"])
                update: dict[str, Any] = {"last_attempt_at": current.isoformat(), "last_reason": reason}
                if reason in {"bootstrap", "regular_refresh", "exact_reset"}:
                    update["last_regular_at"] = current.isoformat()
                if reason == "seven_day_probe":
                    update["last_7d_probe_at"] = current.isoformat()
                if reason == "exact_reset":
                    update["last_exact_at"] = current.isoformat()
                    update["last_exact_fingerprint"] = selected_item.get("exact_fingerprint") or ""

                if not refreshed_result.get("success"):
                    update["last_error_code"] = str(refreshed_result.get("error_code") or "")
                    update["last_error_at"] = current.isoformat()
                    auth_code = _auth_error_code(refreshed_result.get("error_code"))
                    if auth_code:
                        key = f"auth:{account_id}:active_usage:{auth_code}"
                        if not self._known_event(key, account_id, state):
                            row = selected_item["row"]
                            pending_updates[key] = {
                                "account_id": account_id,
                                "account_name": row.get("name") or "-",
                                "plan_type": oauth_plan_type(row),
                                "status": "auth_failed",
                                "stage": "active_usage",
                                "error_code": auth_code,
                                "error": str(refreshed_result.get("error") or ""),
                                "checked_at": current.isoformat(),
                                "dedupe_key": key,
                            }
                    scheduler_updates[account_id] = update
                    continue

                successful_results[account_id] = refreshed_result
                update["last_success_at"] = current.isoformat()
                update["last_error_code"] = ""
                current_metadata = _scheduler_row(scheduler, account_id)
                notified = [
                    str(item)
                    for item in current_metadata.get("notified_keys") or []
                    if not str(item).startswith(f"auth:{account_id}:active_usage:")
                ]
                update["notified_keys"] = notified
                for key in (state.get("pending_events") or {}):
                    if str(key).startswith(f"auth:{account_id}:active_usage:"):
                        remove_pending.add(str(key))

                refreshed_summary = oauth_quota_summary_from_result(selected_item["row"], refreshed_result)
                transition = oauth_recovery_transition(
                    selected_item["previous_summary"],
                    refreshed_summary,
                    now=current,
                )
                if transition and bool(getattr(self.settings, "telegram_oauth_recovery_monitor_enabled", True)):
                    fingerprint = str(transition.get("fingerprint") or "")
                    tested = [str(item) for item in current_metadata.get("tested_fingerprints") or []]
                    if fingerprint and fingerprint not in tested:
                        test_jobs.append((selected_item, transition))
                scheduler_updates[account_id] = update

            test_workers = min(
                len(test_jobs),
                _positive_int(getattr(self.settings, "telegram_oauth_recovery_test_concurrency", 2), 2, 1, 8),
            )
            test_results: dict[int, dict[str, Any]] = {}
            if test_jobs:
                model_id = str(
                    getattr(self.settings, "telegram_oauth_recovery_test_model_id", DEFAULT_TEST_MODEL_ID)
                    or DEFAULT_TEST_MODEL_ID
                ).strip()
                with ThreadPoolExecutor(max_workers=max(1, test_workers)) as executor:
                    futures = {
                        executor.submit(
                            self.test_runner,
                            int(item["account_id"]),
                            model_id,
                            base_url=base_url,
                            admin_token=token,
                            timeout_seconds=30,
                        ): (item, transition)
                        for item, transition in test_jobs
                    }
                    for future in as_completed(futures):
                        item, transition = futures[future]
                        account_id = int(item["account_id"])
                        try:
                            test_results[account_id] = future.result()
                        except Exception as exc:
                            test_results[account_id] = {
                                "success": False,
                                "error": str(exc),
                                "error_code": "account_test_error",
                                "model_id": model_id,
                            }
                        fingerprint = str(transition.get("fingerprint") or "")
                        current_metadata = _scheduler_row(scheduler, account_id)
                        tested = [str(value) for value in current_metadata.get("tested_fingerprints") or []]
                        if fingerprint and fingerprint not in tested:
                            tested.append(fingerprint)
                        scheduler_updates.setdefault(account_id, {})["tested_fingerprints"] = tested[-20:]
                        test_result = test_results[account_id]
                        status = "recovered" if test_result.get("success") else "test_failed"
                        error_code = str(test_result.get("error_code") or "")
                        suffix = "success" if status == "recovered" else f"failure:{error_code or 'unknown'}"
                        key = f"recovery:{account_id}:{fingerprint}:{suffix}"
                        if self._known_event(key, account_id, state):
                            continue
                        event = {
                            **transition,
                            "account_id": account_id,
                            "account_name": item["row"].get("name") or "-",
                            "status": status,
                            "checked_at": current.isoformat(),
                            "model_id": str(test_result.get("model_id") or model_id),
                            "duration_ms": test_result.get("duration_ms", test_result.get("latency_ms")),
                            "test_success": bool(test_result.get("success")),
                            "error_code": error_code,
                            "error": str(test_result.get("error") or ""),
                            "dedupe_key": key,
                        }
                        pending_updates[key] = event

            self.store.commit(
                results=successful_results,
                scheduler_updates=scheduler_updates,
                pending_events=pending_updates,
                remove_pending_keys=remove_pending,
            )
            duration_ms = int((time.monotonic() - started) * 1000)
            write_audit(
                self.settings.audit_path,
                "oauth_monitor_cycle",
                {
                    "candidate_count": len(candidates),
                    "queried_count": len(selected),
                    "tested_count": len(test_jobs),
                    "pending_count": len(pending_updates),
                    "duration_ms": duration_ms,
                    "active_usage_concurrency": workers,
                    "test_concurrency": test_workers,
                    "regular_refresh_interval_seconds": getattr(
                        self.settings, "telegram_oauth_regular_refresh_interval_seconds", 3600
                    ),
                    "seven_day_probe_interval_seconds": getattr(
                        self.settings, "telegram_oauth_7d_probe_interval_seconds", 3600
                    ),
                },
            )
            return self.store.cached_pending_events()
        finally:
            self._run_lock.release()
