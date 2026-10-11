"""Read-only account quality evidence. No upstream calls or persistent state."""
from __future__ import annotations

import copy
import logging
import math
import re
import threading
import time
from collections import Counter, defaultdict
from datetime import datetime, timedelta, timezone
from statistics import median
from typing import Any, Callable

from .error_evidence import is_local_throttle

LOG = logging.getLogger(__name__)
ERROR_CURVE = [(0, 100), (.005, 95), (.01, 90), (.03, 75), (.05, 60), (.1, 35), (.2, 0)]
PERF_CURVE = [(.25, 0), (.5, 40), (.75, 70), (1, 85), (1.5, 95), (2, 100)]
CAUSES = {"auth": "认证失败", "timeout": "频繁超时", "network": "连接不稳", "stream": "流式中断", "rate": "频繁限流", "upstream": "错误偏多"}
SUPPORTED = "deleted_at IS NULL AND platform IN ('openai','grok') AND type IN ('oauth','apikey')"
SLOW_TTFT_THRESHOLD_MS = 10_000
SLOW_TTFT_SAMPLE_COUNT = 10
SLOW_TTFT_MIN_COUNT = 8
SLOW_TTFT_WINDOW = timedelta(hours=24)


def timestamp(value: Any) -> datetime | None:
    if isinstance(value, datetime):
        return value.replace(tzinfo=timezone.utc) if value.tzinfo is None else value.astimezone(timezone.utc)
    try:
        result = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
        return timestamp(result)
    except (ValueError, TypeError, OverflowError):
        return None


def number(value: Any) -> float | None:
    if isinstance(value, bool):
        return None
    try:
        result = float(value)
        return result if math.isfinite(result) else None
    except (ValueError, TypeError, OverflowError):
        return None


def quantile(values: list[float], p: float) -> float | None:
    if not values:
        return None
    data = sorted(values)
    index = (len(data) - 1) * p
    lower = int(index)
    return data[lower] + (data[min(lower + 1, len(data) - 1)] - data[lower]) * (index - lower)


def interpolate(value: float, anchors: list[tuple[float, float]]) -> float:
    if value <= anchors[0][0]:
        return anchors[0][1]
    for (x, y), (nx, ny) in zip(anchors, anchors[1:]):
        if value <= nx:
            return y + (value - x) * (ny - y) / (nx - x)
    return anchors[-1][1]


def failure_cause(record: dict[str, Any]) -> str | None:
    """Only explicit exclusions; a generic 429 is NOT quota exhaustion."""
    if is_local_throttle(record):
        return None
    code = number(record.get("upstream_status_code") or record.get("status_code"))
    text = " ".join(str(record.get(k) or "") for k in ("message", "error_message", "upstream_error_message",
        "provider_error_code", "provider_error_type", "error_type", "network_error_type", "kind")).lower()
    if code == 499 or any(s in text for s in ("client canceled", "client cancelled", "client disconnected", "context canceled", "context cancelled", "客户端取消")):
        return None
    if any(s in text for s in ("content_policy_violation", "content_filter", "safety_violation", "content policy", "内容策略", "内容审核拒绝")):
        return None
    quota = ("usage_limit_reached", "usage limit has been reached", "insufficient_quota", "quota_exceeded", "quota exhausted",
        "weekly limit reached", "monthly limit reached", "可用账号额度等待恢复", "账号额度已用尽", "余额不足", "credits exhausted")
    if any(s in text for s in quota):
        return None
    if any(s in text for s in ("client api key", "客户端 api key", "user rate limit", "用户请求参数")):
        return None
    if code in (400, 422) and any(s in text for s in ("invalid_request", "invalid parameter", "invalid argument", "invalid value", "context_length_exceeded", "maximum context length", "missing required")):
        return None
    if code in (401, 402, 403) or any(s in text for s in ("invalid_api_key", "authentication", "unauthorized", "token expired", "needs_reauth")):
        return "auth"
    if code in (408, 504) or any(s in text for s in ("timeout", "timed out", "deadline exceeded", "超时")):
        return "timeout"
    if re.search(r"\bstream(?:ing)?(?:\b|_)", text) and any(s in text for s in ("failed", "interrupt", "eof", "closed", "broken")):
        return "stream"
    if any(s in text for s in ("connection", "network", "dial tcp", "dns", "eof", "socket")):
        return "network"
    return "rate" if code == 429 else "upstream"


