"""Recognize recorded upstream capacity errors, never request payloads."""
from __future__ import annotations

import json
import re
from typing import Any

MESSAGES = (
    "Our servers are currently overloaded. Please try again later.",
    "Selected model is at capacity. Please try a different model.",
    "stream disconnected before completion: Concurrency limit exceeded for account, please retry later",
)

# Sub2API emits this message for the local per-minute limiter.  Keep the
# matcher deliberately narrow: a generic 429, an upstream rate limit, or a
# request body containing similar text is not local evidence.
_LOCAL_THROTTLE_RE = re.compile(
    r"(?:您\s*已\s*(?:达到|超过)\s*请求(?:数|数量)\s*限制|"
    r"you\s*(?:have\s*)?(?:reached|exceeded)\s*(?:the\s*)?(?:request\s*)?(?:rate\s*)?limit)"
    r"[^\n]{0,120}?"
    r"(?:(?:1\s*分钟内|每\s*(?:1\s*)?分钟)\s*(?:最多|至多)?\s*请求\s*[0-9]+\s*次|[0-9]+\s*requests?\s*(?:per|\/|a)\s*minute)",
    re.IGNORECASE,
)
_LOCAL_THROTTLE_EN_RE = re.compile(
    r"(?:request\s*(?:rate\s*)?limit|maximum\s+(?:number\s+of\s+)?requests?|"
    r"requests?\s*(?:rate\s*)?limit)[^\n]{0,120}"
    r"(?:maximum\s*)?[0-9]+\s*(?:requests?|reqs?)\s*(?:per|\/|a)\s*minute",
    re.IGNORECASE,
)


def normalize(value: str) -> str:
    return " ".join(value.casefold().split()).rstrip(".!?。！？,，;；:： ")


def recorded_message(value: Any, *, plain: bool = False) -> str | None:
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except ValueError:
            return next((m for m in MESSAGES if plain and normalize(value) == normalize(m)), None)
    if isinstance(value, list):
        for item in value:
            found = recorded_message(item, plain=True)
            if found:
                return found
        return None
    if isinstance(value, dict):
        if isinstance(value.get("message"), str):
            found = recorded_message(value["message"], plain=True)
            if found:
                return found
        if isinstance(value.get("error_message"), str):
            found = recorded_message(value["error_message"], plain=True)
            if found:
                return found
        error = value.get("error")
        if isinstance(error, dict):
            return recorded_message(error.get("message"), plain=True)
        if value.get("type") in {"response.failed", "response.incomplete"}:
            return recorded_message(value.get("response"))
    return None


def _recorded_texts(value: Any) -> list[str]:
    """Return only structured error text, never arbitrary request JSON."""
    if isinstance(value, list):
        texts: list[str] = []
        for item in value:
            texts.extend(_recorded_texts(item))
        return texts
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except ValueError:
            return [value]
    if isinstance(value, dict):
        texts: list[str] = []
        if isinstance(value.get("message"), str):
            texts.append(value["message"])
        if isinstance(value.get("error_message"), str):
            texts.append(value["error_message"])
        error = value.get("error")
        if isinstance(error, dict) and isinstance(error.get("message"), str):
            texts.append(error["message"])
        if value.get("type") in {"response.failed", "response.incomplete"}:
            response = value.get("response")
            if isinstance(response, dict):
                nested = response.get("error")
                if isinstance(nested, dict) and isinstance(nested.get("message"), str):
                    texts.append(nested["message"])
        return texts
    return []


_LOCAL_CONCURRENCY = re.compile(
    r"(?:too\s+many\s+concurrent\s+requests\s+for\s+(?:user|client|api[ -]?key)|"
    r"(?:user|client|api[ -]?key)\s+concurrency\s+limit\s+exceeded|"
    r"(?:用户|客户端|密钥)\s*(?:并发请求|并发)\s*(?:数)?\s*(?:已)?(?:达到|超过|超出).{0,20}(?:限制|上限))", re.I)


def is_local_throttle(row: dict[str, Any]) -> bool:
    """Whether recorded structured fields prove a Companion local limiter hit."""
    # These are the fields populated from Sub2API's structured upstream error;
    # intentionally do not inspect request/prompt/content fields.
    for field in ("upstream_error_message", "error_message", "message", "upstream_errors", "error_body", "upstream_error_detail"):
        value = row.get(field)
        if isinstance(value, str):
            try:
                parsed = json.loads(value)
            except ValueError:
                values = [value]
            else:
                values = _recorded_texts(parsed)
        else:
            values = _recorded_texts(value)
        local_concurrency = row.get("error_owner") in {"client", "platform"} and row.get("error_phase") in {"concurrency", "rate_limit", "admission"}
        if any(_LOCAL_THROTTLE_RE.search(text) or _LOCAL_THROTTLE_EN_RE.search(text)
               or (local_concurrency and _LOCAL_CONCURRENCY.search(text)) for text in values):
            return True
    return False


