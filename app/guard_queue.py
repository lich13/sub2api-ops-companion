from __future__ import annotations

from collections import OrderedDict
from datetime import datetime, timezone
from typing import Any


UNGROUPED_KEY = "__ungrouped__"


def _safe_int(value: object, default: int = 0) -> int:
    try:
        return int(value or default)
    except (TypeError, ValueError):
        return default


def _is_oauth_account(row: dict[str, Any]) -> bool:
    return str(row.get("account_type") or row.get("type") or "").strip().lower() == "oauth"


def _priority_value(row: dict[str, Any]) -> int:
    return _safe_int(row.get("group_priority") or row.get("priority"), 50)


def membership_key(row: dict[str, Any]) -> str:
    group_id = row.get("group_id")
    group_ref = str(group_id if group_id not in (None, "") else row.get("group_name") or UNGROUPED_KEY)
    return f"{group_ref}:{_safe_int(row.get('id'))}"


def parse_membership_key(value: object) -> tuple[str, int] | None:
    raw = str(value or "").strip()
    if ":" not in raw:
        return None
    group_ref, account_ref = raw.rsplit(":", 1)
    account_id = _safe_int(account_ref)
    if not group_ref or account_id <= 0:
        return None
    return group_ref, account_id


def queue_position(row: dict[str, Any]) -> dict[str, Any]:
    priority = max(1, _priority_value(row))
    css_class = "good" if priority <= 3 else "warn" if priority <= 10 else "muted"
    return {
        "key": f"p{priority}",
        "label": f"P{priority}",
        "priority": priority,
        "position": priority,
        "class": css_class,
    }


def queue_tier(row: dict[str, Any]) -> dict[str, Any]:
    return queue_position(row)


def _is_cooling(row: dict[str, Any]) -> bool:
    value = row.get("temp_unschedulable_until")
    if not value:
        return False
    if isinstance(value, datetime):
        parsed = value
    else:
        try:
            parsed = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
        except ValueError:
            return False
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed > datetime.now(parsed.tzinfo)


def _quality_errors(row: dict[str, Any]) -> int:
    keys = (
        "account_quality_errors_window",
        "balance_or_quota_window",
        "blocked_403_window",
        "rate_limit_window",
        "unstable_5xx_stream_window",
    )
    total = 0
    for key in keys:
        total += _safe_int(row.get(key))
    return total


def _is_healthy_candidate(row: dict[str, Any]) -> bool:
    return bool(row.get("schedulable", True)) and not _is_cooling(row) and _quality_errors(row) == 0


def _success_count(row: dict[str, Any]) -> int:
    return _safe_int(row.get("success_window") or row.get("usage_request_count"))


def _sort_key(row: dict[str, Any]) -> tuple[int, int, int, int]:
    return (
        -_success_count(row),
        _priority_value(row),
        _safe_int(row.get("account_priority"), 999999),
        _safe_int(row.get("id")),
    )


def _group_order_key(row: dict[str, Any]) -> tuple[str, int, str]:
    return (
        str(row.get("platform") or ""),
        _safe_int(row.get("group_sort_order"), 999999),
        str(row.get("group_name") or ""),
    )


def _display_order_key(row: dict[str, Any]) -> tuple[str, int, str, int, int, int]:
    return (
        *_group_order_key(row),
        _priority_value(row),
        _safe_int(row.get("account_priority"), 999999),
        _safe_int(row.get("id")),
    )


def _membership_order_key(row: dict[str, Any]) -> tuple[int, int, int]:
    return (
        _priority_value(row),
        _safe_int(row.get("account_priority"), 999999),
        _safe_int(row.get("id")),
    )


def _group_key(row: dict[str, Any]) -> str:
    return str(row.get("group_id") or row.get("group_name") or UNGROUPED_KEY)


def _group_label(row: dict[str, Any]) -> str:
    return str(row.get("group_name") or "未分组")


