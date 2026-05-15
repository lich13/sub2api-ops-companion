from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .turnstile import clean_setting_value, setting_bool


@dataclass(frozen=True)
class SSORuntimeConfig:
    enabled: bool = False
    base_url: str = ""
    required_role: str = "admin"
    session_ttl_seconds: int = 86400
    verify_timeout_seconds: int = 5
    updated_at: str | None = None
    updated_by: str = ""


def _int_setting(value: Any, default: int, minimum: int, maximum: int) -> int:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        return default
    return min(max(parsed, minimum), maximum)


def _clean_base_url(value: Any) -> str:
    return clean_setting_value(value).rstrip("/")


def load_sso_runtime_config(
    path: str,
    *,
    env_enabled: bool = False,
    env_base_url: str = "",
    env_required_role: str = "admin",
    env_session_ttl_seconds: int = 86400,
    env_verify_timeout_seconds: int = 5,
) -> SSORuntimeConfig:
    defaults = SSORuntimeConfig(
        enabled=env_enabled,
        base_url=_clean_base_url(env_base_url),
        required_role=clean_setting_value(env_required_role) or "admin",
        session_ttl_seconds=_int_setting(env_session_ttl_seconds, 86400, 300, 604800),
        verify_timeout_seconds=_int_setting(env_verify_timeout_seconds, 5, 1, 20),
    )
    try:
        raw = Path(path).read_text(encoding="utf-8")
        data = json.loads(raw)
    except (OSError, json.JSONDecodeError):
        return defaults
    if not isinstance(data, dict):
        return defaults

    return SSORuntimeConfig(
        enabled=setting_bool(data.get("enabled", defaults.enabled)),
        base_url=_clean_base_url(data.get("base_url", defaults.base_url)),
        required_role=clean_setting_value(data.get("required_role", defaults.required_role)) or "admin",
        session_ttl_seconds=_int_setting(data.get("session_ttl_seconds"), defaults.session_ttl_seconds, 300, 604800),
        verify_timeout_seconds=_int_setting(data.get("verify_timeout_seconds"), defaults.verify_timeout_seconds, 1, 20),
        updated_at=clean_setting_value(data.get("updated_at")) or None,
        updated_by=clean_setting_value(data.get("updated_by")),
    )


def build_sso_panel_config(config: SSORuntimeConfig, *, base_path: str) -> dict[str, Any]:
    menu_url = ""
    if config.base_url:
        menu_url = f"{config.base_url}{base_path}/sso/start"
    return {
        "enabled": config.enabled,
        "base_url": config.base_url,
        "base_url_set": bool(config.base_url),
        "required_role": config.required_role,
        "session_ttl_seconds": config.session_ttl_seconds,
        "verify_timeout_seconds": config.verify_timeout_seconds,
        "updated_at": config.updated_at,
        "updated_by": config.updated_by,
        "menu_url": menu_url,
    }


def save_sso_config(
    path: str,
    *,
    enabled: bool,
    base_url: str,
    required_role: str,
    session_ttl_seconds: int,
    verify_timeout_seconds: int,
    updated_by: str,
) -> SSORuntimeConfig:
    updated = SSORuntimeConfig(
        enabled=enabled,
        base_url=_clean_base_url(base_url),
        required_role=required_role.strip() or "admin",
        session_ttl_seconds=_int_setting(session_ttl_seconds, 86400, 300, 604800),
        verify_timeout_seconds=_int_setting(verify_timeout_seconds, 5, 1, 20),
        updated_at=datetime.now(timezone.utc).isoformat(),
        updated_by=updated_by,
    )
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(
        json.dumps(
            {
                "enabled": updated.enabled,
                "base_url": updated.base_url,
                "required_role": updated.required_role,
                "session_ttl_seconds": updated.session_ttl_seconds,
                "verify_timeout_seconds": updated.verify_timeout_seconds,
                "updated_at": updated.updated_at,
                "updated_by": updated.updated_by,
            },
            ensure_ascii=True,
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    return updated