def match_message(row: dict[str, Any]) -> str | None:
    if row.get("account_platform") != "openai" or row.get("account_type") not in {"oauth", "apikey"}:
        return None
    provider = row.get("error_owner") == "provider" and row.get("error_phase") == "upstream"
    wrapped = (row.get("error_owner") == "platform" and row.get("error_phase") == "internal"
               and row.get("error_source") == "gateway")
    if not provider and not wrapped:
        return None
    for field in ("upstream_error_message", "error_message", "error_body", "upstream_error_detail"):
        if wrapped and field == "error_message" and row.get("stream") is not True:
            continue
        if is_local_throttle({field: row.get(field)}):
            continue
        found = recorded_message(row.get(field), plain=field.endswith("_message"))
        if found:
            return found
    events = row.get("upstream_errors")
    if isinstance(events, list):
        for event in events:
            if not isinstance(event, dict) or is_local_throttle(event):
                continue
            found = recorded_message(event, plain=True)
            if found:
                return found
    return None


def _capacity_sql(alias: str = "e", *, gateway_only: bool = False) -> str:
    # These are error-response fields written by Sub2API's logger. The anchored
    # extractor never searches a nested request dump for an error/message key.
    fields = [f"CASE WHEN {alias}.stream IS TRUE OR {alias}.error_owner='provider' THEN {alias}.error_message END", f"{alias}.upstream_error_message"]
    for field in ("error_body", "upstream_error_detail"):
        fields.append("substring(" + alias + "." + field + r''' FROM '^\s*\{\s*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"')''')
        fields.append("CASE WHEN " + alias + "." + field + r''' ~ '^\s*\{[^{}]*"type"\s*:\s*"response\.(failed|incomplete)"' THEN substring('''
                      + alias + "." + field + r''' FROM '^\s*\{[^{}]*"response"\s*:\s*\{[^{}]*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"') END''')
    choices = ",".join("'" + normalize(m).replace("'", "''") + "'" for m in MESSAGES)
    matches = [f"regexp_replace(regexp_replace(lower(btrim({field})), '[[:space:]]+', ' ', 'g'), '[.!?。！？,，;；:：[:space:]]+$', '', 'g') IN ({choices})" for field in fields]
    event_fields = ["event->>'message'", "event->>'error_message'", "event->'error'->>'message'", "event->'response'->'error'->>'message'"]
    event_matches = [f"regexp_replace(regexp_replace(lower(btrim({field})), '[[:space:]]+', ' ', 'g'), '[.!?。！？,，;；:：[:space:]]+$', '', 'g') IN ({choices})" for field in event_fields]
    matches.append("EXISTS (SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(to_jsonb(" + alias
                   + ")->'upstream_errors')='array' THEN to_jsonb(" + alias + ")->'upstream_errors' ELSE '[]'::jsonb END) event WHERE "
                   + " OR ".join(event_matches) + ")")
    owner = f"({alias}.error_owner='platform' AND {alias}.error_phase='internal' AND {alias}.error_source='gateway')"
    if not gateway_only:
        owner = f"({owner} OR ({alias}.error_owner='provider' AND {alias}.error_phase='upstream'))"
    return (f"({alias}.account_id IS NOT NULL AND {owner} "
            f"AND EXISTS (SELECT 1 FROM accounts evidence_account WHERE evidence_account.id={alias}.account_id "
            "AND evidence_account.platform='openai' AND evidence_account.type IN ('oauth','apikey')) AND (" + " OR ".join(matches) + "))")


def gateway_capacity_sql(alias: str = "e") -> str:
    return _capacity_sql(alias, gateway_only=True)


