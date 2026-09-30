"""The single admission point for OpenAI OAuth active usage requests."""
from __future__ import annotations

import hashlib
import json
import threading
import time
from concurrent.futures import Future
from datetime import datetime, timedelta
from typing import Any, Callable

from .audit import write_audit
from .usage_query import (
    oauth_quota_summary_from_result, oauth_windows_by_key, parse_iso_datetime,
    percent_or_none, required_oauth_window_keys,
)

MIN_QUERY_SECONDS = 3600
AUTO_WINDOW_SECONDS = 86400
AUTO_QUERY_LIMIT = 6
RESET_GRACE_SECONDS = 60
QUERY_BACKOFF_SECONDS = (3600, 10800, 21600, 43200)
AUTH_ERRORS = {"401", "402", "http_401", "http_402"}


def credential_fingerprint(row: dict[str, Any]) -> str:
    credentials = row.get("credentials") or {}
    auth = {key: credentials.get(key) for key in ("access_token", "refresh_token", "id_token")}
    return hashlib.sha256(json.dumps(auth, sort_keys=True, default=str).encode()).hexdigest()


def query_metadata(metadata: dict[str, Any], saved: dict[str, Any] | None) -> dict[str, Any]:
    if "quota_query" in metadata and not isinstance(metadata["quota_query"], dict):
        raise ValueError("OAuth 查询状态无效")
    value = dict(metadata.get("quota_query") or {})
    attempts = value.get("automatic_attempts", [])
    if not isinstance(attempts, list) or any(parse_iso_datetime(t) is None for t in attempts):
        raise ValueError("OAuth 查询预算无效")
    failures = value.get("failure_count", 0)
    if isinstance(failures, bool) or not isinstance(failures, int) or failures < 0:
        raise ValueError("OAuth 查询退避状态无效")
    for key in ("last_query_at", "retry_at"):
        if value.get(key) not in (None, "") and parse_iso_datetime(value[key]) is None:
            raise ValueError("OAuth 查询时间无效")
    if not value.get("last_query_at"):
        times = [parse_iso_datetime(t) for t in (
            metadata.get("last_attempt_at"), metadata.get("last_success_at"),
            (saved or {}).get("queried_at") if (saved or {}).get("source") != "passive" else None,
        )]
        value["last_query_at"] = max((t for t in times if t), default=None)
        if value["last_query_at"]:
            value["last_query_at"] = value["last_query_at"].isoformat()
    return value


def quota_complete(row: dict[str, Any], result: dict[str, Any] | None) -> bool:
    if not result or not result.get("success"):
        return False
    summary = oauth_quota_summary_from_result(row, result)
    windows = oauth_windows_by_key(summary.get("ui_windows"))
    return all(percent_or_none(windows.get(key, {}).get("used_percent")) is not None
               for key in required_oauth_window_keys(summary.get("plan_type")))


def fresh_quota(row: dict[str, Any], result: dict[str, Any] | None, now: datetime) -> bool:
    observed = parse_iso_datetime((result or {}).get("queried_at"))
    if not observed or not 0 <= (now - observed).total_seconds() <= MIN_QUERY_SECONDS:
        return False
    if not quota_complete(row, result):
        return False
    summary = oauth_quota_summary_from_result(row, result)
    windows = oauth_windows_by_key(summary.get("ui_windows"))
    return all(not (deadline := parse_iso_datetime(windows.get(key, {}).get("reset_at")))
               or deadline > now for key in required_oauth_window_keys(summary.get("plan_type")))


