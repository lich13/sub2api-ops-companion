"""Codex client identity shared by account-pinned upstream requests.

Sub2API keeps the Codex version and full User-Agent in its settings table.  The
Companion only reads those values; it never writes or derives identity from a
model-test result.  Keeping the formatting here makes model-test requests use
the same identity contract as the gateway.
"""
from __future__ import annotations

import re
from typing import Any

from .model_catalog import CLIENT_VERSION

_VERSION = re.compile(r"(?<![A-Za-z0-9])(?:v)?(\d+\.\d+\.\d+)(?![A-Za-z0-9])")
_LEADING = re.compile(r"(codex-tui/)\d+(?:\.\d+){2}", re.IGNORECASE)
_TRAILING = re.compile(r"(\(\s*codex-tui\s*;\s*)\d+(?:\.\d+){2}(\s*\))", re.IGNORECASE)
_MAX_USER_AGENT = 512


def valid_codex_version(value: Any) -> str | None:
    text = str(value or "").strip()
    match = _VERSION.fullmatch(text)
    return match.group(1) if match else None


def standard_codex_user_agent(version: str) -> str:
    return f"codex-tui/{version} (Ubuntu 22.4.0; x86_64) WindowsTerminal (codex-tui; {version})"


def synced_codex_user_agent(custom: Any, version: str) -> str:
    """Return a bounded full UA with every Codex version declaration synced."""
    text = str(custom or "").strip()
    if not text:
        return standard_codex_user_agent(version)
    text = text.replace("\r", " ").replace("\n", " ")[:_MAX_USER_AGENT].strip()
    text = _LEADING.sub(rf"\g<1>{version}", text, count=1)
    text = _TRAILING.sub(rf"\g<1>{version}\g<2>", text, count=1)
    return text or standard_codex_user_agent(version)


def choose_codex_version(values: dict[str, Any]) -> str:
    """Apply Sub2API's manual, synced, then built-in version precedence."""
    return (valid_codex_version(values.get("openai_codex_client_version"))
            or valid_codex_version(values.get("openai_codex_client_version_synced"))
            or valid_codex_version(CLIENT_VERSION)
            or "0.146.0")


def codex_identity(values: dict[str, Any]) -> tuple[str, str]:
    version = choose_codex_version(values)
    return version, synced_codex_user_agent(values.get("openai_codex_user_agent"), version)
