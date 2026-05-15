from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlencode
from urllib.request import Request, urlopen


TURNSTILE_VERIFY_URL = "https://challenges.cloudflare.com/turnstile/v0/siteverify"


@dataclass(frozen=True)
class TurnstileRuntimeConfig:
    enabled: bool = False
    site_key: str = ""
    secret_key: str = ""
    updated_at: str | None = None
    updated_by: str = ""


@dataclass(frozen=True)
class TurnstileVerifyResult:
    ok: bool
    reason: str
    detail: str = ""


def setting_bool(value: Any) -> bool:
    return str(value or "").strip().lower() in {"1", "true", "yes", "on"}


def clean_setting_value(value: Any) -> str:
    return str(value or "").strip()


def load_turnstile_runtime_config(path: str) -> TurnstileRuntimeConfig:
    try:
        raw = Path(path).read_text(encoding="utf-8")
        data = json.loads(raw)
    except (OSError, json.JSONDecodeError):
        return TurnstileRuntimeConfig()
    if not isinstance(data, dict):
        return TurnstileRuntimeConfig()
    return TurnstileRuntimeConfig(
        enabled=setting_bool(data.get("enabled")),
        site_key=clean_setting_value(data.get("site_key")),
        secret_key=clean_setting_value(data.get("secret_key")),
        updated_at=clean_setting_value(data.get("updated_at")) or None,
        updated_by=clean_setting_value(data.get("updated_by")),
    )


def build_turnstile_panel_config(config: TurnstileRuntimeConfig) -> dict[str, Any]:
    return {
        "enabled": config.enabled,
        "site_key": config.site_key,
        "site_key_set": bool(config.site_key),
        "secret_key_set": bool(config.secret_key),
        "updated_at": config.updated_at,
        "updated_by": config.updated_by,
    }


def save_turnstile_config(
    path: str,
    *,
    enabled: bool,
    site_key: str,
    secret_key: str | None,
    updated_by: str,
) -> TurnstileRuntimeConfig:
    existing = load_turnstile_runtime_config(path)
    updated = TurnstileRuntimeConfig(
        enabled=enabled,
        site_key=site_key.strip(),
        secret_key=secret_key.strip() if secret_key is not None else existing.secret_key,
        updated_at=datetime.now(timezone.utc).isoformat(),
        updated_by=updated_by,
    )
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(
        json.dumps(
            {
                "enabled": updated.enabled,
                "site_key": updated.site_key,
                "secret_key": updated.secret_key,
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


def verify_turnstile_token(
    config: TurnstileRuntimeConfig,
    *,
    token: str,
    remote_ip: str,
    timeout_seconds: int = 5,
    opener: Callable[..., Any] = urlopen,
) -> TurnstileVerifyResult:
    if not config.enabled:
        return TurnstileVerifyResult(True, "disabled")
    if not config.site_key or not config.secret_key:
        return TurnstileVerifyResult(False, "not_configured")

    clean_token = token.strip()
    if not clean_token:
        return TurnstileVerifyResult(False, "missing_token")

    payload: dict[str, str] = {
        "secret": config.secret_key,
        "response": clean_token,
    }
    if remote_ip.strip():
        payload["remoteip"] = remote_ip.strip()

    request = Request(
        TURNSTILE_VERIFY_URL,
        data=urlencode(payload).encode("utf-8"),
        headers={"Content-Type": "application/x-www-form-urlencoded"},
        method="POST",
    )
    try:
        with opener(request, timeout=timeout_seconds) as response:
            raw = response.read()
        data = json.loads(raw.decode("utf-8"))
    except (OSError, TimeoutError, json.JSONDecodeError) as exc:
        return TurnstileVerifyResult(False, "request_failed", str(exc))

    if data.get("success") is True:
        return TurnstileVerifyResult(True, "verified")

    error_codes = data.get("error-codes")
    if isinstance(error_codes, list):
        detail = ",".join(str(item) for item in error_codes)
    else:
        detail = ""
    return TurnstileVerifyResult(False, "verification_failed", detail)
