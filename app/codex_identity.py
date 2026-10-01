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

_VERSION = re.compile(r"[0-9]+(?:\.[0-9]+){1,3}(?:-[0-9A-Za-z.]+)?")
_OFFICIAL = {"codex_cli_rs", "codex-tui", "codex_vscode", "codex_vscode_copilot",
             "codex_app", "codex_chatgpt_desktop", "codex_atlas", "codex_exec", "codex_sdk_ts"}
_MAX_USER_AGENT = 512


def valid_codex_version(value: Any) -> str | None:
    text = str(value or "").strip()
    return text if len(text) <= 64 and _VERSION.fullmatch(text) else None


def standard_codex_user_agent(version: str) -> str:
    return f"codex-tui/{version} (Ubuntu 22.4.0; x86_64) xterm-256color"


def official_originator(value: str) -> str | None:
    if not value or len(value) > 64 or any(ord(c) < 32 or ord(c) > 126 for c in value):
        return None
    return value.lower() if value.lower() in _OFFICIAL else value if value.lower().startswith("codex ") else None


def codex_originator(user_agent: str) -> str:
    return official_originator(user_agent.split('/', 1)[0].strip()) or "codex-tui"


def synced_codex_user_agent(custom: Any, version: str) -> str:
    """Return a bounded full UA with every Codex version declaration synced."""
    text = str(custom or "")
    if not text or len(text) > _MAX_USER_AGENT or any(ord(c) < 32 or ord(c) > 126 for c in text):
        return standard_codex_user_agent(version)
    text = text.strip()
    client, slash, rest = text.partition('/')
    trailer = re.search(r"\(([^();/]+);\s*[^()]*\)\s*$", text)
    originator = official_originator(client.strip()) or (official_originator(trailer[1].strip()) if trailer else None)
    if not slash or not rest.strip() or not originator:
        return standard_codex_user_agent(version)
    tail = rest[rest.index(' '):] if ' ' in rest else ''
    text = f"{originator}/{version}{tail}"
    trailer = re.search(r"\(([^();/]+);\s*[^()]*\)\s*$", text)
    if trailer and official_originator(trailer[1].strip()):
        text = text[:trailer.start()] + f"({trailer[1].strip()}; {version})"
    return text


def choose_codex_version(values: dict[str, Any]) -> str:
    """Apply Sub2API's manual, synced, then built-in version precedence."""
    version = (valid_codex_version(values.get("openai_codex_client_version"))
            or valid_codex_version(values.get("openai_codex_client_version_synced"))
            or valid_codex_version(CLIENT_VERSION)
            or "0.146.0")
    parts = tuple(int(v) for v in version.split('-')[0].split('.'))
    return version if (parts + (0, 0, 0))[:3] >= (0, 144, 0) else CLIENT_VERSION


def codex_identity(values: dict[str, Any]) -> tuple[str, str]:
    version = choose_codex_version(values)
    return version, synced_codex_user_agent(values.get("openai_codex_user_agent"), version)
