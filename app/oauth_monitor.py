from __future__ import annotations

import hashlib
import json
import threading
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any
from zoneinfo import ZoneInfo

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

STATE_VERSION = 3
INVENTORY_REFRESH_SECONDS = 60
EXACT_RESET_RETRY_SECONDS = 60
DEFAULT_REGULAR_REFRESH_SECONDS = 3600
DEFAULT_SEVEN_DAY_PROBE_SECONDS = 3600
DEFAULT_TEST_MODEL_ID = "gpt-5.6-luna"
RECOVERY_RETRY_SECONDS = (60, 300, 900, 1800)
BEIJING_TZ = ZoneInfo("Asia/Shanghai")
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


def _normal_recovery_intent(value: object) -> dict[str, Any]:
    intent = dict(value) if isinstance(value, dict) else {}
    status = str(intent.get("status") or "").strip()
    if status == "testing":
        intent["status"] = "retry"
        intent["next_retry_at"] = str(
            intent.get("next_retry_at")
            or intent.get("tested_at")
            or intent.get("confirmed_at")
            or intent.get("due_at")
            or ""
        )
    return intent


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
                    normalized_row = dict(value)
                    if "recovery_intent" in normalized_row:
                        normalized_row["recovery_intent"] = _normal_recovery_intent(
                            normalized_row.get("recovery_intent")
                        )
                    scheduler[str(account_id)] = normalized_row

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


def in_beijing_night_cooldown(now: datetime, *, enabled: bool = True) -> bool:
    if not enabled:
        return False
    local = _utc(now).astimezone(BEIJING_TZ)
    return 0 <= local.hour < 5


def beijing_cooldown_end(now: datetime) -> datetime:
    local = _utc(now).astimezone(BEIJING_TZ)
    end = local.replace(hour=5, minute=0, second=0, microsecond=0)
    if local.hour >= 5:
        end += timedelta(days=1)
    return end.astimezone(timezone.utc)


def _canonical_value(value: object) -> object:
    if isinstance(value, datetime):
        return _utc(value).isoformat()
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    return str(value)


def recovery_block_signature(row: dict[str, Any] | None) -> str:
    account = row or {}
    extra = account.get("extra") if isinstance(account.get("extra"), dict) else {}
    payload = {
        key: _canonical_value(account.get(key))
        for key in (
            "id",
            "platform",
            "type",
            "status",
            "schedulable",
            "concurrency",
            "rate_limited_at",
            "rate_limit_reset_at",
            "overload_until",
            "temp_unschedulable_until",
            "temp_unschedulable_reason",
        )
    }
    payload["model_rate_limits"] = extra.get("model_rate_limits")
    payload["antigravity_quota_scopes"] = extra.get("antigravity_quota_scopes")
    encoded = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"), default=str)
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def recovery_block_change_is_safe(
    before: dict[str, Any] | None, after: dict[str, Any] | None
) -> bool:
    previous = before or {}
    current = after or {}
    for field in ("id", "platform", "type", "status", "schedulable", "concurrency"):
        if _canonical_value(previous.get(field)) != _canonical_value(current.get(field)):
            return False
    for field in (
        "rate_limited_at",
        "rate_limit_reset_at",
        "overload_until",
        "temp_unschedulable_until",
        "temp_unschedulable_reason",
    ):
        old_value = _canonical_value(previous.get(field))
        new_value = _canonical_value(current.get(field))
        if new_value not in (None, "") and new_value != old_value:
            return False
    previous_extra = previous.get("extra") if isinstance(previous.get("extra"), dict) else {}
    current_extra = current.get("extra") if isinstance(current.get("extra"), dict) else {}
    for field in ("model_rate_limits", "antigravity_quota_scopes"):
        old_value = previous_extra.get(field)
        new_value = current_extra.get(field)
        if new_value not in (None, "", {}, []) and new_value != old_value:
            return False
    return True


def _threshold_reason_details(value: object) -> dict[str, Any]:
    reason = str(value or "").strip()
    try:
        payload = json.loads(reason)
    except (json.JSONDecodeError, TypeError):
        return {}
    if (
        not isinstance(payload, dict)
        or str(payload.get("source") or "").strip() != "account_scheduling_threshold"
        or str(payload.get("platform") or "").strip().lower() != "openai"
    ):
        return {}
    window = str(payload.get("window") or "").strip().lower().removeprefix("codex_")
    if window not in {"5h", "7d"}:
        return {}
    threshold_percent = percent_or_none(payload.get("threshold_percent"))
    if threshold_percent is None or not 0 < threshold_percent <= 100:
        return {}
    until_unix = payload.get("until_unix")
    if isinstance(until_unix, bool) or percent_or_none(until_unix) is None:
        return {}
    until_at = parse_iso_datetime(until_unix)
    if until_at is None:
        return {}
    return {
        "window_key": f"codex_{window}",
        "threshold_percent": threshold_percent,
        "until_at": until_at,
    }


