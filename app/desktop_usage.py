"""Sub2API usage-cell projections. Reads saved evidence; never calls an upstream.

Stats and branches follow Sub2API a3eb7ef302961cba716dc78b39b93b60c467db0e.
Only explicitly selected fields leave this module, including for action responses.
"""
from __future__ import annotations

import json
from datetime import datetime, timedelta
from typing import Any
from zoneinfo import ZoneInfo

from .quota_snapshot import _window, number, usage_windows
from .usage_query import parse_iso_datetime

BEIJING = ZoneInfo("Asia/Shanghai")

STATS_SQL = """
SELECT w.account_id,w.key,s.* FROM jsonb_to_recordset(%(windows)s::jsonb)
 AS w(account_id bigint,key text,start_at timestamptz)
LEFT JOIN LATERAL (
 SELECT count(*) AS requests,
 coalesce(sum(input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens),0) AS tokens,
 coalesce(sum(coalesce(account_stats_cost,total_cost) * coalesce(account_rate_multiplier,1)),0) AS cost,
 coalesce(sum(total_cost),0) AS standard_cost, coalesce(sum(actual_cost),0) AS user_cost
 FROM usage_logs WHERE account_id=w.account_id AND created_at >= w.start_at
) s ON true
"""


def obj(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def reset_credits(extra: dict[str, Any], now: datetime) -> dict[str, Any]:
    snapshot = obj(extra.get("codex_reset_credit_snapshot"))
    count = number(snapshot.get("available_count"))
    expirations = []
    alive = 0
    credits = snapshot.get("credits")
    if isinstance(credits, list):
        for item in credits:
            if not isinstance(item, dict):
                continue
            expires = parse_iso_datetime(item.get("expires_at"))
            if expires and expires <= now:
                continue
            alive += 1
            if expires:
                expirations.append(expires.isoformat())
        if credits and count is not None:
            count = min(count, alive)
    return {"available": int(count) if count is not None else None,
            "expires_at": sorted(set(expirations)),
            "observed_at": parse_iso_datetime(snapshot.get("fetched_at") or snapshot.get("updated_at"))}


def is_grok_free(extra: dict[str, Any], billing: dict[str, Any]) -> bool:
    def free(value: str) -> bool:
        return "free" in value or "basic" in value
    def paid(value: str) -> bool:
        return bool(value and not free(value) and "unknown" not in value)
    tier = str(extra.get("subscription_tier") or "").strip().lower()
    plan = str(billing.get("plan") or "").strip().lower()
    if free(tier):
        return True
    if paid(tier) or any(number(billing.get(k)) is not None for k in ("usage_percent", "used_percent")) or (number(billing.get("monthly_limit_cents")) or 0) > 0:
        return False
    if paid(plan):
        return False
    return free(plan) or free(str(extra.get("grok_entitlement_status") or "").lower()) or bool(billing)


def project_usage(row: dict[str, Any], now: datetime, result: dict[str, Any] | None = None) -> dict[str, Any]:
    extra = obj(row.get("extra"))
    platform, kind = row.get("platform"), row.get("type")
    value: dict[str, Any] = {"branch": "none", "windows": [], "today": None,
                             "actions": [], "reset_credits": None}
    if platform not in {"openai", "grok"}:
        return value
    if kind == "apikey":
        value["branch"] = "apikey"
        value["today_start"] = now.astimezone(BEIJING).replace(hour=0, minute=0, second=0, microsecond=0).isoformat()
        for key, label, prefix, duration, color in (
            ("daily", "1d", "quota_daily", 1, "indigo"),
            ("weekly", "7d", "quota_weekly", 7, "emerald"),
            ("total", "总", "quota", 0, "purple"),
        ):
            limit = number(extra.get(prefix + "_limit"))
            if not limit:
                continue
            # Sub2API GetQuota*Used defaults absent counters to zero.
            used = number(extra.get(prefix + "_used")) if prefix + "_used" in extra else 0
            reset = None
            if duration:
                if extra.get(prefix + "_reset_mode") == "fixed":
                    reset = parse_iso_datetime(extra.get(prefix + "_reset_at"))
                else:
                    start = parse_iso_datetime(extra.get(prefix + "_start"))
                    reset = start + timedelta(days=duration) if start else None
            window = _window(key, label, used / limit * 100 if used is not None else None,
                             reset, now, now, source="configured_quota")
            window.update(color=color, used=used, limit=limit)
            value["windows"].append(window)
    elif platform == "openai" and kind == "oauth":
        value.update(branch="openai_oauth", actions=["query_usage", "query_reset_credits"],
                     reset_credits=reset_credits(extra, now))
        if not row.get("parent_account_id"):
            value["actions"].append("reset_quota")
        for window in usage_windows(row, now, result):
            duration = timedelta(hours=5) if window["key"] == "codex_5h" else timedelta(days=7)
            reset = parse_iso_datetime(window["reset_at"])
            window.update(color="indigo" if window["key"] == "codex_5h" else "emerald",
                          stats_start=((reset if reset and reset > now else now) - duration).isoformat())
            value["windows"].append(window)
    elif platform == "grok" and kind == "oauth":
        billing = obj(extra.get("grok_billing_snapshot"))
        snapshot = obj(extra.get("grok_usage_snapshot"))
        extra = {**extra, "subscription_tier": row.get("quota_grok_tier") or snapshot.get("subscription_tier") or extra.get("subscription_tier") or billing.get("plan"),
                 "grok_entitlement_status": row.get("quota_grok_entitlement") or snapshot.get("entitlement_status") or extra.get("grok_entitlement_status")}
        free = is_grok_free(extra, billing)
        value.update(branch="grok_free" if free else "grok_paid", actions=["probe_quota"])
        if free:
            window = _window("grok_24h", "24h", None, None, now, now, source="local_24h")
            window.update(color="emerald", stats_start=(now - timedelta(hours=24)).isoformat())
            value["windows"].append(window)
        else:
            monthly = number(billing.get("used_percent"))
            limit, used = number(billing.get("monthly_limit_cents")), number(billing.get("used_cents"))
            if monthly is None and limit and used is not None:
                monthly = used / limit * 100
            weekly = number(billing.get("usage_percent")) if billing.get("period_type", "").lower() == "weekly" else None
            if billing.get("period_type", "").lower() == "weekly" and limit is None:
                monthly = None
            failed = billing.get("failed_windows") or []
            for key, label, percent, start_raw, end_raw in (
                ("weekly", "7d", weekly, billing.get("period_start"), billing.get("period_end")),
                ("monthly", "30d", monthly, billing.get("billing_period_start"), billing.get("billing_period_end") or billing.get("period_end")),
            ):
                if percent is None and key not in failed:
                    continue
                code = number(billing.get(key + "_status_code") or billing.get("status_code"))
                observed = billing.get(key + "_updated_at") or billing.get("fetched_at") or billing.get("updated_at")
                window = _window("grok_" + key, label, min(100, percent) if percent is not None else None,
                                 end_raw, observed, now, source="billing_probe",
                                 failed=key in failed or bool(billing.get("partial") and not failed) or bool(code and not 200 <= code < 300))
                start, end = parse_iso_datetime(start_raw), parse_iso_datetime(end_raw)
                window.update(color="indigo", stats_start=start.isoformat() if start and end and start <= now < end else None)
                value["windows"].append(window)
            value["prepaid_balance"] = number(billing.get("prepaid_balance"))
            value["monthly_limit"] = number(billing.get("monthly_limit"))
            if value["monthly_limit"] is None and limit is not None:
                value["monthly_limit"] = limit / 100
            value["monthly_used"] = number(billing.get("monthly_used"))
            if value["monthly_used"] is None and used is not None:
                value["monthly_used"] = used / 100
    return value


def stats_specs(account_id: int, usage: dict[str, Any]) -> list[dict[str, Any]]:
    specs = [{"account_id": account_id, "key": w["key"], "start_at": w["stats_start"]}
             for w in usage["windows"] if w.get("stats_start")]
    if usage.get("today_start"):
        specs.append({"account_id": account_id, "key": "today", "start_at": usage["today_start"]})
    return specs


def read_stats(db: Any, specs: list[dict[str, Any]]) -> dict[tuple[int, str], dict[str, Any]]:
    if not specs:
        return {}
    rows = db.fetch_all(STATS_SQL, {"windows": json.dumps(specs)})
    return {(row["account_id"], row["key"]): {
        "requests": int(row["requests"]), "tokens": int(row["tokens"]),
        **{field: float(row[field]) for field in ("cost", "standard_cost", "user_cost")},
    } for row in rows}


def attach_stats(account_id: int, usage: dict[str, Any], stats: dict, *, free_token_limit: int) -> None:
    usage.pop("today_start", None)
    usage["today"] = stats.get((account_id, "today"))
    for window in usage["windows"]:
        window.pop("stats_start", None)
        data = stats.get((account_id, window["key"]))
        window["stats"] = data
        if window["key"] == "grok_24h" and data is not None:
            window.update(used_percent=min(100, data["tokens"] / free_token_limit * 100),
                          status="known", limit=free_token_limit, used=data["tokens"])
        if window["key"] == "codex_7d" and data and data["cost"] > 0 and (window["used_percent"] or 0) > 0:
            window["estimated_total_cost"] = data["cost"] * 100 / window["used_percent"]
