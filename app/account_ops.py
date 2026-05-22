from __future__ import annotations

from datetime import datetime, timedelta, timezone
from typing import Any

from .audit import write_audit
from .db import Database
from .sql import (
    GUARD_ACCOUNT_GROUP_PRIORITY_UPDATE_SQL,
    GUARD_ACCOUNT_LOAD_FACTOR_UPDATE_SQL,
    GUARD_ACCOUNT_PRIORITY_UPDATE_SQL,
    GUARD_ACCOUNT_ROUTING_UPDATE_SQL,
    QUALITY_SQL_COMPAT_NO_LOAD_FACTOR,
)


def is_cooling(row: dict[str, Any]) -> bool:
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


def account_state(row: dict[str, Any]) -> str:
    if not row.get("schedulable"):
        return "已停"
    if is_cooling(row):
        return "冷却中"
    return "可调度"


def quality_rows(db: Database, group: str, platform: str, hours: int) -> list[dict[str, Any]]:
    range_start = datetime.now(timezone.utc) - timedelta(hours=int(hours or 24))
    return db.fetch_all(
        QUALITY_SQL_COMPAT_NO_LOAD_FACTOR,
        {"group_names": [group], "platform": platform, "range_start": range_start, "range_end": None},
    )


def account_summary(rows: list[dict[str, Any]]) -> dict[str, int]:
    return {
        "total": len(rows),
        "active": sum(1 for row in rows if row.get("schedulable") and not is_cooling(row)),
        "paused": sum(1 for row in rows if not row.get("schedulable")),
        "cooling": sum(1 for row in rows if row.get("schedulable") and is_cooling(row)),
        "bad": sum(1 for row in rows if int(row.get("account_quality_errors_window") or 0) > 0),
        "balance": sum(1 for row in rows if int(row.get("balance_or_quota_window") or 0) > 0),
        "blocked": sum(1 for row in rows if int(row.get("blocked_403_window") or 0) > 0),
        "rate": sum(1 for row in rows if int(row.get("rate_limit_window") or 0) > 0),
        "unstable": sum(1 for row in rows if int(row.get("unstable_5xx_stream_window") or 0) > 0),
    }


def filter_rows(rows: list[dict[str, Any]], filter_name: str) -> list[dict[str, Any]]:
    key = normalize_filter(filter_name)
    if key == "active":
        return [row for row in rows if row.get("schedulable") and not is_cooling(row)]
    if key == "paused":
        return [row for row in rows if not row.get("schedulable")]
    if key == "cooling":
        return [row for row in rows if row.get("schedulable") and is_cooling(row)]
    if key == "bad":
        return [row for row in rows if int(row.get("account_quality_errors_window") or 0) > 0]
    if key == "balance":
        return [row for row in rows if int(row.get("balance_or_quota_window") or 0) > 0]
    if key == "blocked":
        return [row for row in rows if int(row.get("blocked_403_window") or 0) > 0]
    if key == "rate":
        return [row for row in rows if int(row.get("rate_limit_window") or 0) > 0]
    if key == "unstable":
        return [row for row in rows if int(row.get("unstable_5xx_stream_window") or 0) > 0]
    return list(rows)


def normalize_filter(value: str) -> str:
    key = value.strip().lower()
    aliases = {
        "act": "active",
        "可调度": "active",
        "off": "paused",
        "paused": "paused",
        "已停": "paused",
        "停": "paused",
        "cd": "cooling",
        "cooldown": "cooling",
        "冷却": "cooling",
        "冷却中": "cooling",
        "error": "bad",
        "errors": "bad",
        "错误": "bad",
        "quota": "balance",
        "余额": "balance",
        "额度": "balance",
        "403": "blocked",
        "block": "blocked",
        "blocked": "blocked",
        "阻断": "blocked",
        "429": "rate",
        "limit": "rate",
        "限流": "rate",
        "5xx": "unstable",
        "stream": "unstable",
        "流式": "unstable",
    }
    return aliases.get(key, key if key else "all")


def filter_title(filter_name: str) -> str:
    return {
        "all": "全部账号",
        "active": "可调度账号",
        "paused": "已停账号",
        "cooling": "冷却中账号",
        "bad": "有错误账号",
        "balance": "额度/余额错误账号",
        "blocked": "403 阻断账号",
        "rate": "限流账号",
        "unstable": "5xx/流式错误账号",
    }.get(normalize_filter(filter_name), "全部账号")