def automatic_recovery_eligible(
    row: dict[str, Any] | None,
    *,
    exhausted_window_keys: list[str] | tuple[str, ...] | None = None,
    now: datetime | None = None,
) -> bool:
    account = row or {}
    if (
        int(account.get("id") or 0) <= 0
        or str(account.get("platform") or "").lower() != "openai"
        or str(account.get("type") or "").lower() != "oauth"
        or account.get("deleted_at") not in (None, "")
        or str(account.get("status") or "").lower() != "active"
        or account.get("schedulable") is not True
    ):
        return False
    reason = str(account.get("temp_unschedulable_reason") or "").strip()
    raw_temp_until = account.get("temp_unschedulable_until")
    threshold = _threshold_reason_details(reason)
    if reason and not threshold:
        return False
    observed = set(exhausted_window_keys or ())
    if raw_temp_until not in (None, "") and not threshold:
        return False
    temp_until = parse_iso_datetime(raw_temp_until)
    reason_until = threshold.get("until_at") if threshold else None
    if (
        temp_until is not None
        and reason_until is not None
        and abs((temp_until - reason_until).total_seconds()) > 1
    ):
        return False
    if account.get("rate_limited_at") or account.get("rate_limit_reset_at"):
        return True
    if temp_until is None or not threshold:
        return False
    if not observed or not observed.issubset({"codex_5h", "codex_7d"}):
        return False
    threshold_window = str(threshold.get("window_key") or "")
    return not threshold_window or threshold_window in observed


def account_recovery_confirmed(row: dict[str, Any] | None) -> bool:
    account = row or {}
    return bool(
        int(account.get("id") or 0) > 0
        and str(account.get("platform") or "").lower() == "openai"
        and str(account.get("type") or "").lower() == "oauth"
        and account.get("deleted_at") in (None, "")
        and str(account.get("status") or "").lower() == "active"
        and account.get("schedulable") is True
        and not account.get("rate_limited_at")
        and not account.get("rate_limit_reset_at")
        and not account.get("overload_until")
        and not account.get("temp_unschedulable_until")
        and not account.get("temp_unschedulable_reason")
    )


def _quota_recovery_descriptor(summary: dict[str, Any], now: datetime) -> dict[str, Any] | None:
    windows = oauth_windows_by_key(summary.get("ui_windows"))
    depleted: list[tuple[str, dict[str, Any], datetime | None]] = []
    for key in required_oauth_window_keys(summary.get("plan_type")):
        window = windows.get(key)
        used = percent_or_none((window or {}).get("used_percent"))
        if window and used is not None and used >= 100:
            depleted.append((key, window, parse_iso_datetime(window.get("reset_at"))))
    if not depleted:
        return None
    parsed = [item[2] for item in depleted if item[2] is not None]
    due = max(parsed) if len(parsed) == len(depleted) else _utc(now)
    sources = [str(item[1].get("reset_source") or "server_exact") for item in depleted]
    source = "server_exact" if all(item == "server_exact" for item in sources) else "estimated_from_remaining"
    parts = [f"{key}@{reset.isoformat() if reset else 'unknown'}" for key, _window, reset in depleted]
    return {
        "fingerprint": "|".join(parts),
        "due_at": due.isoformat(),
        "source": source,
        "window_keys": [item[0] for item in depleted],
        "kind": "quota_exhausted",
    }


def _threshold_recovery_descriptor(
    row: dict[str, Any], summary: dict[str, Any]
) -> dict[str, Any] | None:
    threshold = _threshold_reason_details(row.get("temp_unschedulable_reason"))
    window_key = str(threshold.get("window_key") or "") if threshold else ""
    due_at = parse_iso_datetime(row.get("temp_unschedulable_until"))
    if not window_key or due_at is None:
        return None
    if not automatic_recovery_eligible(row, exhausted_window_keys=[window_key]):
        return None
    window = oauth_windows_by_key(summary.get("ui_windows")).get(window_key) or {}
    return {
        "fingerprint": f"{window_key}@{due_at.isoformat()}",
        "due_at": due_at.isoformat(),
        "source": str(window.get("reset_source") or "server_exact"),
        "window_keys": [window_key],
        "kind": "account_scheduling_threshold",
        "threshold_percent": threshold.get("threshold_percent"),
    }


def _threshold_quota_available(summary: dict[str, Any], descriptor: dict[str, Any]) -> bool:
    window_keys = [str(value) for value in descriptor.get("window_keys") or []]
    if len(window_keys) != 1:
        return False
    window = oauth_windows_by_key(summary.get("ui_windows")).get(window_keys[0]) or {}
    used = percent_or_none(window.get("used_percent"))
    threshold = percent_or_none(descriptor.get("threshold_percent"))
    if used is None:
        return False
    return used < (threshold if threshold is not None else 100)


