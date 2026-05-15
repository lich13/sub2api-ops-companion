from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any, Callable
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit, urlunsplit
from urllib.request import Request, urlopen


class Sub2APISSOError(RuntimeError):
    def __init__(self, reason: str, message: str) -> None:
        super().__init__(message)
        self.reason = reason
        self.message = message


@dataclass(frozen=True)
class Sub2APISSOUser:
    id: int
    username: str
    role: str
    email: str = ""


def normalize_base_url(raw: str) -> str:
    candidate = str(raw or "").strip()
    if not candidate:
        raise Sub2APISSOError("missing_base_url", "Sub2API base URL is required")

    parts = urlsplit(candidate)
    if parts.scheme not in {"http", "https"} or not parts.netloc:
        raise Sub2APISSOError("invalid_base_url", "Sub2API base URL must be an absolute http(s) URL")

    path = parts.path.rstrip("/")
    return urlunsplit((parts.scheme, parts.netloc, path, "", ""))


def _extract_user_payload(payload: Any) -> dict[str, Any]:
    if not isinstance(payload, dict):
        raise Sub2APISSOError("invalid_response", "Sub2API auth response is not a JSON object")

    if "data" in payload:
        code = payload.get("code")
        if code not in (None, 0, "0"):
            raise Sub2APISSOError("invalid_response", "Sub2API auth response reports an error")
        data = payload.get("data")
        if not isinstance(data, dict):
            raise Sub2APISSOError("invalid_response", "Sub2API auth response data is not an object")
        return data

    return payload


def validate_sub2api_token(
    base_url: str,
    token: str,
    *,
    expected_user_id: str | None = None,
    required_role: str = "admin",
    timeout_seconds: int = 5,
    opener: Callable[..., Any] = urlopen,
) -> Sub2APISSOUser:
    raw_token = str(token or "").strip()
    if not raw_token:
        raise Sub2APISSOError("missing_token", "Sub2API token is required")

    normalized_base = normalize_base_url(base_url)
    request = Request(
        f"{normalized_base}/api/v1/auth/me",
        headers={
            "Authorization": f"Bearer {raw_token}",
            "Accept": "application/json",
        },
    )

    try:
        with opener(request, timeout=timeout_seconds) as response:
            body = response.read()
    except HTTPError as exc:
        if exc.code in (401, 403):
            raise Sub2APISSOError("upstream_unauthorized", "Sub2API rejected the token") from exc
        raise Sub2APISSOError("upstream_error", f"Sub2API auth lookup failed with HTTP {exc.code}") from exc
    except (URLError, TimeoutError, OSError) as exc:
        raise Sub2APISSOError("upstream_unreachable", "Unable to reach Sub2API for SSO validation") from exc

    try:
        payload = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise Sub2APISSOError("invalid_response", "Sub2API auth response is not valid JSON") from exc

    data = _extract_user_payload(payload)

    try:
        user = Sub2APISSOUser(
            id=int(data["id"]),
            username=str(data.get("username", "")).strip(),
            role=str(data.get("role", "")).strip(),
            email=str(data.get("email", "")).strip(),
        )
    except (KeyError, TypeError, ValueError) as exc:
        raise Sub2APISSOError("invalid_response", "Sub2API auth response is missing user fields") from exc

    if not user.username:
        raise Sub2APISSOError("invalid_response", "Sub2API auth response is missing username")

    if expected_user_id is not None and str(user.id) != str(expected_user_id).strip():
        raise Sub2APISSOError("user_mismatch", "Sub2API user id does not match the embedded menu user")

    normalized_required_role = str(required_role or "").strip()
    if (
        normalized_required_role
        and normalized_required_role != "*"
        and user.role.lower() != normalized_required_role.lower()
    ):
        raise Sub2APISSOError("forbidden_role", "Sub2API user role is not allowed to access Ops Companion")

    return user
