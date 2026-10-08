"""Passive OpenAI capacity alerts. No quota, model, or account writes."""
from __future__ import annotations

import asyncio
import fcntl
import hashlib
import json
import threading
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Callable

from fastapi import HTTPException

from .atomic_config import write_json
from .audit import write_audit
from .bark import _beijing_time, sanitize_error_text
from .usage_query import parse_iso_datetime
from .error_evidence import MESSAGES, match_message as match_evidence
from .account_quality import slow_ttft_sample, slow_ttft_warning

RETRY_SECONDS = (5, 30, 120, 600)
TITLE = "⚠️ Codex 疑似降智"
PUSH_OPTIONS = {"level": "critical", "sound": "alarm", "group": "Sub2Ops 疑似降智"}
_LOCK = threading.RLock()
_ACCOUNT_LOCKS: dict[tuple[str, int], threading.RLock] = {}


def utcnow() -> datetime:
    return datetime.now(timezone.utc)


def match_message(row: dict[str, Any]) -> str | None:
    return None if row.get("account_deleted_at") else match_evidence(row)


def mark_view(account_id: int, mark: dict[str, Any] | None = None) -> dict[str, Any]:
    value = mark or {}
    return {"marked": value.get("marked", False), "marked_at": value.get("marked_at"),
            "version": hashlib.sha256(json.dumps([account_id, value], sort_keys=True).encode()).hexdigest()}