def automatic_eligible(row: dict[str, Any], now: datetime) -> bool:
    if (row.get("platform") != "openai" or row.get("type") != "oauth"
            or row.get("deleted_at") not in (None, "") or row.get("status") != "active"
            or row.get("schedulable") is not True):
        return False
    reason = str(row.get("temp_unschedulable_reason") or "")
    try:
        threshold = json.loads(reason) if reason else {}
    except (ValueError, TypeError):
        threshold = {}
    if reason and (not isinstance(threshold, dict)
                   or threshold.get("source") != "account_scheduling_threshold"
                   or threshold.get("platform") != "openai"):
        return False
    if row.get("temp_unschedulable_until") and not reason:
        return False
    expires = parse_iso_datetime(row.get("expires_at"))
    return not (row.get("auto_pause_on_expired") and row.get("expires_at")
                and (not expires or expires <= now))


def reset_not_before(row: dict[str, Any], result: dict[str, Any] | None) -> datetime | None:
    summary = oauth_quota_summary_from_result(row, result)
    windows = oauth_windows_by_key(summary.get("ui_windows"))
    deadlines = [parse_iso_datetime(row.get(key)) for key in
                 ("rate_limit_reset_at", "temp_unschedulable_until", "overload_until")]
    for key in required_oauth_window_keys(summary.get("plan_type")):
        window = windows.get(key, {})
        used = percent_or_none(window.get("used_percent"))
        if used is not None and used >= 100:
            deadlines.append(parse_iso_datetime(window.get("reset_at")))
    latest = max((t for t in deadlines if t), default=None)
    return latest + timedelta(seconds=RESET_GRACE_SECONDS) if latest else None


def automatic_gate(row: dict[str, Any], saved: dict[str, Any] | None,
                   metadata: dict[str, Any], now: datetime, *,
                   wait_for_reset: bool = True) -> tuple[str, datetime | None]:
    if not automatic_eligible(row, now):
        return "account_ineligible", None
    query = query_metadata(metadata, saved)
    auth = query.get("auth_fingerprint")
    if auth and auth == credential_fingerprint(row):
        return "auth_paused", None
    if not auth and metadata.get("last_error_code") in AUTH_ERRORS:
        return "auth_paused", None
    bounds: list[tuple[datetime, str]] = []
    last = parse_iso_datetime(query.get("last_query_at"))
    if last:
        bounds.append((last + timedelta(seconds=MIN_QUERY_SECONDS), "query_cooldown"))
    for value, reason in ((query.get("retry_at"), "query_backoff"),
                          (reset_not_before(row, saved) if wait_for_reset else None, "waiting_reset")):
        deadline = parse_iso_datetime(value)
        if deadline:
            bounds.append((deadline, reason))
    attempts = sorted(t for value in query.get("automatic_attempts", [])
                      if (t := parse_iso_datetime(value)) is not None
                      and t > now - timedelta(seconds=AUTO_WINDOW_SECONDS))
    if len(attempts) >= AUTO_QUERY_LIMIT:
        bounds.append((attempts[-AUTO_QUERY_LIMIT] + timedelta(seconds=AUTO_WINDOW_SECONDS), "query_budget"))
    deadline, reason = max(bounds, default=(now, ""))
    return (reason, deadline) if deadline > now else ("", None)


