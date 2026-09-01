from __future__ import annotations

import json
import math
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone
from typing import Any, Callable
from urllib.parse import urlsplit


UsageOpener = Callable[[dict[str, Any], int], Any]
OAUTH_QUOTA_WINDOW_FIELDS = (
    (
        "codex_5h",
        "5h",
        "codex_5h_used_percent",
        "codex_5h_reset_at",
        "codex_5h_reset_after_seconds",
        "codex_5h_window_minutes",
    ),
    (
        "codex_7d",
        "7d",
        "codex_7d_used_percent",
        "codex_7d_reset_at",
        "codex_7d_reset_after_seconds",
        "codex_7d_window_minutes",
    ),
)


class UsageQueryError(ValueError):
    pass


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def normalize_timeout(value: object) -> int:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        parsed = 10
    return max(2, min(30, parsed))


def numeric_or_none(value: object) -> float | None:
    if value is None or isinstance(value, bool):
        return None
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return None
    return parsed if math.isfinite(parsed) else None


def percent_or_none(value: object) -> float | None:
    if isinstance(value, str):
        value = value.strip().removesuffix("%").strip()
    return numeric_or_none(value)


def clamp_percent(value: object) -> float:
    numeric = numeric_or_none(value)
    if numeric is None:
        return 0.0
    return max(0.0, min(100.0, numeric))


def json_object(value: Any) -> dict[str, Any]:
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError:
            return {}
        return parsed if isinstance(parsed, dict) else {}
    return {}


def first_string(payloads: list[dict[str, Any]], keys: tuple[str, ...]) -> str:
    for payload in payloads:
        for key in keys:
            value = payload.get(key)
            if value in (None, ""):
                continue
            text = str(value).strip()
            if text:
                return text
    return ""


def parse_iso_datetime(value: object) -> datetime | None:
    if value in (None, ""):
        return None
    if isinstance(value, datetime):
        parsed = value
    else:
        try:
            parsed = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
        except ValueError:
            return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def normalize_oauth_plan_type(value: object) -> str:
    text = str(value or "").strip()
    for prefix in ("计划 ", "Codex ", "codex "):
        if text.startswith(prefix):
            text = text[len(prefix) :].strip()
    if not text:
        return "oauth"
    lowered = text.lower()
    if lowered in {"free", "plus", "pro", "team", "enterprise", "oauth"}:
        return lowered
    return text


def oauth_plan_type(account_row: dict[str, Any] | None) -> str:
    row = account_row or {}
    credentials = json_object(row.get("credentials"))
    extra = json_object(row.get("extra"))
    value = first_string([credentials], ("plan_type", "chatgpt_plan_type"))
    if not value:
        value = first_string([extra], ("plan_type",))
    return normalize_oauth_plan_type(value or "oauth")


def required_oauth_window_keys(plan_type: object) -> tuple[str, ...]:
    return ("codex_7d",) if normalize_oauth_plan_type(plan_type) == "free" else ("codex_5h", "codex_7d")


def oauth_reset_at(
    explicit_reset_at: str,
    reset_after_seconds: float | None,
    updated_at: str,
    now: datetime | None = None,
) -> str:
    if explicit_reset_at:
        return explicit_reset_at
    if reset_after_seconds is None:
        return ""
    base = parse_iso_datetime(updated_at) or now
    if base is None:
        return ""
    return (base.astimezone(timezone.utc) + timedelta(seconds=max(0, int(reset_after_seconds)))).isoformat()


