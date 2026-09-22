from __future__ import annotations

from typing import Any

from .db import Database


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
      expires_at,
      auto_pause_on_expired,
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


def current_grok_oauth_accounts(db: Database) -> list[dict[str, Any]]:
    return db.fetch_all(
        """
        SELECT id, name, platform, type, status, schedulable,
          temp_unschedulable_until, rate_limit_reset_at, overload_until,
          expires_at, auto_pause_on_expired,
          jsonb_build_object('grok_needs_reauth',
            coalesce(extra->'grok_needs_reauth', 'false'::jsonb)) AS extra
        FROM accounts
        WHERE deleted_at IS NULL
          AND lower(coalesce(platform, '')) = 'grok'
          AND lower(coalesce(type, '')) = 'oauth'
        ORDER BY id
        """
    )


def fallback_account(db: Database, account_id: int) -> dict[str, Any] | None:
    return db.fetch_one(
        _account_select("AND id = %(account_id)s").replace("ORDER BY id", "LIMIT 1"),
        {"account_id": int(account_id)},
    )


def live_fallback_apikey_accounts(db: Database) -> list[dict[str, Any]]:
    rows = db.fetch_all(
        """
        SELECT
          id,
          name,
          platform,
          type,
          status,
          schedulable,
          expires_at,
          auto_pause_on_expired
        FROM accounts
        WHERE deleted_at IS NULL
          AND lower(coalesce(platform, '')) IN ('openai', 'grok')
          AND lower(coalesce(type, '')) = 'apikey'
        ORDER BY id
        """
    )
    accounts: list[dict[str, Any]] = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        try:
            account_id = int(row.get("id") or 0)
        except (TypeError, ValueError):
            continue
        if (
            account_id <= 0
            or str(row.get("platform") or "").strip().lower() not in {"openai", "grok"}
            or str(row.get("type") or "").strip().lower() != "apikey"
            or row.get("deleted_at") not in (None, "")
        ):
            continue
        accounts.append(dict(row))
    accounts.sort(key=lambda item: int(item.get("id") or 0))
    return accounts
