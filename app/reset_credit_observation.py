"""Verified reset-credit observations; no network access or secret persistence."""
from __future__ import annotations

import hashlib
import json
import re
from datetime import datetime, timezone
from typing import Any

from .usage_query import parse_iso_datetime


def digest(value: Any) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def normalized_credits(value: Any) -> dict[str, Any] | None:
    if not isinstance(value, dict):
        return None
    count = value.get("available_count")
    if type(count) is not int or count < 0:
        return None
    cards = value.get("credits", [])
    if not isinstance(cards, list) or len(cards) > 10000:
        return None
    expirations = []
    for card in cards:
        expiry = parse_iso_datetime(card.get("expires_at")) if isinstance(card, dict) else None
        if expiry is None:
            return None
        expirations.append(expiry.astimezone(timezone.utc).isoformat())
    if count > len(expirations):
        return None
    # Ordering and extra upstream metadata (including card IDs) are irrelevant.
    return {"available_count": count, "expires_at": sorted(expirations)}


def limit_fingerprint(row: dict[str, Any]) -> str:
    return digest([str(row.get("id")), *[
        stamp.astimezone(timezone.utc).isoformat() if (stamp := parse_iso_datetime(row.get(key))) else None
        for key in ("rate_limited_at", "rate_limit_reset_at")]])


def validate_observation(value: Any) -> None:
    keys = {"version", "observed_at", "available_count", "expires_at", "snapshot_sha256",
            "credential_fingerprint", "window_reset_at", "window_minutes", "limit_fingerprint"}
    if not isinstance(value, dict) or set(value) != keys or type(value.get("version")) is not int or value["version"] != 1:
        raise ValueError("重置卡观测状态无效")
    if any(not isinstance(value.get(key), str) or not re.fullmatch(r"[0-9a-f]{64}", value[key])
           for key in ("snapshot_sha256", "credential_fingerprint", "limit_fingerprint")):
        raise ValueError("重置卡观测摘要无效")
    if any(parse_iso_datetime(value.get(key)) is None for key in ("observed_at", "window_reset_at")):
        raise ValueError("重置卡观测时间无效")
    minutes = value.get("window_minutes")
    if type(minutes) not in (int, float) or not 0 < minutes <= 5256000:
        raise ValueError("重置卡观测窗口无效")
    expires = value.get("expires_at")
    if not isinstance(expires, list):
        raise ValueError("重置卡到期信息无效")
    content = normalized_credits({"available_count": value.get("available_count"),
                                  "credits": [{"expires_at": stamp} for stamp in expires]})
    if content is None or content["expires_at"] != expires or digest(content) != value["snapshot_sha256"]:
        raise ValueError("重置卡观测内容无效")


def create_observation(row: dict[str, Any], content: dict[str, Any], observed: datetime,
                       credential: str, reset: datetime, minutes: float) -> dict[str, Any]:
    value = {"version": 1, "observed_at": observed.astimezone(timezone.utc).isoformat(), **content,
             "snapshot_sha256": digest(content), "credential_fingerprint": credential,
             "window_reset_at": reset.astimezone(timezone.utc).isoformat(), "window_minutes": minutes,
             "limit_fingerprint": limit_fingerprint(row)}
    validate_observation(value)
    return value


def receipt_diagnostics(status: int | None, data: Any) -> dict[str, Any]:
    """Never retain response text, credentials, headers or full card identifiers."""
    data = data if isinstance(data, dict) else {}
    out: dict[str, Any] = {"http_status": status}
    code = data.get("code")
    if type(code) is int:
        out["business_code"] = code
    elif isinstance(code, str):
        # Known generic results and namespaced machine codes only. Arbitrary
        # strings are not safe diagnostics even when they fit in a short field.
        out["business_code"] = code if (code in {"success", "ok", "error", "failed", "redeemed"}
            or re.fullmatch(r"OPENAI_[A-Z_]{1,56}", code)) else "unrecognized"
    if type(data.get("windows_reset")) is int:
        out["windows_reset"] = data["windows_reset"]
    for key in ("cache_persisted", "cache_refreshed", "account_state_recovered"):
        if type(data.get(key)) is bool:
            out[key] = data[key]
    return out