def failures(errors: list[dict], accounts: dict[int, dict], now: datetime) -> list[dict]:
    found: dict[tuple, dict] = {}
    start = now - timedelta(days=7)
    for row in errors:
        events = row.get("upstream_errors")
        detailed = isinstance(events, list) and bool(events)
        # A detailed array is authoritative, including events without attribution.
        if not detailed and (row.get("error_owner") != "provider" or row.get("error_phase") not in ("upstream", "account_auth")):
            continue
        for event in events if detailed else [row]:
            if not isinstance(event, dict):
                continue
            account_id = number(event.get("account_id"))
            if account_id not in accounts:
                continue
            cause = failure_cause(event)
            if cause is None:
                continue
            at = timestamp(row.get("created_at"))
            millis = number(event.get("at_unix_ms"))
            if millis and millis > 0:
                try:
                    at = datetime.fromtimestamp(millis / 1000, timezone.utc)
                except (ValueError, OverflowError, OSError):
                    pass
            if not at or not start <= at <= now:
                continue
            # Never merge across the unrelated usage_logs request-ID namespace.
            request_id = row.get("request_id") or row.get("client_request_id")
            request = str(request_id or f"log:{row['id']}")
            key = (int(account_id), request)
            item = {"account_id": int(account_id), "at": at, "cause": cause, "request": request, "request_known": bool(request_id)}
            if key not in found or at < found[key]["at"]:
                found[key] = item
    return list(found.values())


def performance(row: dict, account: dict) -> dict | None:
    if row.get("stream") is not True:
        return None
    endpoint = str(row.get("inbound_endpoint") or "").lower()
    model = str(row.get("upstream_model") or row.get("model") or "").strip()
    # Explicit non-text evidence; missing/invalid timing is never synthesized.
    if any(number(row.get(k)) not in (None, 0) for k in ("image_count", "image_output_tokens", "video_count", "video_duration_seconds")):
        return None
    if row.get("billing_mode") == "image":
        return None
    if any(s in (endpoint + " " + model.lower()) for s in ("/audio", "/images", "/videos", "-image", "-video", "-tts", "-stt", "realtime", "whisper", "dall-e")):
        return None
    first, duration, output = (number(row.get(k)) for k in ("first_token_ms", "duration_ms", "output_tokens"))
    at = timestamp(row.get("created_at"))
    if not model or not at or duration is None or duration <= 0 or output is None or output <= 0:
        return None
    input_size = number(row.get("input_tokens"))
    if input_size is None or input_size < 0:
        return None
    # Sub2API stores uncached input separately from cache read/creation tokens.
    # Older schemas may omit either optional counter. Preserve timing evidence
    # without inventing a known input-size bucket from incomplete token data.
    input_known = True
    for key in ("cache_read_tokens", "cache_creation_tokens"):
        if row.get(key) is None:
            input_known = False
            continue
        cached = number(row[key])
        if cached is None or cached < 0:
            return None
        input_size += cached
    size = "未知" if not input_known else "≤8K" if input_size <= 8192 else "8–32K" if input_size <= 32768 else "32–128K" if input_size <= 131072 else ">128K"
    cohort = (account["platform"], model, str(row.get("reasoning_effort") or "default"),
        str(row.get("service_tier") or "default"), "websocket" if row.get("openai_ws_mode") else "sse", size)
    return {"account_id": account["id"], "at": at, "cohort": cohort,
        "ttft": first / 1000 if first is not None and 0 < first < duration else None,
        # Sub2API latencyHealth uses total duration, including the initial wait.
        "tps": output / (duration / 1000) if output >= 32 and duration >= 500 else None}


