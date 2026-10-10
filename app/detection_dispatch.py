"""Durable automatic dispatch budget and confirmed-disposition notifications."""
from __future__ import annotations

import asyncio
import threading
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta

from fastapi import HTTPException

from .audit import write_audit
from .bark import _beijing_time, sanitize_error_text
from .usage_query import parse_iso_datetime

COOLDOWN_SECONDS = 300
RETRY_SECONDS = (5, 30, 120, 600)
TITLE = "⚠️ Codex 已判定降智，已停止调度"
RECOVERED_TITLE = "✅ Codex 检测恢复正常，已取消降智标记"
OPTIONS = {"level": "critical", "sound": "alarm", "group": "Sub2Ops 降智处置"}


class DetectionDispatch:
    def next_allowed_at(self, aid):
        record = self.store.read().get("automatic_starts", {}).get(str(aid))
        if not record:
            return None
        at = parse_iso_datetime(record.get("started_at"))
        if at is None or not isinstance(record.get("job_id"), str):
            raise ValueError("自动检测派发状态无效")
        return (at + timedelta(seconds=COOLDOWN_SECONDS)).isoformat()

    def register_dispatch(self, job):
        if not job.get("automatic"):
            return
        with self.store.transaction() as data:
            starts = data.setdefault("automatic_starts", {})
            previous = starts.get(str(job["account_id"]))
            if previous:
                at = parse_iso_datetime(previous.get("started_at"))
                if at is None:
                    raise ValueError("自动检测派发时间无效")
                if previous.get("job_id") == job["id"]:
                    return
                if self.clock() < at + timedelta(seconds=COOLDOWN_SECONDS):
                    raise HTTPException(409, "自动检测仍在五分钟冷却内")
            starts[str(job["account_id"])] = {"job_id": job["id"], "started_at": self.clock().isoformat()}

    def migrate_dispatches(self):
        """Import the last actual automatic start without replaying old results."""
        jobs = self.s.model_tests.store.read()["jobs"]
        with self.store.transaction() as data:
            starts = data.setdefault("automatic_starts", {})
            for job in jobs.values():
                at = parse_iso_datetime(job.get("started_at"))
                if not job.get("automatic") or at is None:
                    continue
                previous = parse_iso_datetime(starts.get(str(job["account_id"]), {}).get("started_at"))
                if previous is None or at > previous:
                    starts[str(job["account_id"])] = {"job_id": job["id"], "started_at": at.isoformat()}

    def commit_disposition(self, aid, disposition, row):
        """The successful receipt and outbox entry share one atomic commit."""
        with self.store.transaction() as data:
            value = data["accounts"][str(aid)]
            if value["generation"] != disposition["generation"]:
                raise HTTPException(409, "人工操作已替代自动处置")
            value.update(disposition=disposition, status="paused",
                         reason="已标记降智" if disposition["schedule_verified"] else disposition.get("reason", "停调度待核对"))
            if disposition["marked"] and disposition["schedule_verified"] and disposition["status"] == "completed":
                now = self.clock().isoformat()
                data.setdefault("disposition_notifications", {}).setdefault(disposition["job_id"], {
                    "job_id": disposition["job_id"], "account_id": aid,
                    "account_name": sanitize_error_text(row.get("name"), 120),
                    "account_type": row.get("type"), "generation": disposition["generation"],
                    "model_id": disposition.get("model_id", "未知"),
                    "prediction": disposition.get("prediction", "未知"),
                    "probability": disposition.get("probability"), "triggers": disposition.get("triggers", []),
                    "created_at": now, "next_at": now, "attempts": 0, "status": "queued"})
        self.s.invalidate()

    def notification_is_current(self, event, current):
        if event.get('status') not in {'queued', 'retry'} or current.get('generation') != event['generation']:
            return False
        recovered = event.get('kind') == 'recovered'
        receipt = current.get('recheck_recovery' if recovered else 'disposition') or {}
        if receipt.get('job_id') != event['job_id'] or receipt.get('status') != 'completed':
            return False
        if recovered:
            mark = self.mark(event['account_id'])
            return receipt.get('mark_cleared') is True and not mark['marked'] and mark['version'] == event['mark_version']
        return True

    def _deliver_disposition(self, key):
        event = self.store.read().get("disposition_notifications", {}).get(key)
        if not event or event.get("status") not in {"queued", "retry"}:
            return
        due = parse_iso_datetime(event.get("next_at"))
        if due is None:
            raise ValueError("处置通知时间无效")
        if due > self.clock():
            return
        notifier = self.r.bark_notifier
        runtime = notifier.runtime_config()
        if not runtime.config_valid:
            return
        with self.store.transaction() as data:
            event = data["disposition_notifications"][key]
            current = data["accounts"].get(str(event["account_id"]), {})
            if not runtime.enabled or not self.notification_is_current(event, current):
                event.update(status="suppressed", reason="disabled" if not runtime.enabled else "manual_intervention")
                return
            event["attempts"] += 1
            event["next_at"] = (self.clock() + timedelta(seconds=RETRY_SECONDS[min(event["attempts"] - 1, 3)])).isoformat()
        causes = event["triggers"]
        labels = []
        for prefix, label in (("error:", "上游错误"), ("slow:", "慢首字"), ("scheduled", "定时检测"),
                              ("recovery", "额度恢复"), ("credit", "用卡恢复")):
            if any(cause.startswith(prefix) for cause in causes):
                labels.append(label)
        probability = event.get("probability")
        match = f"{probability * 100:.2f}%" if isinstance(probability, (int, float)) else "未知"
        recovered = event.get('kind') == 'recovered'
        body = (f"账号：{event['account_name']} #{event['account_id']}（{'Key' if event['account_type'] == 'apikey' else 'OAuth'}）\n"
                f"触发：{'、'.join(labels) or '自动检测'}\n请求模型：{event['model_id']}\n"
                f"首组指纹推测：{event.get('first_group_prediction') or event['prediction']}\n")
        if recovered:
            body += (f"本轮指纹推测：{event['prediction']}\n有效组数：{event['valid_groups']}\n"
                     f"调度保持{'开启' if event['schedulable'] else '关闭'}\n")
        body += f"匹配度：{match}\n时间：{_beijing_time(event['created_at'])}"
        latest = self.store.read().get('disposition_notifications', {}).get(key)
        if not latest or not self.notification_is_current(latest, self.control(event['account_id'])):
            with self.store.transaction() as data:
                data['disposition_notifications'][key].update(status='suppressed', reason='manual_intervention')
            return
        try:
            result = notifier.push(RECOVERED_TITLE if recovered else TITLE, body, timeout=3, options=OPTIONS)
            success, code = result.success, result.error_code
        except Exception:
            success, code = False, "notification_failed"
        with self.store.transaction() as data:
            saved = data["disposition_notifications"][key]
            if not success and saved['status'] == 'suppressed':
                return
            saved.update(status="delivered" if success else "retry", delivered_at=self.clock().isoformat() if success else None,
                         error_code=None if success else code)
        write_audit(self.r.settings.audit_path, "model_detection_notification", {
            "account_id": event["account_id"], "job_id": key, "kind": event.get('kind', 'degraded'),
            "status": "delivered" if success else "retry"})

    def deliver_notifications(self):
        if not hasattr(self, "_notification_lock"):
            self._notification_lock = threading.Lock()
        if not self._notification_lock.acquire(blocking=False):
            return
        try:
            keys = [key for key, value in self.store.read().get("disposition_notifications", {}).items()
                    if value.get("status") in {"queued", "retry"}]
            with ThreadPoolExecutor(max_workers=4) as executor:
                list(executor.map(self._deliver_disposition, keys))
        finally:
            self._notification_lock.release()

    async def notification_loop(self):
        while True:
            try:
                await asyncio.to_thread(self.deliver_notifications)
            except Exception as exc:
                write_audit(self.r.settings.audit_path, "model_detection_notification_failed", {"kind": type(exc).__name__})
            await asyncio.sleep(2)
