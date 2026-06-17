from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Settings:
    database_url: str
    session_secret: str
    session_ttl_seconds: int
    base_path: str
    audit_path: str
    session_store_path: str = "/data/sessions.json"
    app_name: str = "Sub2API Ops Companion"
    guard_enabled: bool = True
    guard_interval_seconds: int = 5
    guard_balance_error_threshold: int = 1
    guard_balance_error_max_age_hours: int = 24
    guard_quality_hours: int = 24
    guard_state_path: str = "/data/guard-state.json"
    guard_event_batch_size: int = 100
    usage_query_state_path: str = "/data/usage-query-state.json"
    telegram_config_path: str = "/data/telegram-config.json"
    telegram_enabled: bool = False
    telegram_bot_token: str = ""
    telegram_pairing_enabled: bool = True
    telegram_pairing_code: str = ""
    telegram_allowed_user_ids: tuple[int, ...] = ()
    telegram_allowed_chat_ids: tuple[int, ...] = ()
    telegram_state_path: str = "/data/telegram-state.json"
    telegram_default_group: str = "openai-default"
    telegram_default_platform: str = "openai"
    telegram_quality_hours: int = 24
    telegram_poll_timeout_seconds: int = 25
    telegram_error_alert_enabled: bool = True
    telegram_error_alert_interval_seconds: int = 2
    telegram_error_alert_batch_size: int = 50
    telegram_oauth_usage_refresh_enabled: bool = True
    telegram_oauth_recovery_monitor_enabled: bool = True
    telegram_oauth_recovery_push_enabled: bool = True
    telegram_oauth_usage_refresh_concurrency: int = 4
    telegram_oauth_recovery_test_concurrency: int = 2
    telegram_oauth_early_probe_interval_seconds: int = 15
    telegram_oauth_early_probe_batch_size: int = 8
    update_enabled: bool = True
    update_workdir: str = "/workspace"
    update_branch: str = "main"
    sso_config_path: str = "/data/sso-config.json"
    sub2api_base_url: str = ""
    sub2api_verify_base_url: str = ""
    sub2api_sso_enabled: bool = False
    sub2api_sso_required_role: str = "admin"
    sub2api_sso_session_ttl_seconds: int = 86400
    sub2api_sso_verify_timeout_seconds: int = 5


def bool_env(name: str, default: bool) -> bool:
    return bool_value(os.getenv(name), default)


def bool_value(raw: object, default: bool) -> bool:
    if raw is None:
        return default
    if isinstance(raw, bool):
        return raw
    return str(raw).strip().lower() in {"1", "true", "yes", "on"}


def int_env(name: str, default: int, minimum: int, maximum: int) -> int:
    return int_value(os.getenv(name), default, minimum, maximum)


def int_value(raw: object, default: int, minimum: int, maximum: int) -> int:
    if raw is None:
        return default
    try:
        parsed = int(str(raw).strip())
    except (TypeError, ValueError):
        return default
    return max(minimum, min(maximum, parsed))


def int_tuple_env(name: str) -> tuple[int, ...]:
    return int_tuple_value(os.getenv(name, ""))


def int_tuple_value(raw: object) -> tuple[int, ...]:
    values: list[int] = []
    if isinstance(raw, (list, tuple)):
        parts = raw
    else:
        parts = str(raw or "").replace(";", ",").split(",")
    for part in parts:
        item = str(part).strip()
        if not item:
            continue
        try:
            values.append(int(item))
        except ValueError:
            continue
    return tuple(dict.fromkeys(values))


def read_json_config(path: str) -> dict[str, object]:
    try:
        raw = Path(path).read_text(encoding="utf-8")
        data = json.loads(raw)
    except (OSError, json.JSONDecodeError):
        return {}
    return data if isinstance(data, dict) else {}