def find_accounts(rows: list[dict[str, Any]], query: str, limit: int = 12) -> list[dict[str, Any]]:
    raw = query.strip()
    needle = raw.lstrip("#").lower()
    if not needle:
        return []
    matches = [
        row
        for row in rows
        if str(row.get("id")) == needle or needle in str(row.get("name") or "").lower()
    ]
    matches.sort(
        key=lambda row: (
            0 if str(row.get("id")) == needle else 1,
            0 if str(row.get("name") or "").lower() == raw.lower() else 1,
            int(row.get("group_priority") or 999999),
            int(row.get("account_priority") or row.get("priority") or 999999),
            int(row.get("id") or 0),
        )
    )
    return matches[:limit]


def account_by_id(rows: list[dict[str, Any]], account_id: int) -> dict[str, Any] | None:
    return next((row for row in rows if int(row.get("id") or 0) == account_id), None)


def fallback_account(db: Database, account_id: int, load_factor_supported: bool = False) -> dict[str, Any] | None:
    load_factor_sql = (
        """
          load_factor,
          COALESCE(NULLIF(load_factor, 0), NULLIF(concurrency, 0), 1) AS effective_load_factor,
        """
        if load_factor_supported
        else """
          NULL::integer AS load_factor,
          COALESCE(NULLIF(concurrency, 0), 1) AS effective_load_factor,
        """
    )
    return db.fetch_one(
        f"""
        SELECT
          id,
          name,
          platform,
          type,
          credentials,
          extra,
          status,
          schedulable,
          priority AS account_priority,
          NULL::integer AS group_priority,
          concurrency,
{load_factor_sql}
          temp_unschedulable_until,
          temp_unschedulable_reason,
          0 AS success_window,
          0 AS output_tokens_window,
          0 AS account_quality_errors_window,
          0 AS blocked_403_window,
          0 AS balance_or_quota_window,
          0 AS rate_limit_window,
          0 AS unstable_5xx_stream_window,
          NULL::numeric AS error_rate_window_pct,
          NULL::timestamptz AS last_error_at,
          NULL::integer AS last_error_status,
          NULL::text AS last_error_category,
          NULL::text AS last_error_message,
          NULL::numeric AS avg_duration_ms,
          NULL::numeric AS avg_first_token_ms,
          NULL::numeric AS avg_ms_per_output_token
        FROM accounts
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        """,
        {"account_id": account_id},
    )


def normalize_priority_value(value: object, default: int = 50) -> int:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        parsed = default
    return max(1, parsed)


def normalize_load_factor_value(value: object) -> int | None:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        return None
    return parsed if parsed > 0 else None


def pause_account(db: Database, audit_path: str, account_id: int, actor: str, reason: str) -> dict[str, Any] | None:
    row = db.fetch_one(
        """
        UPDATE accounts
        SET schedulable = false,
            temp_unschedulable_until = NULL,
            temp_unschedulable_reason = %(reason)s,
            rate_limited_at = NULL,
            rate_limit_reset_at = NULL,
            overload_until = NULL,
            error_message = NULL,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        RETURNING
          id,
          name,
          schedulable,
          temp_unschedulable_until,
          temp_unschedulable_reason,
          rate_limited_at,
          rate_limit_reset_at,
          overload_until,
          error_message
        """,
        {"account_id": account_id, "reason": reason},
    )
    write_audit(audit_path, "pause_account", {"user": actor, "account": row, "reason": reason})
    return row


def guard_pause_account(db: Database, account_id: int, reason: str) -> dict[str, Any] | None:
    return db.fetch_one(
        """
        UPDATE accounts
        SET schedulable = false,
            temp_unschedulable_until = NULL,
            temp_unschedulable_reason = %(reason)s,
            rate_limited_at = NULL,
            rate_limit_reset_at = NULL,
            overload_until = NULL,
            error_message = NULL,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        RETURNING
          id,
          name,
          schedulable,
          temp_unschedulable_until,
          temp_unschedulable_reason,
          rate_limited_at,
          rate_limit_reset_at,
          overload_until,
          error_message
        """,
        {"account_id": account_id, "reason": reason},
    )