def slow_ttft_sample(row: dict, account: dict) -> dict | None:
    """Return a valid text-stream timing sample for the fixed warning rule."""
    if account.get("platform") != "openai" or account.get("type") not in {"oauth", "apikey"}:
        return None
    if row.get("stream") is not True:
        return None
    endpoint = str(row.get("inbound_endpoint") or "").lower()
    model = str(row.get("upstream_model") or row.get("model") or "").strip()
    if not model or any(number(row.get(k)) not in (None, 0) for k in
                        ("image_count", "image_output_tokens", "video_count", "video_duration_seconds")):
        return None
    if any(s in (endpoint + " " + model.lower()) for s in
           ("/audio", "/images", "/videos", "-image", "-video", "-tts", "-stt", "realtime", "whisper", "dall-e")):
        return None
    at = timestamp(row.get("created_at"))
    first = number(row.get("first_token_ms"))
    duration = number(row.get("duration_ms"))
    output = number(row.get("output_tokens"))
    if not at or first is None or duration is None or output is None or first <= 0 or duration <= first or output < 0:
        return None
    return {"account_id": account["id"], "id": int(row.get("id") or 0), "at": at,
            "first_token_ms": first, "duration_ms": duration, "output_tokens": output}


def slow_ttft_warning(samples: list[dict], now: datetime) -> dict | None:
    """Evaluate the latest ten valid calls in the rolling 24-hour window."""
    start = now - SLOW_TTFT_WINDOW
    rows = [s for s in samples if s.get("at") and start <= s["at"] <= now]
    rows.sort(key=lambda s: (s["at"], int(s.get("id") or 0)), reverse=True)
    rows = rows[:SLOW_TTFT_SAMPLE_COUNT]
    if len(rows) < SLOW_TTFT_SAMPLE_COUNT:
        return None
    slow_count = sum(number(row.get("first_token_ms")) is not None and row["first_token_ms"] > SLOW_TTFT_THRESHOLD_MS for row in rows)
    latest_fast = rows[0]["first_token_ms"] <= SLOW_TTFT_THRESHOLD_MS
    return {"kind": "slow_ttft", "sample_count": len(rows), "slow_count": slow_count,
            "threshold_ms": SLOW_TTFT_THRESHOLD_MS,
            "active": slow_count >= SLOW_TTFT_MIN_COUNT and not latest_fast,
            "latest_sample_id": int(rows[0].get("id") or 0),
            "latest_first_token_ms": rows[0]["first_token_ms"],
            "latest_at": rows[0]["at"].isoformat()}


def build_baselines(samples: list[dict]) -> dict:
    grouped: dict = defaultdict(lambda: defaultdict(list))
    for s in samples:
        for key in (s["cohort"], s["cohort"][:-1]):
            grouped[key][s["account_id"]].append(s)
    result = {}
    for key, account_samples in grouped.items():
        for metric, tail in (("ttft", .9), ("tps", .1)):
            eligible = [[s[metric] for s in rows if s[metric] is not None] for rows in account_samples.values()]
            eligible = [values for values in eligible if len(values) >= 10]
            if len(eligible) >= 2 and sum(map(len, eligible)) >= 50:
                result[(key, metric)] = {"p50": median(quantile(v, .5) for v in eligible),
                    "tail": median(quantile(v, tail) for v in eligible), "accounts": len(eligible), "samples": sum(map(len, eligible))}
    return result


