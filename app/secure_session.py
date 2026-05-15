from __future__ import annotations

import hashlib
import hmac
import json
import secrets
import tempfile
import threading
import time
from dataclasses import asdict, dataclass
from pathlib import Path


SESSION_COOKIE_PREFIX = "v2."
_LOCK = threading.Lock()


@dataclass(frozen=True)
class SessionPayload:
    username: str
    issued_at: int
    expires_at: int
    source: str = "password"


def _session_key(session_id: str, secret: str) -> str:
    return hmac.new(secret.encode("utf-8"), session_id.encode("utf-8"), hashlib.sha256).hexdigest()


def _cookie_signature(session_id: str, secret: str) -> str:
    return hmac.new(
        secret.encode("utf-8"),
        f"sub2ops-session:{session_id}".encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()


def _read_store(path: Path) -> dict[str, dict[str, object]]:
    try:
        raw = path.read_text(encoding="utf-8")
        data = json.loads(raw)
    except (OSError, json.JSONDecodeError):
        return {}
    return data if isinstance(data, dict) else {}


def _write_store(path: Path, data: dict[str, dict[str, object]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=path.parent, delete=False) as tmp:
        json.dump(data, tmp, ensure_ascii=False, separators=(",", ":"))
        tmp_path = Path(tmp.name)
    tmp_path.replace(path)


def _payload_from_dict(data: dict[str, object]) -> SessionPayload | None:
    try:
        username = str(data["username"]).strip()
        issued_at = int(data["issued_at"])
        expires_at = int(data["expires_at"])
        source = str(data.get("source", "password")).strip() or "password"
    except (KeyError, TypeError, ValueError):
        return None
    if not username or issued_at < 0 or expires_at <= issued_at:
        return None
    return SessionPayload(username=username, issued_at=issued_at, expires_at=expires_at, source=source)


def _cleanup_expired(store: dict[str, dict[str, object]], now: int) -> dict[str, dict[str, object]]:
    return {
        key: value
        for key, value in store.items()
        if isinstance(value, dict) and (payload := _payload_from_dict(value)) is not None and payload.expires_at >= now
    }


def create_session_cookie(
    username: str,
    secret: str,
    *,
    store_path: str,
    issued_at: int | None = None,
    ttl_seconds: int,
    source: str = "password",
) -> str:
    issued = int(time.time()) if issued_at is None else int(issued_at)
    session_id = secrets.token_urlsafe(32)
    payload = SessionPayload(
        username=username,
        issued_at=issued,
        expires_at=issued + int(ttl_seconds),
        source=source,
    )
    path = Path(store_path)
    with _LOCK:
        store = _cleanup_expired(_read_store(path), issued)
        store[_session_key(session_id, secret)] = asdict(payload)
        _write_store(path, store)
    return f"{SESSION_COOKIE_PREFIX}{session_id}.{_cookie_signature(session_id, secret)}"


def read_session_cookie(
    raw: str | None,
    secret: str,
    *,
    store_path: str,
    max_age_seconds: int,
    now: int | None = None,
) -> SessionPayload | None:
    if not raw or not raw.startswith(SESSION_COOKIE_PREFIX):
        return None

    current_time = int(time.time()) if now is None else int(now)
    token = raw.removeprefix(SESSION_COOKIE_PREFIX)
    parts = token.rsplit(".", 1)
    if len(parts) != 2:
        return None

    session_id, signature = parts
    if not session_id or not signature:
        return None
    if not secrets.compare_digest(signature, _cookie_signature(session_id, secret)):
        return None

    path = Path(store_path)
    with _LOCK:
        store = _read_store(path)
        payload = _payload_from_dict(store.get(_session_key(session_id, secret), {}))
        if payload is None:
            return None
        if current_time > payload.expires_at or current_time - payload.issued_at > int(max_age_seconds):
            cleaned = _cleanup_expired(store, current_time)
            if len(cleaned) != len(store):
                _write_store(path, cleaned)
            return None
        return payload