def oauth_quota_windows(account_row: dict[str, Any] | None, *, now: datetime | None = None) -> dict[str, Any]:
    row = account_row or {}
    extra = json_object(row.get("extra"))
    plan_type = oauth_plan_type(row)
    updated_at = first_string([extra], ("codex_usage_updated_at",))
    windows: list[dict[str, Any]] = []
    for key, label, used_field, reset_field, reset_after_field, window_minutes_field in OAUTH_QUOTA_WINDOW_FIELDS:
        if plan_type == "free" and key == "codex_5h":
            continue
        used_percent = percent_or_none(extra.get(used_field))
        if used_percent is None:
            continue
        reset_after_seconds = numeric_or_none(extra.get(reset_after_field))
        window_minutes = numeric_or_none(extra.get(window_minutes_field))
        reset_at = oauth_reset_at(
            first_string([extra], (reset_field,)),
            reset_after_seconds,
            updated_at,
            now,
        )
        item: dict[str, Any] = {
            "key": key,
            "label": label,
            "used_percent": clamp_percent(used_percent),
            "remaining_percent": clamp_percent(100 - used_percent),
            "depleted": used_percent >= 100,
        }
        if reset_at:
            item["reset_at"] = reset_at
        if reset_after_seconds is not None:
            item["reset_after_seconds"] = int(reset_after_seconds)
        if window_minutes is not None:
            item["window_minutes"] = int(window_minutes)
        windows.append(item)
    return {
        "plan_type": plan_type,
        "updated_at": updated_at,
        "ui_windows": windows,
        "telegram_windows": [item for item in windows if float(item["used_percent"]) < 100],
    }


def oauth_windows_by_key(raw_windows: Any) -> dict[str, dict[str, Any]]:
    if not isinstance(raw_windows, list):
        return {}
    windows: dict[str, dict[str, Any]] = {}
    for raw in raw_windows:
        if not isinstance(raw, dict):
            continue
        key = str(raw.get("key") or "").strip()
        label = str(raw.get("label") or "").strip().lower()
        if not key:
            key = "codex_5h" if label == "5h" else "codex_7d" if label == "7d" else ""
        if key:
            windows[key] = dict(raw)
    return windows


def sanitize_oauth_quota_summary(
    account_row: dict[str, Any] | None,
    summary: dict[str, Any],
) -> dict[str, Any]:
    plan_type = oauth_plan_type(account_row)
    sanitized = dict(summary)
    sanitized["plan_type"] = plan_type
    windows = [dict(item) for item in sanitized.get("ui_windows") or [] if isinstance(item, dict)]
    if plan_type == "free":
        windows = [item for item in windows if item.get("key") != "codex_5h"]
    sanitized["ui_windows"] = windows
    sanitized["telegram_windows"] = [
        item for item in windows if (percent_or_none(item.get("used_percent")) or 0) < 100
    ]
    return sanitized


def oauth_quota_summary_from_result(
    account_row: dict[str, Any] | None,
    result: dict[str, Any] | None = None,
) -> dict[str, Any]:
    if isinstance(result, dict) and result.get("success"):
        data = result.get("data")
        if isinstance(data, dict) and (
            isinstance(data.get("five_hour"), dict) or isinstance(data.get("seven_day"), dict)
        ):
            return oauth_quota_from_usage_data(
                data,
                account_row,
                now=parse_iso_datetime(result.get("queried_at")),
            )
        cached = result.get("oauth_quota")
        if isinstance(cached, dict):
            return sanitize_oauth_quota_summary(account_row, cached)
    return oauth_quota_windows(account_row)


def oauth_quota_from_usage_data(
    data: dict[str, Any],
    account_row: dict[str, Any] | None = None,
    *,
    now: datetime | None = None,
) -> dict[str, Any]:
    row = dict(account_row or {})
    extra = json_object(row.get("extra"))
    merged_extra = dict(extra)
    for source_key, prefix in (("five_hour", "codex_5h"), ("seven_day", "codex_7d")):
        source = data.get(source_key)
        if not isinstance(source, dict):
            continue
        used_percent = source.get("utilization", source.get("used_percent", source.get("used")))
        if used_percent is not None:
            merged_extra[f"{prefix}_used_percent"] = used_percent
        reset_at = first_string([source], ("resets_at", "reset_at"))
        if reset_at:
            merged_extra[f"{prefix}_reset_at"] = reset_at
        reset_after = source.get("remaining_seconds", source.get("reset_after_seconds"))
        if reset_after is not None:
            merged_extra[f"{prefix}_reset_after_seconds"] = reset_after
        window_stats = json_object(source.get("window_stats"))
        window_minutes = source.get("window_minutes", window_stats.get("window_minutes"))
        if window_minutes is not None:
            merged_extra[f"{prefix}_window_minutes"] = window_minutes
    merged_extra["codex_usage_updated_at"] = (now or datetime.now(timezone.utc)).astimezone(timezone.utc).isoformat()
    row["extra"] = merged_extra
    return oauth_quota_windows(row, now=now)