def metric_window(samples: list[dict], metric: str, baselines: dict) -> dict:
    rows = [s for s in samples if s[metric] is not None]
    groups: dict = defaultdict(list)
    for s in rows:
        key = s["cohort"] if (s["cohort"], metric) in baselines else s["cohort"][:-1]
        if (key, metric) in baselines:
            groups[key].append(s[metric])
    compared = sum(map(len, groups.values()))
    details, score = [], 0.0
    tail = .9 if metric == "ttft" else .1
    for key, values in sorted(groups.items()):
        base = baselines[(key, metric)]
        p50, ptail = quantile(values, .5), quantile(values, tail)
        ratios = (base["p50"] / p50, base["tail"] / ptail) if metric == "ttft" else (p50 / base["p50"], ptail / base["tail"])
        part = .7 * interpolate(ratios[0], PERF_CURVE) + .3 * interpolate(ratios[1], PERF_CURVE)
        score += part * len(values) / compared
        details.append({"platform": key[0], "model": key[1], "reasoning_effort": key[2], "service_tier": key[3],
            "transport": key[4], "input_bucket": key[5] if len(key) == 6 else "全部输入规模", "samples": len(values),
            "p50": p50, "tail": ptail, "baseline_p50": base["p50"], "baseline_tail": base["tail"],
            "baseline_accounts": base["accounts"], "baseline_samples": base["samples"], "score": part})
    return {"samples": len(rows), "compared": compared, "coverage": compared / len(rows) if rows else 0,
        "score": score if compared else None, "p50": quantile([s[metric] for s in rows], .5),
        "tail": quantile([s[metric] for s in rows], tail), "cohorts": details}


def metric_result(samples: list[dict], metric: str, baselines: dict, recent: datetime) -> dict:
    all_rows = metric_window(samples, metric, baselines)
    current = metric_window([s for s in samples if s["at"] >= recent], metric, baselines)
    history = metric_window([s for s in samples if s["at"] < recent], metric, baselines)
    chosen = all_rows
    mode = "7d"
    # Both score and coverage use the same temporal weighting.
    if current["samples"] >= 10:
        chosen = dict(current)
        mode = "24h"
        if current["score"] is not None and history["score"] is not None:
            for key in ("score", "coverage", "p50", "tail"):
                chosen[key] = .7 * current[key] + .3 * history[key]
            mode = "70/30"
    return {**chosen, "mode": mode, "samples": all_rows["samples"], "recent": current, "history": history, "all": all_rows}


def reliability(successes: list[dict], errors: list[dict], recent: datetime) -> dict:
    def window(s, f):
        n = len(s) + len(f)
        rate = len(f) / n if n else None
        return {"successes": len(s), "failures": len(f), "total": n, "rate": rate,
            "score": interpolate(rate, ERROR_CURVE) if rate is not None else None}
    all_rows = window(successes, errors)
    current = window([s for s in successes if s["at"] >= recent], [e for e in errors if e["at"] >= recent])
    history = window([s for s in successes if s["at"] < recent], [e for e in errors if e["at"] < recent])
    mode, score, rate = "7d", all_rows["score"], all_rows["rate"]
    if current["total"] >= 20:
        mode, score, rate = "24h", current["score"], current["rate"]
        if history["total"]:
            mode, score, rate = "70/30", .7 * score + .3 * history["score"], .7 * rate + .3 * history["rate"]
    return {**all_rows, "score": score, "effective_rate": rate, "mode": mode, "recent": current, "history": history,
        "causes": dict(Counter(e["cause"] for e in errors))}


