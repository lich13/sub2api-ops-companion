from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass
from pathlib import Path

from .bark import DEFAULT_BARK_SERVER_URL, normalize_bark_server_url


@dataclass
class Settings:
    database_url: str
    base_path: str
    audit_path: str
    app_name: str = "Sub2API Ops Companion"
    usage_query_state_path: str = "/data/usage-query-state.json"
    oauth_config_path: str = "/data/oauth-config.json"
    bark_config_path: str = "/data/bark-config.json"
    key_fallback_config_path: str = "/data/key-fallback-config.json"
    bark_enabled: bool = False
    bark_device_key: str = ""
    bark_server_url: str = "https://api.day.app"
    bark_config_valid: bool = True
    oauth_recovery_monitor_enabled: bool = True
    oauth_daily_test_enabled: bool = True
    oauth_daily_test_time: str = "05:00"
    oauth_usage_refresh_concurrency: int = 4
    oauth_recovery_test_concurrency: int = 2
    oauth_early_probe_batch_size: int = 8
    oauth_recovery_test_model_id: str = "gpt-5.6-luna"
    sub2api_config_path: str = "/data/sub2api-config.json"
    model_config_path: str = "/data/group-model-config.json"
    sub2api_base_url: str = ""
    sub2api_verify_base_url: str = ""


def daily_test_time(value: object) -> str:
    text = str(value or "").strip()
    if not re.fullmatch(r"(?:[01][0-9]|2[0-3]):[0-5][0-9]", text):
        raise ValueError("测活时间必须为 00:00–23:59（北京时间）")
    return text


def load_daily_test_time(value: object) -> str:
    try:
        return daily_test_time(value)
    except ValueError:
        return "05:00"


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
    oauth_config_path = os.getenv("OAUTH_CONFIG_PATH", "/data/oauth-config.json")
    oauth_config = read_json_config(oauth_config_path)
    bark_config_path = os.getenv("BARK_CONFIG_PATH", "/data/bark-config.json")
    bark_config, bark_config_valid = read_optional_json_config(bark_config_path)
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
        base_path=base_path,
        audit_path=os.getenv("AUDIT_PATH", "/data/audit.jsonl"),
        usage_query_state_path=os.getenv("USAGE_QUERY_STATE_PATH", "/data/usage-query-state.json"),
        oauth_config_path=oauth_config_path,
        bark_config_path=bark_config_path,
        key_fallback_config_path=os.getenv(
            "KEY_FALLBACK_CONFIG_PATH", "/data/key-fallback-config.json"
        ),
        bark_enabled=bark_enabled,
        bark_device_key=bark_device_key,
        bark_server_url=bark_server_url,
        bark_config_valid=bark_config_valid,
        oauth_recovery_monitor_enabled=bool_value(
            oauth_config.get(
                "oauth_recovery_monitor_enabled", os.getenv("OAUTH_RECOVERY_MONITOR_ENABLED")
            ),
            True,
        ),
        oauth_daily_test_enabled=bool_value(
            oauth_config.get("oauth_daily_test_enabled", os.getenv("OAUTH_DAILY_TEST_ENABLED")), True
        ),
        oauth_daily_test_time=load_daily_test_time(
            oauth_config.get("oauth_daily_test_time", os.getenv("OAUTH_DAILY_TEST_TIME", "05:00"))
        ),
        oauth_usage_refresh_concurrency=int_value(
            oauth_config.get(
                "oauth_usage_refresh_concurrency", os.getenv("OAUTH_USAGE_REFRESH_CONCURRENCY")
            ),
            4,
            1,
            16,
        ),
        oauth_recovery_test_concurrency=int_value(
            oauth_config.get(
                "oauth_recovery_test_concurrency", os.getenv("OAUTH_RECOVERY_TEST_CONCURRENCY")
            ),
            2,
            1,
            8,
        ),
        oauth_early_probe_batch_size=int_value(
            oauth_config.get(
                "oauth_early_probe_batch_size", os.getenv("OAUTH_EARLY_PROBE_BATCH_SIZE")
            ),
            8,
            1,
            50,
        ),
        oauth_recovery_test_model_id=str(
            oauth_config.get(
                "oauth_recovery_test_model_id",
                os.getenv("OAUTH_RECOVERY_TEST_MODEL_ID", "gpt-5.6-luna"),
            )
            or "gpt-5.6-luna"
        ).strip()
        or "gpt-5.6-luna",
        sub2api_config_path=os.getenv("SUB2API_CONFIG_PATH", "/data/sub2api-config.json"),
        model_config_path=os.getenv("GROUP_MODEL_CONFIG_PATH", "/data/group-model-config.json"),
        sub2api_base_url=os.getenv("SUB2API_BASE_URL", "").rstrip("/"),
        sub2api_verify_base_url=os.getenv("SUB2API_VERIFY_BASE_URL", "").rstrip("/"),
    )
