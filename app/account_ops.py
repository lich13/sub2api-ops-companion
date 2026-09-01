from __future__ import annotations

from datetime import datetime, timezone
from typing import Any

from .audit import write_audit
from .db import Database


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
    return parsed > datetime.now(timezone.utc)


def account_state(row: dict[str, Any]) -> str:
    if not row.get("schedulable"):
        return "已停"
    if is_cooling(row):
        return "冷却中"
    return "可调度"


def _account_select(where: str) -> str:
    return f"""
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
      concurrency,
      temp_unschedulable_until,
      temp_unschedulable_reason,
      rate_limited_at,
      rate_limit_reset_at,
      overload_until,
      error_message,
      updated_at
    FROM accounts
    WHERE deleted_at IS NULL
      {where}
    ORDER BY id
    """


def current_oauth_accounts(db: Database) -> list[dict[str, Any]]:
    return db.fetch_all(
        _account_select(
            "AND lower(coalesce(platform, '')) = 'openai' "
            "AND lower(coalesce(type, '')) = 'oauth'"
        )
    )


def fallback_account(db: Database, account_id: int) -> dict[str, Any] | None:
    return db.fetch_one(
        _account_select("AND id = %(account_id)s").replace("ORDER BY id", "LIMIT 1"),
        {"account_id": int(account_id)},
    )
def pause_account(
    db: Database,
    audit_path: str,
    account_id: int,
    actor: str,
    reason: str,
) -> dict[str, Any] | None:
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
        RETURNING id, name, status, schedulable, temp_unschedulable_until,
                  temp_unschedulable_reason, rate_limited_at, rate_limit_reset_at,
                  overload_until, error_message
        """,
        {"account_id": int(account_id), "reason": reason},
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
    duration = max(1, min(1440, int(minutes or 15)))
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
        RETURNING id, name, status, schedulable, temp_unschedulable_until,
                  temp_unschedulable_reason, rate_limited_at, rate_limit_reset_at,
                  overload_until, error_message
        """,
        {"account_id": int(account_id), "minutes": duration, "reason": reason},
    )
    write_audit(
        audit_path,
        "cooldown_account",
        {"user": actor, "account": row, "minutes": duration, "reason": reason},
    )
    return row


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
        RETURNING id, name, status, schedulable, temp_unschedulable_until,
                  temp_unschedulable_reason, rate_limited_at, rate_limit_reset_at,
                  overload_until, error_message
        """,
        {"account_id": int(account_id)},
    )
    write_audit(audit_path, "resume_account", {"user": actor, "account": row, "reason": reason})
    return row