class CapacityAlertStore:
    def __init__(self, path: Path):
        self.path = path
        # The persistent lock also witnesses a prior initialization after restart.
        self.existed = path.exists() or path.with_suffix(".lock").exists()

    def _read(self) -> dict[str, Any]:
        try:
            data = json.loads(self.path.read_text())
        except FileNotFoundError:
            if self.existed:
                raise OSError("告警状态文件已丢失") from None
            return {"version": 1, "cursor": None, "initialized_at": None, "seen": {}, "marks": {}, "pending": {},
                    "slow_ttft": {}, "slow_pending": {}, "detection_events": {}}
        if not isinstance(data, dict) or data.get("version") != 1:
            raise ValueError("告警状态版本无效")
        data.setdefault("slow_ttft", {})
        data.setdefault("slow_pending", {})
        data.setdefault("detection_events", {})
        if any(not isinstance(data.get(k), dict) for k in ("seen", "marks", "pending", "slow_ttft", "slow_pending", "detection_events")):
            raise ValueError("告警状态无效")
        if any(key in data and not isinstance(data[key], dict) for key in ("notifications", "profile_intents", "detection_events")):
            raise ValueError("告警扩展状态无效")
        if data.get("gateway_since") and not parse_iso_datetime(data["gateway_since"]):
            raise ValueError("告警证据水位无效")
        if data.get("cursor") is not None and (type(data["cursor"]) is not int or data["cursor"] < 0 or not parse_iso_datetime(data.get("initialized_at"))):
            raise ValueError("告警水位无效")
        for mark in data["marks"].values():
            if not isinstance(mark, dict) or type(mark.get("marked")) is not bool or not parse_iso_datetime(mark.get("changed_at")):
                raise ValueError("降智标记状态无效")
        if any(not parse_iso_datetime(value) for value in data["seen"].values()):
            raise ValueError("告警去重状态无效")
        for event in data["pending"].values():
            if (not isinstance(event, dict) or type(event.get("account_id")) is not int or event["account_id"] < 1
                    or type(event.get("id")) is not int or type(event.get("attempts")) is not int
                    or not parse_iso_datetime(event.get("next_at")) or not parse_iso_datetime(event.get("created_at"))
                    or any(not isinstance(event.get(key), str) for key in ("account_name", "requested_model", "upstream_model", "message"))):
                raise ValueError("待发告警状态无效")
        for state in data["slow_ttft"].values():
            if (not isinstance(state, dict) or type(state.get("active")) is not bool
                    or type(state.get("alerted", False)) is not bool
                    or (state.get("last_sample_id") is not None and type(state.get("last_sample_id")) is not int)
                    or (state.get("reset_sample_id") is not None and type(state.get("reset_sample_id")) is not int)):
                raise ValueError("慢首字阶段状态无效")
        for event in data["slow_pending"].values():
            if (not isinstance(event, dict) or event.get("kind") != "slow_ttft"
                    or type(event.get("account_id")) is not int or event["account_id"] < 1
                    or type(event.get("id")) is not int or type(event.get("attempts")) is not int
                    or not parse_iso_datetime(event.get("next_at")) or not parse_iso_datetime(event.get("created_at"))
                    or type(event.get("sample_count")) is not int or type(event.get("slow_count")) is not int
                    or not isinstance(event.get("account_name"), str) or not isinstance(event.get("account_type"), str)):
                raise ValueError("慢首字待发告警状态无效")
        self.existed = True
        return data

    @contextmanager
    def transaction(self):
        with _LOCK:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            lock_path = self.path.with_suffix(".lock")
            with lock_path.open("a+") as handle:
                lock_path.chmod(0o600)
                fcntl.flock(handle, fcntl.LOCK_EX)
                try:
                    data = self._read()
                    yield data
                    write_json(self.path, data)
                    self.existed = True
                finally:
                    fcntl.flock(handle, fcntl.LOCK_UN)

    def snapshot(self) -> dict[str, Any]:
        with _LOCK:
            return self._read()

    @contextmanager
    def account_guard(self, account_id: int):
        with _LOCK:
            lock = _ACCOUNT_LOCKS.setdefault((str(self.path), account_id), threading.RLock())
        with lock:
            directory = self.path.parent / "capacity-alert-locks"
            directory.mkdir(parents=True, exist_ok=True, mode=0o700)
            path = directory / f"{account_id}.lock"
            with path.open("a+") as handle:
                path.chmod(0o600)
                fcntl.flock(handle, fcntl.LOCK_EX)
                try:
                    yield
                finally:
                    fcntl.flock(handle, fcntl.LOCK_UN)

    def set_mark(self, account_id: int, marked: bool, expected: str, now: datetime,
                 *, detection_job_id: str | None = None) -> dict[str, Any]:
        with self.account_guard(account_id), self.transaction() as data:
            key = str(account_id)
            if mark_view(account_id, data["marks"].get(key))["version"] != expected:
                raise HTTPException(409, "降智标记已变化，请刷新后重试")
            mark = {"marked": marked, "marked_at": now.isoformat() if marked else None, "changed_at": now.isoformat()}
            if detection_job_id:
                mark["detection_job_id"] = detection_job_id
            data["marks"][key] = mark
            for error_id, event in data["pending"].items():
                if event["account_id"] == account_id:
                    data.setdefault("notifications", {})[error_id] = {"status": "suppressed", "reason": "degradation_mark" if marked else "mark_changed", "at": now.isoformat()}
            data["pending"] = {k: e for k, e in data["pending"].items() if e["account_id"] != account_id}
            for notification_id, event in data["slow_pending"].items():
                if event["account_id"] == account_id:
                    data.setdefault("notifications", {})[notification_id] = {
                        "status": "suppressed", "reason": "degradation_mark" if marked else "mark_changed", "at": now.isoformat()}
            data["slow_pending"] = {k: e for k, e in data["slow_pending"].items() if e["account_id"] != account_id}
            slow_state = data.setdefault("slow_ttft", {}).get(key)
            if slow_state and marked:
                slow_state["alerted"] = True
        return mark_view(account_id, mark)


FIELDS = """e.id,e.account_id,e.created_at,e.error_owner,e.error_phase,e.error_source,e.stream,e.requested_model,e.model,e.upstream_model,
 e.upstream_error_message,e.error_message,to_jsonb(e)->'error_body' AS error_body,
 to_jsonb(e)->'upstream_error_detail' AS upstream_error_detail,
 to_jsonb(e)->'upstream_errors' AS upstream_errors,
 a.name AS account_name,a.platform AS account_platform,a.type AS account_type,a.deleted_at AS account_deleted_at"""


