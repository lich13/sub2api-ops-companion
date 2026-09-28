"""Sub2API connection settings, independent of browser authentication."""
from __future__ import annotations

from urllib.parse import urlsplit

from .settings import read_json_config


def normalize_url(value: object) -> str:
    text = str(value or "").strip().rstrip("/")
    if not text:
        return ""
    parsed = urlsplit(text)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("Sub2API 服务地址无效")
    return text


def connection_config(settings) -> dict[str, str]:
    config = read_json_config(settings.sub2api_config_path)
    return {
        "base_url": normalize_url(config.get("base_url", settings.sub2api_base_url)),
        "verify_base_url": normalize_url(config.get("verify_base_url", settings.sub2api_verify_base_url)),
    }
