from __future__ import annotations

from datetime import datetime, timedelta, timezone
from typing import Any

from .audit import write_audit
from .db import Database
from .sql import QUALITY_SQL


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
        QUALITY_SQL,
        {"group_name": group, "platform": platform, "range_start": range_start, "range_end": None},
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


def fallback_account(db: Database, account_id: int) -> dict[str, Any] | None:
    return db.fetch_one(
        """
        SELECT
          id,
          name,
          platform,
          type,
          status,
          schedulable,
          priority AS account_priority,
          NULL::integer AS group_priority,
          concurrency,
          temp_unschedulable_until,
          temp_unschedulable_reason,
          0 AS success_window,
          0 AS account_quality_errors_window,
          0 AS blocked_403_window,
          0 AS balance_or_quota_window,
          0 AS rate_limit_window,
          0 AS unstable_5xx_stream_window,
          NULL::numeric AS error_rate_window_pct,
          NULL::timestamptz AS last_error_at,
          NULL::integer AS last_error_status,
          NULL::text AS last_error_category,
          NULL::text AS last_error_message
        FROM accounts
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        """,
        {"account_id": account_id},
    )


def pause_account(db: Database, audit_path: str, account_id: int, actor: str, reason: str) -> dict[str, Any] | None:
    row = db.fetch_one(
        """
        UPDATE accounts
        SET schedulable = false,
            temp_unschedulable_until = NULL,
            temp_unschedulable_reason = %(reason)s,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        RETURNING id, name, schedulable, temp_unschedulable_until, temp_unschedulable_reason
        """,
        {"account_id": account_id, "reason": reason},
    )
    write_audit(audit_path, "pause_account", {"user": actor, "account": row, "reason": reason})
    return row


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
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        RETURNING id, name, schedulable, temp_unschedulable_until, temp_unschedulable_reason
        """,
        {"account_id": account_id, "minutes": minutes, "reason": reason},
    )
    write_audit(
        audit_path,
        "cooldown_account",
        {"user": actor, "account": row, "minutes": minutes, "reason": reason},
    )
    return row


def resume_account(db: Database, audit_path: str, account_id: int, actor: str) -> dict[str, Any] | None:
    row = db.fetch_one(
        """
        UPDATE accounts
        SET schedulable = true,
            temp_unschedulable_until = NULL,
            temp_unschedulable_reason = NULL,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
        RETURNING id, name, schedulable, temp_unschedulable_until, temp_unschedulable_reason
        """,
        {"account_id": account_id},
    )
    write_audit(audit_path, "resume_account", {"user": actor, "account": row})
    return row
