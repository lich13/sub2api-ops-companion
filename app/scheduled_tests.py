from __future__ import annotations

from datetime import datetime, timedelta
from typing import Any

SCHEDULE_INTERVALS: tuple[dict[str, Any], ...] = (
    {"minutes": 60, "label": "每小时", "cron": "0 * * * *"},
    {"minutes": 30, "label": "每30分钟", "cron": "*/30 * * * *"},
    {"minutes": 15, "label": "每15分钟", "cron": "*/15 * * * *"},
    {"minutes": 5, "label": "每5分钟", "cron": "*/5 * * * *"},
)
DEFAULT_INTERVAL_MINUTES = 30


def interval_options() -> list[dict[str, Any]]:
    return [dict(item) for item in SCHEDULE_INTERVALS]


def normalize_interval_minutes(value: Any, default: int = DEFAULT_INTERVAL_MINUTES) -> int:
    try:
        minutes = int(str(value).strip())
    except (TypeError, ValueError):
        minutes = default
    allowed = {int(item["minutes"]) for item in SCHEDULE_INTERVALS}
    if minutes in allowed:
        return minutes
    return default if default in allowed else DEFAULT_INTERVAL_MINUTES


def schedule_cron(minutes: Any) -> str:
    normalized = normalize_interval_minutes(minutes)
    for item in SCHEDULE_INTERVALS:
        if item["minutes"] == normalized:
            return str(item["cron"])
    return "*/30 * * * *"


def interval_from_cron(cron_expression: str) -> int:
    cron = " ".join(str(cron_expression or "").strip().split())
    for item in SCHEDULE_INTERVALS:
        if cron == item["cron"]:
            return int(item["minutes"])
    return DEFAULT_INTERVAL_MINUTES


def next_aligned_run(from_time: datetime, minutes: Any) -> datetime:
    interval = normalize_interval_minutes(minutes)
    current = from_time.replace(second=0, microsecond=0)
    if from_time.second or from_time.microsecond:
        current += timedelta(minutes=1)

    # Cron expressions like */15 are aligned to minute 0 within each hour.
    while current.minute % interval != 0:
        current += timedelta(minutes=1)
    if current <= from_time:
        current += timedelta(minutes=interval)
    return current.replace(second=0, microsecond=0)