def oauth_has_available_seven_day(summary: dict[str, Any] | None) -> bool:
    if not isinstance(summary, dict):
        return False
    seven_day = oauth_windows_by_key(summary.get("ui_windows")).get("codex_7d")
    if not seven_day:
        return False
    used = percent_or_none(seven_day.get("used_percent"))
    return used is not None and used < 100


def oauth_recovery_transition(
    previous_summary: dict[str, Any] | None,
    refreshed_summary: dict[str, Any] | None,
    *,
    now: datetime | None = None,
) -> dict[str, Any] | None:
    if not isinstance(previous_summary, dict) or not isinstance(refreshed_summary, dict):
        return None
    plan_type = normalize_oauth_plan_type(refreshed_summary.get("plan_type") or "oauth")
    required_keys = required_oauth_window_keys(plan_type)
    previous_windows = oauth_windows_by_key(previous_summary.get("ui_windows"))
    refreshed_windows = oauth_windows_by_key(refreshed_summary.get("ui_windows"))
    refreshed_required: list[dict[str, Any]] = []
    previously_full: list[dict[str, Any]] = []
    for key in required_keys:
        previous = previous_windows.get(key)
        refreshed = refreshed_windows.get(key)
        if not refreshed:
            return None
        refreshed_used = percent_or_none(refreshed.get("used_percent"))
        if refreshed_used is None or refreshed_used >= 100:
            return None
        refreshed_required.append(refreshed)
        if previous and (percent_or_none(previous.get("used_percent")) or 0) >= 100:
            previously_full.append(previous)
    if not previously_full or not oauth_has_available_seven_day(refreshed_summary):
        return None

    current = (now or datetime.now(timezone.utc)).astimezone(timezone.utc)
    reset_values = [str(item.get("reset_at") or "").strip() for item in previously_full]
    reset_values = [item for item in reset_values if item]
    fingerprint = "|".join(reset_values) or f"transition:{current.isoformat()}"
    seven_before = previous_windows.get("codex_7d") or {}
    seven_after = refreshed_windows.get("codex_7d") or {}
    old_seven_used = percent_or_none(seven_before.get("used_percent"))
    new_seven_used = percent_or_none(seven_after.get("used_percent"))
    old_reset_at = str(seven_before.get("reset_at") or "")
    old_reset_time = parse_iso_datetime(old_reset_at)
    early_reset = bool(
        old_seven_used is not None
        and old_seven_used >= 100
        and new_seven_used is not None
        and new_seven_used < 100
        and old_reset_time is not None
        and current < old_reset_time
    )
    candidate: dict[str, Any] = {
        "plan_type": plan_type,
        "windows": refreshed_required,
        "window_labels": [str(item.get("label") or "-") for item in refreshed_required],
        "trigger_window_labels": [str(item.get("label") or "-") for item in previously_full],
        "fingerprint": f"early:{old_reset_at}" if early_reset else fingerprint,
        "reset_at": max(reset_values) if reset_values else current.isoformat(),
        "remaining_summary": " / ".join(
            f"{item.get('label') or '-'} {format_percent_value(item.get('remaining_percent'))}"
            for item in refreshed_required
        ),
    }
    if early_reset:
        candidate.update(
            {
                "early_reset_detected": True,
                "old_7d_used_percent": old_seven_used,
                "new_7d_used_percent": new_seven_used,
                "old_reset_at": old_reset_at,
                "detected_at": current.isoformat(),
            }
        )
    return candidate


def format_percent_value(value: object) -> str:
    numeric = numeric_or_none(value)
    if numeric is None:
        return "-"
    return f"{numeric:.2f}".rstrip("0").rstrip(".") + "%"


