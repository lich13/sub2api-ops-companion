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
      nullif(to_jsonb(accounts)->>'parent_account_id', '')::bigint AS parent_account_id,
      extra,
      status,
      schedulable,
      priority AS account_priority,
      concurrency,
      load_factor,
      proxy_id,
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


def recovery_account_states(db: Database, account_ids: list[int]) -> list[dict[str, Any]]:
    """Small read-only projection for the recovery clock, with no credential data."""
    if not account_ids:
        return []
    return db.fetch_all('''
        SELECT id, updated_at, status, schedulable, rate_limited_at, rate_limit_reset_at,
               temp_unschedulable_until, temp_unschedulable_reason, overload_until,
               expires_at, auto_pause_on_expired,
               jsonb_build_object(
                 'codex_usage_updated_at', extra->'codex_usage_updated_at',
                 'codex_5h_used_percent', extra->'codex_5h_used_percent',
                 'codex_5h_reset_at', extra->'codex_5h_reset_at',
                 'codex_5h_reset_after_seconds', extra->'codex_5h_reset_after_seconds',
                 'codex_5h_window_minutes', extra->'codex_5h_window_minutes',
                 'codex_7d_used_percent', extra->'codex_7d_used_percent',
                 'codex_7d_reset_at', extra->'codex_7d_reset_at',
                 'codex_7d_reset_after_seconds', extra->'codex_7d_reset_after_seconds',
                 'codex_7d_window_minutes', extra->'codex_7d_window_minutes'
               ) AS recovery_extra
        FROM accounts WHERE deleted_at IS NULL AND platform='openai' AND type='oauth'
          AND id = ANY(%(ids)s) ORDER BY id
    ''', {'ids': account_ids})


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
