from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path

from .bark import DEFAULT_BARK_SERVER_URL, normalize_bark_server_url


@dataclass
class Settings:
    database_url: str
    session_secret: str
    session_ttl_seconds: int
    base_path: str
    audit_path: str
    session_store_path: str = "/data/sessions.json"
    app_name: str = "Sub2API Ops Companion"
    usage_query_state_path: str = "/data/usage-query-state.json"
    telegram_config_path: str = "/data/telegram-config.json"
    bark_config_path: str = "/data/bark-config.json"
    bark_enabled: bool = False
    bark_device_key: str = ""
    bark_server_url: str = "https://api.day.app"
    bark_config_valid: bool = True
    telegram_enabled: bool = False
    telegram_bot_token: str = ""
    telegram_pairing_enabled: bool = True
    telegram_pairing_code: str = ""
    telegram_allowed_user_ids: tuple[int, ...] = ()
    telegram_allowed_chat_ids: tuple[int, ...] = ()
    telegram_state_path: str = "/data/telegram-state.json"
    telegram_poll_timeout_seconds: int = 25
    telegram_oauth_usage_refresh_enabled: bool = True
    telegram_oauth_recovery_monitor_enabled: bool = True
    telegram_oauth_night_recovery_cooldown_enabled: bool = True
    telegram_oauth_usage_refresh_concurrency: int = 4
    telegram_oauth_recovery_test_concurrency: int = 2
    telegram_oauth_early_probe_batch_size: int = 8
    telegram_oauth_regular_refresh_interval_seconds: int = 3600
    telegram_oauth_7d_probe_interval_seconds: int = 3600
    telegram_oauth_recovery_test_model_id: str = "gpt-5.6-luna"
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


def strict_bool_value(raw: object, default: bool) -> tuple[bool, bool]:
    if raw is None:
        return default, True
    if isinstance(raw, bool):
        return raw, True
    normalized = str(raw).strip().lower()
    if normalized in {"1", "true", "yes", "on"}:
        return True, True
    if normalized in {"0", "false", "no", "off"}:
        return False, True
    return default, False


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


def int_tuple_value(raw: object) -> tuple[int, ...]:
    parts = raw if isinstance(raw, (list, tuple)) else str(raw or "").replace(";", ",").split(",")
    values: list[int] = []
    for part in parts:
        try:
            values.append(int(str(part).strip()))
        except (TypeError, ValueError):
            continue
    return tuple(dict.fromkeys(values))


def read_json_config(path: str) -> dict[str, object]:
    try:
        data = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        return {}
    return data if isinstance(data, dict) else {}


def read_optional_json_config(path: str) -> tuple[dict[str, object], bool]:
    config_path = Path(path)
    try:
        raw = config_path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return {}, True
    except (OSError, UnicodeError):
        return {}, False
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return {}, False
    return (data, True) if isinstance(data, dict) else ({}, False)


def bark_config_schema_valid(config: dict[str, object], parsed: bool) -> bool:
    if not parsed:
        return False
    expected_types = {
        "enabled": bool,
        "device_key": str,
        "server_url": str,
    }
    for key, expected_type in expected_types.items():
        if key in config and not isinstance(config[key], expected_type):
            return False
    return True


