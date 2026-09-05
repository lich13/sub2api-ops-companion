from __future__ import annotations

import json
import os
import tempfile
import threading
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from . import account_ops
from .audit import write_audit
from .settings import int_value, strict_bool_value
from .usage_query import (
    oauth_quota_from_usage_data,
    oauth_windows_by_key,
    parse_iso_datetime,
    percent_or_none,
    required_oauth_window_keys,
    sanitize_oauth_quota_summary,
)

AVAILABLE = "available"
UNAVAILABLE = "unavailable"
UNKNOWN = "unknown"
EVAL_INTERVAL_SECONDS = 30
DEFAULT_FRESHNESS_SECONDS = 3600
MAX_CONFIG_VERSION = 1_000_000_000
MANUAL_ACCOUNT_ACTIONS = {"pause", "cooldown", "resume"}
SCHEDULABLE_REQUEST_TIMEOUT_SECONDS = 3
DISPATCH_BUDGET_SECONDS = 10


def _strict_positive_int(value: object) -> int | None:
    if isinstance(value, bool) or isinstance(value, float):
        return None
    if isinstance(value, int):
        return value if value > 0 else None
    if isinstance(value, str):
        text = value.strip()
        if not text.isdigit():
            return None
        account_id = int(text)
        if account_id <= 0 or str(account_id) != text:
            return None
        return account_id
    return None


def _is_live_openai_apikey(row: dict[str, Any] | None, account_id: int | None = None) -> bool:
    if not isinstance(row, dict):
        return False
    parsed = _strict_positive_int(row.get("id"))
    if parsed is None:
        return False
    if account_id is not None and parsed != int(account_id):
        return False
    return (
        str(row.get("platform") or "").strip().lower() == "openai"
        and str(row.get("type") or "").strip().lower() == "apikey"
        and row.get("deleted_at") in (None, "")
    )


def fresh_oauth_quota_summary(
    row: dict[str, Any] | None,
    result: dict[str, Any] | None,
) -> dict[str, Any] | None:
    if not isinstance(result, dict):
        return None
    data = result.get("data")
    if isinstance(data, dict) and (
        isinstance(data.get("five_hour"), dict) or isinstance(data.get("seven_day"), dict)
    ):
        return oauth_quota_from_usage_data(
            data,
            row,
            now=parse_iso_datetime(result.get("queried_at")),
        )
    cached = result.get("oauth_quota")
    if isinstance(cached, dict):
        return sanitize_oauth_quota_summary(row, cached)
    return None


class KeyFallbackConfigError(ValueError):
    pass


@dataclass(frozen=True)
class KeyFallbackConfig:
    enabled: bool
    managed_account_ids: tuple[int, ...]
    config_version: int
    updated_at: str
    updated_by: str
    valid: bool


def default_key_fallback_config() -> KeyFallbackConfig:
    return KeyFallbackConfig(False, (), 0, "", "", True)


def deadline_is_future(value: object, now: datetime) -> bool:
    parsed = parse_iso_datetime(value)
    return parsed is not None and parsed > now


def oauth_db_blocked(row: dict[str, Any] | None, now: datetime) -> bool:
    account = row or {}
    if str(account.get("status") or "").strip().lower() != "active":
        return True
    if account.get("schedulable") is not True:
        return True
    if deadline_is_future(account.get("temp_unschedulable_until"), now):
        return True
    if deadline_is_future(account.get("rate_limit_reset_at"), now):
        return True
    if deadline_is_future(account.get("overload_until"), now):
        return True
    auto_pause, _ok = strict_bool_value(account.get("auto_pause_on_expired"), False)
    if auto_pause:
        expires_at = parse_iso_datetime(account.get("expires_at"))
        if expires_at is not None and expires_at <= now:
            return True
    return False


def result_is_fresh(result: dict[str, Any] | None, now: datetime, freshness_seconds: int) -> bool:
    if not isinstance(result, dict):
        return False
    queried_at = parse_iso_datetime(result.get("queried_at"))
    if queried_at is None or queried_at > now:
        return False
    return (now - queried_at).total_seconds() <= max(0, int(freshness_seconds))


