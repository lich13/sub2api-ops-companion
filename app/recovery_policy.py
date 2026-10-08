"""Explicit account selection for automatic recovery; no upstream requests."""
from __future__ import annotations

CONNECTION_IDS = "oauth_recovery_connection_account_ids"
MODEL_IDS = "oauth_recovery_model_account_ids"


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


def migrate_recovery_selection(runtime):
    from . import account_ops
    config = runtime.oauth_config_file()
    if CONNECTION_IDS in config or MODEL_IDS in config:
        connection = account_ids(config.get(CONNECTION_IDS, []))
        models = account_ids(config.get(MODEL_IDS, []))
        if set(connection) & set(models):
            raise ValueError("两类恢复账号不能重复")
        runtime.apply_oauth_runtime_config({CONNECTION_IDS: connection, MODEL_IDS: models})
        return False
    rows = account_ops.current_oauth_accounts(runtime.db)
    connection = sorted({int(row["id"]) for row in rows
                         if row.get("platform") == "openai" and row.get("type") == "oauth"
                         and not row.get("deleted_at") and not row.get("parent_account_id")
                         and not (row.get("extra") or {}).get("parent_account_id")})
    payload = {**config, CONNECTION_IDS: connection, MODEL_IDS: []}
    runtime.save_oauth_runtime_config(payload)
    runtime.apply_oauth_runtime_config(payload)
    return True
