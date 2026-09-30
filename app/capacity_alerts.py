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

MESSAGES = (
    "Our servers are currently overloaded. Please try again later.",
    "Selected model is at capacity. Please try a different model.",
)
RETRY_SECONDS = (5, 30, 120, 600)
TITLE = "⚠️ Codex OAuth 疑似降智"
PUSH_OPTIONS = {"level": "critical", "sound": "alarm", "group": "Sub2Ops 疑似降智"}
_LOCK = threading.RLock()
_ACCOUNT_LOCKS: dict[tuple[str, int], threading.RLock] = {}


def utcnow() -> datetime:
    return datetime.now(timezone.utc)


def match_message(row: dict[str, Any]) -> str | None:
    if (row.get("account_platform"), row.get("account_type")) != ("openai", "oauth"):
        return None
    if row.get("account_deleted_at") or row.get("error_owner") != "provider" or row.get("error_phase") != "upstream":
        return None

    def extract(value: Any, *, allow_plain: bool = True) -> str | None:
        if isinstance(value, str):
            try:
                value = json.loads(value)
            except ValueError:
                if not allow_plain:
                    return None
                normalized = " ".join(value.casefold().split()).rstrip(".!?。！？,，;；:： ")
                for message in MESSAGES:
                    if normalized == message.casefold().rstrip("."):
                        return message
                return None
        if isinstance(value, dict):
            error = value.get("error")
            if isinstance(error, dict):
                return extract(error.get("message"), allow_plain=True)
        return None

    for field in ("upstream_error_message", "error_message"):
        found = extract(row.get(field), allow_plain=True)
        if found:
            return found
    for field in ("error_body", "upstream_error_detail"):
        found = extract(row.get(field), allow_plain=False)
        if found:
            return found
    return None


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
            return {"version": 1, "cursor": None, "initialized_at": None, "seen": {}, "marks": {}, "pending": {}}
        if not isinstance(data, dict) or data.get("version") != 1:
            raise ValueError("告警状态版本无效")
        if any(not isinstance(data.get(k), dict) for k in ("seen", "marks", "pending")):
            raise ValueError("告警状态无效")
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

    def set_mark(self, account_id: int, marked: bool, expected: str, now: datetime) -> dict[str, Any]:
        with self.account_guard(account_id), self.transaction() as data:
            key = str(account_id)
            if mark_view(account_id, data["marks"].get(key))["version"] != expected:
                raise HTTPException(409, "降智标记已变化，请刷新后重试")
            mark = {"marked": marked, "marked_at": now.isoformat() if marked else None, "changed_at": now.isoformat()}
            data["marks"][key] = mark
            data["pending"] = {k: e for k, e in data["pending"].items() if e["account_id"] != account_id}
        return mark_view(account_id, mark)


FIELDS = """e.id,e.account_id,e.created_at,e.error_owner,e.error_phase,e.requested_model,e.model,e.upstream_model,
 e.upstream_error_message,e.error_message,to_jsonb(e)->'error_body' AS error_body,
 to_jsonb(e)->'upstream_error_detail' AS upstream_error_detail,
 a.name AS account_name,a.platform AS account_platform,a.type AS account_type,a.deleted_at AS account_deleted_at"""


class CapacityAlerts:
    def __init__(self, settings: Any, db: Any, notifier: Any, *, clock: Callable[[], datetime] = utcnow):
        self.store = CapacityAlertStore(Path(settings.usage_query_state_path).with_name("capacity-alert-state.json"))
        self.db, self.notifier, self.clock, self.audit_path = db, notifier, clock, settings.audit_path

    def audit(self, action: str, **data: Any) -> None:
        write_audit(self.audit_path, "capacity_alert_" + action, data)

    def poll(self) -> None:
        state = self.store.snapshot()
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
                mark = data["marks"].get(str(row["account_id"]), {})
                changed = parse_iso_datetime(mark.get("changed_at"))
                suppressed = mark.get("marked") or (changed and at <= changed) or (runtime.config_valid and not runtime.enabled)
                event = {"id": int(row["id"]), "account_id": int(row["account_id"]), "account_name": sanitize_error_text(row.get("account_name"), 120),
                         "requested_model": sanitize_error_text(row.get("requested_model") or row.get("model") or "未知", 160),
                         "upstream_model": sanitize_error_text(row.get("upstream_model") or "未知", 160), "message": message,
                         "created_at": at.isoformat(), "next_at": now.isoformat(), "attempts": 0}
                if not suppressed:
                    data["pending"][key] = event
                emitted.append(("suppressed" if suppressed else "queued", event["id"], event["account_id"]))
            if advance:
                data["cursor"] = max(data["cursor"], max(int(r["id"]) for r in rows))
        for result, record_id, account_id in emitted:
            self.audit(result, error_id=record_id, account_id=account_id)

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
                          or live.get("deleted_at") or (live.get("platform"), live.get("type")) != ("openai", "oauth"))
            if suppressed:
                with self.store.transaction() as data:
                    data["pending"].pop(key, None)
                self.audit("suppressed", error_id=event["id"], account_id=event["account_id"])
                return
            # Persist an attempt before the network boundary; crashes resume conservatively.
            with self.store.transaction() as data:
                queued = data["pending"][key]
                queued["attempts"] += 1
                queued["next_at"] = (self.clock() + timedelta(seconds=RETRY_SECONDS[min(queued["attempts"] - 1, 3)])).isoformat()
            body = (f"账号：{event['account_name']} #{event['account_id']}\n请求模型：{event['requested_model']}\n"
                    f"上游模型：{event['upstream_model']}\n错误：{event['message']}\n"
                    f"时间：{_beijing_time(event['created_at'])}\n错误记录：#{event['id']}")
            result = self.notifier.push(TITLE, body, timeout=3, options=PUSH_OPTIONS)
            with self.store.transaction() as data:
                if result.success:
                    data["pending"].pop(key, None)
            self.audit("delivered" if result.success else "retry", error_id=event["id"], account_id=event["account_id"], error_code=result.error_code)

    def deliver_due(self) -> None:
        pending = self.store.snapshot()["pending"]
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
                list(executor.map(self.deliver_one, chosen))

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