def local_throttle_sql(alias: str = "e") -> str:
    """SQL predicate matching only structured local limiter messages."""
    fields = [
        f"CASE WHEN {alias}.upstream_error_message !~ '^\\s*\\{{' THEN {alias}.upstream_error_message END",
        f"CASE WHEN {alias}.error_message !~ '^\\s*\\{{' THEN {alias}.error_message END",
    ]
    for field in ("error_body", "upstream_error_detail"):
        fields.append("substring(" + alias + "." + field + r''' FROM '^\s*\{\s*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"')''')
        fields.append("CASE WHEN " + alias + "." + field + r''' ~ '^\s*\{[^{}]*"type"\s*:\s*"response\.(failed|incomplete)"' THEN substring('''
                      + alias + "." + field + r''' FROM '^\s*\{[^{}]*"response"\s*:\s*\{[^{}]*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"') END''')
    patterns = (
        r"您\s*已\s*(?:达到|超过)\s*请求(?:数|数量)\s*限制[^\n]{0,120}(?:1\s*分钟内|每\s*(?:1\s*)?分钟)\s*(?:最多|至多)?\s*请求\s*[0-9]+\s*次",
        r"you\s*(?:have\s*)?(?:reached|exceeded)\s*(?:the\s*)?(?:request\s*)?(?:rate\s*)?limit[^\n]{0,120}[0-9]+\s*(?:requests?|reqs?)\s*(?:per|/|a)\s*minute",
        r"(?:request\s*(?:rate\s*)?limit|maximum\s+(?:number\s+of\s+)?requests?|requests?\s*(?:rate\s*)?limit)[^\n]{0,120}(?:maximum\s*)?[0-9]+\s*(?:requests?|reqs?)\s*(?:per|/|a)\s*minute",
    )
    local_direct = "(" + " OR ".join(
        f"{field} ~* '{pattern.replace(chr(39), chr(39) * 2)}'" for field in fields for pattern in patterns
    ) + ")"
    event_fields = ["event->>'message'", "event->>'error_message'", "event->'error'->>'message'", "event->'response'->'error'->>'message'"]
    event_match = " OR ".join(
        f"{field} ~* '{pattern.replace(chr(39), chr(39) * 2)}'" for field in event_fields for pattern in patterns
    )
    local_array = ("EXISTS (SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(to_jsonb(" + alias
             + ")->'upstream_errors')='array' THEN to_jsonb(" + alias + ")->'upstream_errors' ELSE '[]'::jsonb END) event WHERE "
             + event_match + ")")
    concurrent = " OR ".join(f"{field} ~* '" + _LOCAL_CONCURRENCY.pattern.replace("'", "''") + "'" for field in fields)
    concurrent_array = ("EXISTS (SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(to_jsonb(" + alias
                        + ")->'upstream_errors')='array' THEN to_jsonb(" + alias + ")->'upstream_errors' ELSE '[]'::jsonb END) event WHERE "
                        + " OR ".join(f"{field} ~* '" + _LOCAL_CONCURRENCY.pattern.replace("'", "''") + "'" for field in event_fields) + ")")
    local = ("(" + local_direct + " OR " + local_array
             + f" OR ({alias}.error_owner IN ('client','platform') AND {alias}.error_phase IN ('concurrency','rate_limit','admission') AND ({concurrent} OR {concurrent_array})))")
    choices = ",".join("'" + normalize(m).replace("'", "''") + "'" for m in MESSAGES)
    capacity_direct = "(" + " OR ".join(
        f"regexp_replace(regexp_replace(lower(btrim({field})), '[[:space:]]+', ' ', 'g'), '[.!?。！？,，;；:：[:space:]]+$', '', 'g') IN ({choices})"
        for field in fields
    ) + ")"
    capacity_array = ("EXISTS (SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(to_jsonb(" + alias
                      + ")->'upstream_errors')='array' THEN to_jsonb(" + alias + ")->'upstream_errors' ELSE '[]'::jsonb END) event WHERE "
                      + " OR ".join(
                          f"regexp_replace(regexp_replace(lower(btrim({field})), '[[:space:]]+', ' ', 'g'), '[.!?。！？,，;；:：[:space:]]+$', '', 'g') IN ({choices})"
                          for field in event_fields
                      ) + ")")
    # A single log may contain multiple upstream events. Suppress a local-only
    # record, while keeping a row that also carries a real capacity message.
    return "(COALESCE(" + local + ", FALSE) AND NOT COALESCE(" + capacity_direct + " OR " + capacity_array + ", FALSE))"


ERROR_WHERE = "((e.account_id IS NOT NULL AND e.error_phase IN ('upstream', 'account_auth') AND e.error_owner='provider') OR " + gateway_capacity_sql() + ") AND NOT " + local_throttle_sql()


def error_category_sql(category: str | None = None) -> str:
    if category is None:
        return ERROR_WHERE
    if category not in {"degradation", "other"}:
        raise ValueError("未知错误分类")
    capacity = "COALESCE(" + _capacity_sql() + ", FALSE)"
    return "NOT COALESCE(" + local_throttle_sql() + ", FALSE) AND " + (capacity if category == "degradation" else "NOT " + capacity)
