from __future__ import annotations

from datetime import date, datetime, timedelta, timezone
from typing import Any
from urllib.parse import urlencode
from zoneinfo import ZoneInfo

BEIJING_TZ = ZoneInfo("Asia/Shanghai")
TIME_RANGE_PRESETS = (
    ("today", "今天"),
    ("last7", "近7天"),
    ("last30", "近30天"),
    ("all", "全部时间"),
)


def clamp_int(value: object, default: int, minimum: int, maximum: int) -> int:
    try:
        parsed = int(value or default)
    except (TypeError, ValueError):
        return default
    return max(minimum, min(maximum, parsed))


def date_param(value: str | None) -> date | None:
    raw = str(value or "").strip().replace("/", "-")
    if not raw:
        return None
    try:
        return datetime.strptime(raw, "%Y-%m-%d").date()
    except ValueError:
        return None


def local_midnight(value: date) -> datetime:
    return datetime(value.year, value.month, value.day, tzinfo=BEIJING_TZ)


def date_value(value: date | None) -> str:
    return value.isoformat() if value else ""


def date_label(value: date) -> str:
    return value.strftime("%Y/%m/%d")


def preset_dates(preset: str, today: date) -> tuple[date | None, date | None]:
    if preset == "today":
        return today, today
    if preset == "last7":
        return today - timedelta(days=6), today
    if preset == "last30":
        return today - timedelta(days=29), today
    if preset == "all":
        return None, None
    return today, today


def time_range_preset_items(today: date, selected: str) -> list[dict[str, Any]]:
    items: list[dict[str, Any]] = []
    for value, label in TIME_RANGE_PRESETS:
        start_date, end_date = preset_dates(value, today)
        items.append(
            {
                "value": value,
                "label": label,
                "start_date": date_value(start_date),
                "end_date": date_value(end_date),
                "selected": value == selected,
            }
        )
    return items


def rolling_hours_range(hours: int) -> dict[str, Any]:
    parsed_hours = clamp_int(hours, 24, 1, 24 * 365 * 10)
    now = datetime.now(timezone.utc)
    start_at = now - timedelta(hours=parsed_hours)
    start_day = start_at.astimezone(BEIJING_TZ).date()
    end_day = now.astimezone(BEIJING_TZ).date()
    return {
        "preset": "custom",
        "label": f"近 {parsed_hours} 小时",
        "start_date": date_value(start_day),
        "end_date": date_value(end_day),
        "start_at": start_at,
        "end_at": None,
        "query_args": {"hours": parsed_hours},
        "presets": time_range_preset_items(end_day, "custom"),
    }


def build_time_range(
    preset: str = "",
    start_date: str = "",
    end_date: str = "",
    legacy_hours: int | None = None,
) -> dict[str, Any]:
    today = datetime.now(BEIJING_TZ).date()
    selected = str(preset or "").strip().lower()
    if not selected and legacy_hours is not None:
        return rolling_hours_range(legacy_hours)
    if selected not in {item[0] for item in TIME_RANGE_PRESETS} | {"custom"}:
        selected = "today"

    if selected == "custom":
        start_day = date_param(start_date)
        end_day = date_param(end_date)
        if start_day is None and end_day is None:
            start_day = today
            end_day = today
        elif start_day is None:
            start_day = end_day
        elif end_day is None:
            end_day = start_day
        if end_day < start_day:
            start_day, end_day = end_day, start_day
    else:
        start_day, end_day = preset_dates(selected, today)

    if selected == "all":
        start_at = None
        end_at = None
        label = "全部时间"
        query_args = {"time_range": "all"}
    else:
        start_at = local_midnight(start_day)
        end_at = local_midnight(end_day + timedelta(days=1))
        label = dict(TIME_RANGE_PRESETS).get(selected, "")
        if selected == "custom":
            label = (
                date_label(start_day)
                if start_day == end_day
                else f"{date_label(start_day)} - {date_label(end_day)}"
            )
            query_args = {
                "time_range": "custom",
                "start_date": date_value(start_day),
                "end_date": date_value(end_day),
            }
        else:
            query_args = {"time_range": selected}

    return {
        "preset": selected,
        "label": label,
        "start_date": date_value(start_day),
        "end_date": date_value(end_day),
        "start_at": start_at,
        "end_at": end_at,
        "query_args": query_args,
        "presets": time_range_preset_items(today, selected),
    }


def clean_query_string(params: dict[str, Any]) -> str:
    cleaned = {key: value for key, value in params.items() if value not in (None, "")}
    return urlencode(cleaned)