def calculate(accounts: list[dict], usage: list[dict], errors: list[dict], now: datetime,
              baselines: dict | None = None) -> tuple[dict[int, dict], dict]:
    live = {a["id"]: a for a in accounts if a.get("platform") in ("openai", "grok") and a.get("type") in ("oauth", "apikey") and not a.get("deleted_at")}
    start, recent = now - timedelta(days=7), now - timedelta(days=1)
    success_by, failure_by, sample_by, slow_by = defaultdict(list), defaultdict(list), defaultdict(list), defaultdict(list)
    seen = set()
    for row in usage:
        account = live.get(row.get("account_id"))
        at = timestamp(row.get("created_at"))
        if not account or not at or not start <= at <= now or row["id"] in seen:
            continue
        seen.add(row["id"])
        success_by[account["id"]].append({"at": at})
        sample = performance(row, account)
        if sample:
            sample_by[account["id"]].append(sample)
        slow_sample = slow_ttft_sample(row, account)
        if slow_sample:
            slow_by[account["id"]].append(slow_sample)
    for error in failures(errors, live, now):
        failure_by[error["account_id"]].append(error)
    if baselines is None:
        baselines = build_baselines([s for rows in sample_by.values() for s in rows])
    output = {}
    for account_id in live:
        successes, failed, samples = success_by[account_id], failure_by[account_id], sample_by[account_id]
        rel = reliability(successes, failed, recent)
        ttft, tps = (metric_result(samples, metric, baselines, recent) for metric in ("ttft", "tps"))
        rel["missing_reason"] = "insufficient_calls" if rel["total"] < 20 else None
        for metric, absent in ((ttft, "first_token_missing"), (tps, "throughput_unavailable")):
            metric["missing_reason"] = ("insufficient_calls" if rel["total"] < 20 else
                absent if metric["samples"] == 0 else
                "insufficient_calls" if metric["samples"] < 10 else
                "no_comparable_baseline" if metric["coverage"] < .6 or metric["score"] is None else None)
        complete = rel["total"] >= 20 and all(m["samples"] >= 10 and m["coverage"] >= .6 and m["score"] is not None for m in (ttft, tps))
        score = .5 * rel["score"] + .25 * ttft["score"] + .25 * tps["score"] if complete else None
        cap, forced = 100, []
        last_success = max((s["at"] for s in successes), default=start)
        consecutive = [e for e in failed if e["request_known"] and e["at"] >= now - timedelta(minutes=15) and e["at"] > last_success]
        if len(consecutive) >= 3:
            cap, forced = 39, ["连续失败"]
            if any(e["cause"] == "auth" for e in consecutive):
                forced.append("认证失败")
        if rel["total"] >= 20 and rel["effective_rate"] >= .03:
            cap = min(cap, 59 if rel["effective_rate"] >= .1 else 84)
        for metric, label, bad, severe, reverse in ((ttft, "首字偏慢", 30, 60, False), (tps, "输出偏慢", 5, 2, True)):
            p50 = metric["p50"]
            if metric["samples"] >= 10 and p50 is not None and (p50 <= bad if reverse else p50 >= bad):
                severity = p50 <= severe if reverse else p50 >= severe
                cap = min(cap, 49 if severity else 79)
                if severity:
                    forced.append(label)
        if score is not None:
            score = min(cap, math.floor(score + .5))
        grade = "red" if cap < 60 or score is not None and score < 60 else "green" if score is not None and score >= 85 else "yellow"
        status = "complete" if complete else "insufficient" if rel["total"] else "empty"
        losses = []
        if rel["score"] is not None:
            dominant = max(rel["causes"], key=rel["causes"].get) if rel["causes"] else "upstream"
            losses.append(((100 - rel["score"]) * .5, CAUSES[dominant]))
        for metric, label in ((ttft, "首字偏慢"), (tps, "输出偏慢")):
            if metric["score"] is not None:
                tail_ratio = metric["tail"] / metric["p50"] if metric["p50"] else 1
                unstable = tail_ratio >= 3 if label == "首字偏慢" else tail_ratio <= .33
                losses.append(((100 - metric["score"]) * .25, "波动较大" if unstable else label))
        reasons = list(dict.fromkeys(forced))[:2]
        if not reasons:
            if status != "complete":
                missing_labels = {"insufficient_calls": "调用不足", "first_token_missing": "首字未记录",
                    "throughput_unavailable": "无有效输出样本", "no_comparable_baseline": "暂无可比基准"}
                reasons = ["错误偏多"] if cap < 60 else list(dict.fromkeys(
                    missing_labels[m["missing_reason"]] for m in (rel, ttft, tps) if m["missing_reason"]))[:2]
            elif grade == "green":
                reasons = ["表现良好"]
            else:
                ranked = sorted(losses, reverse=True)
                reasons = list(dict.fromkeys(label for loss, label in ranked if loss >= 5))[:2] or [ranked[0][1]]
        output[account_id] = {"account_id": account_id, "score": score, "grade": grade, "reasons": reasons,
            "sample_status": status, "computed_at": now.isoformat(), "data_status": "fresh", "cap": cap,
            "period": {"start": start.isoformat(), "end": now.isoformat(), "recent_start": recent.isoformat()},
            "reliability": rel, "ttft": ttft, "throughput": tps, "consecutive_failures": len(consecutive),
            "coverage": min(ttft["coverage"], tps["coverage"]),
            "warnings": ([warning] if (warning := slow_ttft_warning(slow_by[account_id], now)) and warning["active"] else [])}
    return output, baselines