def is_auth_error(result: dict[str, Any] | None) -> bool:
    code = str((result or {}).get("error_code") or "").strip().lower()
    return code in {"401", "http_401", "402", "http_402"}


def latest_completed_oauth_result(
    result: dict[str, Any] | None,
    scheduler_row: dict[str, Any] | None,
) -> dict[str, Any] | None:
    stored = result if isinstance(result, dict) else None
    meta = scheduler_row if isinstance(scheduler_row, dict) else {}
    error_code = str(meta.get("last_error_code") or "").strip()
    error_at = parse_iso_datetime(meta.get("last_error_at"))
    success_at = parse_iso_datetime(meta.get("last_success_at"))
    queried_at = parse_iso_datetime((stored or {}).get("queried_at"))
    if success_at is not None and queried_at is not None:
        stored_success_at = max(success_at, queried_at)
    else:
        stored_success_at = success_at or queried_at
    if error_code and error_at is not None and (
        stored_success_at is None or error_at > stored_success_at
    ):
        return {
            "success": False,
            "queried_at": error_at.isoformat(),
            "error_code": error_code,
        }
    return stored


def classify_oauth_account(
    row: dict[str, Any] | None,
    result: dict[str, Any] | None,
    now: datetime,
    freshness_seconds: int,
) -> str:
    if oauth_db_blocked(row, now):
        return UNAVAILABLE
    if not result_is_fresh(result, now, freshness_seconds):
        return UNKNOWN
    if is_auth_error(result):
        return UNAVAILABLE
    if not isinstance(result, dict) or not result.get("success"):
        return UNKNOWN
    summary = fresh_oauth_quota_summary(row, result)
    if summary is None:
        return UNKNOWN
    windows = oauth_windows_by_key(summary.get("ui_windows"))
    has_exhaustion = False
    has_all_available = True
    for key in required_oauth_window_keys(summary.get("plan_type")):
        used = percent_or_none((windows.get(key) or {}).get("used_percent"))
        if used is None:
            has_all_available = False
            continue
        if used >= 100:
            has_exhaustion = True
            has_all_available = False
    if has_all_available:
        return AVAILABLE
    if has_exhaustion:
        return UNAVAILABLE
    return UNKNOWN


def desired_key_schedulable(states: list[str]) -> bool | None:
    if not states:
        return None
    if any(state == AVAILABLE for state in states):
        return False
    if all(state == UNAVAILABLE for state in states):
        return True
    return None


def parse_managed_account_ids(values: list[Any]) -> list[int]:
    parsed: list[int] = []
    seen: set[int] = set()
    invalid = False
    for value in values:
        text = str(value).strip()
        try:
            account_id = int(text)
        except (TypeError, ValueError):
            invalid = True
            continue
        if account_id <= 0:
            invalid = True
            continue
        if account_id not in seen:
            seen.add(account_id)
            parsed.append(account_id)
    if invalid:
        raise KeyFallbackConfigError("Key 回退保存失败：账号 ID 必须是唯一的正整数")
    return parsed


def _schedulable_envelope_matches(body: object, account_id: int, schedulable: bool) -> bool:
    if not isinstance(body, dict) or body.get("code") not in (0, "0"):
        return False
    data = body.get("data")
    if not isinstance(data, dict) or "schedulable" not in data:
        return False
    observed, ok = strict_bool_value(data.get("schedulable"), False)
    if not ok or observed is not bool(schedulable):
        return False
    if "id" in data:
        observed_id = _strict_positive_int(data.get("id"))
        if observed_id != int(account_id):
            return False
    return True


