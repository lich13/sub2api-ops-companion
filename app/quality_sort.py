from __future__ import annotations

from typing import Any

def row_int(row: dict[str, Any], key: str) -> int:
    try:
        return int(row.get(key) or 0)
    except (TypeError, ValueError):
        return 0


def row_bool(row: dict[str, Any], key: str, default: bool = False) -> bool:
    value = row.get(key, default)
    if isinstance(value, bool):
        return value
    return str(value).strip().lower() in {"1", "true", "yes", "on"}


def default_speed_sort_key(row: dict[str, Any]) -> tuple[int, int, int, int, int]:
    sample_count = max(row_int(row, "usage_request_count"), row_int(row, "success_window"))
    return (
        0 if row_bool(row, "schedulable", True) else 1,
        -sample_count,
        row_int(row, "group_priority"),
        row_int(row, "account_priority"),
        row_int(row, "id"),
    )


def sort_speed_rows(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return sorted(rows, key=default_speed_sort_key)
