"""Recognize recorded upstream capacity errors, never request payloads."""
from __future__ import annotations

import json
from typing import Any

MESSAGES = (
    "Our servers are currently overloaded. Please try again later.",
    "Selected model is at capacity. Please try a different model.",
    "stream disconnected before completion: Concurrency limit exceeded for account, please retry later",
)


def normalize(value: str) -> str:
    return " ".join(value.casefold().split()).rstrip(".!?。！？,，;；:： ")


def recorded_message(value: Any, *, plain: bool = False) -> str | None:
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except ValueError:
            return next((m for m in MESSAGES if plain and normalize(value) == normalize(m)), None)
    if isinstance(value, dict):
        error = value.get("error")
        if isinstance(error, dict):
            return recorded_message(error.get("message"), plain=True)
        if value.get("type") in {"response.failed", "response.incomplete"}:
            return recorded_message(value.get("response"))
    return None


def match_message(row: dict[str, Any]) -> str | None:
    if (row.get("account_platform"), row.get("account_type")) != ("openai", "oauth"):
        return None
    provider = row.get("error_owner") == "provider" and row.get("error_phase") == "upstream"
    wrapped = (row.get("error_owner") == "platform" and row.get("error_phase") == "internal"
               and row.get("error_source") == "gateway")
    if not provider and not wrapped:
        return None
    for field in ("upstream_error_message", "error_message", "error_body", "upstream_error_detail"):
        if wrapped and field == "error_message" and row.get("stream") is not True:
            continue
        found = recorded_message(row.get(field), plain=field.endswith("_message"))
        if found:
            return found
    return None


def gateway_capacity_sql(alias: str = "e") -> str:
    # These are error-response fields written by Sub2API's logger. The anchored
    # extractor never searches a nested request dump for an error/message key.
    fields = [f"CASE WHEN {alias}.stream IS TRUE THEN {alias}.error_message END", f"{alias}.upstream_error_message"]
    for field in ("error_body", "upstream_error_detail"):
        fields.append("substring(" + alias + "." + field + r''' FROM '^\s*\{\s*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"')''')
        fields.append("CASE WHEN " + alias + "." + field + r''' ~ '^\s*\{[^{}]*"type"\s*:\s*"response\.(failed|incomplete)"' THEN substring('''
                      + alias + "." + field + r''' FROM '^\s*\{[^{}]*"response"\s*:\s*\{[^{}]*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"') END''')
    choices = ",".join("'" + normalize(m).replace("'", "''") + "'" for m in MESSAGES)
    matches = [f"regexp_replace(regexp_replace(lower(btrim({field})), '[[:space:]]+', ' ', 'g'), '[.!?。！？,，;；:：[:space:]]+$', '', 'g') IN ({choices})" for field in fields]
    return (f"({alias}.account_id IS NOT NULL "
            f"AND {alias}.error_owner='platform' AND {alias}.error_phase='internal' AND {alias}.error_source='gateway' "
            f"AND EXISTS (SELECT 1 FROM accounts evidence_account WHERE evidence_account.id={alias}.account_id "
            "AND evidence_account.platform='openai' AND evidence_account.type='oauth') AND (" + " OR ".join(matches) + "))")


ERROR_WHERE = "(e.account_id IS NOT NULL AND e.error_phase IN ('upstream', 'account_auth') AND e.error_owner='provider') OR " + gateway_capacity_sql()
ERROR_WHERE = "(" + ERROR_WHERE + ")"