def execute_sub2api_set_schedulable(
    account_id: int,
    schedulable: bool,
    *,
    base_url: str,
    admin_token: str,
    timeout_seconds: int = SCHEDULABLE_REQUEST_TIMEOUT_SECONDS,
    urlopen: Callable[..., Any] = urllib.request.urlopen,
) -> dict[str, Any]:
    base = str(base_url or "").strip().rstrip("/")
    token = str(admin_token or "").strip()
    if not base or not token:
        return {
            "success": False,
            "error_code": "missing_sub2api_admin_credentials",
            "error": "缺少 Sub2API 地址或 Admin API Key",
        }
    target_id = int(account_id)
    requested = bool(schedulable)
    payload = {"schedulable": requested}
    request = urllib.request.Request(
        f"{base}/api/v1/admin/accounts/{target_id}/schedulable",
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Accept": "application/json",
            "Content-Type": "application/json",
            "x-api-key": token,
        },
        method="POST",
    )
    timeout = max(2, min(60, int(timeout_seconds or SCHEDULABLE_REQUEST_TIMEOUT_SECONDS)))
    try:
        with urlopen(request, timeout=timeout) as response:
            status_value = getattr(response, "status", None)
            if status_value is None:
                status_value = response.getcode()
            status = int(status_value)
            raw = response.read().decode("utf-8", errors="replace")
            try:
                body = json.loads(raw) if raw else None
            except json.JSONDecodeError:
                body = None
            if 200 <= status < 300 and _schedulable_envelope_matches(body, target_id, requested):
                data = body.get("data") if isinstance(body, dict) else {}
                return {
                    "success": True,
                    "status_code": status,
                    "data": data if isinstance(data, dict) else {},
                }
            return {
                "success": False,
                "status_code": status,
                "error_code": (
                    "invalid_schedulable_response" if 200 <= status < 300 else f"http_{status}"
                ),
            }
    except urllib.error.HTTPError as exc:
        try:
            exc.read()
        except Exception:
            pass
        return {
            "success": False,
            "status_code": int(exc.code),
            "error_code": f"http_{exc.code}",
        }
    except TimeoutError:
        return {"success": False, "error_code": "timeout"}
    except urllib.error.URLError as exc:
        reason = getattr(exc, "reason", exc)
        code = "timeout" if isinstance(reason, TimeoutError) else "network_error"
        return {"success": False, "error_code": code}
    except Exception:
        return {"success": False, "error_code": "schedulable_request_error"}


def _atomic_write_json(path: Path, payload: dict[str, Any]) -> None:
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


