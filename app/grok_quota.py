from __future__ import annotations

import asyncio
from datetime import datetime, timezone
from typing import Any
from zoneinfo import ZoneInfo

from . import account_ops
from .bark import sanitize_error_text
from .usage_query import (
    format_percent_value, oauth_usage_payload_data, open_usage_request,
    parse_iso_datetime, percent_or_none, usage_query_error_code,
    validate_oauth_usage_request,
)

BEIJING_TZ = ZoneInfo("Asia/Shanghai")


def safe_text(value: object, limit: int = 160) -> str:
    return " ".join(sanitize_error_text(str(value or "")).split())[:limit]


def beijing_time(value: object) -> str:
    parsed = parse_iso_datetime(value)
    return parsed.astimezone(BEIJING_TZ).strftime("%Y-%m-%d %H:%M:%S") if parsed else "未知"


def is_grok_oauth(row: Any) -> bool:
    return bool(isinstance(row, dict) and int(row.get("id") or 0) > 0
                and row.get("deleted_at") in (None, "")
                and str(row.get("platform") or "").lower() == "grok"
                and str(row.get("type") or "").lower() == "oauth")


def query_grok_billing(account_id: int, base_url: str, token: str, *, opener=None,
                       now: datetime | None = None) -> dict[str, Any]:
    current = now or datetime.now(timezone.utc)
    result: dict[str, Any] = {"account_id": account_id, "queried_at": current.isoformat()}
    try:
        if not base_url or not token:
            raise ValueError("缺少 Sub2API 地址或 Admin API Key")
        request = {
            "url": f"{base_url.rstrip('/')}/api/v1/admin/accounts/{account_id}/usage?source=active&force=true",
            "method": "GET", "headers": {"Accept": "application/json", "x-api-key": token},
        }
        validate_oauth_usage_request(request)
        payload = (opener or open_usage_request)(request, 30)
        if isinstance(payload, dict) and payload.get("code") not in (None, 0, "0"):
            raise ValueError(str(payload.get("message") or payload.get("error") or f"HTTP {payload.get('code')}"))
        result.update(success=True, data=oauth_usage_payload_data(payload))
    except Exception as exc:
        result.update(success=False, error=safe_text(exc), error_code=usage_query_error_code(exc))
    return result


def format_grok_account(row: dict[str, Any], result: dict[str, Any]) -> str:
    data = result.get("data") or {}
    billing = data.get("grok_billing") or {}
    plan = safe_text(data.get("subscription_tier") or billing.get("plan") or "未知套餐", 48)
    lines = [f"#{int(row['id'])} {safe_text(row.get('name') or '-', 80)} · {plan}"]
    if not result.get("success"):
        lines.append(f"查询失败：{safe_text(result.get('error'))} [{safe_text(result.get('error_code') or 'unknown', 64)}]")
        return "\n".join(lines)
    fetched = parse_iso_datetime(billing.get("fetched_at"))
    requested = parse_iso_datetime(result.get("queried_at"))
    fresh = bool(fetched and requested and fetched >= requested.replace(microsecond=0))
    failed = set(billing.get("failed_windows") or [])
    partial = bool(billing.get("partial"))
    lines.append(f"账单采集：{beijing_time(billing.get('fetched_at'))}" + (" · 部分失败" if partial else ""))
    windows = {
        "weekly": dict(data.get("seven_day") or {}),
        "monthly": dict(data.get("thirty_day") or {}),
    }
    # Sub2API only materializes thirty_day when a paid monthly limit exists;
    # retain the billing fields as the authoritative fallback when it does.
    if "utilization" not in windows["weekly"] and percent_or_none(billing.get("usage_percent")) is not None:
        windows["weekly"]["utilization"] = billing.get("usage_percent")
        windows["weekly"]["resets_at"] = billing.get("period_end")
    if "utilization" not in windows["monthly"]:
        monthly_used = percent_or_none(billing.get("used_percent"))
        used_cents = percent_or_none(billing.get("used_cents"))
        limit_cents = percent_or_none(billing.get("monthly_limit_cents"))
        if monthly_used is None and used_cents is not None and limit_cents and limit_cents > 0:
            monthly_used = used_cents / limit_cents * 100
        if monthly_used is not None:
            windows["monthly"]["utilization"] = monthly_used
            windows["monthly"]["resets_at"] = billing.get("billing_period_end") or billing.get("period_end")
    for key, field, label in (("weekly", "weekly", "7d"), ("monthly", "monthly", "月度")):
        window = windows[key]
        status_code = billing.get(f"{key}_status_code") or billing.get("status_code") or 0
        used = percent_or_none(window.get("utilization"))
        valid = fresh and key not in failed and not (partial and not failed) and status_code in (0, 200) and used is not None
        if not valid:
            reason = "本次刷新失败" if key in failed or partial and not failed else "无新鲜官方数据"
            lines.append(f"{label} 剩余未知（{reason}）")
        else:
            remaining = max(0.0, min(100.0, 100 - used))
            exhausted = " · 耗尽" if remaining == 0 else ""
            lines.append(f"{label} 剩余 {format_percent_value(remaining)}{exhausted} · 恢复时间 {beijing_time(window.get('resets_at'))}")
    code = safe_text(data.get("error_code") or "", 64)
    if data.get("needs_reauth") or code in {"unauthenticated", "forbidden", "spending_limit"}:
        lines.append(f"认证异常 [{code or 'needs_reauth'}]")
    elif code and code != "quota_unknown":
        lines.append(f"状态：{code} · {safe_text(data.get('error'))}")
    observed = beijing_time(data.get("grok_last_headers_seen_at"))
    for field, label in (("grok_request_quota", "请求"), ("grok_token_quota", "Token")):
        snapshot = data.get(field)
        if isinstance(snapshot, dict) and snapshot:
            remaining = snapshot.get("remaining")
            limit = snapshot.get("limit")
            lines.append(f"{label}限流历史快照：{safe_text(remaining if remaining is not None else '未知', 32)}/{safe_text(limit if limit is not None else '未知', 32)} · 采集 {observed}")
        else:
            lines.append(f"{label}限流历史快照：未知")
    return "\n".join(lines)


async def grok_quota_reply(db: Any, base_url: str, token: str, concurrency: int, *, runner=query_grok_billing) -> str:
    try:
        rows = await asyncio.to_thread(account_ops.current_grok_oauth_accounts, db)
        rows = [row for row in rows if is_grok_oauth(row)]
    except Exception as exc:
        return f"Grok OAuth\n读取账号失败：{safe_text(exc)}"
    if not rows:
        return "Grok OAuth\n没有 Grok OAuth 账号。"
    semaphore = asyncio.Semaphore(max(1, min(16, concurrency)))

    async def query(row: dict[str, Any]) -> str:
        async with semaphore:
            account_id = int(row["id"])
            try:
                latest = await asyncio.to_thread(account_ops.fallback_account, db, account_id)
                if not is_grok_oauth(latest):
                    return f"#{account_id} 已删除或类型变化，已跳过。"
                result = await asyncio.wait_for(asyncio.to_thread(runner, account_id, base_url, token), 30)
                latest = await asyncio.to_thread(account_ops.fallback_account, db, account_id)
                if not is_grok_oauth(latest):
                    return f"#{account_id} 已删除或类型变化，结果已忽略。"
                return format_grok_account(latest, result)
            except Exception as exc:
                return format_grok_account(row, {"success": False, "error": safe_text(exc) or "请求超时", "error_code": usage_query_error_code(exc)})

    return "Grok OAuth\n官方账单额度（北京时间）\n\n" + "\n\n".join(await asyncio.gather(*(query(row) for row in rows)))
