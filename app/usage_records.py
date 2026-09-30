"""Read-only, bounded history of calls already recorded by Sub2API."""
from __future__ import annotations

import base64
import hashlib
import json
import re
from datetime import date, datetime, time, timedelta, timezone
from decimal import Decimal, localcontext
from typing import Any
from zoneinfo import ZoneInfo

from fastapi import HTTPException

# Never select credentials, API key contents, headers or request/response bodies.
FIELDS = """id user_id api_key_id account_id group_id request_id upstream_request_id
model requested_model upstream_model upstream_response_model model_mapping_chain
upstream_model_mismatch reasoning_effort requested_reasoning_effort request_type stream
openai_ws_mode input_tokens output_tokens cache_creation_tokens cache_read_tokens
cache_creation_5m_tokens cache_creation_1h_tokens input_cost output_cost cache_creation_cost
cache_read_cost total_cost actual_cost account_stats_cost account_rate_multiplier rate_multiplier
first_token_ms duration_ms created_at user_agent ip_address inbound_endpoint upstream_endpoint
billing_type billing_mode service_tier image_count image_output_tokens image_output_cost
image_input_tokens image_input_cost video_count video_duration_seconds video_resolution""".split()
MONEY = set("input_cost output_cost cache_creation_cost cache_read_cost total_cost actual_cost account_stats_cost account_rate_multiplier rate_multiplier image_output_cost image_input_cost".split())
SELECT = ",".join("u." + field for field in FIELDS) + """,
 p.username AS user_name,p.email AS user_email,k.name AS api_key_name,
 a.name AS account_name,g.name AS group_name"""
JOINS = """LEFT JOIN users p ON p.id=u.user_id
 LEFT JOIN api_keys k ON k.id=u.api_key_id
 LEFT JOIN accounts a ON a.id=u.account_id
 LEFT JOIN groups g ON g.id=u.group_id"""
NAMES = ("user_name", "user_email", "api_key_name", "account_name", "group_name")
REQUEST_TYPE = "CASE WHEN u.request_type IN (1,2,3) THEN u.request_type WHEN u.openai_ws_mode THEN 3 WHEN u.stream THEN 2 ELSE 1 END"
MISMATCH = """(u.upstream_model_mismatch IS TRUE OR
 (nullif(u.upstream_response_model,'') IS NOT NULL AND u.upstream_response_model <>
 coalesce(nullif(u.upstream_model,''),nullif(u.requested_model,''),nullif(u.model,''))))"""
BEIJING = ZoneInfo("Asia/Shanghai")


def date_window(start_date: str, end_date: str) -> tuple[datetime, datetime]:
    try:
        if not all(re.fullmatch(r"\d{4}-\d{2}-\d{2}", v) for v in (start_date, end_date)):
            raise ValueError()
        start, end = date.fromisoformat(start_date), date.fromisoformat(end_date)
        if start > end:
            raise ValueError()
        return (datetime.combine(start, time.min, BEIJING).astimezone(timezone.utc),
                datetime.combine(end + timedelta(days=1), time.min, BEIJING).astimezone(timezone.utc))
    except (ValueError, TypeError, OverflowError):
        raise HTTPException(422, "请选择有效的北京时间日期范围") from None


def timestamp(value: str | datetime) -> datetime:
    try:
        dt = value if isinstance(value, datetime) else datetime.fromisoformat(value.replace("Z", "+00:00"))
        if dt.tzinfo is None:
            raise ValueError()
        return dt.astimezone(timezone.utc)
    except (ValueError, TypeError, OverflowError, AttributeError):
        raise HTTPException(422, "时间必须包含时区") from None


def project(row: dict[str, Any]) -> dict[str, Any]:
    result = {key: row.get(key) for key in [*FIELDS, *NAMES]}
    for key in MONEY:
        if result[key] is not None:
            result[key] = str(result[key])
    if result["created_at"] is not None:
        result["created_at"] = timestamp(result["created_at"]).isoformat()
    base = row.get("account_stats_cost")
    if base is None:
        base = row.get("total_cost")
    multiplier = row.get("account_rate_multiplier")
    result["account_cost"] = None
    if base is not None:
        amount = Decimal(str(base))
        rate = Decimal(str(multiplier if multiplier is not None else 1))
        with localcontext() as context:
            context.prec = max(28, len(amount.as_tuple().digits) + len(rate.as_tuple().digits))
            result["account_cost"] = str(amount * rate)
    kind = row.get("request_type")
    if kind not in (1, 2, 3):
        kind = 3 if row.get("openai_ws_mode") else 2 if row.get("stream") else 1
    result["request_type"] = {1: "sync", 2: "stream", 3: "ws_v2"}[kind]
    requested = row.get("requested_model") or row.get("model") or None
    forwarded = row.get("upstream_model") or requested
    response = row.get("upstream_response_model") or None
    result["requested_model"] = requested
    result["upstream_model_mismatch"] = bool(row.get("upstream_model_mismatch") or (response and forwarded and response != forwarded))
    return result


