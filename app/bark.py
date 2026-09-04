from __future__ import annotations

import ipaddress
import http.client
import json
import re
import socket
import threading
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Callable
from urllib.parse import quote, urlsplit, urlunsplit
from zoneinfo import ZoneInfo


DEFAULT_BARK_SERVER_URL = "https://api.day.app"
MAX_BARK_BODY_BYTES = 2400
MAX_ERROR_TEXT_LENGTH = 400
BEIJING_TZ = ZoneInfo("Asia/Shanghai")

_SECRET_ASSIGNMENT = re.compile(
    r"(?i)\b(authorization|api[ _-]?key|access[ _-]?(?:key|token)|private[ _-]?key|"
    r"token|secret|password)"
    r"(\s*[:=]\s*)([^\s,;]+)"
)
_BEARER_TOKEN = re.compile(r"(?i)\bbearer\s+[^\s,;]+")
_JWT = re.compile(r"\b[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b")
_QUERY_SECRET = re.compile(
    r"(?i)([?&](?:api_?key|access_?token|token|secret|password)=)[^&#\s]+"
)
_COOKIE_HEADER = re.compile(r"(?im)\b(set-cookie|cookie)(\s*:\s*)[^\r\n]+")
_URL_USERINFO = re.compile(r"(?i)\b(https?://)[^/@\s]+@")
_PRIVATE_KEY_BLOCK = re.compile(
    r"-----BEGIN [^-\r\n]*PRIVATE KEY-----.*?-----END [^-\r\n]*PRIVATE KEY-----",
    re.DOTALL,
)
_SENSITIVE_PAIR = re.compile(
    r"(?i)(?:[\"']?)"
    r"(authorization|proxy-authorization|x-api-key|x-auth-token|api[ _-]?key|"
    r"access[ _-]?(?:key|token)|private[ _-]?key|token|secret|password|"
    r"set-cookie|cookie)"
    r"(?:[\"']?)(\s*[:=]\s*)"
    r"(?:\"[^\"]*\"|'[^']*'|[^,;}\r\n]+)"
)
_SENSITIVE_NARRATIVE = re.compile(
    r"(?i)\b(api[ _-]?key|access[ _-]?(?:key|token)|private[ _-]?key|token|secret|password)\b"
    r"(\s+(?:provided|is|was|equals?|value(?:\s+is)?)\s*[:=]?\s*)"
    r"(?:\"[^\"]*\"|'[^']*'|[^\s,;}\]\)]+)"
)
_KNOWN_SECRET_TOKEN = re.compile(
    r"(?i)\b(?:sk-(?:proj-|live-|test-)?[A-Za-z0-9_-]{6,}|"
    r"gh[pousr]_[A-Za-z0-9_]{10,}|xox[abprs]-[A-Za-z0-9-]{10,})\b"
)


class _NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args: Any, **_kwargs: Any) -> None:
        return None


def _urlopen_no_redirect(request: Any, *, timeout: float) -> Any:
    return urllib.request.build_opener(_NoRedirectHandler()).open(request, timeout=timeout)


@dataclass(frozen=True)
class BarkPushResult:
    success: bool
    error_code: str = ""
    sent_parts: int = 0
    total_parts: int = 0


@dataclass(frozen=True)
class BarkRuntimeConfig:
    enabled: bool
    config_valid: bool
    device_key: str
    server_url: str


def normalize_bark_server_url(value: str) -> str:
    raw = str(value or "").strip()
    if not raw or any(char.isspace() or ord(char) < 0x20 or ord(char) == 0x7F for char in raw):
        raise ValueError("invalid_server_url")
    try:
        parsed = urlsplit(raw)
    except ValueError as exc:
        raise ValueError("invalid_server_url") from exc
    scheme = parsed.scheme.lower()
    if scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError("invalid_server_url")
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("invalid_server_url")
    try:
        parsed.port
    except ValueError as exc:
        raise ValueError("invalid_server_url") from exc
    if scheme == "http" and not _is_loopback_host(parsed.hostname):
        raise ValueError("invalid_server_url")
    path = parsed.path.rstrip("/")
    if re.search(r"%(?![0-9A-Fa-f]{2})", path):
        raise ValueError("invalid_server_url")
    path = quote(path, safe="/%:@!$&'()*+,;=-._~%")
    return urlunsplit((scheme, parsed.netloc, path, "", ""))


def bark_push_url(server_url: str) -> str:
    return f"{normalize_bark_server_url(server_url)}/push"


def split_utf8_chunks(text: str, max_bytes: int = MAX_BARK_BODY_BYTES) -> list[str]:
    if max_bytes <= 0:
        raise ValueError("max_bytes must be positive")
    if not text:
        return [""]
    chunks: list[str] = []
    current: list[str] = []
    current_bytes = 0
    for char in text:
        size = len(char.encode("utf-8"))
        if current and current_bytes + size > max_bytes:
            chunks.append("".join(current))
            current = []
            current_bytes = 0
        current.append(char)
        current_bytes += size
    if current:
        chunks.append("".join(current))
    return chunks