class QualityEvidenceError(ValueError):
    def __init__(self, stage: str, kind: str):
        super().__init__(kind)
        self.stage, self.kind = stage, kind


def read_evidence(db: Any, now: datetime) -> tuple[list, list, list]:
    """Bounded, consistent snapshot on its own pooled connection, never writes."""
    params = {"start": now - timedelta(days=7), "end": now}
    with db.connection() as conn, conn.transaction():
        conn.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        conn.execute("SET LOCAL statement_timeout = '5s'")
        def query(stage, sql, values=None):
            try:
                return conn.execute(sql, values).fetchall() if values is not None else conn.execute(sql).fetchall()
            except Exception as exc:
                kind = "query_timeout" if isinstance(exc, TimeoutError) or getattr(exc, "sqlstate", None) == "57014" or type(exc).__name__ == "QueryCanceled" else "query_failed"
                raise QualityEvidenceError(stage, kind) from None
        columns = {r["attname"] for r in query("schema", """SELECT attname FROM pg_attribute
            WHERE attrelid=to_regclass('usage_logs') AND attnum>0 AND NOT attisdropped""")}
        accounts = query("accounts", f"SELECT id,platform,type FROM accounts WHERE {SUPPORTED}")
        required = ["id", "account_id", "created_at", "model", "input_tokens", "output_tokens"]
        optional = ["upstream_model", "cache_read_tokens", "cache_creation_tokens", "stream", "duration_ms",
            "first_token_ms", "reasoning_effort", "service_tier", "openai_ws_mode", "inbound_endpoint",
            "image_count", "image_output_tokens", "video_count", "video_duration_seconds", "billing_mode"]
        # Identifiers come only from these constants. Never serialize whole rows:
        # doing so detoasts unrelated request metadata before applying the LIMIT.
        projection = ",".join([f"u.{c}" for c in required] +
            [f"u.{c}" if c in columns else f"NULL AS {c}" for c in optional])
        usage = query("usage", f"""SELECT {projection}
            FROM usage_logs u JOIN accounts a ON a.id=u.account_id WHERE a.deleted_at IS NULL
            AND a.platform IN ('openai','grok') AND a.type IN ('oauth','apikey')
            AND u.created_at >= %(start)s AND u.created_at <= %(end)s LIMIT 250001""", params)
        errors = query("errors", """SELECT e.id,e.account_id,e.request_id,e.client_request_id,e.created_at,e.error_owner,e.error_phase,
            e.upstream_status_code,e.status_code,e.provider_error_code,e.provider_error_type,e.error_type,e.network_error_type,
            left(e.error_message,2000) AS error_message,left(e.upstream_error_message,2000) AS upstream_error_message,
            (SELECT jsonb_agg(jsonb_build_object('account_id',x->'account_id','at_unix_ms',x->'at_unix_ms',
             'upstream_status_code',x->'upstream_status_code','kind',x->'kind','message',left(x->>'message',2000)))
             FROM jsonb_array_elements(CASE WHEN jsonb_typeof(e.upstream_errors)='array' THEN e.upstream_errors ELSE '[]'::jsonb END) x) AS upstream_errors
            FROM ops_error_logs e WHERE e.created_at >= %(start)s AND e.created_at <= %(end)s
            AND (e.account_id IS NOT NULL OR jsonb_array_length(CASE WHEN jsonb_typeof(e.upstream_errors)='array' THEN e.upstream_errors ELSE '[]'::jsonb END)>0)
            LIMIT 75001""", params)
        if len(usage) > 250000 or len(errors) > 75000:
            raise QualityEvidenceError("usage" if len(usage) > 250000 else "errors", "evidence_limit")
    return accounts, usage, errors