class UsageRecords:
    def __init__(self, db: Any):
        self.db = db

    def list(self, *, from_at: str | None = None, to_at: str | None = None,
             start_date: str | None = None, end_date: str | None = None, include_summary: bool = False,
             account_id: int | None = None, api_key_id: int | None = None, user_id: int | None = None,
             model: str | None = None, request_type: str | None = None,
             mismatch_only: bool = False, cursor: str | None = None,
             after_id: int | None = None, limit: int = 50) -> dict[str, Any]:
        now = datetime.now(timezone.utc)
        if not 1 <= limit <= 100 or any(v is not None and not 1 <= v <= 9223372036854775807 for v in (account_id, api_key_id, user_id)) or (after_id is not None and after_id < 0):
            raise HTTPException(422, "分页或筛选参数无效")
        if cursor and after_id is not None:
            raise HTTPException(422, "分页和新增检查不能同时使用")
        dates = start_date is not None or end_date is not None
        window = None
        if dates:
            if not start_date or not end_date or from_at is not None or to_at is not None:
                raise HTTPException(422, "日期必须成对提供，不能与时间参数混用")
            window = date_window(start_date, end_date)
        criteria = [from_at, to_at, account_id, api_key_id, model, request_type, mismatch_only]
        if user_id is not None:
            criteria.append(user_id)
        if dates:
            criteria.extend([start_date, end_date])
        signature = hashlib.sha256(json.dumps(criteria).encode()).hexdigest()
        saved = None
        if cursor:
            try:
                if len(cursor) > 2048:
                    raise ValueError()
                saved = json.loads(base64.urlsafe_b64decode(cursor.encode()))
                if saved["signature"] != signature or int(saved["id"]) < 1 or int(saved["watermark"]) < 0:
                    raise ValueError()
                timestamp(saved["at"])
                timestamp(saved["from_at"])
                timestamp(saved["to_at"])
            except (ValueError, KeyError, TypeError, UnicodeError, OverflowError):
                raise HTTPException(422, "分页已失效，请刷新记录") from None
        if saved:
            start, end = timestamp(saved["from_at"]), timestamp(saved["to_at"])
        elif window:
            start, end = window
        else:
            start = timestamp(from_at) if from_at else now - timedelta(hours=24)
            end = timestamp(to_at) if to_at else now
        if start >= end:
            raise HTTPException(422, "开始时间必须早于结束时间")
        clauses = ["u.created_at >= %(from_at)s", f"u.created_at {'<' if dates else '<='} %(to_at)s"]
        params: dict[str, Any] = {"from_at": start, "to_at": end, "limit": limit + 1}
        for key, value in (("account_id", account_id), ("api_key_id", api_key_id), ("user_id", user_id)):
            if value is not None:
                clauses.append(f"u.{key} = %({key})s")
                params[key] = value
        if model:
            if len(model) > 200:
                raise HTTPException(422, "模型筛选过长")
            params["model"] = "%" + model.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + "%"
            clauses.append("(" + " OR ".join(f"u.{field} ILIKE %(model)s" for field in ("model", "requested_model", "upstream_model", "upstream_response_model", "model_mapping_chain")) + ")")
        if request_type:
            if request_type not in ("sync", "stream", "ws_v2"):
                raise HTTPException(422, "请求类型无效")
            params["request_type"] = {"sync": 1, "stream": 2, "ws_v2": 3}[request_type]
            clauses.append(f"({REQUEST_TYPE}) = %(request_type)s")
        if mismatch_only:
            clauses.append(MISMATCH)
        summary = None
        with self.db.connection() as conn, conn.transaction():
            conn.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            conn.execute("SET LOCAL statement_timeout = '5s'")
            watermark = int(conn.execute("SELECT coalesce(max(id),0) AS id FROM usage_logs").fetchone()["id"])
            params["watermark"] = int(saved["watermark"]) if saved else watermark
            clauses.append("u.id <= %(watermark)s")
            if after_id is not None:
                params["after_id"] = after_id
                clauses.append("u.id > %(after_id)s")
                count = conn.execute("SELECT count(*) AS count FROM usage_logs u WHERE " + " AND ".join(clauses), params).fetchone()["count"]
                return {"new_count": count, "latest_id": watermark}
            if include_summary and not saved:
                total = conn.execute("SELECT coalesce(sum(u.actual_cost),0) AS actual_cost FROM usage_logs u WHERE " + " AND ".join(clauses), params).fetchone()["actual_cost"]
                summary = {"actual_cost": format(Decimal(str(total)), "f")}
            if saved:
                clauses.append("(u.created_at,u.id) < (%(before_at)s,%(before_id)s)")
                params.update(before_at=timestamp(saved["at"]), before_id=int(saved["id"]))
            rows = conn.execute(f"SELECT {SELECT} FROM usage_logs u {JOINS} WHERE " + " AND ".join(clauses) + " ORDER BY u.created_at DESC,u.id DESC LIMIT %(limit)s", params).fetchall()
        items = [project(row) for row in rows[:limit]]
        next_cursor = None
        if len(rows) > limit:
            last = items[-1]
            next_cursor = base64.urlsafe_b64encode(json.dumps({"at": last["created_at"], "id": last["id"], "watermark": params["watermark"], "from_at": start.isoformat(), "to_at": end.isoformat(), "signature": signature}).encode()).decode()
        page = {"items": items, "next_cursor": next_cursor, "latest_id": params["watermark"], "observed_at": now.isoformat()}
        if summary is not None:
            page["summary"] = summary
        return page

    def options(self, *, kind: str, q: str | None = None, user_id: int | None = None,
                cursor: str | None = None, limit: int = 50) -> dict[str, Any]:
        if kind not in ("users", "api_keys") or not 1 <= limit <= 100:
            raise HTTPException(422, "目录或分页参数无效")
        if user_id is not None and (kind != "api_keys" or not 1 <= user_id <= 9223372036854775807):
            raise HTTPException(422, "用户筛选无效")
        query = (q or "").strip()
        if len(query) > 200:
            raise HTTPException(422, "搜索内容过长")
        signature = hashlib.sha256(json.dumps([kind, query, user_id]).encode()).hexdigest()
        after = 0
        if cursor:
            try:
                if len(cursor) > 2048:
                    raise ValueError()
                saved = json.loads(base64.urlsafe_b64decode(cursor.encode()))
                after = int(saved["id"])
                if saved["signature"] != signature or not 1 <= after <= 9223372036854775807:
                    raise ValueError()
            except (ValueError, KeyError, TypeError, UnicodeError, OverflowError):
                raise HTTPException(422, "目录分页已失效，请重新搜索") from None
        params: dict[str, Any] = {"after": after, "limit": limit + 1}
        clauses = ["e.id > %(after)s", "e.deleted_at IS NULL"]
        if kind == "users":
            selection = "e.id,e.username AS name,e.email,e.status,(e.deleted_at IS NOT NULL) AS deleted"
            source = "users e"
            text_fields = ("e.username", "e.email")
            fields = ("id", "name", "email", "status", "deleted")
        else:
            selection = "e.id,e.name,e.user_id,p.username AS user_name,p.email AS user_email,e.status,(e.deleted_at IS NOT NULL) AS deleted"
            source = "api_keys e JOIN users p ON p.id=e.user_id AND p.deleted_at IS NULL"
            text_fields = ("e.name",)
            fields = ("id", "name", "user_id", "user_name", "user_email", "status", "deleted")
            if user_id is not None:
                clauses.append("e.user_id = %(user_id)s")
                params["user_id"] = user_id
        if query:
            params["q"] = "%" + query.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + "%"
            matches = [f"{field} ILIKE %(q)s" for field in text_fields]
            digits = query.removeprefix("#")
            if digits.isascii() and digits.isdecimal() and 1 <= int(digits) <= 9223372036854775807:
                params["id"] = int(digits)
                matches.append("e.id = %(id)s")
            clauses.append("(" + " OR ".join(matches) + ")")
        with self.db.connection() as conn, conn.transaction():
            conn.execute("SET TRANSACTION READ ONLY")
            conn.execute("SET LOCAL statement_timeout = '5s'")
            rows = conn.execute(f"SELECT {selection} FROM {source} WHERE " + " AND ".join(clauses) + " ORDER BY e.id ASC LIMIT %(limit)s", params).fetchall()
        items = [{**{field: row.get(field) for field in fields}, "deleted": bool(row.get("deleted"))} for row in rows[:limit]]
        next_cursor = None
        if len(rows) > limit:
            next_cursor = base64.urlsafe_b64encode(json.dumps({"id": items[-1]["id"], "signature": signature}).encode()).decode()
        return {"items": items, "next_cursor": next_cursor}

    def detail(self, record_id: int) -> dict[str, Any]:
        if record_id < 1:
            raise HTTPException(422, "记录编号无效")
        with self.db.connection() as conn, conn.transaction():
            conn.execute("SET TRANSACTION READ ONLY")
            conn.execute("SET LOCAL statement_timeout = '5s'")
            row = conn.execute(f"SELECT {SELECT} FROM usage_logs u {JOINS} WHERE u.id=%(id)s", {"id": record_id}).fetchone()
        if row is None:
            raise HTTPException(404, "记录不存在")
        return project(row)
