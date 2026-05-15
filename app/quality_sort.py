from __future__ import annotations

from typing import Any

STABILITY_SORT_OPTIONS = (
    {"value": "default", "label": "默认排序"},
    {"value": "error_rate", "label": "错误率从高到低"},
)
VALID_STABILITY_SORTS = {item["value"] for item in STABILITY_SORT_OPTIONS}


def normalize_stability_sort(value: str | None) -> str:
    selected = str(value or "").strip().lower()
    return selected if selected in VALID_STABILITY_SORTS else "default"


def row_int(row: dict[str, Any], key: str) -> int:
    try:
        return int(row.get(key) or 0)
    except (TypeError, ValueError):
        return 0


def row_float(row: dict[str, Any], key: str) -> float:
    try:
        return float(row.get(key) or 0)
    except (TypeError, ValueError):
        return 0.0


def row_bool(row: dict[str, Any], key: str, default: bool = False) -> bool:
    value = row.get(key, default)
    if isinstance(value, bool):
        return value
    return str(value).strip().lower() in {"1", "true", "yes", "on"}


def default_stability_sort_key(row: dict[str, Any]) -> tuple[int, int, int, int, int]:
    return (
        0 if row_bool(row, "schedulable", True) else 1,
        -row_int(row, "success_window"),
        row_int(row, "group_priority"),
        row_int(row, "account_priority"),
        row_int(row, "id"),
    )


def default_speed_sort_key(row: dict[str, Any]) -> tuple[int, int, int, int, int]:
    sample_count = max(row_int(row, "usage_request_count"), row_int(row, "success_window"))
    return (
        0 if row_bool(row, "schedulable", True) else 1,
        -sample_count,
        row_int(row, "group_priority"),
        row_int(row, "account_priority"),
        row_int(row, "id"),
    )


def sort_stability_rows(rows: list[dict[str, Any]], selected_sort: str) -> list[dict[str, Any]]:
    if normalize_stability_sort(selected_sort) != "error_rate":
        return sorted(rows, key=default_stability_sort_key)

    return sorted(
        rows,
        key=lambda row: (
            -row_float(row, "error_rate_window_pct"),
            -row_int(row, "account_quality_errors_window"),
            -(row_int(row, "success_window") + row_int(row, "account_quality_errors_window")),
            *default_stability_sort_key(row),
        ),
    )


def sort_speed_rows(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return sorted(rows, key=default_speed_sort_key)