class QualityCache:
    """Demand driven, single in-flight computation; readers never wait for SQL."""
    def __init__(self, db: Any, *, reader: Callable = read_evidence, clock: Callable = time.monotonic,
                 utcnow: Callable = lambda: datetime.now(timezone.utc)):
        self.db, self.reader, self.clock, self.utcnow = db, reader, clock, utcnow
        self._lock = threading.Lock()
        self._results: dict = {}
        self._attempt = float("-inf")
        self._success: float | None = None
        self._baseline_at = float("-inf")
        self._baselines = None
        self._signature = None
        self._failed = False
        self._diagnostic: dict | None = None
        self._closed = False
        self._thread: threading.Thread | None = None

    def close(self):
        with self._lock:
            self._closed = True

    def _refresh(self):
        stage = "usage"
        try:
            now = self.utcnow()
            accounts, usage, errors = self.reader(self.db, now)
            stage = "calculation"
            signature = sorted((a["id"], a["platform"], a["type"]) for a in accounts)
            rebuild = self.clock() - self._baseline_at >= 300 or signature != self._signature
            results, baselines = calculate(accounts, usage, errors, now, None if rebuild else self._baselines)
            with self._lock:
                self._results, self._success, self._failed = results, self.clock(), False
                self._diagnostic = None
                if rebuild:
                    self._baselines, self._baseline_at, self._signature = baselines, self.clock(), signature
        except Exception as exc:
            # Exceptions can contain SQL data. Only log the exception class.
            diagnostic = {"stage": getattr(exc, "stage", stage), "kind": getattr(exc, "kind",
                "calculation_failed" if stage == "calculation" else "query_timeout" if isinstance(exc, TimeoutError) else "query_failed"), "checked_at": self.utcnow().isoformat()}
            LOG.warning("account_quality_refresh_failed stage=%s kind=%s type=%s", diagnostic["stage"], diagnostic["kind"], type(exc).__name__)
            with self._lock:
                self._failed = True
                self._diagnostic = diagnostic

    def get(self, account_ids: list[int], *, detail: bool = False) -> dict:
        with self._lock:
            now = self.clock()
            if not self._closed and now - self._attempt >= 30 and not (self._thread and self._thread.is_alive()):
                self._attempt = now
                self._thread = threading.Thread(target=self._refresh, name="account-quality", daemon=True)
                self._thread.start()
            status = "stale" if self._success is not None and now - self._success > 300 else "delayed" if self._failed else "fresh"
            result = {}
            for account_id in account_ids:
                stored = self._results.get(account_id, {"account_id": account_id, "score": None, "grade": "yellow",
                    "reasons": ["计算中"], "sample_status": "pending", "computed_at": None, "warnings": []})
                fields = ("score", "grade", "reasons", "sample_status", "computed_at", "data_status", "warnings")
                value = copy.deepcopy(stored if detail else {k: stored.get(k) for k in fields})
                value["data_status"] = status
                value["diagnostic"] = copy.deepcopy(self._diagnostic)
                if self._failed:
                    value["reasons"] = ["统计更新失败"]
                elif status == "stale":
                    value["reasons"] = ["等待统计更新"]
                if status != "fresh":
                    value["warnings"] = []
                if not detail:
                    value = {k: value.get(k) for k in ("score", "grade", "reasons", "sample_status", "computed_at", "data_status", "warnings", "diagnostic")}
                result[account_id] = value
            return result