def _quota_all_required_available(summary: dict[str, Any]) -> bool:
    windows = oauth_windows_by_key(summary.get("ui_windows"))
    for key in required_oauth_window_keys(summary.get("plan_type")):
        used = percent_or_none((windows.get(key) or {}).get("used_percent"))
        if used is None or used >= 100:
            return False
    return True


def _rate_limit_recovery_descriptor(
    row: dict[str, Any], summary: dict[str, Any], now: datetime
) -> dict[str, Any] | None:
    if not (row.get("rate_limited_at") or row.get("rate_limit_reset_at")):
        return None
    window_keys = list(required_oauth_window_keys(summary.get("plan_type")))
    if not _quota_all_required_available(summary):
        return None
    if not automatic_recovery_eligible(row, exhausted_window_keys=window_keys, now=now):
        return None
    block_time = parse_iso_datetime(row.get("rate_limited_at")) or parse_iso_datetime(
        row.get("rate_limit_reset_at")
    )
    if block_time is None:
        return None
    normalized = block_time.isoformat()
    return {
        "fingerprint": "|".join(f"{key}@{normalized}" for key in window_keys),
        "due_at": _utc(now).isoformat(),
        "source": "server_exact",
        "window_keys": window_keys,
        "kind": "rate_limit_block",
    }


def _retry_at(now: datetime, attempt_count: int) -> str:
    index = min(max(0, int(attempt_count) - 1), len(RECOVERY_RETRY_SECONDS) - 1)
    return (_utc(now) + timedelta(seconds=RECOVERY_RETRY_SECONDS[index])).isoformat()


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
        intent = _normal_recovery_intent(metadata.get("recovery_intent"))
        intent_status = str(intent.get("status") or "")
        intent_due_value = (
            intent.get("deferred_until")
            if intent_status == "deferred"
            else intent.get("next_retry_at")
            if intent_status in {"retry", "waiting_quota", "testing"}
            else intent.get("due_at")
        )
        intent_due = parse_iso_datetime(intent_due_value)
        if (
            recovery_monitor_enabled
            and intent_status in {"pending", "ready", "deferred", "retry", "waiting_quota", "testing"}
            and (intent_due is None or current >= intent_due)
        ):
            reason, priority = "recovery_intent", 0
            fingerprint = str(intent.get("fingerprint") or "")
        if recovery_monitor_enabled and full_windows:
            reset_times = [parse_iso_datetime(item.get("reset_at")) for item in full_windows]
            if not reason and all(reset_times):
                latest = max(item for item in reset_times if item is not None)
                fingerprint = "|".join(
                    f"{item.get('key')}@{parse_iso_datetime(item.get('reset_at')).isoformat()}"
                    for item in full_windows
                    if parse_iso_datetime(item.get("reset_at")) is not None
                )
                same_exact = metadata.get("last_exact_fingerprint") == fingerprint
                retry_due = _due(metadata.get("last_exact_at"), EXACT_RESET_RETRY_SECONDS, current)
                if current >= latest and (not same_exact or retry_due):
                    reason, priority = "exact_reset", 0

        threshold_descriptor = _threshold_recovery_descriptor(row, summary)
        if not reason and recovery_monitor_enabled and threshold_descriptor:
            threshold_due = parse_iso_datetime(threshold_descriptor.get("due_at"))
            threshold_fingerprint = str(threshold_descriptor.get("fingerprint") or "")
            same_exact = metadata.get("last_exact_fingerprint") == threshold_fingerprint
            retry_due = _due(metadata.get("last_exact_at"), EXACT_RESET_RETRY_SECONDS, current)
            if threshold_due is not None and current >= threshold_due and (not same_exact or retry_due):
                reason, priority = "exact_reset", 0
                fingerprint = threshold_fingerprint

        if (
            not reason
            and not result.get("success")
            and (usage_refresh_enabled or recovery_monitor_enabled)
            and _due(metadata.get("last_attempt_at"), EXACT_RESET_RETRY_SECONDS, current)
        ):
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


def _admin_account_request(
    url: str,
    admin_token: str,
    *,
    method: str = "POST",
    timeout_seconds: int = 30,
) -> dict[str, Any]:
    request = urllib.request.Request(
        url,
        data=b"" if method != "GET" else None,
        headers={"Accept": "application/json", "x-api-key": str(admin_token or "").strip()},
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=_positive_int(timeout_seconds, 30, 2, 60)) as response:
            raw = response.read().decode("utf-8", errors="replace")
            try:
                payload = json.loads(raw) if raw else {}
            except json.JSONDecodeError:
                payload = {}
            return {"success": 200 <= int(response.status) < 300, "status_code": int(response.status), "data": payload}
    except urllib.error.HTTPError as exc:
        preview = exc.read(500).decode("utf-8", errors="replace")
        return {
            "success": False,
            "status_code": int(exc.code),
            "error_code": f"http_{exc.code}",
            "error": preview or f"HTTP {exc.code}",
        }
    except TimeoutError as exc:
        return {"success": False, "error_code": "timeout", "error": str(exc) or "请求超时"}
    except urllib.error.URLError as exc:
        return {"success": False, "error_code": "network_error", "error": str(exc.reason)}
    except Exception as exc:
        return {"success": False, "error_code": "recovery_request_error", "error": str(exc)}


