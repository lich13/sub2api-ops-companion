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


def default_stability_sort_key(row: dict[str, Any]) -> tuple[int, int, int]:
    return (
        row_int(row, "group_priority"),
        row_int(row, "account_priority"),
        row_int(row, "id"),
    )


def sort_stability_rows(rows: list[dict[str, Any]], selected_sort: str) -> list[dict[str, Any]]:
    if normalize_stability_sort(selected_sort) != "error_rate":
        return rows

    return sorted(
        rows,
        key=lambda row: (
            -row_float(row, "error_rate_window_pct"),
            -row_int(row, "account_quality_errors_window"),
            -(row_int(row, "success_window") + row_int(row, "account_quality_errors_window")),
            *default_stability_sort_key(row),
        ),
    )