def cooldown_account(
    db: Database,
    audit_path: str,
    account_id: int,
    actor: str,
    minutes: int,
    reason: str,
) -> dict[str, Any] | None:
    minutes = max(1, min(1440, int(minutes or 30)))
    row = db.fetch_one(
        """
        UPDATE accounts
        SET schedulable = true,
            temp_unschedulable_until = now() + (%(minutes)s::text || ' minutes')::interval,
            temp_unschedulable_reason = %(reason)s,
            rate_limited_at = NULL,
            rate_limit_reset_at = NULL,
            overload_until = NULL,
            error_message = NULL,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        RETURNING
          id,
          name,
          schedulable,
          temp_unschedulable_until,
          temp_unschedulable_reason,
          rate_limited_at,
          rate_limit_reset_at,
          overload_until,
          error_message
        """,
        {"account_id": account_id, "minutes": minutes, "reason": reason},
    )
    write_audit(
        audit_path,
        "cooldown_account",
        {"user": actor, "account": row, "minutes": minutes, "reason": reason},
    )
    return row


def guard_cooldown_account(db: Database, account_id: int, minutes: int, reason: str) -> dict[str, Any] | None:
    minutes = max(1, min(1440, int(minutes or 15)))
    return db.fetch_one(
        """
        UPDATE accounts
        SET schedulable = true,
            temp_unschedulable_until = now() + (%(minutes)s::text || ' minutes')::interval,
            temp_unschedulable_reason = %(reason)s,
            rate_limited_at = NULL,
            rate_limit_reset_at = NULL,
            overload_until = NULL,
            error_message = NULL,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        RETURNING
          id,
          name,
          schedulable,
          temp_unschedulable_until,
          temp_unschedulable_reason,
          rate_limited_at,
          rate_limit_reset_at,
          overload_until,
          error_message
        """,
        {"account_id": account_id, "minutes": minutes, "reason": reason},
    )


def resume_account(
    db: Database,
    audit_path: str,
    account_id: int,
    actor: str,
    reason: str = "resume account from ops companion",
) -> dict[str, Any] | None:
    row = db.fetch_one(
        """
        UPDATE accounts
        SET status = 'active',
            schedulable = true,
            temp_unschedulable_until = NULL,
            temp_unschedulable_reason = NULL,
            rate_limited_at = NULL,
            rate_limit_reset_at = NULL,
            overload_until = NULL,
            error_message = NULL,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        RETURNING
          id,
          name,
          status,
          schedulable,
          temp_unschedulable_until,
          temp_unschedulable_reason,
          rate_limited_at,
          rate_limit_reset_at,
          overload_until,
          error_message
        """,
        {"account_id": account_id},
    )
    write_audit(audit_path, "resume_account", {"user": actor, "account": row, "reason": reason})
    return row


def guard_update_account_routing(
    db: Database,
    audit_path: str,
    account_id: int,
    actor: str,
    priority: int,
    load_factor: int | None,
    load_factor_supported: bool,
    reason: str,
) -> dict[str, Any] | None:
    sql = GUARD_ACCOUNT_ROUTING_UPDATE_SQL if load_factor_supported else GUARD_ACCOUNT_PRIORITY_UPDATE_SQL
    params = {
        "account_id": int(account_id),
        "priority": normalize_priority_value(priority),
        "load_factor": normalize_load_factor_value(load_factor),
    }
    row = db.fetch_one(sql, params)
    write_audit(
        audit_path,
        "guard_account_routing_update",
        {
            "user": actor,
            "account": row,
            "params": params,
            "load_factor_supported": load_factor_supported,
            "reason": reason,
        },
    )
    return row


def guard_update_account_group_priority(
    db: Database,
    audit_path: str,
    account_id: int,
    group_id: int | None,
    group_name: str,
    actor: str,
    group_priority: int,
    reason: str,
) -> dict[str, Any] | None:
    params = {
        "account_id": int(account_id),
        "group_id": int(group_id) if group_id else None,
        "group_name": str(group_name or ""),
        "group_priority": normalize_priority_value(group_priority),
    }
    row = db.fetch_one(GUARD_ACCOUNT_GROUP_PRIORITY_UPDATE_SQL, params)
    write_audit(
        audit_path,
        "guard_account_group_priority_update",
        {
            "user": actor,
            "account_group": row,
            "params": params,
            "reason": reason,
        },
    )
    return row


def guard_update_account_load_factor(
    db: Database,
    audit_path: str,
    account_id: int,
    actor: str,
    load_factor: int | None,
    reason: str,
) -> dict[str, Any] | None:
    params = {"account_id": int(account_id), "load_factor": normalize_load_factor_value(load_factor)}
    row = db.fetch_one(GUARD_ACCOUNT_LOAD_FACTOR_UPDATE_SQL, params)
    write_audit(
        audit_path,
        "guard_account_load_factor_update",
        {
            "user": actor,
            "account": row,
            "params": params,
            "reason": reason,
        },
    )
    return row
