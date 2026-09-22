from __future__ import annotations

import threading
from datetime import datetime, timedelta, timezone
from typing import Any
from zoneinfo import ZoneInfo

from .settings import daily_test_time
from .usage_query import parse_iso_datetime

BEIJING_TZ = ZoneInfo("Asia/Shanghai")


def next_daily_time(now: datetime, value: str) -> datetime:
    hour, minute = map(int, daily_test_time(value).split(":"))
    local = now.astimezone(BEIJING_TZ)
    target = local.replace(hour=hour, minute=minute, second=0, microsecond=0)
    if target <= local:
        target += timedelta(days=1)
    return target.astimezone(timezone.utc)


def daily_account_eligible(row: Any, now: datetime) -> bool:
    if not isinstance(row, dict) or row.get("deleted_at") not in (None, ""):
        return False
    if str(row.get("platform") or "").lower() != "openai" or str(row.get("type") or "").lower() != "oauth":
        return False
    if row.get("status") != "active" or row.get("schedulable") is not True:
        return False
    for field in ("temp_unschedulable_until", "rate_limit_reset_at", "overload_until"):
        value = row.get(field)
        parsed = parse_iso_datetime(value)
        if value not in (None, "") and (parsed is None or parsed > now):
            return False
    reason = str(row.get("temp_unschedulable_reason") or "").lower()
    if any(word in reason for word in ("manual", "telegram", "人工", "手动")):
        return False
    if row.get("rate_limited_at") and not parse_iso_datetime(row.get("rate_limit_reset_at")):
        return False
    expires = parse_iso_datetime(row.get("expires_at"))
    if row.get("auto_pause_on_expired") and row.get("expires_at") and (expires is None or expires <= now):
        return False
    return True


class DailyTestSchedule:
    """Persist due work before acquiring the monitor lock; never replay missed work."""

    def __init__(self, settings: Any, store: Any, now: datetime) -> None:
        self.settings, self.store = settings, store
        self._lock = threading.RLock()
        with self._lock:
            state = self._state()
            for batch in state.get("batches", {}).values():
                if batch.get("status") in {"queued", "running"}:
                    batch.update(status="interrupted", completed_at=now.isoformat())
            self._configure(state, now)
            self.store.commit(daily_test=state)

    def _state(self) -> dict[str, Any]:
        return self.store.cached_snapshot().get("daily_test") or {"batches": {}}

    def _config(self) -> tuple[bool, str]:
        return (bool(getattr(self.settings, "telegram_oauth_daily_test_enabled", True)),
                daily_test_time(getattr(self.settings, "telegram_oauth_daily_test_time", "05:00")))

    @property
    def enabled(self) -> bool:
        return self._config()[0]

    def _configure(self, state: dict[str, Any], now: datetime) -> None:
        enabled, value = self._config()
        state.update(enabled=enabled, time=value,
                     next_run_at=next_daily_time(now, value).isoformat() if enabled else "")

    def tick(self, now: datetime) -> None:
        with self._lock:
            state = self._state()
            enabled, value = self._config()
            if (state.get("enabled"), state.get("time")) != (enabled, value):
                # Already registered due work remains queued. New settings only affect future times.
                self._configure(state, now)
                self.store.commit(daily_test=state)
                return
            target = parse_iso_datetime(state.get("next_run_at"))
            if not enabled or target is None or now < target:
                return
            date = target.astimezone(BEIJING_TZ).date().isoformat()
            batches = state.setdefault("batches", {})
            if date not in batches or (batches[date].get("status") == "missed"
                                       and batches[date].get("scheduled_at") != target.isoformat()):
                # A tick in the configured minute counts as on time; a later tick is missed.
                on_time = now < target + timedelta(minutes=1)
                batches[date] = {"date": date, "scheduled_at": target.isoformat(),
                                 "status": "queued" if on_time else "missed", "accounts": {}}
            state["next_run_at"] = next_daily_time(now, value).isoformat()
            state["batches"] = dict(sorted(batches.items())[-60:])
            self.store.commit(daily_test=state)

    def claim(self, now: datetime) -> dict[str, Any] | None:
        with self._lock:
            state = self._state()
            if not self._config()[0]:
                for batch in state.get("batches", {}).values():
                    if batch.get("status") == "queued":
                        batch.update(status="cancelled", completed_at=now.isoformat())
                self.store.commit(daily_test=state)
                return None
            for batch in state.get("batches", {}).values():
                if batch.get("status") == "queued":
                    batch.update(status="running", started_at=now.isoformat())
                    self.store.commit(daily_test=state)
                    return batch
            return None

    def save_batch(self, batch: dict[str, Any], pending_events: dict[str, Any] | None = None) -> None:
        with self._lock:
            state = self._state()
            state.setdefault("batches", {})[batch["date"]] = batch
            self.store.commit(daily_test=state, pending_events=pending_events)
