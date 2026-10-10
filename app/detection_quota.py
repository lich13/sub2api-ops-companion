"""Automatic detection admission from saved evidence; never queries upstream."""
from __future__ import annotations

from datetime import datetime, timedelta
from decimal import Decimal, InvalidOperation

from .quota_snapshot import FRESHNESS_SECONDS, usage_windows
from .usage_query import parse_iso_datetime


def decimal_number(value):
    if isinstance(value, bool) or value is None:
        return None
    try:
        number = Decimal(str(value))
        return number if number.is_finite() and number >= 0 else None
    except (InvalidOperation, ValueError, TypeError):
        return None


def fresh(value, now):
    stamp = parse_iso_datetime(value)
    return stamp if stamp and 0 <= (now - stamp).total_seconds() <= FRESHNESS_SECONDS else None


def codex_credit_status(row: dict, now: datetime) -> str:
    extra = row.get("extra") if isinstance(row.get("extra"), dict) else {}
    snapshot = extra.get("codex_credits_snapshot")
    if not isinstance(snapshot, dict) or not fresh(snapshot.get("fetched_at"), now):
        return "unknown"
    credits = snapshot.get("credits")
    if not isinstance(credits, dict):
        return "unknown"
    has, unlimited = credits.get("has_credits"), credits.get("unlimited")
    if type(has) is not bool or type(unlimited) is not bool:
        return "unknown"
    raw = credits.get("balance")
    balance = decimal_number(raw)
    if raw is not None and balance is None:
        return "unknown"
    if unlimited:
        return "available"
    if balance is not None and (balance > 0) != has:
        return "unknown"
    # Native hidden balances use has_credits; absence is not a fabricated zero.
    return "available" if has else "empty"


def detection_quota_reason(row: dict, now: datetime, result: dict | None = None) -> str:
    for field, reason in (("rate_limit_reset_at", "账号仍在上游限流中"),
                          ("overload_until", "账号仍在上游过载等待中"),
                          ("temp_unschedulable_until", "账号暂不可调度")):
        until = parse_iso_datetime(row.get(field))
        if until and until > now:
            return reason
    extra = row.get("extra") if isinstance(row.get("extra"), dict) else {}
    if row.get("type") == "apikey":
        for prefix, days in (("quota_daily", 1), ("quota_weekly", 7), ("quota", 0)):
            limit = decimal_number(extra.get(prefix + "_limit"))
            used = decimal_number(extra.get(prefix + "_used", 0))
            if not limit or used is None or used < limit:
                continue
            if days:
                if extra.get(prefix + "_reset_mode") == "fixed":
                    reset = parse_iso_datetime(extra.get(prefix + "_reset_at"))
                else:
                    start = parse_iso_datetime(extra.get(prefix + "_start"))
                    reset = start + timedelta(days=days) if start else None
                if reset and reset <= now:
                    continue
            return "已确认额度耗尽"
        return ""

    # A newer successful call or saved observation supersedes an old rejection.
    observed = [parse_iso_datetime(row.get("last_success_at")),
                parse_iso_datetime(extra.get("codex_usage_updated_at")),
                parse_iso_datetime((extra.get("codex_credits_snapshot") or {}).get("fetched_at"))
                if isinstance(extra.get("codex_credits_snapshot"), dict) else None]
    rejections = [(row.get("last_error_code"), row.get("last_error_at"))]
    if result and result.get("success") is False:
        rejections.append((result.get("error_code"), result.get("queried_at")))
    for code, at in rejections:
        stamp = fresh(at, now)
        expired = parse_iso_datetime(row.get("rate_limit_reset_at"))
        if stamp and expired and stamp <= expired <= now:
            continue
        if isinstance(code, str) and code.lower() in {"insufficient_quota", "quota_exceeded", "usage_limit_reached"} and stamp:
            if not any(value and stamp < value <= now for value in observed):
                return "上游已拒绝额度请求"
    if codex_credit_status(row, now) == "empty" and any(
        window["status"] == "known" and window["used_percent"] >= 100
        for window in usage_windows(row, now, result)
    ):
        return "已确认额度耗尽"
    return ""