def bark_body_parts(text: str, max_bytes: int = MAX_BARK_BODY_BYTES) -> list[str]:
    chunks = split_utf8_chunks(text, max_bytes)
    if len(chunks) == 1:
        return chunks
    total = len(chunks)
    while True:
        prefix_bytes = len(f"第 {total}/{total} 段\n".encode("utf-8"))
        if prefix_bytes >= max_bytes:
            raise ValueError("max_bytes too small for part prefix")
        chunks = split_utf8_chunks(text, max_bytes - prefix_bytes)
        if len(chunks) == total:
            break
        total = len(chunks)
    return [f"第 {index}/{total} 段\n{chunk}" for index, chunk in enumerate(chunks, start=1)]


def sanitize_error_text(value: Any, limit: int = MAX_ERROR_TEXT_LENGTH) -> str:
    text = str(value or "-").replace("\x00", " ")
    text = _PRIVATE_KEY_BLOCK.sub("[REDACTED PRIVATE KEY]", text)
    text = _SENSITIVE_PAIR.sub(r"\1\2[REDACTED]", text)
    text = _SENSITIVE_NARRATIVE.sub(r"\1\2[REDACTED]", text)
    text = _COOKIE_HEADER.sub(r"\1\2[REDACTED]", text)
    text = _URL_USERINFO.sub(r"\1[REDACTED]@", text)
    text = _BEARER_TOKEN.sub("Bearer [REDACTED]", text)
    text = _JWT.sub("[REDACTED]", text)
    text = _QUERY_SECRET.sub(r"\1[REDACTED]", text)
    text = _SECRET_ASSIGNMENT.sub(r"\1\2[REDACTED]", text)
    text = _KNOWN_SECRET_TOKEN.sub("[REDACTED]", text)
    text = " ".join(text.split())
    encoded = text.encode("utf-8", errors="replace")
    if len(encoded) > limit:
        if limit <= 3:
            text = "." * max(0, limit)
        else:
            text = encoded[: limit - 3].decode("utf-8", errors="ignore") + "..."
    return text or "-"


def oauth_event_message(event: dict[str, Any]) -> tuple[str, str]:
    status = str(event.get("status") or "auth_failed")
    titles = {
        "recovered": "OAuth 账号额度已恢复可用",
        "test_failed": "OAuth 账号额度恢复后测试失败",
        "recovery_failed": "OAuth 账号自动恢复失败",
        "auth_failed": "OAuth 账号认证异常",
    }
    title = titles.get(status, titles["auth_failed"])
    windows = " / ".join(
        sanitize_error_text(item, 80) for item in (event.get("window_labels") or [])
    ) or "-"
    stage = "active usage" if event.get("stage") == "active_usage" else sanitize_error_text(
        event.get("stage"), 80
    )
    lines = [
        f"账号 ID：{int(event.get('account_id') or 0)}",
        f"名称：{sanitize_error_text(event.get('account_name'), 120)}",
        f"套餐：{sanitize_error_text(event.get('plan_type') or 'oauth', 80)}",
        f"窗口：{windows}",
        f"模型：{sanitize_error_text(event.get('model_id'), 120)}",
        f"错误码：{sanitize_error_text(event.get('error_code'), 120)}",
    ]
    if status in {"test_failed", "recovery_failed", "auth_failed"}:
        lines.append(f"错误：{sanitize_error_text(event.get('error'))}")
    if status == "auth_failed":
        lines.append(f"阶段：{stage}")
        lines.append(f"时间：{_beijing_time(event.get('checked_at'))}")
    return title, "\n".join(lines)


