from __future__ import annotations

from datetime import datetime, timezone
from typing import Any

P1_PRIORITY = 1
P2_PRIORITY = 2
STANDBY_PRIORITY = 50
DEGRADED_PRIORITY = 90


QUEUE_TIERS: dict[str, dict[str, Any]] = {
    "p1": {"key": "p1", "label": "P1 主队列", "priority": P1_PRIORITY, "load_factor": None, "class": "good"},
    "p2": {"key": "p2", "label": "P2 备用", "priority": P2_PRIORITY, "load_factor": None, "class": "warn"},
    "standby": {"key": "standby", "label": "备用池", "priority": STANDBY_PRIORITY, "load_factor": None, "class": "muted"},
    "degraded": {"key": "degraded", "label": "降载观察", "priority": DEGRADED_PRIORITY, "load_factor": 1, "class": "bad"},
}


def normalize_queue_tier(value: object) -> str:
    key = str(value or "").strip().lower()
    aliases = {
        "1": "p1",
        "p01": "p1",
        "primary": "p1",
        "2": "p2",
        "p02": "p2",
        "backup": "p2",
        "normal": "standby",
        "idle": "standby",
        "slow": "degraded",
        "degrade": "degraded",
    }
    key = aliases.get(key, key)
    return key if key in QUEUE_TIERS else "standby"


def queue_tier(row: dict[str, Any]) -> dict[str, Any]:
    try:
        priority = int(row.get("account_priority") or row.get("priority") or STANDBY_PRIORITY)
    except (TypeError, ValueError):
        priority = STANDBY_PRIORITY
    if priority <= P1_PRIORITY:
        return dict(QUEUE_TIERS["p1"])
    if priority == P2_PRIORITY:
        return dict(QUEUE_TIERS["p2"])
    if priority >= DEGRADED_PRIORITY:
        return dict(QUEUE_TIERS["degraded"])
    return dict(QUEUE_TIERS["standby"])


def tier_routing_values(tier: object, load_factor_supported: bool) -> tuple[int, int | None]:
    spec = QUEUE_TIERS[normalize_queue_tier(tier)]
    load_factor = spec["load_factor"] if load_factor_supported else None
    return int(spec["priority"]), load_factor


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
        try:
            total += int(row.get(key) or 0)
        except (TypeError, ValueError):
            continue
    return total


def _is_healthy_candidate(row: dict[str, Any]) -> bool:
    return bool(row.get("schedulable", True)) and not _is_cooling(row) and _quality_errors(row) == 0


def _success_count(row: dict[str, Any]) -> int:
    try:
        return int(row.get("success_window") or row.get("usage_request_count") or 0)
    except (TypeError, ValueError):
        return 0


def _sort_key(row: dict[str, Any]) -> tuple[int, int, int, int]:
    return (
        -_success_count(row),
        int(row.get("group_priority") or 999999),
        int(row.get("account_priority") or row.get("priority") or 999999),
        int(row.get("id") or 0),
    )


def auto_queue_plan(
    rows: list[dict[str, Any]],
    p1_count: int = 1,
    p2_count: int = 2,
    load_factor_supported: bool = False,
) -> list[dict[str, Any]]:
    p1_count = max(0, min(20, int(p1_count or 0)))
    p2_count = max(0, min(50, int(p2_count or 0)))
    healthy = sorted([row for row in rows if _is_healthy_candidate(row)], key=_sort_key)
    unhealthy = sorted([row for row in rows if not _is_healthy_candidate(row)], key=_sort_key)

    plan: list[dict[str, Any]] = []
    for index, row in enumerate(healthy):
        if index < p1_count:
            tier = "p1"
        elif index < p1_count + p2_count:
            tier = "p2"
        else:
            tier = "standby"
        priority, load_factor = tier_routing_values(tier, load_factor_supported)
        plan.append(
            {
                "account_id": int(row.get("id") or 0),
                "name": row.get("name"),
                "tier": tier,
                "priority": priority,
                "load_factor": load_factor,
            }
        )

    for row in unhealthy:
        priority, load_factor = tier_routing_values("degraded", load_factor_supported)
        plan.append(
            {
                "account_id": int(row.get("id") or 0),
                "name": row.get("name"),
                "tier": "degraded",
                "priority": priority,
                "load_factor": load_factor,
            }
        )
    return [item for item in plan if item["account_id"] > 0]
