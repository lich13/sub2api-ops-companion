"""Explicit account selection for automatic recovery; no upstream requests."""
from __future__ import annotations

CONNECTION_IDS = "oauth_recovery_connection_account_ids"
MODEL_IDS = "oauth_recovery_model_account_ids"
SEEN_IDS = "oauth_recovery_seen_account_ids"


def account_ids(value):
    if not isinstance(value, list) or any(type(aid) is not int or aid <= 0 for aid in value):
        raise ValueError("恢复账号必须为正整数 ID 列表")
    return sorted(set(value))


def recovery_method(settings, account_id):
    connection = getattr(settings, CONNECTION_IDS, None)
    models = getattr(settings, MODEL_IDS, None)
    # Only the pre-migration state uses the former implicit selection. Startup
    # persists an explicit inventory before any automatic worker is launched.
    if connection is None and models is None:
        return "connection"
    if account_id in (models or []):
        return "model"
    if account_id in (connection or []):
        return "connection"
    return None


def eligible_ids(rows):
    return {int(row["id"]) for row in rows
            if row.get("platform") == "openai" and row.get("type") == "oauth"
            and not row.get("deleted_at") and not row.get("parent_account_id")
            and not (row.get("extra") or {}).get("parent_account_id")}


def migrate_recovery_selection(runtime, rows=None):
    """Reconcile under ConfigService's write lock, using only the saved inventory."""
    from . import account_ops
    config = runtime.oauth_config_file()
    if rows is None:
        rows = account_ops.current_oauth_accounts(runtime.db)
    eligible = eligible_ids(rows)
    if CONNECTION_IDS in config or MODEL_IDS in config:
        connection = account_ids(config.get(CONNECTION_IDS, []))
        models = account_ids(config.get(MODEL_IDS, []))
        if set(connection) & set(models):
            raise ValueError("两类恢复账号不能重复")
    else:
        # Keep the original pre-selection migration for older installations.
        connection, models = sorted(eligible), []
    seen = set(account_ids(config[SEEN_IDS])) if SEEN_IDS in config else eligible
    newcomers = eligible - seen - set(connection) - set(models)
    payload = {**config, CONNECTION_IDS: sorted(set(connection) | newcomers), MODEL_IDS: models,
               SEEN_IDS: sorted(seen | eligible | set(connection) | set(models))}
    if payload != config:
        runtime.save_oauth_runtime_config(payload)
    if payload != config or any(getattr(runtime.settings, field, None) != payload[field]
                                for field in (CONNECTION_IDS, MODEL_IDS)):
        runtime.apply_oauth_runtime_config(payload)
    return payload != config