def load_settings() -> Settings:
    base_path = os.getenv("BASE_PATH", "/sub2ops").rstrip("/")
    session_secret = os.getenv("OPS_SESSION_SECRET", "")
    if not session_secret:
        raise RuntimeError("OPS_SESSION_SECRET must be set")
    telegram_config_path = os.getenv("TELEGRAM_CONFIG_PATH", "/data/telegram-config.json")
    telegram_config = read_json_config(telegram_config_path)
    bark_config_path = os.getenv("BARK_CONFIG_PATH", "/data/bark-config.json")
    bark_config, bark_config_valid = read_optional_json_config(bark_config_path)
    bot_token = str(telegram_config.get("bot_token", os.getenv("TELEGRAM_BOT_TOKEN", "")) or "")
    enabled_raw = telegram_config.get("enabled", os.getenv("TELEGRAM_ENABLED"))
    bark_device_key = str(
        bark_config.get("device_key", os.getenv("BARK_DEVICE_KEY", "")) or ""
    )
    bark_server_url = str(
        bark_config.get("server_url", os.getenv("BARK_SERVER_URL", DEFAULT_BARK_SERVER_URL))
        or DEFAULT_BARK_SERVER_URL
    ).strip() or DEFAULT_BARK_SERVER_URL
    bark_enabled, bark_enabled_valid = strict_bool_value(
        bark_config.get("enabled", os.getenv("BARK_ENABLED")), False
    )
    bark_config_valid = bark_config_schema_valid(bark_config, bark_config_valid)
    bark_config_valid = bark_config_valid and bark_enabled_valid
    if bark_config_valid:
        try:
            normalize_bark_server_url(bark_server_url)
        except ValueError:
            bark_config_valid = False

    return Settings(
        database_url=os.environ["DATABASE_URL"],
        session_secret=session_secret,
        session_ttl_seconds=int_env("OPS_SESSION_TTL_SECONDS", 31536000, 300, 31536000),
        base_path=base_path,
        audit_path=os.getenv("AUDIT_PATH", "/data/audit.jsonl"),
        session_store_path=os.getenv("OPS_SESSION_STORE_PATH", "/data/sessions.json"),
        usage_query_state_path=os.getenv("USAGE_QUERY_STATE_PATH", "/data/usage-query-state.json"),
        telegram_config_path=telegram_config_path,
        bark_config_path=bark_config_path,
        bark_enabled=bark_enabled,
        bark_device_key=bark_device_key,
        bark_server_url=bark_server_url,
        bark_config_valid=bark_config_valid,
        telegram_enabled=bool_value(enabled_raw, bool(bot_token.strip())),
        telegram_bot_token=bot_token,
        telegram_pairing_enabled=bool_value(
            telegram_config.get("pairing_enabled", os.getenv("TELEGRAM_PAIRING_ENABLED")), True
        ),
        telegram_pairing_code=str(
            telegram_config.get("pairing_code", os.getenv("TELEGRAM_PAIRING_CODE", "")) or ""
        ),
        telegram_allowed_user_ids=int_tuple_value(
            telegram_config.get("allowed_user_ids", os.getenv("TELEGRAM_ALLOWED_USER_IDS", ""))
        ),
        telegram_allowed_chat_ids=int_tuple_value(
            telegram_config.get("allowed_chat_ids", os.getenv("TELEGRAM_ALLOWED_CHAT_IDS", ""))
        ),
        telegram_state_path=str(
            telegram_config.get("state_path", os.getenv("TELEGRAM_STATE_PATH", "/data/telegram-state.json"))
            or "/data/telegram-state.json"
        ),
        telegram_poll_timeout_seconds=int_value(
            telegram_config.get("poll_timeout_seconds", os.getenv("TELEGRAM_POLL_TIMEOUT_SECONDS")),
            25,
            5,
            50,
        ),
        telegram_oauth_usage_refresh_enabled=bool_value(
            telegram_config.get("oauth_usage_refresh_enabled", os.getenv("TELEGRAM_OAUTH_USAGE_REFRESH_ENABLED")),
            True,
        ),
        telegram_oauth_recovery_monitor_enabled=bool_value(
            telegram_config.get(
                "oauth_recovery_monitor_enabled", os.getenv("TELEGRAM_OAUTH_RECOVERY_MONITOR_ENABLED")
            ),
            True,
        ),
        telegram_oauth_night_recovery_cooldown_enabled=bool_value(
            telegram_config.get(
                "oauth_night_recovery_cooldown_enabled",
                os.getenv("TELEGRAM_OAUTH_NIGHT_RECOVERY_COOLDOWN_ENABLED"),
            ),
            True,
        ),
        telegram_oauth_usage_refresh_concurrency=int_value(
            telegram_config.get(
                "oauth_usage_refresh_concurrency", os.getenv("TELEGRAM_OAUTH_USAGE_REFRESH_CONCURRENCY")
            ),
            4,
            1,
            16,
        ),
        telegram_oauth_recovery_test_concurrency=int_value(
            telegram_config.get(
                "oauth_recovery_test_concurrency", os.getenv("TELEGRAM_OAUTH_RECOVERY_TEST_CONCURRENCY")
            ),
            2,
            1,
            8,
        ),
        telegram_oauth_early_probe_batch_size=int_value(
            telegram_config.get(
                "oauth_early_probe_batch_size", os.getenv("TELEGRAM_OAUTH_EARLY_PROBE_BATCH_SIZE")
            ),
            8,
            1,
            50,
        ),
        telegram_oauth_regular_refresh_interval_seconds=int_value(
            telegram_config.get(
                "oauth_regular_refresh_interval_seconds",
                os.getenv("TELEGRAM_OAUTH_REGULAR_REFRESH_INTERVAL_SECONDS"),
            ),
            3600,
            60,
            86400,
        ),
        telegram_oauth_7d_probe_interval_seconds=int_value(
            telegram_config.get(
                "oauth_7d_probe_interval_seconds",
                os.getenv("TELEGRAM_OAUTH_7D_PROBE_INTERVAL_SECONDS"),
            ),
            3600,
            60,
            86400,
        ),
        telegram_oauth_recovery_test_model_id=str(
            telegram_config.get(
                "oauth_recovery_test_model_id",
                os.getenv("TELEGRAM_OAUTH_RECOVERY_TEST_MODEL_ID", "gpt-5.6-luna"),
            )
            or "gpt-5.6-luna"
        ).strip()
        or "gpt-5.6-luna",
        update_enabled=bool_env("OPS_UPDATE_ENABLED", True),
        update_workdir=os.getenv("OPS_UPDATE_WORKDIR", "/workspace"),
        update_branch=os.getenv("OPS_UPDATE_BRANCH", "main"),
        sso_config_path=os.getenv("OPS_SSO_CONFIG_PATH", "/data/sso-config.json"),
        sub2api_base_url=os.getenv("SUB2API_BASE_URL", "").rstrip("/"),
        sub2api_verify_base_url=os.getenv("SUB2API_VERIFY_BASE_URL", "").rstrip("/"),
        sub2api_sso_enabled=bool_env("SUB2API_SSO_ENABLED", False),
        sub2api_sso_required_role=os.getenv("SUB2API_SSO_REQUIRED_ROLE", "admin").strip() or "admin",
        sub2api_sso_session_ttl_seconds=int_env(
            "SUB2API_SSO_SESSION_TTL_SECONDS", 86400, 300, 604800
        ),
        sub2api_sso_verify_timeout_seconds=int_env(
            "SUB2API_SSO_VERIFY_TIMEOUT_SECONDS", 5, 1, 20
        ),
    )