def load_settings() -> Settings:
    base_path = os.getenv("BASE_PATH", "/sub2ops").rstrip("/")
    if base_path == "":
        base_path = ""
    session_secret = os.getenv("OPS_SESSION_SECRET", "")
    if not session_secret:
        raise RuntimeError("OPS_SESSION_SECRET must be set")
    telegram_config_path = os.getenv("TELEGRAM_CONFIG_PATH", "/data/telegram-config.json")
    telegram_config = read_json_config(telegram_config_path)
    telegram_bot_token = str(telegram_config.get("bot_token", os.getenv("TELEGRAM_BOT_TOKEN", "")) or "")
    telegram_enabled_raw = telegram_config.get("enabled", os.getenv("TELEGRAM_ENABLED"))

    return Settings(
        database_url=os.environ["DATABASE_URL"],
        session_secret=session_secret,
        session_ttl_seconds=int_env("OPS_SESSION_TTL_SECONDS", 31536000, 300, 31536000),
        base_path=base_path,
        audit_path=os.getenv("AUDIT_PATH", "/data/audit.jsonl"),
        session_store_path=os.getenv("OPS_SESSION_STORE_PATH", "/data/sessions.json"),
        guard_enabled=bool_env("GUARD_ENABLED", True),
        guard_interval_seconds=int_env("GUARD_INTERVAL_SECONDS", 5, 1, 3600),
        guard_balance_error_threshold=int_env("GUARD_BALANCE_ERROR_THRESHOLD", 1, 1, 100),
        guard_balance_error_max_age_hours=int_env("GUARD_BALANCE_ERROR_MAX_AGE_HOURS", 24, 1, 720),
        guard_quality_hours=int_env("GUARD_QUALITY_HOURS", 24, 1, 720),
        guard_state_path=os.getenv("GUARD_STATE_PATH", "/data/guard-state.json"),
        guard_event_batch_size=int_env("GUARD_EVENT_BATCH_SIZE", 100, 1, 500),
        usage_query_state_path=os.getenv("USAGE_QUERY_STATE_PATH", "/data/usage-query-state.json"),
        telegram_config_path=telegram_config_path,
        telegram_enabled=bool_value(telegram_enabled_raw, bool(telegram_bot_token.strip())),
        telegram_bot_token=telegram_bot_token,
        telegram_pairing_enabled=bool_value(
            telegram_config.get("pairing_enabled", os.getenv("TELEGRAM_PAIRING_ENABLED")),
            True,
        ),
        telegram_pairing_code=str(telegram_config.get("pairing_code", os.getenv("TELEGRAM_PAIRING_CODE", "")) or ""),
        telegram_allowed_user_ids=int_tuple_value(
            telegram_config.get("allowed_user_ids", os.getenv("TELEGRAM_ALLOWED_USER_IDS", ""))
        ),
        telegram_allowed_chat_ids=int_tuple_value(
            telegram_config.get("allowed_chat_ids", os.getenv("TELEGRAM_ALLOWED_CHAT_IDS", ""))
        ),
        telegram_state_path=str(
            telegram_config.get("state_path", os.getenv("TELEGRAM_STATE_PATH", "/data/telegram-state.json")) or ""
        )
        or "/data/telegram-state.json",
        telegram_default_group=str(
            telegram_config.get("default_group", os.getenv("TELEGRAM_DEFAULT_GROUP", "openai-default")) or ""
        )
        or "openai-default",
        telegram_default_platform=str(
            telegram_config.get("default_platform", os.getenv("TELEGRAM_DEFAULT_PLATFORM", "openai")) or ""
        )
        or "openai",
        telegram_quality_hours=int_value(
            telegram_config.get("quality_hours", os.getenv("TELEGRAM_QUALITY_HOURS")),
            24,
            1,
            168,
        ),
        telegram_poll_timeout_seconds=int_value(
            telegram_config.get("poll_timeout_seconds", os.getenv("TELEGRAM_POLL_TIMEOUT_SECONDS")),
            25,
            5,
            50,
        ),
        telegram_error_alert_enabled=bool_value(
            telegram_config.get("error_alert_enabled", os.getenv("TELEGRAM_ERROR_ALERT_ENABLED")),
            True,
        ),
        telegram_error_alert_interval_seconds=int_value(
            telegram_config.get("error_alert_interval_seconds", os.getenv("TELEGRAM_ERROR_ALERT_INTERVAL_SECONDS")),
            2,
            1,
            60,
        ),
        telegram_error_alert_batch_size=int_value(
            telegram_config.get("error_alert_batch_size", os.getenv("TELEGRAM_ERROR_ALERT_BATCH_SIZE")),
            50,
            1,
            100,
        ),
        telegram_oauth_usage_refresh_enabled=bool_value(
            telegram_config.get("oauth_usage_refresh_enabled", os.getenv("TELEGRAM_OAUTH_USAGE_REFRESH_ENABLED")),
            True,
        ),
        telegram_oauth_recovery_monitor_enabled=bool_value(
            telegram_config.get("oauth_recovery_monitor_enabled", os.getenv("TELEGRAM_OAUTH_RECOVERY_MONITOR_ENABLED")),
            True,
        ),
        telegram_oauth_recovery_push_enabled=bool_value(
            telegram_config.get("oauth_recovery_push_enabled", os.getenv("TELEGRAM_OAUTH_RECOVERY_PUSH_ENABLED")),
            True,
        ),
        telegram_oauth_usage_refresh_concurrency=int_value(
            telegram_config.get(
                "oauth_usage_refresh_concurrency",
                os.getenv("TELEGRAM_OAUTH_USAGE_REFRESH_CONCURRENCY"),
            ),
            4,
            1,
            16,
        ),
        telegram_oauth_recovery_test_concurrency=int_value(
            telegram_config.get(
                "oauth_recovery_test_concurrency",
                os.getenv("TELEGRAM_OAUTH_RECOVERY_TEST_CONCURRENCY"),
            ),
            2,
            1,
            8,
        ),
        telegram_oauth_early_probe_interval_seconds=int_value(
            telegram_config.get(
                "oauth_early_probe_interval_seconds",
                os.getenv("TELEGRAM_OAUTH_EARLY_PROBE_INTERVAL_SECONDS"),
            ),
            15,
            5,
            3600,
        ),
        telegram_oauth_early_probe_batch_size=int_value(
            telegram_config.get(
                "oauth_early_probe_batch_size",
                os.getenv("TELEGRAM_OAUTH_EARLY_PROBE_BATCH_SIZE"),
            ),
            8,
            1,
            50,
        ),
        update_enabled=bool_env("OPS_UPDATE_ENABLED", True),
        update_workdir=os.getenv("OPS_UPDATE_WORKDIR", "/workspace"),
        update_branch=os.getenv("OPS_UPDATE_BRANCH", "main"),
        sso_config_path=os.getenv("OPS_SSO_CONFIG_PATH", "/data/sso-config.json"),
        sub2api_base_url=os.getenv("SUB2API_BASE_URL", "").rstrip("/"),
        sub2api_verify_base_url=os.getenv("SUB2API_VERIFY_BASE_URL", "").rstrip("/"),
        sub2api_sso_enabled=bool_env("SUB2API_SSO_ENABLED", False),
        sub2api_sso_required_role=os.getenv("SUB2API_SSO_REQUIRED_ROLE", "admin").strip() or "admin",
        sub2api_sso_session_ttl_seconds=int_env("SUB2API_SSO_SESSION_TTL_SECONDS", 86400, 300, 604800),
        sub2api_sso_verify_timeout_seconds=int_env("SUB2API_SSO_VERIFY_TIMEOUT_SECONDS", 5, 1, 20),
    )
