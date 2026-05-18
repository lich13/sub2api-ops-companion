from __future__ import annotations

import re
from typing import Any


CLIENT_BAD_REQUEST_TERMS = (
    "input must be a list",
    "instructions are required",
)

BALANCE_OR_QUOTA_TERMS = (
    "用户额度不足",
    "额度不足",
    "额度已用尽",
    "令牌额度已用尽",
    "预扣费额度失败",
    "剩余额度",
    "insufficient_user_quota",
    "insufficient balance",
    "insufficient_balance",
    "not enough credits",
    "pre_consume_token_quota_failed",
    "token quota is not enough",
    "quota exceeded",
)

RATE_LIMIT_TERMS = (
    "rate limit",
    "too many pending",
)

UNSTABLE_TERMS = (
    "terminal event",
    "missing terminal event",
    "truncated",
)

NEGATIVE_REMAIN_QUOTA_RE = re.compile(r"RemainQuota\s*=\s*-", re.IGNORECASE)


def _text(row: dict[str, Any]) -> str:
    parts = [
        row.get("kind"),
        row.get("message"),
        row.get("search_text"),
        row.get("error_message"),
        row.get("error_body"),
    ]
    return " ".join(str(part or "") for part in parts)


def classify_guard_event(row: dict[str, Any]) -> str:
    text = _text(row)
    lower = text.lower()
    status_code = int(row.get("status_code") or row.get("upstream_status_code") or 0)

    if not row.get("account_id"):
        return "client_pre_route"
    if row.get("error_owner") == "client" or row.get("error_source") == "client_request":
        return "client_request"
    if status_code == 400 and any(term in lower for term in CLIENT_BAD_REQUEST_TERMS):
        return "client_bad_request"
    if any(term in lower for term in BALANCE_OR_QUOTA_TERMS) or NEGATIVE_REMAIN_QUOTA_RE.search(text):
        return "provider_balance_or_quota"
    if status_code == 403 and "blocked" in lower:
        return "provider_blocked_403"
    if status_code == 429 or any(term in lower for term in RATE_LIMIT_TERMS) or ("quota" in lower and "remainquota" not in lower):
        return "provider_rate_limit"
    if 500 <= status_code <= 599 or any(term in lower for term in UNSTABLE_TERMS):
        return "upstream_unstable_5xx_stream"
    return "account_other_error"