class BarkNotifier:
    def __init__(
        self,
        settings: Any,
        *,
        urlopen: Callable[..., Any] = _urlopen_no_redirect,
    ) -> None:
        self.settings = settings
        self._urlopen = urlopen
        self._config_lock = threading.Lock()
        self._runtime_config = BarkRuntimeConfig(
            enabled=bool(settings.bark_enabled),
            config_valid=bool(getattr(settings, "bark_config_valid", True)),
            device_key=str(settings.bark_device_key or ""),
            server_url=str(settings.bark_server_url or DEFAULT_BARK_SERVER_URL),
        )

    def configure_from_settings(self, settings: Any) -> None:
        runtime = BarkRuntimeConfig(
            enabled=bool(settings.bark_enabled),
            config_valid=bool(getattr(settings, "bark_config_valid", True)),
            device_key=str(settings.bark_device_key or ""),
            server_url=str(settings.bark_server_url or DEFAULT_BARK_SERVER_URL),
        )
        with self._config_lock:
            self._runtime_config = runtime

    def runtime_config(self) -> BarkRuntimeConfig:
        with self._config_lock:
            return self._runtime_config

    @property
    def enabled(self) -> bool:
        return self.runtime_config().enabled

    def notify_oauth_monitor_events(
        self,
        events: list[dict[str, Any]],
        *,
        config: BarkRuntimeConfig | None = None,
    ) -> list[dict[str, Any]]:
        runtime = config or self.runtime_config()
        if not runtime.config_valid or not runtime.enabled:
            return []
        delivered: list[dict[str, Any]] = []
        for event in events:
            try:
                title, body = oauth_event_message(event)
                if self._push_with_config(title, body, runtime, timeout=3).success:
                    delivered.append(event)
            except (TypeError, ValueError, UnicodeError):
                continue
        return delivered

    def push_test(self) -> BarkPushResult:
        return self.push(
            "Sub2API Ops Companion",
            "Bark 推送通道正常。",
            timeout=8,
        )

    def push(self, title: str, body: str, *, timeout: float = 3) -> BarkPushResult:
        return self._push_with_config(title, body, self.runtime_config(), timeout=timeout)

    def _push_with_config(
        self,
        title: str,
        body: str,
        config: BarkRuntimeConfig,
        *,
        timeout: float,
    ) -> BarkPushResult:
        if not config.config_valid:
            return BarkPushResult(False, "invalid_config")
        key = config.device_key.strip()
        if not key:
            return BarkPushResult(False, "missing_device_key")
        try:
            key.encode("utf-8")
        except UnicodeEncodeError:
            return BarkPushResult(False, "invalid_device_key_encoding")
        try:
            url = bark_push_url(config.server_url or DEFAULT_BARK_SERVER_URL)
        except ValueError:
            return BarkPushResult(False, "invalid_server_url")
        try:
            chunks = bark_body_parts(str(body or ""))
        except (UnicodeEncodeError, ValueError):
            return BarkPushResult(False, "invalid_payload_encoding")
        total = len(chunks)
        for index, chunk in enumerate(chunks, start=1):
            part_title = str(title)
            part_body = chunk
            if total > 1:
                part_title = f"{part_title} ({index}/{total})"
            result = self._push_part(
                url,
                key,
                part_title,
                part_body,
                timeout=timeout,
                sent_parts=index - 1,
                total_parts=total,
            )
            if not result.success:
                return result
        return BarkPushResult(True, sent_parts=total, total_parts=total)

    def _push_part(
        self,
        url: str,
        key: str,
        title: str,
        body: str,
        *,
        timeout: float,
        sent_parts: int,
        total_parts: int,
    ) -> BarkPushResult:
        try:
            data = json.dumps(
                {"device_key": key, "title": title, "body": body},
                ensure_ascii=False,
            ).encode("utf-8")
        except UnicodeEncodeError:
            return BarkPushResult(
                False, "invalid_payload_encoding", sent_parts, total_parts
            )
        try:
            request = urllib.request.Request(
                url,
                data=data,
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            with self._urlopen(request, timeout=timeout) as response:
                status_value = getattr(response, "status", None)
                if status_value is None:
                    status_value = response.getcode()
                status = int(status_value)
                raw = response.read(65537)
        except urllib.error.HTTPError:
            return BarkPushResult(False, "http_status", sent_parts, total_parts)
        except (http.client.InvalidURL, UnicodeError, ValueError):
            return BarkPushResult(False, "invalid_server_url", sent_parts, total_parts)
        except (TimeoutError, socket.timeout):
            return BarkPushResult(False, "timeout", sent_parts, total_parts)
        except urllib.error.URLError as exc:
            code = "timeout" if isinstance(exc.reason, (TimeoutError, socket.timeout)) else "request_error"
            return BarkPushResult(False, code, sent_parts, total_parts)
        except OSError:
            return BarkPushResult(False, "request_error", sent_parts, total_parts)
        except http.client.HTTPException:
            return BarkPushResult(False, "request_error", sent_parts, total_parts)
        if status < 200 or status >= 300:
            return BarkPushResult(False, "http_status", sent_parts, total_parts)
        if len(raw) > 65536:
            return BarkPushResult(False, "response_decode", sent_parts, total_parts)
        try:
            payload = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            return BarkPushResult(False, "response_decode", sent_parts, total_parts)
        if not isinstance(payload, dict) or payload.get("code") != 200:
            return BarkPushResult(False, "bark_response_code", sent_parts, total_parts)
        return BarkPushResult(True, sent_parts=sent_parts + 1, total_parts=total_parts)


def _is_loopback_host(hostname: str) -> bool:
    host = hostname.rstrip(".").lower()
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def _beijing_time(value: Any) -> str:
    if value in (None, ""):
        return "-"
    if isinstance(value, datetime):
        parsed = value
    else:
        try:
            parsed = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
        except ValueError:
            return sanitize_error_text(value, 120)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(BEIJING_TZ).strftime("%Y-%m-%d %H:%M")