class CapacityAlerts:
    def __init__(self, settings: Any, db: Any, notifier: Any, *, clock: Callable[[], datetime] = utcnow):
        self.store = CapacityAlertStore(Path(settings.usage_query_state_path).with_name("capacity-alert-state.json"))
        self.db, self.notifier, self.clock, self.audit_path = db, notifier, clock, settings.audit_path

    def audit(self, action: str, **data: Any) -> None:
        write_audit(self.audit_path, "capacity_alert_" + action, data)

    def poll(self) -> None:
        self.retire_legacy_warnings()
        state = self.store.snapshot()
        if not state.get("gateway_since"):
            # New evidence rules must not turn the lookback window into a
            # notification replay of errors predating this upgrade.
            with self.store.transaction() as data:
                data.setdefault("gateway_since", self.clock().isoformat())
        if not state.get("apikey_since"):
            with self.store.transaction() as data:
                data.setdefault("apikey_since", self.clock().isoformat())
        if state["cursor"] is None:
            initialized_at = self.clock().isoformat()
            row = self.db.fetch_one("SELECT coalesce(max(id),0) AS id FROM ops_error_logs")
            with self.store.transaction() as data:
                if data["cursor"] is None:
                    data["cursor"], data["initialized_at"] = int(row["id"]), initialized_at
            return
        # Bound each turn while draining every page on subsequent turns.
        for _ in range(20):
            cursor = self.store.snapshot()["cursor"]
            rows = self.db.fetch_all(f"SELECT {FIELDS} FROM ops_error_logs e LEFT JOIN accounts a ON a.id=e.account_id WHERE e.id>%(cursor)s ORDER BY e.id LIMIT 200", {"cursor": cursor})
            if not rows:
                break
            self._collect(rows, advance=True)
            if len(rows) < 200:
                break
        # Logs can commit out of ID order. Replay a bounded indexed time window.
        since = max(self.clock() - timedelta(minutes=5), parse_iso_datetime(state["initialized_at"]))
        after = 0
        while True:
            rows = self.db.fetch_all(f"SELECT {FIELDS} FROM ops_error_logs e LEFT JOIN accounts a ON a.id=e.account_id WHERE e.created_at>=%(since)s AND e.id>%(after)s ORDER BY e.id LIMIT 200", {"since": since, "after": after})
            if not rows:
                break
            self._collect(rows, advance=False)
            after = int(rows[-1]["id"])
            if len(rows) < 200:
                break
        self._poll_slow_ttft()

    def _collect(self, rows: list[dict[str, Any]], *, advance: bool) -> None:
        now, emitted = self.clock(), []
        with self.store.transaction() as data:
            data["seen"] = {k: t for k, t in data["seen"].items() if int(k) > data["cursor"] or parse_iso_datetime(t) >= now - timedelta(minutes=10)}
            for row in rows:
                key = str(row["id"])
                if key in data["seen"] or key in data["pending"]:
                    continue
                data["seen"][key] = now.isoformat()
                message = match_message(row)
                if not message:
                    continue
                at = parse_iso_datetime(row.get("created_at"))
                if not at or at < parse_iso_datetime(data["initialized_at"]):
                    continue
                if row.get("error_owner") == "platform" and at < (parse_iso_datetime(data.get("gateway_since")) or now):
                    continue
                if row.get("account_type") == "apikey" and at < (parse_iso_datetime(data.get("apikey_since")) or now):
                    continue
                mark = data["marks"].get(str(row["account_id"]), {})
                changed = parse_iso_datetime(mark.get("changed_at"))
                suppressed = mark.get("marked") or (changed and at <= changed)
                event = {"id": int(row["id"]), "account_id": int(row["account_id"]), "account_name": sanitize_error_text(row.get("account_name"), 120),
                         "requested_model": sanitize_error_text(row.get("requested_model") or row.get("model") or "未知", 160),
                         "upstream_model": sanitize_error_text(row.get("upstream_model") or "未知", 160), "message": message, "account_type": row.get("account_type", "oauth"),
                         "created_at": at.isoformat(), "next_at": now.isoformat(), "attempts": 0}
                detection_since = parse_iso_datetime(data.get("detection_since"))
                if detection_since and at > detection_since and not mark.get("marked") and not (changed and at <= changed):
                    data.setdefault("detection_events", {}).setdefault("error:" + key,
                        {"account_id": event["account_id"], "created_at": at.isoformat()})
                reason = "degradation_mark" if mark.get("marked") else "mark_changed" if changed and at <= changed else None
                status = "suppressed" if suppressed else "clue"
                data.setdefault("notifications", {})[key] = {"status": status, "reason": reason, "at": now.isoformat()}
                emitted.append((status, event["id"], event["account_id"], reason))
            if advance:
                data["cursor"] = max(data["cursor"], max(int(r["id"]) for r in rows))
        for result, record_id, account_id, reason in emitted:
            self.audit("matched", error_id=record_id, account_id=account_id)
            self.audit(result, error_id=record_id, account_id=account_id, reason=reason)

    def _poll_slow_ttft(self) -> None:
        """Read recent usage metrics only; never calls an upstream service."""
        start, now = self.clock() - timedelta(hours=24), self.clock()
        query = """
            WITH ranked AS (
              SELECT u.id,u.account_id,u.created_at,u.model,u.upstream_model,u.stream,
                     u.first_token_ms,u.duration_ms,u.output_tokens,u.inbound_endpoint,
                     u.image_count,u.image_output_tokens,
                     to_jsonb(u)->>'video_count' AS video_count,
                     to_jsonb(u)->>'video_duration_seconds' AS video_duration_seconds,
                     a.name AS account_name,a.platform AS account_platform,a.type AS account_type,
                     row_number() OVER (PARTITION BY u.account_id ORDER BY u.created_at DESC,u.id DESC) AS sample_rank
              FROM usage_logs u JOIN accounts a ON a.id=u.account_id
              WHERE a.deleted_at IS NULL AND a.platform='openai' AND a.type IN ('oauth','apikey')
                AND u.created_at >= %(start)s AND u.created_at <= %(now)s
                AND u.stream IS TRUE AND u.first_token_ms > 0 AND u.duration_ms > u.first_token_ms
                AND u.output_tokens >= 0 AND coalesce(u.image_count,0)=0 AND coalesce(u.image_output_tokens,0)=0
                AND coalesce(to_jsonb(u)->>'video_count','0') IN ('0','0.0')
                AND coalesce(to_jsonb(u)->>'video_duration_seconds','0') IN ('0','0.0')
                AND coalesce(u.upstream_model,u.model,'') <> ''
                AND (coalesce(u.inbound_endpoint,'') || ' ' || coalesce(u.upstream_model,u.model,'')) !~* '(/audio|/images|/videos|-image|-video|-tts|-stt|realtime|whisper|dall-e)'
            )
            SELECT r.id,a.id AS account_id,r.created_at,r.model,r.upstream_model,r.stream,
                   r.first_token_ms,r.duration_ms,r.output_tokens,r.inbound_endpoint,
                   r.image_count,r.image_output_tokens,r.video_count,r.video_duration_seconds,
                   a.name AS account_name,a.platform AS account_platform,a.type AS account_type
            FROM accounts a LEFT JOIN ranked r ON r.account_id=a.id AND r.sample_rank <= 10
            WHERE a.deleted_at IS NULL AND a.platform='openai' AND a.type IN ('oauth','apikey')
            ORDER BY account_id,r.created_at DESC,r.id DESC
        """
        try:
            rows = self.db.fetch_all(query, {"start": start, "now": now})
        except Exception as exc:
            self.audit("slow_ttft_query_error", error=type(exc).__name__)
            return
        grouped: dict[int, list[dict[str, Any]]] = {}
        accounts: dict[int, dict[str, Any]] = {}
        for row in rows:
            try:
                account_id = int(row["account_id"])
            except (KeyError, TypeError, ValueError):
                continue
            accounts[account_id] = {"id": account_id, "platform": row.get("account_platform"), "type": row.get("account_type"),
                                    "name": row.get("account_name") or f"账号 {account_id}"}
            grouped.setdefault(account_id, []).append(row)
        with self.store.transaction() as data:
            states = data.setdefault("slow_ttft", {})
            for key in list(states):
                if int(key) not in accounts:
                    states.pop(key, None)
            for aid, rows in grouped.items():
                samples = [sample for row in rows if (sample := slow_ttft_sample(row, accounts[aid]))]
                warning = slow_ttft_warning(samples, now)
                previous = states.get(str(aid), {"active": False, "alerted": False})
                newest = max(samples, key=lambda sample: (sample['at'], sample['id']), default=None)
                prior_at = parse_iso_datetime(previous.get('last_sample_at') or previous.get('last_at'))
                prior_id = int(previous.get('last_sample_id') or 0)
                fast = [sample for sample in samples if sample['first_token_ms'] <= 10000
                        and (prior_at is None or (sample['at'], sample['id']) > (prior_at, prior_id))]
                active = bool(warning and warning["active"])
                state = {**previous, "active": active}
                if newest:
                    state.update(last_sample_id=newest['id'], last_sample_at=newest['at'].isoformat())
                if fast:
                    reset = max(fast, key=lambda sample: (sample['at'], sample['id']))
                    state.update(alerted=False, reset_sample_id=reset['id'])
                if active and not state.get('alerted'):
                    key = f"slow:{aid}:{warning['latest_sample_id']}"
                    mark = data["marks"].get(str(aid), {})
                    since = parse_iso_datetime(data.get("detection_since"))
                    at = parse_iso_datetime(warning["latest_at"])
                    changed = parse_iso_datetime(mark.get("changed_at"))
                    allowed = bool(since and at and at > since and not mark.get("marked") and not (changed and at <= changed))
                    if allowed:
                        data.setdefault("detection_events", {}).setdefault(key, {"account_id": aid, "created_at": at.isoformat()})
                    state.update(alerted=True, last_at=warning["latest_at"])
                    data.setdefault("notifications", {})[key] = {"status": "clue" if allowed else "suppressed",
                        "reason": None if allowed else "degradation_mark" if mark.get("marked") else "historical", "at": now.isoformat()}
                    self.audit("slow_ttft_matched", account_id=aid, sample_id=warning["latest_sample_id"])
                states[str(aid)] = state

    def retire_legacy_warnings(self):
        if self.store.snapshot().get("clue_mode_since"):
            return
        with self.store.transaction() as data:
            if data.get("clue_mode_since"):
                return
            now = self.clock().isoformat()
            for key in (*data["pending"], *data.get("slow_pending", {})):
                data.setdefault("notifications", {})[key] = {"status": "retired", "reason": "clue_only", "at": now}
            data["pending"], data["slow_pending"], data["detection_events"] = {}, {}, {}
            data["clue_mode_since"] = data["detection_since"] = now

    def deliver_one(self, key: str) -> None:
        self.retire_legacy_warnings()

    def deliver_slow_one(self, key: str) -> None:
        self.retire_legacy_warnings()

    def deliver_due(self) -> None:
        self.retire_legacy_warnings()

    async def collect_loop(self) -> None:
        failures = 0
        while True:
            try:
                await asyncio.to_thread(self.poll)
                failures = 0
            except Exception:
                failures += 1
                self.audit("collector_error", error="错误记录或告警状态读取失败")
            await asyncio.sleep(min(30, 2 ** min(failures + 1, 5)))

    async def delivery_loop(self) -> None:
        failures = 0
        while True:
            try:
                await asyncio.to_thread(self.deliver_due)
                failures = 0
            except Exception:
                failures += 1
                self.audit("delivery_error", error="告警投递或状态保存失败")
            await asyncio.sleep(min(30, 2 ** min(failures, 5)) if failures else .1)