class KeyFallbackController:
    def __init__(
        self,
        settings: Any,
        db: Any,
        *,
        oauth_monitor: Any | None = None,
        base_url_provider: Callable[[], str],
        admin_token_provider: Callable[[], str],
        oauth_inventory: Callable[[Any], list[dict[str, Any]]] = account_ops.current_oauth_accounts,
        key_inventory: Callable[[Any], list[dict[str, Any]]] = account_ops.live_openai_apikey_accounts,
        account_reader: Callable[[Any, int], dict[str, Any] | None] = account_ops.fallback_account,
        schedulable_runner: Callable[..., dict[str, Any]] = execute_sub2api_set_schedulable,
        monotonic: Callable[[], float] = time.monotonic,
    ) -> None:
        self.settings = settings
        self.db = db
        self.oauth_monitor = oauth_monitor
        self.base_url_provider = base_url_provider
        self.admin_token_provider = admin_token_provider
        self.oauth_inventory = oauth_inventory
        self.key_inventory = key_inventory
        self.account_reader = account_reader
        self.schedulable_runner = schedulable_runner
        self._monotonic = monotonic
        self._lock = threading.RLock()
        self._dispatch_resume_id: int | None = None

    def load_config(self) -> KeyFallbackConfig:
        with self._lock:
            return self._read_config_unlocked()

    def panel_snapshot(self) -> dict[str, Any]:
        config = self.load_config()
        return {
            "enabled": bool(config.enabled and config.valid),
            "managed_account_ids": list(config.managed_account_ids),
            "config_valid": config.valid,
            "config_updated_at": config.updated_at or None,
        }

    def save_user_config(
        self,
        *,
        enabled: bool,
        managed_account_ids: list[Any],
        user: str,
    ) -> KeyFallbackConfig:
        ids = parse_managed_account_ids(list(managed_account_ids or []))
        with self._lock:
            live_ids: set[int] = set()
            for row in self.key_inventory(self.db):
                if not _is_live_openai_apikey(row):
                    continue
                parsed = _strict_positive_int(row.get("id"))
                if parsed is not None:
                    live_ids.add(parsed)
            stale = [account_id for account_id in ids if account_id not in live_ids]
            if stale:
                listed = "、".join(f"#{account_id}" for account_id in stale)
                raise KeyFallbackConfigError(
                    f"Key 回退保存失败：账号 {listed} 不是有效的 OpenAI apikey"
                )
            current = self._read_config_unlocked()
            written = self._write_config_unlocked(
                enabled=bool(enabled),
                managed_account_ids=ids,
                config_version=int(current.config_version) + 1,
                updated_by=str(user or ""),
            )
        write_audit(
            self.settings.audit_path,
            "key_fallback_config_update",
            {
                "user": str(user or ""),
                "enabled": written.enabled,
                "managed_account_ids": list(written.managed_account_ids),
                "config_version": written.config_version,
            },
        )
        return written

    def release_managed_account(self, account_id: int) -> None:
        target_id = _strict_positive_int(account_id)
        if target_id is None:
            return
        with self._lock:
            remaining = self._unmanage_unlocked(target_id)
        if remaining is not None:
            write_audit(
                self.settings.audit_path,
                "key_fallback_unmanage",
                {"account_id": target_id, "remaining_managed_account_ids": remaining},
            )

    def run_manual_account_action(
        self,
        account_id: int,
        action: str,
        *,
        actor_name: str,
        minutes: int | None = None,
        reason: str | None = None,
    ) -> dict[str, Any] | None:
        if action not in MANUAL_ACCOUNT_ACTIONS:
            raise KeyFallbackConfigError("Key 回退账号操作无效")
        target_id = _strict_positive_int(account_id)
        if target_id is None:
            return None
        actor = str(actor_name or "")
        with self._lock:
            remaining = self._unmanage_unlocked(target_id)
            if action == "pause":
                row = account_ops.pause_account(
                    self.db,
                    self.settings.audit_path,
                    target_id,
                    actor,
                    reason or f"telegram pause by {actor}",
                )
            elif action == "resume":
                row = account_ops.resume_account(
                    self.db,
                    self.settings.audit_path,
                    target_id,
                    actor,
                    reason or "resume account from ops companion",
                )
            else:
                row = account_ops.cooldown_account(
                    self.db,
                    self.settings.audit_path,
                    target_id,
                    actor,
                    int(minutes or 15),
                    reason or f"telegram cooldown {int(minutes or 15)}m by {actor}",
                )
        if remaining is not None:
            write_audit(
                self.settings.audit_path,
                "key_fallback_unmanage",
                {"account_id": target_id, "remaining_managed_account_ids": remaining},
            )
        return row

    def _unmanage_unlocked(self, account_id: int) -> list[int] | None:
        current = self._read_config_unlocked()
        if not current.valid or account_id not in current.managed_account_ids:
            return None
        remaining = [item for item in current.managed_account_ids if item != account_id]
        self._write_config_unlocked(
            enabled=current.enabled,
            managed_account_ids=remaining,
            config_version=int(current.config_version) + 1,
            updated_by=f"telegram-unmanage:{account_id}",
        )
        return remaining

    def run_once(self, now: datetime | None = None) -> dict[str, Any]:
        current = now or datetime.now(timezone.utc)
        if current.tzinfo is None:
            current = current.replace(tzinfo=timezone.utc)
        else:
            current = current.astimezone(timezone.utc)
        with self._lock:
            return self._evaluate_unlocked(current)

    def _evaluate_unlocked(self, now: datetime) -> dict[str, Any]:
        config = self._read_config_unlocked()
        report: dict[str, Any] = {
            "skipped": False,
            "desired": None,
            "changed_ids": [],
            "failed_ids": [],
            "reason": "",
        }
        if not config.valid:
            report["skipped"] = True
            report["reason"] = "invalid_config"
            return report
        if not config.enabled or not config.managed_account_ids:
            report["skipped"] = True
            report["reason"] = "disabled_or_empty"
            return report
        monitor = self.oauth_monitor
        guard_factory = getattr(monitor, "evaluation_guard", None) if monitor is not None else None
        if monitor is None or not callable(guard_factory):
            report["skipped"] = True
            report["reason"] = "monitor_unavailable"
            return report
        with guard_factory() as session:
            if session is None:
                report["skipped"] = True
                report["reason"] = "refresh_in_progress"
                return report
            snapshot = session.committed_snapshot()
            if not isinstance(snapshot, dict):
                report["skipped"] = True
                report["reason"] = "refresh_in_progress"
                return report
            return self._observe_and_apply_unlocked(report, config, snapshot, now)

    def _observe_and_apply_unlocked(
        self,
        report: dict[str, Any],
        config: KeyFallbackConfig,
        snapshot: dict[str, Any],
        now: datetime,
    ) -> dict[str, Any]:
        try:
            oauth_rows = [dict(row) for row in self.oauth_inventory(self.db) if isinstance(row, dict)]
        except Exception:
            write_audit(
                self.settings.audit_path,
                "key_fallback_inventory_failed",
                {"error_code": "inventory_failed"},
            )
            report["skipped"] = True
            report["reason"] = "inventory_failed"
            return report

        oauth_rows = [
            row
            for row in oauth_rows
            if _strict_positive_int(row.get("id")) is not None
            and str(row.get("platform") or "").strip().lower() == "openai"
            and str(row.get("type") or "").strip().lower() == "oauth"
            and row.get("deleted_at") in (None, "")
        ]
        results = _oauth_results_from_snapshot(snapshot)
        scheduler_rows = _scheduler_from_snapshot(snapshot)
        freshness = int_value(
            getattr(self.settings, "telegram_oauth_regular_refresh_interval_seconds", DEFAULT_FRESHNESS_SECONDS),
            DEFAULT_FRESHNESS_SECONDS,
            60,
            86400,
        )
        states: list[str] = []
        oauth_states: dict[int, str] = {}
        for row in oauth_rows:
            account_id = _strict_positive_int(row.get("id"))
            if account_id is None:
                continue
            state = classify_oauth_account(
                row,
                latest_completed_oauth_result(results.get(account_id), scheduler_rows.get(account_id)),
                now,
                freshness,
            )
            states.append(state)
            oauth_states[account_id] = state
        desired = desired_key_schedulable(states)
        report["desired"] = desired
        report["oauth_states"] = oauth_states
        if desired is None:
            report["skipped"] = True
            report["reason"] = "keep_existing"
            return report

        version = config.config_version
        managed = set(config.managed_account_ids)
        ordered = _fair_dispatch_order(list(config.managed_account_ids), self._dispatch_resume_id)
        changed: list[int] = []
        failed: list[int] = []
        exhausted = False
        deadline = self._monotonic() + DISPATCH_BUDGET_SECONDS
        for account_id in ordered:
            latest = self._read_config_unlocked()
            if not latest.valid or latest.config_version != version:
                report["skipped"] = True
                report["reason"] = "config_changed"
                break
            if not latest.enabled or account_id not in latest.managed_account_ids:
                continue
            if account_id not in managed:
                continue
            try:
                live = self.account_reader(self.db, account_id)
            except Exception:
                continue
            if not _is_live_openai_apikey(live, account_id):
                continue
            current_schedulable = (live or {}).get("schedulable") is True
            if current_schedulable is desired:
                continue
            if self._monotonic() >= deadline:
                self._dispatch_resume_id = account_id
                exhausted = True
                report["reason"] = "dispatch_budget_exhausted"
                break
            result = self._set_schedulable(account_id, desired)
            if result.get("success"):
                changed.append(account_id)
                write_audit(
                    self.settings.audit_path,
                    "key_fallback_set_schedulable",
                    {
                        "account_id": account_id,
                        "schedulable": desired,
                        "success": True,
                    },
                )
            else:
                failed.append(account_id)
                write_audit(
                    self.settings.audit_path,
                    "key_fallback_set_schedulable",
                    {
                        "account_id": account_id,
                        "schedulable": desired,
                        "success": False,
                        "error_code": str(result.get("error_code") or "schedulable_request_error"),
                    },
                )
        else:
            if not exhausted:
                self._dispatch_resume_id = None
        report["changed_ids"] = changed
        report["failed_ids"] = failed
        return report

    def _set_schedulable(self, account_id: int, schedulable: bool) -> dict[str, Any]:
        token = str(self.admin_token_provider() or "").strip()
        base_url = str(self.base_url_provider() or "").strip().rstrip("/")
        if not token or not base_url:
            return {
                "success": False,
                "error_code": "missing_sub2api_admin_credentials",
            }
        return self.schedulable_runner(
            int(account_id),
            bool(schedulable),
            base_url=base_url,
            admin_token=token,
            timeout_seconds=SCHEDULABLE_REQUEST_TIMEOUT_SECONDS,
        )

    def _read_config_unlocked(self) -> KeyFallbackConfig:
        path = Path(str(getattr(self.settings, "key_fallback_config_path", "") or ""))
        try:
            raw = path.read_text(encoding="utf-8")
        except FileNotFoundError:
            return default_key_fallback_config()
        except (OSError, UnicodeError):
            return KeyFallbackConfig(False, (), 0, "", "", False)
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            return KeyFallbackConfig(False, (), 0, "", "", False)
        if not isinstance(data, dict):
            return KeyFallbackConfig(False, (), 0, "", "", False)
        if "enabled" in data:
            enabled, enabled_ok = strict_bool_value(data.get("enabled"), False)
            if not enabled_ok:
                return KeyFallbackConfig(False, (), 0, "", "", False)
        else:
            enabled = False
        ids_raw = data.get("managed_account_ids", [])
        if "managed_account_ids" in data and not isinstance(ids_raw, list):
            return KeyFallbackConfig(False, (), 0, "", "", False)
        ids: list[int] = []
        for item in ids_raw:
            account_id = _strict_positive_int(item)
            if account_id is None:
                return KeyFallbackConfig(False, (), 0, "", "", False)
            if account_id not in ids:
                ids.append(account_id)
        version = int_value(data.get("config_version"), 0, 0, MAX_CONFIG_VERSION)
        return KeyFallbackConfig(
            enabled=bool(enabled),
            managed_account_ids=tuple(ids),
            config_version=version,
            updated_at=str(data.get("updated_at") or ""),
            updated_by=str(data.get("updated_by") or ""),
            valid=True,
        )

    def _write_config_unlocked(
        self,
        *,
        enabled: bool,
        managed_account_ids: list[int],
        config_version: int,
        updated_by: str,
    ) -> KeyFallbackConfig:
        updated_at = datetime.now(timezone.utc).isoformat()
        payload = {
            "enabled": bool(enabled),
            "managed_account_ids": [int(item) for item in managed_account_ids],
            "config_version": int(config_version),
            "updated_at": updated_at,
            "updated_by": str(updated_by or ""),
        }
        try:
            _atomic_write_json(Path(self.settings.key_fallback_config_path), payload)
        except OSError as exc:
            raise KeyFallbackConfigError("Key 回退配置保存失败") from exc
        return KeyFallbackConfig(
            enabled=bool(enabled),
            managed_account_ids=tuple(int(item) for item in managed_account_ids),
            config_version=int(config_version),
            updated_at=updated_at,
            updated_by=str(updated_by or ""),
            valid=True,
        )


def _snapshot_int_map(snapshot: dict[str, Any], field: str) -> dict[int, dict[str, Any]]:
    raw = snapshot.get(field) if isinstance(snapshot, dict) else {}
    if not isinstance(raw, dict):
        return {}
    mapped: dict[int, dict[str, Any]] = {}
    for key, value in raw.items():
        account_id = _strict_positive_int(key)
        if account_id is not None and isinstance(value, dict):
            mapped[account_id] = value
    return mapped


def _oauth_results_from_snapshot(snapshot: dict[str, Any]) -> dict[int, dict[str, Any]]:
    return _snapshot_int_map(snapshot, "oauth_results")


def _scheduler_from_snapshot(snapshot: dict[str, Any]) -> dict[int, dict[str, Any]]:
    return _snapshot_int_map(snapshot, "scheduler")


def _fair_dispatch_order(account_ids: list[int], resume_id: int | None) -> list[int]:
    if not account_ids:
        return []
    if resume_id in account_ids:
        start = account_ids.index(resume_id)
        return account_ids[start:] + account_ids[:start]
    return list(account_ids)