def group_queue_rows(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    grouped: OrderedDict[str, dict[str, Any]] = OrderedDict()
    for row in rows:
        key = _group_key(row)
        if key not in grouped:
            grouped[key] = {
                "key": key,
                "label": _group_label(row),
                "platform": row.get("platform"),
                "group_id": row.get("group_id"),
                "group_sort_order": row.get("group_sort_order"),
                "rows": [],
            }
        grouped[key]["rows"].append(row)
    for group in grouped.values():
        group["rows"].sort(key=_membership_order_key)
    return list(grouped.values())


def _group_rows_in_input_order(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    grouped: OrderedDict[str, dict[str, Any]] = OrderedDict()
    for row in rows:
        key = _group_key(row)
        if key not in grouped:
            grouped[key] = {
                "key": key,
                "label": _group_label(row),
                "platform": row.get("platform"),
                "group_id": row.get("group_id"),
                "group_sort_order": row.get("group_sort_order"),
                "rows": [],
            }
        grouped[key]["rows"].append(row)
    return list(grouped.values())


def _priority_for_position(position: int) -> int:
    return max(1, int(position or 1))


def _row_load_factor(row: dict[str, Any], load_factor_supported: bool, degrade: bool = False) -> int | None:
    if not load_factor_supported:
        return None
    if degrade:
        return 1
    value = row.get("load_factor")
    parsed = _safe_int(value)
    return parsed if parsed > 0 else None


def _plan_item(
    row: dict[str, Any],
    position: int,
    load_factor_supported: bool,
    reason: str,
    degrade: bool = False,
) -> dict[str, Any]:
    priority = _priority_for_position(position)
    return {
        "account_id": _safe_int(row.get("id")),
        "membership_key": membership_key(row),
        "group_id": _safe_int(row.get("group_id")) or None,
        "name": row.get("name"),
        "group_name": row.get("group_name"),
        "position": position,
        "group_priority": priority,
        "priority": priority,
        "load_factor": _row_load_factor(row, load_factor_supported, degrade=degrade),
        "reason": reason,
    }


def auto_queue_plan(
    rows: list[dict[str, Any]],
    load_factor_supported: bool = False,
) -> list[dict[str, Any]]:
    plan: list[dict[str, Any]] = []
    for group in group_queue_rows([row for row in rows if not _is_oauth_account(row)]):
        group_rows = list(group["rows"])
        healthy = sorted([row for row in group_rows if _is_healthy_candidate(row)], key=_sort_key)
        unhealthy = sorted([row for row in group_rows if not _is_healthy_candidate(row)], key=_display_order_key)
        ordered = healthy + unhealthy
        for index, row in enumerate(ordered, start=1):
            plan.append(
                _plan_item(
                    row,
                    index,
                    load_factor_supported,
                    "auto health order" if row in healthy else "auto tail for problem account",
                    degrade=row in unhealthy,
                )
            )
    return [item for item in plan if item["account_id"] > 0]


def reorder_queue_plan(
    rows: list[dict[str, Any]],
    ordered_membership_keys: list[object],
    load_factor_supported: bool = False,
) -> list[dict[str, Any]]:
    row_by_key = {membership_key(row): row for row in rows if _safe_int(row.get("id")) > 0}
    legacy_rows_by_account: dict[int, dict[str, Any]] = {}
    for row in rows:
        account_id = _safe_int(row.get("id"))
        if account_id > 0 and account_id not in legacy_rows_by_account:
            legacy_rows_by_account[account_id] = row
    submitted: list[dict[str, Any]] = []
    seen: set[str] = set()
    for raw_key in ordered_membership_keys:
        row = row_by_key.get(str(raw_key or "").strip())
        if row is None:
            parsed = _safe_int(raw_key)
            row = legacy_rows_by_account.get(parsed)
        if row is None:
            continue
        key = membership_key(row)
        if key in seen:
            continue
        seen.add(key)
        submitted.append(row)

    for row in sorted(rows, key=_display_order_key):
        key = membership_key(row)
        if _safe_int(row.get("id")) > 0 and key not in seen:
            seen.add(key)
            submitted.append(row)

    plan: list[dict[str, Any]] = []
    for group in _group_rows_in_input_order(submitted):
        for index, row in enumerate(group["rows"], start=1):
            plan.append(_plan_item(row, index, load_factor_supported, "manual queue reorder"))
    return [item for item in plan if item["account_id"] > 0]
