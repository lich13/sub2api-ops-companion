"""Read saved quota evidence only. This module never performs network requests."""
from __future__ import annotations

import math
from datetime import datetime
from typing import Any

from .usage_query import (
    oauth_quota_summary_from_result, oauth_quota_windows, oauth_windows_by_key,
    parse_iso_datetime, required_oauth_window_keys,
)

FRESHNESS_SECONDS = 3600


def number(value: Any) -> float | None:
    if isinstance(value, bool) or value is None:
        return None
    try:
        result = float(value)
        return result if math.isfinite(result) and result >= 0 else None
    except (TypeError, ValueError):
        return None


def latest_openai_result(row: dict[str, Any] | None, result: dict[str, Any] | None,
                         now: datetime) -> dict[str, Any] | None:
    """Select one complete observation; never mix old/new windows or mask newer errors."""
    summary = oauth_quota_windows(row)
    observed = parse_iso_datetime(summary.get("updated_at"))
    previous = parse_iso_datetime((result or {}).get("queried_at"))
    if not observed or observed > now or not summary.get("ui_windows"):
        return result
    if previous and previous >= observed:
        return result
    return {"success": True, "queried_at": observed.isoformat(),
            "oauth_quota": summary, "source": "passive"}


def _window(key: str, label: str, used: Any, reset: Any, observed: Any,
            now: datetime, *, source: str, failed: bool = False) -> dict[str, Any]:
    percent = number(used)
    stamp = parse_iso_datetime(observed)
    deadline = parse_iso_datetime(reset)
    status = "known"
    if failed:
        status = "error"
    elif percent is None or not stamp or stamp > now:
        status = "unknown"
    elif (now - stamp).total_seconds() > FRESHNESS_SECONDS or (deadline and deadline <= now):
        status = "stale"
    return {"key": key, "label": label, "used_percent": percent,
            "reset_at": deadline.isoformat() if deadline else None,
            "observed_at": stamp.isoformat() if stamp else None,
            "status": status, "source": source}


def usage_windows(row: dict[str, Any], now: datetime,
                  result: dict[str, Any] | None = None) -> list[dict[str, Any]]:
    if row.get("quota_plan_type"):
        row = {**row, "credentials": {"plan_type": row["quota_plan_type"]}}
    extra = row.get("extra") if isinstance(row.get("extra"), dict) else {}
    platform, kind = row.get("platform"), row.get("type")
    if platform == "openai" and kind == "oauth":
        latest = latest_openai_result(row, result, now)
        summary = oauth_quota_summary_from_result(row, latest)
        windows = oauth_windows_by_key(summary.get("ui_windows"))
        observed = summary.get("updated_at") or (latest or {}).get("queried_at")
        return [_window(key, "5h" if key == "codex_5h" else "7d",
                        windows.get(key, {}).get("used_percent"), windows.get(key, {}).get("reset_at"),
                        observed, now, source="passive", failed=bool(latest and not latest.get("success")))
                for key in required_oauth_window_keys(summary.get("plan_type"))]
    if platform == "grok" and kind in {"oauth", "apikey"}:
        snapshot = extra.get("grok_usage_snapshot")
        if not isinstance(snapshot, dict):
            snapshot = extra.get("grok_quota_snapshot")
        snapshot = snapshot if isinstance(snapshot, dict) else {}
        observed = snapshot.get("last_headers_seen_at") or snapshot.get("updated_at")
        code = number(snapshot.get("status_code"))
        failed = bool(code and not 200 <= code < 300)
        windows = []
        for key, label in (("requests", "请求"), ("tokens", "Token")):
            raw = snapshot.get(key)
            raw = raw if isinstance(raw, dict) else {}
            limit, remaining = number(raw.get("limit")), number(raw.get("remaining"))
            valid = bool(limit and remaining is not None and remaining <= limit)
            used = limit - remaining if valid else None
            window = _window(key, label, used / limit * 100 if valid else None,
                             raw.get("reset_at") or raw.get("resets_at"), observed, now,
                             source="upstream_headers", failed=failed)
            window.update(limit=limit, remaining=remaining, used=used)
            windows.append(window)
        return windows
    return []