class OAuthQueryCoordinator:
    def __init__(self, store: Any, runner: Callable[..., dict[str, Any]],
                 base_url: Callable[[], str], clock: Callable[[], datetime], audit_path: str,
                 account_reader: Callable[[int], dict[str, Any] | None] | None = None) -> None:
        self.store, self.runner, self.base_url, self.clock = store, runner, base_url, clock
        self.audit_path = audit_path
        self.account_reader = account_reader
        self._lock = threading.Lock()
        self._inflight: dict[int, Future] = {}
        self._completed: dict[int, tuple[float, str, dict[str, Any]]] = {}
        self._deferred: dict[int, tuple[str, str | None]] = {}

    def external_read(self, row: dict[str, Any], *, source: str, reason: str,
                      operation: Callable[[], dict[str, Any]],
                      validate: Callable[[dict[str, Any]], bool],
                      now: datetime | None = None) -> dict[str, Any]:
        """Account for quota reads bundled into credit refresh/consumption.

        The caller holds the monitor operation lock. Unlike a pure read, a
        consumption operation must never join/replay another operation's result.
        A future reservation survives an interrupted/uncertain HTTP operation.
        """
        if source not in {"automatic", "manual"}:
            raise ValueError("Invalid quota query source")
        account_id = int(row["id"])
        current = now or self.clock()
        reservation = current + timedelta(seconds=180)
        with self._lock:
            if account_id in self._inflight:
                return {"success": False, "skipped": True, "error_code": "query_busy"}
            future: Future = Future()
            self._inflight[account_id] = future
        admitted = False
        result: dict[str, Any] = {}
        try:
            def reserve(data: dict[str, Any]) -> dict[str, Any]:
                if not validate(data):
                    return {"success": False, "skipped": True, "error_code": "reset_state_changed"}
                meta = data["scheduler"].setdefault(str(account_id), {})
                saved = data["oauth_results"].get(str(account_id))
                query = query_metadata(meta, saved)
                if source == "automatic":
                    # Only the validated credit workflow may operate before the
                    # natural reset, or while it owns a temporary scheduling hold.
                    gate_row = {**row, "schedulable": True}
                    blocked, deadline = automatic_gate(gate_row, saved, meta, current, wait_for_reset=False)
                    if blocked:
                        return {"success": False, "skipped": True, "error_code": blocked,
                                "next_query_at": deadline.isoformat() if deadline else None}
                    query["automatic_attempts"] = [value for value in query.get("automatic_attempts", [])
                        if (t := parse_iso_datetime(value)) and t > current - timedelta(seconds=AUTO_WINDOW_SECONDS)] + [reservation.isoformat()]
                query.update(last_query_at=reservation.isoformat(), last_source=source, last_reason=reason)
                meta["quota_query"] = query
                meta["last_attempt_at"] = current.isoformat()
                return {"admitted": True}
            decision = self.store.transaction(reserve)
            if not decision.get("admitted"):
                result = decision
            else:
                admitted = True
                try:
                    result = dict(operation())
                except Exception:
                    result = {"success": False, "error_code": "result_uncertain"}
                completed = max(current, self.clock())
                # A server may continue post-processing after a client timeout.
                recorded = max(completed, reservation) if result.get("uncertain") or result.get("error_code") in {"timeout", "result_uncertain"} else completed
                def finish(data: dict[str, Any]) -> None:
                    meta = data["scheduler"].setdefault(str(account_id), {})
                    query = query_metadata(meta, data["oauth_results"].get(str(account_id)))
                    if source == "automatic":
                        query["automatic_attempts"] = [recorded.isoformat() if t == reservation.isoformat() else t
                                                       for t in query.get("automatic_attempts", [])]
                    query["last_query_at"] = max(recorded, parse_iso_datetime(query.get("last_query_at"))
                                                 or recorded if query.get("last_query_at") != reservation.isoformat() else recorded).isoformat()
                    quota = result.get("quota_result")
                    if isinstance(quota, dict) and quota_complete(row, quota):
                        data["oauth_results"][str(account_id)] = dict(quota)
                        meta.update(last_success_at=completed.isoformat(), last_error_code="")
                        query.pop("auth_fingerprint", None)
                        if result.get("success"):
                            query.update(failure_count=0, retry_at=None)
                    code = str(result.get("error_code") or "")
                    if code in AUTH_ERRORS:
                        query["auth_fingerprint"] = credential_fingerprint(row)
                        meta.update(last_error_at=completed.isoformat(), last_error_code=code)
                    if not result.get("success"):
                        if not result.get("skipped"):
                            meta.update(last_error_at=completed.isoformat(), last_error_code=code)
                        failures = int(query.get("failure_count") or 0) + 1
                        query.update(failure_count=failures, retry_at=(recorded + timedelta(
                            seconds=QUERY_BACKOFF_SECONDS[min(failures - 1, 3)])).isoformat())
                    meta["quota_query"] = query
                self.store.transaction(finish)
        except (OSError, ValueError, TypeError):
            result = {**result, "success": False, "skipped": not admitted,
                      "error_code": "query_state_unavailable", "uncertain": admitted}
        finally:
            with self._lock:
                quota = result.get("quota_result")
                shared = quota if isinstance(quota, dict) else {"success": False, "error_code": "incomplete_quota"}
                if admitted:
                    self._completed[account_id] = (time.monotonic(), credential_fingerprint(row), shared)
                future.set_result(shared)
                self._inflight.pop(account_id, None)
            write_audit(self.audit_path, "oauth_usage_query", {"account_id": account_id, "source": source,
                "reason": reason, "requested": admitted, "success": bool(result.get("success")),
                "error_code": result.get("error_code", ""), "next_query_at": result.get("next_query_at")})
        return result

    def observe_gates(self, rows: list[dict[str, Any]], results: dict[int, dict[str, Any]],
                      scheduler: dict[int, dict[str, Any]], now: datetime) -> None:
        from .quota_snapshot import latest_openai_result
        for row in rows:
            account_id = int(row["id"])
            blocked, deadline = automatic_gate(row, latest_openai_result(row, results.get(account_id), now),
                                               scheduler.get(account_id, {}), now)
            state = (blocked, deadline.isoformat() if deadline else None)
            if blocked and self._deferred.get(account_id) != state:
                write_audit(self.audit_path, "oauth_usage_deferred", {
                    "account_id": account_id, "source": "automatic", "reason": blocked,
                    "next_query_at": state[1],
                })
            self._deferred[account_id] = state

    def join_existing(self, account_id: int, requested_at: float) -> dict[str, Any] | None:
        with self._lock:
            pending = self._inflight.get(account_id)
            previous = self._completed.get(account_id)
        if pending:
            result = pending.result(timeout=40)
        elif previous and previous[0] >= requested_at:
            result = previous[2]
        else:
            return None
        return {**result, "coalesced": True} if not result.get("skipped") else None

    def query(self, row: dict[str, Any], token: str, *, source: str, reason: str,
              now: datetime | None = None, requested_at: float | None = None,
              timeout_seconds: int = 10) -> dict[str, Any]:
        if source not in {"automatic", "manual"}:
            raise ValueError("Invalid quota query source")
        account_id = int(row["id"])
        if self.account_reader is not None:
            try:
                live = self.account_reader(account_id)
            except Exception:
                return {"success": False, "skipped": True, "error_code": "account_read_failed"}
            if not live or (live.get("platform"), live.get("type")) != ("openai", "oauth"):
                return {"success": False, "skipped": True, "error_code": "account_changed"}
            row = live
        fingerprint = credential_fingerprint(row)
        with self._lock:
            future = self._inflight.get(account_id)
            previous = self._completed.get(account_id)
            if future is None and requested_at is not None and previous and previous[0] >= requested_at and previous[1] == fingerprint:
                return {**previous[2], "coalesced": True}
            owner = future is None
            if owner:
                future = Future()
                self._inflight[account_id] = future
        if not owner:
            return {**future.result(timeout=max(40, timeout_seconds + 5)), "coalesced": True}
        current = now or self.clock()
        result: dict[str, Any] = {}
        admitted = False
        try:
            def reserve(data: dict[str, Any]) -> dict[str, Any]:
                metadata = data["scheduler"].setdefault(str(account_id), {})
                saved = data["oauth_results"].get(str(account_id))
                from .quota_snapshot import latest_openai_result
                evidence = latest_openai_result(row, saved, current)
                query = query_metadata(metadata, saved)
                if source == "automatic":
                    blocked, deadline = automatic_gate(row, evidence, metadata, current)
                    if blocked:
                        return {"success": False, "skipped": True, "error_code": blocked,
                                "next_query_at": deadline.isoformat() if deadline else None}
                    attempts = [value for value in query.get("automatic_attempts", [])
                                if (t := parse_iso_datetime(value)) is not None
                                and t > current - timedelta(seconds=AUTO_WINDOW_SECONDS)]
                    query["automatic_attempts"] = [*attempts, current.isoformat()]
                query.update(last_query_at=current.isoformat(), last_source=source, last_reason=reason)
                metadata["quota_query"] = query
                metadata["last_attempt_at"] = current.isoformat()
                return {"admitted": True}
            admission = self.store.transaction(reserve)
            if not admission.get("admitted"):
                result = admission
            else:
                admitted = True
                try:
                    result = self.runner(account_id, self.base_url(), token, account_row=row,
                                         timeout_seconds=timeout_seconds, now=current)
                except Exception:
                    result = {"success": False, "error_code": "oauth_usage_query_error",
                              "error": "额度查询失败", "queried_at": current.isoformat()}
                result = dict(result)
                if result.get("success") and not quota_complete(row, result):
                    result.update(success=False, error_code="incomplete_quota", error="额度响应缺少必要窗口")
                def finish(data: dict[str, Any]) -> None:
                    metadata = data["scheduler"].setdefault(str(account_id), {})
                    query = dict(metadata.get("quota_query") or {})
                    code = str(result.get("error_code") or "")
                    if result.get("success"):
                        data["oauth_results"][str(account_id)] = dict(result)
                        metadata.update(last_success_at=current.isoformat(), last_error_code="")
                        query.pop("auth_fingerprint", None)
                        intent = dict(metadata.get("recovery_intent") or {})
                        if intent.get("status") == "auth_failed":
                            intent.update(status="retry", next_retry_at=(current + timedelta(seconds=MIN_QUERY_SECONDS)).isoformat())
                            metadata["recovery_intent"] = intent
                    else:
                        metadata.update(last_error_at=current.isoformat(), last_error_code=code)
                    if code in AUTH_ERRORS:
                        query["auth_fingerprint"] = fingerprint
                    summary = oauth_quota_summary_from_result(row, result)
                    windows = oauth_windows_by_key(summary.get("ui_windows"))
                    full = any((percent_or_none(windows.get(key, {}).get("used_percent")) or 0) >= 100
                               for key in required_oauth_window_keys(summary.get("plan_type")))
                    until = reset_not_before(row, result)
                    unresolved = not result.get("success") or (full and (not until or until <= current))
                    try:
                        threshold = json.loads(str(row.get("temp_unschedulable_reason") or "{}"))
                        window = windows.get("codex_" + str(threshold.get("window", "")).removeprefix("codex_"), {})
                        used = percent_or_none(window.get("used_percent"))
                        limit = percent_or_none(threshold.get("threshold_percent"))
                        unresolved |= bool(used is not None and limit is not None and used >= limit
                                           and (not until or until <= current))
                    except (ValueError, TypeError, AttributeError):
                        pass
                    if unresolved:
                        failures = int(query.get("failure_count") or 0) + 1
                        query.update(failure_count=failures,
                                     retry_at=(current + timedelta(seconds=QUERY_BACKOFF_SECONDS[min(failures - 1, 3)])).isoformat())
                    elif not full:
                        query.update(failure_count=0, retry_at="")
                    metadata["quota_query"] = query
                self.store.transaction(finish)
        except (OSError, ValueError, TypeError):
            result = {"success": False, "skipped": not admitted, "error_code": "query_state_unavailable",
                      "error": "额度查询状态无法安全保存"}
        finally:
            with self._lock:
                if admitted:
                    self._completed[account_id] = (time.monotonic(), fingerprint, dict(result))
                future.set_result(dict(result))
                self._inflight.pop(account_id, None)
            write_audit(self.audit_path, "oauth_usage_query", {
                "account_id": account_id, "source": source, "reason": reason,
                "requested": admitted, "success": bool(result.get("success")),
                "error_code": result.get("error_code", ""), "next_query_at": result.get("next_query_at"),
            })
        return result