def execute_oauth_usage_query(
    account_id: int,
    base_url: str,
    admin_token: str,
    *,
    account_row: dict[str, Any] | None = None,
    opener: UsageOpener | None = None,
    timeout_seconds: int = 10,
    now: datetime | None = None,
) -> dict[str, Any]:
    query_now = (now or datetime.now(timezone.utc)).astimezone(timezone.utc)
    queried_at = query_now.isoformat()
    account_id = int(account_id)
    base = str(base_url or "").strip().rstrip("/")
    token = str(admin_token or "").strip()
    if not base or not token:
        return build_oauth_failure_result(
            account_id,
            "缺少 Sub2API 地址或 Admin API Key",
            queried_at,
            account_row,
            error_code="missing_sub2api_admin_credentials",
        )
    request = {
        "url": f"{base}/api/v1/admin/accounts/{account_id}/usage?source=active&force=true",
        "method": "GET",
        "headers": {"Accept": "application/json", "x-api-key": token},
    }
    try:
        validate_oauth_usage_request(request)
        payload = (opener or open_usage_request)(request, normalize_timeout(timeout_seconds))
        data = oauth_usage_payload_data(payload)
        summary = oauth_quota_from_usage_data(data, account_row, now=query_now)
        return {
            "account_id": account_id,
            "template_type": "oauth",
            "success": True,
            "data": data,
            "error": "",
            "queried_at": queried_at,
            "unit": "%",
            "plan_name": summary.get("plan_type", "oauth"),
            "oauth_quota": summary,
            "source": "sub2api_admin_usage",
        }
    except Exception as exc:
        return build_oauth_failure_result(
            account_id,
            str(exc),
            queried_at,
            account_row,
            error_code=usage_query_error_code(exc),
        )


def build_oauth_failure_result(
    account_id: int,
    error: str,
    queried_at: str,
    account_row: dict[str, Any] | None = None,
    *,
    error_code: str = "",
) -> dict[str, Any]:
    return {
        "account_id": int(account_id),
        "template_type": "oauth",
        "success": False,
        "data": {},
        "error": error,
        "queried_at": queried_at,
        "unit": "%",
        "oauth_quota": oauth_quota_windows(account_row),
        "source": "sub2api_admin_usage",
        "error_code": error_code or usage_query_error_code(error),
    }


def usage_query_error_code(error: object) -> str:
    if isinstance(error, TimeoutError):
        return "timeout"
    if isinstance(error, urllib.error.HTTPError):
        return f"http_{int(error.code)}"
    if isinstance(error, urllib.error.URLError):
        return "network_error"
    text = str(error or "").strip()
    upper = text.upper()
    if upper.startswith("HTTP "):
        digits = ""
        for char in text[5:]:
            if char.isdigit():
                digits += char
            else:
                break
        if digits:
            return f"http_{digits}"
    lowered = text.lower()
    if "timeout" in lowered or "timed out" in lowered or "超时" in text:
        return "timeout"
    if "network" in lowered or "urlerror" in lowered or "请求失败" in text:
        return "network_error"
    return "usage_query_error"


def validate_oauth_usage_request(request: dict[str, Any]) -> None:
    parsed = urlsplit(str(request.get("url") or ""))
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise UsageQueryError("Sub2API 地址必须是 http/https 完整 URL")


def oauth_usage_payload_data(payload: Any) -> dict[str, Any]:
    if not isinstance(payload, dict):
        raise UsageQueryError("Sub2API usage 响应必须是对象")
    if "data" not in payload:
        return payload
    code = payload.get("code")
    if payload.get("success") is False or code not in (None, 0, "0"):
        raise UsageQueryError(str(payload.get("message") or payload.get("error") or "Sub2API usage 查询失败"))
    data = payload.get("data")
    if not isinstance(data, dict):
        raise UsageQueryError("Sub2API usage 响应缺少 data 对象")
    return data


def open_usage_request(request: dict[str, Any], timeout_seconds: int) -> Any:
    req = urllib.request.Request(
        str(request["url"]),
        headers={str(key): str(value) for key, value in dict(request.get("headers") or {}).items()},
        method=str(request.get("method") or "GET").upper(),
    )
    try:
        with urllib.request.urlopen(req, timeout=normalize_timeout(timeout_seconds)) as response:
            raw = response.read().decode("utf-8")
    except urllib.error.HTTPError as exc:
        raise UsageQueryError(f"HTTP {exc.code}") from exc
    except urllib.error.URLError as exc:
        if isinstance(exc.reason, TimeoutError):
            raise TimeoutError("请求超时") from exc
        raise
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise UsageQueryError("响应不是有效 JSON") from exc
