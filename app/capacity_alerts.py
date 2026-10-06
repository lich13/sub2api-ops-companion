"""Passive OpenAI capacity alerts. No quota, model, or account writes."""
from __future__ import annotations

import asyncio
import fcntl
import hashlib
import json
import threading
from concurrent.futures import ThreadPoolExecutor
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
        runtime = self.notifier.runtime_config()
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
                suppressed = mark.get("marked") or (changed and at <= changed) or (runtime.config_valid and not runtime.enabled)
                event = {"id": int(row["id"]), "account_id": int(row["account_id"]), "account_name": sanitize_error_text(row.get("account_name"), 120),
                         "requested_model": sanitize_error_text(row.get("requested_model") or row.get("model") or "未知", 160),
                         "upstream_model": sanitize_error_text(row.get("upstream_model") or "未知", 160), "message": message, "account_type": row.get("account_type", "oauth"),
                         "created_at": at.isoformat(), "next_at": now.isoformat(), "attempts": 0}
                detection_since = parse_iso_datetime(data.get("detection_since"))
                if detection_since and at > detection_since and not mark.get("marked") and not (changed and at <= changed):
                    data.setdefault("detection_events", {}).setdefault("error:" + key,
                        {"account_id": event["account_id"], "created_at": at.isoformat()})
                if not suppressed:
                    data["pending"][key] = event
                reason = "degradation_mark" if mark.get("marked") else "mark_changed" if changed and at <= changed else "disabled" if suppressed else None
                data.setdefault("notifications", {})[key] = {"status": "suppressed" if suppressed else "queued", "reason": reason, "at": now.isoformat()}
                emitted.append(("suppressed" if suppressed else "queued", event["id"], event["account_id"], reason))
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
            SELECT * FROM ranked WHERE sample_rank <= 10
            ORDER BY account_id,created_at DESC,id DESC
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
        runtime = self.notifier.runtime_config()
        emitted: list[tuple[str, str, int]] = []
        with self.store.transaction() as data:
            slow_state = data.setdefault("slow_ttft", {})
            slow_pending = data.setdefault("slow_pending", {})
            # A live-account query is authoritative for removal of stale state.
            for key in list(slow_state):
                if int(key) not in accounts:
                    slow_state.pop(key, None)
                    for pending_key, event in list(slow_pending.items()):
                        if event["account_id"] == int(key):
                            slow_pending.pop(pending_key, None)
            for account_id, raw_rows in grouped.items():
                samples = []
                account = accounts[account_id]
                for row in raw_rows:
                    sample = slow_ttft_sample(row, account)
                    if sample:
                        samples.append(sample)
                warning = slow_ttft_warning(samples, now)
                key = str(account_id)
                previous = slow_state.get(key, {"active": False, "alerted": False})
                if warning is None:
                    for pending_key, event in list(slow_pending.items()):
                        if event["account_id"] == account_id:
                            slow_pending.pop(pending_key, None)
                            data.setdefault("notifications", {})[pending_key] = {"status": "reset", "at": now.isoformat()}
                    slow_state[key] = {**previous, "active": False, "last_sample_id": max((s["id"] for s in samples), default=previous.get("last_sample_id")),
                                       "reset_sample_id": max((s["id"] for s in samples), default=previous.get("reset_sample_id"))}
                    continue
                is_active = bool(warning["active"])
                latest_id = int(warning["latest_sample_id"])
                state = {**previous, "active": is_active, "last_sample_id": latest_id, "last_at": warning["latest_at"]}
                if not is_active:
                    state["alerted"] = False
                    state["reset_sample_id"] = latest_id
                    for pending_key, event in list(slow_pending.items()):
                        if event["account_id"] == account_id:
                            slow_pending.pop(pending_key, None)
                            data.setdefault("notifications", {})[pending_key] = {"status": "reset", "at": now.isoformat()}
                    slow_state[key] = state
                    continue
                mark = data["marks"].get(key, {})
                detection_since = parse_iso_datetime(data.get("detection_since"))
                sample_at = parse_iso_datetime(warning.get("latest_at"))
                if not previous.get("active") and not mark.get("marked") and detection_since and sample_at and sample_at > detection_since:
                    data.setdefault("detection_events", {}).setdefault(f"slow:{account_id}:{latest_id}",
                        {"account_id": account_id, "created_at": sample_at.isoformat()})
                if mark.get("marked") or (runtime.config_valid and not runtime.enabled):
                    for pending_key, event in list(slow_pending.items()):
                        if event["account_id"] == account_id:
                            slow_pending.pop(pending_key, None)
                            data.setdefault("notifications", {})[pending_key] = {
                                "status": "suppressed", "reason": "degradation_mark" if mark.get("marked") else "disabled", "at": now.isoformat()}
                    state["alerted"] = True
                    slow_state[key] = state
                    notification_key = f"slow:{account_id}:{latest_id}"
                    data.setdefault("notifications", {})[notification_key] = {
                        "status": "suppressed", "reason": "degradation_mark" if mark.get("marked") else "disabled", "at": now.isoformat()}
                    self.audit("slow_ttft_suppressed", account_id=account_id, sample_id=latest_id,
                               reason="degradation_mark" if mark.get("marked") else "disabled")
                    continue
                notification_key = f"slow:{account_id}:{latest_id}"
                if not previous.get("active"):
                    state["alerted"] = False
                existing_key = next((pending_key for pending_key, pending_event in slow_pending.items()
                                     if pending_event["account_id"] == account_id), None)
                if not state.get("alerted") and existing_key:
                    # Keep one durable notification per slow stage even when a
                    # newer sample arrives while the original push is retrying.
                    pending_event = slow_pending[existing_key]
                    pending_event.update(id=latest_id, sample_count=warning["sample_count"],
                                         slow_count=warning["slow_count"], latest_first_token_ms=warning["latest_first_token_ms"])
                elif not state.get("alerted"):
                    event = {"kind": "slow_ttft", "id": latest_id, "account_id": account_id,
                             "account_name": sanitize_error_text(account.get("name"), 120),
                             "account_type": account.get("type", "oauth"), "sample_count": warning["sample_count"],
                             "slow_count": warning["slow_count"], "threshold_ms": warning["threshold_ms"],
                             "latest_first_token_ms": warning["latest_first_token_ms"], "created_at": now.isoformat(),
                             "next_at": now.isoformat(), "attempts": 0}
                    slow_pending[notification_key] = event
                    data.setdefault("notifications", {})[notification_key] = {"status": "queued", "at": now.isoformat()}
                    emitted.append(("queued", notification_key, account_id))
                slow_state[key] = state
        for result, notification_key, account_id in emitted:
            self.audit("slow_ttft_matched", account_id=account_id, notification=notification_key)
            self.audit(result, account_id=account_id, notification=notification_key)

    def deliver_one(self, key: str) -> None:
        pending = self.store.snapshot()["pending"].get(key)
        if not pending:
            return
        with self.store.account_guard(pending["account_id"]):
            state = self.store.snapshot()
            event = state["pending"].get(key)
            if not event or parse_iso_datetime(event["next_at"]) > self.clock():
                return
            runtime = self.notifier.runtime_config()
            if not runtime.config_valid:
                return
            live = self.db.fetch_one("SELECT platform,type,deleted_at FROM accounts WHERE id=%(id)s", {"id": event["account_id"]})
            suppressed = (state["marks"].get(str(event["account_id"]), {}).get("marked") or not runtime.enabled or not live
                          or live.get("deleted_at") or live.get("platform") != "openai" or live.get("type") not in {"oauth", "apikey"})
            if suppressed:
                with self.store.transaction() as data:
                    data["pending"].pop(key, None)
                    data.setdefault("notifications", {})[key] = {"status": "suppressed", "reason": "degradation_mark" if state["marks"].get(str(event["account_id"]), {}).get("marked") else "disabled", "at": self.clock().isoformat()}
                self.audit("suppressed", error_id=event["id"], account_id=event["account_id"])
                return
            # Persist an attempt before the network boundary; crashes resume conservatively.
            with self.store.transaction() as data:
                queued = data["pending"][key]
                queued["attempts"] += 1
                queued["next_at"] = (self.clock() + timedelta(seconds=RETRY_SECONDS[min(queued["attempts"] - 1, 3)])).isoformat()
            account_kind = '（Key）' if event.get('account_type') == 'apikey' else ''
            body = (f"账号：{event['account_name']} #{event['account_id']}{account_kind}\n请求模型：{event['requested_model']}\n"
                    f"上游模型：{event['upstream_model']}\n错误：{event['message']}\n"
                    f"时间：{_beijing_time(event['created_at'])}\n错误记录：#{event['id']}")
            result = self.notifier.push(TITLE, body, timeout=3, options=PUSH_OPTIONS)
            with self.store.transaction() as data:
                if result.success:
                    data["pending"].pop(key, None)
                data.setdefault("notifications", {})[key] = {"status": "delivered" if result.success else "retry", "at": self.clock().isoformat(), "attempts": queued["attempts"], "next_at": None if result.success else queued["next_at"]}
            self.audit("delivered" if result.success else "retry", error_id=event["id"], account_id=event["account_id"], error_code=result.error_code)

    def deliver_slow_one(self, key: str) -> None:
        pending = self.store.snapshot().get("slow_pending", {}).get(key)
        if not pending:
            return
        account_id = pending["account_id"]
        with self.store.account_guard(account_id):
            state = self.store.snapshot()
            event = state.get("slow_pending", {}).get(key)
            if not event or parse_iso_datetime(event["next_at"]) > self.clock():
                return
            runtime = self.notifier.runtime_config()
            if not runtime.config_valid:
                return
            live = self.db.fetch_one("SELECT platform,type,deleted_at FROM accounts WHERE id=%(id)s", {"id": account_id})
            marked = state["marks"].get(str(account_id), {}).get("marked")
            suppressed = marked or not runtime.enabled or not live or live.get("deleted_at") or live.get("platform") != "openai" or live.get("type") not in {"oauth", "apikey"}
            if suppressed:
                with self.store.transaction() as data:
                    data.setdefault("slow_pending", {}).pop(key, None)
                    data.setdefault("notifications", {})[key] = {"status": "suppressed", "reason": "degradation_mark" if marked else "disabled", "at": self.clock().isoformat()}
                    if marked:
                        data.setdefault("slow_ttft", {}).setdefault(str(account_id), {})["alerted"] = True
                self.audit("slow_ttft_suppressed", account_id=account_id, sample_id=event["id"])
                return
            with self.store.transaction() as data:
                queued = data["slow_pending"][key]
                queued["attempts"] += 1
                queued["next_at"] = (self.clock() + timedelta(seconds=RETRY_SECONDS[min(queued["attempts"] - 1, 3)])).isoformat()
            body = (f"账号：{event['account_name']} #{account_id}（{'Key' if event.get('account_type') == 'apikey' else 'OAuth'}）\n"
                    f"慢首字：{event['slow_count']}/{event['sample_count']} 条超过 {event['threshold_ms'] / 1000:g} 秒\n"
                    f"最近首字：{event['latest_first_token_ms'] / 1000:.2f}s\n时间：{_beijing_time(event['created_at'])}")
            result = self.notifier.push(TITLE, body, timeout=3, options=PUSH_OPTIONS)
            with self.store.transaction() as data:
                if result.success:
                    data["slow_pending"].pop(key, None)
                    data.setdefault("slow_ttft", {}).setdefault(str(account_id), {})["alerted"] = True
                data.setdefault("notifications", {})[key] = {"status": "delivered" if result.success else "retry", "at": self.clock().isoformat(),
                                                               "attempts": queued["attempts"], "next_at": None if result.success else queued["next_at"]}
            self.audit("delivered" if result.success else "retry", account_id=account_id, notification=key, error_code=result.error_code)

    def deliver_due(self) -> None:
        state = self.store.snapshot()
        pending = {**state["pending"], **state.get("slow_pending", {})}
        due = [k for k, e in pending.items() if parse_iso_datetime(e["next_at"]) <= self.clock()]
        # Each account remains ordered; different accounts cannot block one another.
        chosen, accounts = [], set()
        for key in due:
            account_id = pending[key]["account_id"]
            if account_id not in accounts:
                chosen.append(key)
                accounts.add(account_id)
            if len(chosen) == 4:
                break
        if chosen:
            with ThreadPoolExecutor(max_workers=4) as executor:
                list(executor.map(lambda key: self.deliver_slow_one(key) if key.startswith("slow:") else self.deliver_one(key), chosen))

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