def execute_sub2api_account_recovery(
    account_id: int,
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
            "error_code": "missing_sub2api_admin_credentials",
            "error": "缺少 Sub2API 地址或 Admin API Key",
        }
    prefix = f"{base}/api/v1/admin/accounts/{int(account_id)}"
    primary = _admin_account_request(
        f"{prefix}/recover-state", token, timeout_seconds=timeout_seconds
    )
    if primary.get("success"):
        return {**primary, "method": "recover-state", "fallback": False}
    if int(primary.get("status_code") or 0) != 404:
        return {**primary, "method": "recover-state", "fallback": False}
    clear_rate = _admin_account_request(
        f"{prefix}/clear-rate-limit", token, timeout_seconds=timeout_seconds
    )
    if not clear_rate.get("success"):
        return {**clear_rate, "method": "clear-rate-limit", "fallback": True}
    clear_temp = _admin_account_request(
        f"{prefix}/temp-unschedulable",
        token,
        method="DELETE",
        timeout_seconds=timeout_seconds,
    )
    if not clear_temp.get("success"):
        return {**clear_temp, "method": "temp-unschedulable", "fallback": True}
    return {
        "success": True,
        "status_code": int(clear_temp.get("status_code") or 200),
        "method": "clear-rate-limit+temp-unschedulable",
        "fallback": True,
    }


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
        recovery_runner: Callable[..., dict[str, Any]] = execute_sub2api_account_recovery,
        account_reader: Callable[[Any, int], dict[str, Any] | None] = account_ops.fallback_account,
    ) -> None:
        self.settings = settings
        self.db = db
        self.store = OAuthStateStore(settings.usage_query_state_path)
        self.base_url_provider = base_url_provider
        self.inventory_loader = inventory_loader
        self.usage_runner = usage_runner
        self.test_runner = test_runner
        self.recovery_runner = recovery_runner
        self.account_reader = account_reader
        self._accounts: list[dict[str, Any]] = []
        self._inventory_loaded_at: datetime | None = None
        self._run_lock = threading.Lock()
        self._force_condition = threading.Condition()
        self._force_running = False
        self._last_force_report: dict[str, Any] = {}

    def _refresh_inventory(self, now: datetime, *, force: bool = False) -> None:
        if (
            not force
            and self._inventory_loaded_at
            and (now - self._inventory_loaded_at).total_seconds() < INVENTORY_REFRESH_SECONDS
        ):
            return
        self._accounts = list(self.inventory_loader(self.db))
        self._inventory_loaded_at = now
        self.store.reload()

    def _read_account(self, account_id: int) -> dict[str, Any] | None:
        row = self.account_reader(self.db, int(account_id))
        return dict(row) if isinstance(row, dict) else None

    def _known_event(self, key: str, account_id: int, state: dict[str, Any]) -> bool:
        if key in (state.get("pending_events") or {}):
            return True
        metadata = (state.get("scheduler") or {}).get(str(account_id)) or {}
        return key in [str(item) for item in metadata.get("notified_keys") or []]

    def run_once(self, now: datetime | None = None) -> list[dict[str, Any]]:
        if not self._run_lock.acquire(blocking=False):
            return self.store.cached_pending_events()
        try:
            events, _report = self._run_cycle(_utc(now), force=False)
            return events
        finally:
            self._run_lock.release()

    def force_refresh(
        self,
        timeout_seconds: float = 120,
        *,
        now: datetime | None = None,
    ) -> dict[str, Any]:
        timeout = max(0.1, float(timeout_seconds))
        deadline = time.monotonic() + timeout
        with self._force_condition:
            if self._force_running:
                while self._force_running:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        return {
                            "success": False,
                            "timed_out": True,
                            "error": "OAuth 全量刷新超时",
                            "error_code": "force_refresh_timeout",
                        }
                    self._force_condition.wait(remaining)
                report = dict(self._last_force_report)
                report["coalesced"] = True
                return report
            self._force_running = True
        report: dict[str, Any]
        try:
            remaining = max(0.0, deadline - time.monotonic())
            if not self._run_lock.acquire(timeout=remaining):
                report = {
                    "success": False,
                    "timed_out": True,
                    "error": "OAuth 全量刷新等待监控锁超时",
                    "error_code": "force_refresh_timeout",
                }
            else:
                try:
                    _events, report = self._run_cycle(_utc(now), force=True)
                finally:
                    self._run_lock.release()
        except Exception as exc:
            report = {
                "success": False,
                "timed_out": False,
                "refresh_at": _utc(now).isoformat(),
                "error": str(exc),
                "error_code": "force_refresh_failed",
            }
        finally:
            with self._force_condition:
                self._last_force_report = dict(locals().get("report") or {})
                self._force_running = False
                self._force_condition.notify_all()
        return report

    def _run_cycle(self, current: datetime, *, force: bool) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        started = time.monotonic()
        mode = "force" if force else "scheduled"
        try:
            self._refresh_inventory(current, force=force)
        except Exception as exc:
            report = {
                "success": False,
                "refresh_at": current.isoformat(),
                "error": str(exc),
                "error_code": "oauth_inventory_failed",
                "total_count": 0,
                "queried_count": 0,
                "success_count": 0,
                "failure_count": 0,
                "depleted_count": 0,
                "night_deferred_count": 0,
                "recovered_count": 0,
            }
            write_audit(self.settings.audit_path, "oauth_monitor_inventory_failed", report)
            return self.store.cached_pending_events(), report

        state = self.store.cached_snapshot()
        results = {int(key): value for key, value in (state.get("oauth_results") or {}).items()}
        scheduler = {int(key): value for key, value in (state.get("scheduler") or {}).items()}
        if force:
            candidates = [
                {
                    "account_id": int(row.get("id") or 0),
                    "row": row,
                    "reason": "force_refresh",
                    "priority": -1,
                    "exact_fingerprint": str(
                        (_scheduler_row(scheduler, int(row.get("id") or 0)).get("recovery_intent") or {}).get(
                            "fingerprint"
                        )
                    ),
                    "previous_summary": oauth_quota_summary_from_result(
                        row, results.get(int(row.get("id") or 0)) or {}
                    ),
                }
                for row in self._accounts
                if int(row.get("id") or 0) > 0
            ]
        else:
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
        selected = candidates if force else candidates[:batch_size]
        empty_report = {
            "success": True,
            "refresh_at": current.isoformat(),
            "total_count": len(self._accounts),
            "queried_count": 0,
            "success_count": 0,
            "failure_count": 0,
            "depleted_count": 0,
            "night_deferred_count": 0,
            "recovered_count": 0,
            "coalesced": False,
        }
        if not selected:
            return self.store.cached_pending_events(), empty_report

        token = str((state.get("settings") or {}).get("sub2api_admin_token") or "").strip()
        base_url = str(self.base_url_provider() or "").strip().rstrip("/")
        if not token or not base_url:
            report = {
                **empty_report,
                "success": False,
                "error": "缺少 Sub2API 地址或 Admin API Key",
                "error_code": "missing_sub2api_admin_credentials",
                "failure_count": len(selected),
            }
            write_audit(self.settings.audit_path, "oauth_monitor_cycle", {**report, "mode": mode})
            return self.store.cached_pending_events(), report

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
        night_enabled = bool(
            getattr(self.settings, "telegram_oauth_night_recovery_cooldown_enabled", True)
        )
        night = in_beijing_night_cooldown(current, enabled=night_enabled)
        recovery_enabled = bool(getattr(self.settings, "telegram_oauth_recovery_monitor_enabled", True))
        depleted_count = 0
        night_deferred_count = 0

        for account_id, refreshed_result in usage_results.items():
            selected_item = selected_by_id[account_id]
            reason = str(selected_item["reason"])
            metadata = _scheduler_row(scheduler, account_id)
            update: dict[str, Any] = {"last_attempt_at": current.isoformat(), "last_reason": reason}
            if reason in {"bootstrap", "regular_refresh", "exact_reset", "force_refresh", "recovery_intent"}:
                update["last_regular_at"] = current.isoformat()
            if reason == "seven_day_probe":
                update["last_7d_probe_at"] = current.isoformat()
            if reason == "exact_reset":
                update["last_exact_at"] = current.isoformat()
                update["last_exact_fingerprint"] = selected_item.get("exact_fingerprint") or ""

            if not refreshed_result.get("success"):
                error_code = str(refreshed_result.get("error_code") or "")
                update["last_error_code"] = error_code
                update["last_error_at"] = current.isoformat()
                intent = _normal_recovery_intent(metadata.get("recovery_intent"))
                if intent and _auth_error_code(error_code):
                    intent.update(
                        {
                            "status": "auth_failed",
                            "last_error": str(refreshed_result.get("error") or ""),
                            "last_error_code": _auth_error_code(error_code),
                            "next_retry_at": "",
                        }
                    )
                    update["recovery_intent"] = intent
                elif intent:
                    attempts = int(intent.get("attempt_count") or 0) + 1
                    intent.update(
                        {
                            "status": "retry",
                            "attempt_count": attempts,
                            "next_retry_at": _retry_at(current, attempts),
                            "last_error": str(refreshed_result.get("error") or ""),
                            "last_error_code": error_code or "oauth_usage_query_error",
                        }
                    )
                    update["recovery_intent"] = intent
                auth_code = _auth_error_code(error_code)
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
            update["notified_keys"] = [
                str(item)
                for item in metadata.get("notified_keys") or []
                if not str(item).startswith(f"auth:{account_id}:active_usage:")
            ]
            for key in (state.get("pending_events") or {}):
                if str(key).startswith(f"auth:{account_id}:active_usage:"):
                    remove_pending.add(str(key))

            refreshed_summary = oauth_quota_summary_from_result(selected_item["row"], refreshed_result)
            quota_descriptor = _quota_recovery_descriptor(refreshed_summary, current)
            threshold_descriptor = _threshold_recovery_descriptor(
                selected_item["row"], refreshed_summary
            )
            descriptor = quota_descriptor or threshold_descriptor
            prior_intent = _normal_recovery_intent(metadata.get("recovery_intent"))
            transition = oauth_recovery_transition(
                selected_item["previous_summary"], refreshed_summary, now=current
            )
            if descriptor:
                if quota_descriptor:
                    depleted_count += 1
                if descriptor["fingerprint"] != prior_intent.get("fingerprint"):
                    intent = {
                        **descriptor,
                        "status": "pending",
                        "deferred_until": "",
                        "next_retry_at": "",
                        "attempt_count": 0,
                        "block_signature": "",
                        "confirmed_at": "",
                        "tested_at": "",
                        "recovered_at": "",
                        "last_error": "",
                        "last_error_code": "",
                    }
                else:
                    intent = {**prior_intent, **descriptor}
                due_at = parse_iso_datetime(intent.get("due_at"))
                threshold_still_blocked = bool(
                    threshold_descriptor
                    and not _threshold_quota_available(refreshed_summary, threshold_descriptor)
                )
                if due_at is None or (current >= due_at and (quota_descriptor or threshold_still_blocked)):
                    intent.update(
                        {
                            "status": "waiting_quota",
                            "next_retry_at": (current + timedelta(seconds=EXACT_RESET_RETRY_SECONDS)).isoformat(),
                            "last_error": "到点后必要额度窗口仍未释放",
                            "last_error_code": "quota_not_released",
                        }
                    )
                update["recovery_intent"] = intent
                prior_intent = intent
                if due_at is None or current < due_at or quota_descriptor or threshold_still_blocked:
                    scheduler_updates[account_id] = update
                    continue

            if not recovery_enabled or not _quota_all_required_available(refreshed_summary):
                if prior_intent and prior_intent.get("status") not in {"recovered", "auth_failed", "blocked"}:
                    attempts = int(prior_intent.get("attempt_count") or 0) + 1
                    prior_intent.update(
                        {
                            "status": "retry",
                            "attempt_count": attempts,
                            "next_retry_at": _retry_at(current, attempts),
                            "last_error": "新鲜额度响应缺少必要窗口",
                            "last_error_code": "incomplete_quota",
                        }
                    )
                    update["recovery_intent"] = prior_intent
                scheduler_updates[account_id] = update
                continue

            intent = prior_intent
            if transition:
                transition_fingerprint = str(transition.get("fingerprint") or "")
                if transition_fingerprint and transition_fingerprint != intent.get("fingerprint"):
                    intent = {
                        "fingerprint": transition_fingerprint,
                        "due_at": str(transition.get("reset_at") or current.isoformat()),
                        "source": str(transition.get("reset_source") or "server_exact"),
                        "window_keys": list(transition.get("window_keys") or transition.get("depleted_window_keys") or []),
                        "status": "ready",
                        "deferred_until": "",
                        "next_retry_at": "",
                        "attempt_count": 0,
                        "block_signature": "",
                        "confirmed_at": current.isoformat(),
                        "tested_at": "",
                        "recovered_at": "",
                        "last_error": "",
                        "last_error_code": "",
                    }
                    for key in (
                        "early_reset_detected",
                        "old_7d_used_percent",
                        "new_7d_used_percent",
                        "old_reset_at",
                        "detected_at",
                    ):
                        if key in transition:
                            intent[key] = transition[key]
            if not intent or intent.get("status") in {"recovered", "auth_failed", "blocked"}:
                blocked_descriptor = _rate_limit_recovery_descriptor(
                    selected_item["row"], refreshed_summary, current
                )
                if blocked_descriptor and blocked_descriptor["fingerprint"] != intent.get("fingerprint"):
                    intent = {
                        **blocked_descriptor,
                        "status": "ready",
                        "deferred_until": "",
                        "next_retry_at": "",
                        "attempt_count": 0,
                        "block_signature": "",
                        "confirmed_at": current.isoformat(),
                        "tested_at": "",
                        "recovered_at": "",
                        "last_error": "",
                        "last_error_code": "",
                    }
            if not intent or intent.get("status") in {"recovered", "auth_failed", "blocked"}:
                scheduler_updates[account_id] = update
                continue
            intent["confirmed_at"] = current.isoformat()
            if night:
                intent.update(
                    {
                        "status": "deferred",
                        "deferred_until": beijing_cooldown_end(current).isoformat(),
                        "next_retry_at": "",
                    }
                )
                night_deferred_count += 1
                update["recovery_intent"] = intent
            else:
                retry_at = parse_iso_datetime(intent.get("next_retry_at"))
                if retry_at is None or current >= retry_at:
                    intent["status"] = "ready"
                    update["recovery_intent"] = intent
                    test_jobs.append((selected_item, intent))
            scheduler_updates[account_id] = update

        self.store.commit(
            results=successful_results,
            scheduler_updates=scheduler_updates,
            pending_events=pending_updates,
            remove_pending_keys=remove_pending,
        )

        runnable_jobs: list[tuple[dict[str, Any], dict[str, Any], dict[str, Any]]] = []
        pretest_updates: dict[int, dict[str, Any]] = {}
        for item, intent in test_jobs:
            account_id = int(item["account_id"])
            try:
                row = self._read_account(account_id)
            except Exception as exc:
                attempts = int(intent.get("attempt_count") or 0) + 1
                retry = dict(intent)
                retry.update(
                    {
                        "status": "retry",
                        "attempt_count": attempts,
                        "next_retry_at": _retry_at(current, attempts),
                        "last_error": str(exc) or "读取账号状态失败",
                        "last_error_code": "account_read_failed",
                    }
                )
                pretest_updates[account_id] = {"recovery_intent": retry}
                continue
            window_keys = [str(value) for value in intent.get("window_keys") or []]
            if not automatic_recovery_eligible(row, exhausted_window_keys=window_keys, now=current):
                blocked = dict(intent)
                blocked.update(
                    {
                        "status": "blocked",
                        "last_error": "账号状态或阻断原因不符合自动恢复门禁",
                        "last_error_code": "recovery_ineligible",
                        "next_retry_at": "",
                    }
                )
                pretest_updates[account_id] = {"recovery_intent": blocked}
                continue
            signature = recovery_block_signature(row)
            testing = dict(intent)
            testing.update(
                {
                    "status": "testing",
                    "block_signature": signature,
                    "confirmed_at": current.isoformat(),
                    "tested_at": current.isoformat(),
                    "attempt_count": int(intent.get("attempt_count") or 0) + 1,
                    "last_error": "",
                    "last_error_code": "",
                }
            )
            pretest_updates[account_id] = {"recovery_intent": testing}
            runnable_jobs.append((item, testing, row or {}))
        if pretest_updates:
            self.store.commit(scheduler_updates=pretest_updates)

        test_workers = min(
            len(runnable_jobs),
            _positive_int(getattr(self.settings, "telegram_oauth_recovery_test_concurrency", 2), 2, 1, 8),
        )
        test_results: dict[int, dict[str, Any]] = {}
        model_id = str(
            getattr(self.settings, "telegram_oauth_recovery_test_model_id", DEFAULT_TEST_MODEL_ID)
            or DEFAULT_TEST_MODEL_ID
        ).strip()
        if runnable_jobs:
            with ThreadPoolExecutor(max_workers=max(1, test_workers)) as executor:
                futures = {
                    executor.submit(
                        self.test_runner,
                        int(item["account_id"]),
                        model_id,
                        base_url=base_url,
                        admin_token=token,
                        timeout_seconds=30,
                    ): (item, intent, frozen_row)
                    for item, intent, frozen_row in runnable_jobs
                }
                for future in as_completed(futures):
                    item, intent, _frozen_row = futures[future]
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

        final_updates: dict[int, dict[str, Any]] = {}
        recovered_count = 0
        for item, intent, frozen_row in runnable_jobs:
            account_id = int(item["account_id"])
            test_result = test_results[account_id]
            final_intent = dict(intent)
            error_code = str(test_result.get("error_code") or "")
            success = bool(test_result.get("success"))
            recovery_result: dict[str, Any] = {}
            try:
                post_test = self._read_account(account_id)
            except Exception as exc:
                post_test = None
                success = False
                error_code = "account_read_failed"
                recovery_result = {"error": str(exc) or "读取账号状态失败"}
            if not success:
                auth_code = _auth_error_code(error_code)
                final_intent.update(
                    {
                        "status": "auth_failed" if auth_code else "retry",
                        "next_retry_at": "" if auth_code else _retry_at(current, int(intent["attempt_count"])),
                        "last_error": str(test_result.get("error") or "账号测试失败"),
                        "last_error_code": auth_code or error_code or "account_test_error",
                    }
                )
            elif account_recovery_confirmed(post_test):
                success = True
            else:
                window_keys = [str(value) for value in intent.get("window_keys") or []]
                if not automatic_recovery_eligible(
                    post_test, exhausted_window_keys=window_keys, now=current
                ):
                    success = False
                    error_code = "recovery_state_changed"
                    recovery_result = {"error": "账号在测活后发生并发状态变化，已停止自动恢复"}
                elif not recovery_block_change_is_safe(frozen_row, post_test):
                    success = False
                    error_code = "recovery_state_changed"
                    recovery_result = {"error": "账号阻断签名已变化，已停止自动恢复"}
                else:
                    recovery_result = self.recovery_runner(
                        account_id,
                        base_url=base_url,
                        admin_token=token,
                        timeout_seconds=30,
                    )
                    try:
                        recovered_row = self._read_account(account_id)
                    except Exception as exc:
                        recovered_row = None
                        recovery_result = {
                            **recovery_result,
                            "error": str(exc) or "读取账号恢复状态失败",
                            "error_code": "account_read_failed",
                        }
                    success = bool(recovery_result.get("success")) and account_recovery_confirmed(recovered_row)
                    if not success:
                        error_code = str(recovery_result.get("error_code") or "recovery_not_confirmed")
            if success:
                final_intent.update(
                    {
                        "status": "recovered",
                        "recovered_at": current.isoformat(),
                        "next_retry_at": "",
                        "deferred_until": "",
                        "last_error": "",
                        "last_error_code": "",
                    }
                )
                recovered_count += 1
                event_status = "recovered"
                suffix = "success"
            else:
                if final_intent.get("status") != "auth_failed":
                    final_intent.update(
                        {
                            "status": "retry",
                            "next_retry_at": _retry_at(current, int(intent["attempt_count"])),
                            "last_error": str(
                                recovery_result.get("error")
                                or test_result.get("error")
                                or "自动恢复未确认成功"
                            ),
                            "last_error_code": error_code or "recovery_failed",
                        }
                    )
                if final_intent.get("status") == "auth_failed":
                    event_status = "auth_failed"
                else:
                    event_status = "test_failed" if not test_result.get("success") else "recovery_failed"
                suffix = f"failure:{final_intent.get('last_error_code') or 'unknown'}"
            final_updates[account_id] = {"recovery_intent": final_intent}
            key = f"recovery:{account_id}:{intent.get('fingerprint')}:{suffix}"
            if not self._known_event(key, account_id, state):
                pending_updates[key] = {
                    "account_id": account_id,
                    "account_name": item["row"].get("name") or "-",
                    "plan_type": oauth_plan_type(item["row"]),
                    "window_labels": [value.removeprefix("codex_") for value in intent.get("window_keys") or []],
                    "fingerprint": intent.get("fingerprint"),
                    "reset_at": intent.get("due_at"),
                    "reset_source": intent.get("source"),
                    **{
                        field: intent[field]
                        for field in (
                            "early_reset_detected",
                            "old_7d_used_percent",
                            "new_7d_used_percent",
                            "old_reset_at",
                            "detected_at",
                        )
                        if field in intent
                    },
                    "status": event_status,
                    "stage": "account_test" if event_status == "auth_failed" else "recovery",
                    "checked_at": current.isoformat(),
                    "model_id": str(test_result.get("model_id") or model_id),
                    "duration_ms": test_result.get("duration_ms", test_result.get("latency_ms")),
                    "test_success": bool(test_result.get("success")),
                    "error_code": str(final_intent.get("last_error_code") or ""),
                    "error": str(final_intent.get("last_error") or ""),
                    "dedupe_key": key,
                }
        if final_updates or pending_updates:
            self.store.commit(scheduler_updates=final_updates, pending_events=pending_updates)

        duration_ms = int((time.monotonic() - started) * 1000)
        success_count = sum(1 for value in usage_results.values() if value.get("success"))
        report = {
            "success": success_count == len(selected),
            "refresh_at": current.isoformat(),
            "total_count": len(self._accounts),
            "queried_count": len(selected),
            "success_count": success_count,
            "failure_count": len(selected) - success_count,
            "depleted_count": depleted_count,
            "night_deferred_count": night_deferred_count,
            "recovered_count": recovered_count,
            "night_cooldown_active": night,
            "coalesced": False,
            "duration_ms": duration_ms,
        }
        write_audit(
            self.settings.audit_path,
            "oauth_monitor_cycle",
            {
                **report,
                "mode": mode,
                "candidate_count": len(candidates),
                "tested_count": len(runnable_jobs),
                "pending_count": len(pending_updates),
                "active_usage_concurrency": workers,
                "test_concurrency": test_workers,
            },
        )
        return self.store.cached_pending_events(), report
