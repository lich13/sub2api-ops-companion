"""Scheduled rechecks can clear a degradation mark, never change scheduling."""
from __future__ import annotations

import asyncio

from fastapi import HTTPException

from .account_locks import control_lock
from .audit import write_audit
from .bark import sanitize_error_text
from .operation_versions import versions

TARGET = "gpt-5.6-luna"


class DegradationRechecks:
    @staticmethod
    def owns_degradation_pause(value):
        previous = value.get("disposition") or {}
        return (value.get("hold") is True and previous.get("status") == "completed"
                and previous.get("schedule_verified") is True)

    @staticmethod
    def recheck_result_is_normal(job):
        report = job.get("report") or {}
        first, prediction = job.get("first_group_prediction"), report.get("prediction")
        groups = job.get("groups") or []
        used = report.get("used_outputs")
        return (job.get("automatic") is True and job.get("degradation_recheck") is True
                and job.get("status") == "completed" and job.get("first_group_valid") is True
                and isinstance(first, str) and bool(first.strip()) and first != TARGET
                and isinstance(prediction, str) and bool(prediction.strip()) and prediction != TARGET
                and type(used) is int and used >= 1 and not job.get("error_code")
                and not job.get("can_retry")
                and bool(groups) and used == sum(group.get("status") == "completed" for group in groups)
                and all(group.get("status") in {"completed", "skipped"} for group in groups))

    def recheck_target_changed(self, row, value, context):
        return (not row or not value.get("enabled")
                or versions(row)["model_test"] != context.get("recheck_account_version")
                or row.get("schedulable") is not context.get("recheck_schedulable"))

    async def complete_recheck(self, job):
        if not self.recheck_result_is_normal(job):
            return None
        aid = job["account_id"]
        current = self.control(aid).get("recheck_recovery") or {}
        if current.get("job_id") == job["id"]:
            return await asyncio.to_thread(self.finish_recheck_recovery, aid)
        await self.guard(job)

        def prepare():
            from .desktop_api import ACCOUNT_SQL
            with control_lock(self.r.db, aid):
                value, mark = self.control(aid), self.mark(aid)
                row = self.r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": aid})
                if (value["generation"] != job.get("detection_generation")
                        or mark["version"] != job.get("detection_mark_version")
                        or self.recheck_target_changed(row, value, job)):
                    raise HTTPException(409, "账号或人工操作已变化，旧复查不再解除标记")
                reason = self.reason(row, mark, recheck=True)
                if reason:
                    raise HTTPException(409, reason)
                report = job["report"]
                receipt = {
                    "status": "pending", "job_id": job["id"], "generation": value["generation"],
                    "mark_version": mark["version"], "mark_cleared": False,
                    "recheck_account_version": job["recheck_account_version"],
                    "recheck_schedulable": job["recheck_schedulable"],
                    "created_at": self.clock().isoformat(), "model_id": job["requested_model"],
                    "prediction": report["prediction"], "first_group_prediction": job["first_group_prediction"],
                    "probability": report.get("probability"), "valid_groups": report["used_outputs"],
                }
                self.update(aid, recheck_recovery=receipt, status="handling", reason="正在取消降智标记")
        await asyncio.to_thread(prepare)
        return await asyncio.to_thread(self.finish_recheck_recovery, aid)

    def finish_recheck_recovery(self, aid):
        """Resume a saved local mutation by its origin; no model or admin requests."""
        from .desktop_api import ACCOUNT_SQL
        with control_lock(self.r.db, aid):
            value = self.control(aid)
            receipt = value.get("recheck_recovery")
            if not receipt or receipt.get("status") != "pending":
                return receipt
            row = self.r.db.fetch_one(ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": aid})
            state = self.r.capacity_alerts.store.snapshot()["marks"].get(str(aid), {})
            mark = self.mark(aid)
            adopted = not mark["marked"] and state.get("detection_job_id") == receipt["job_id"]
            changed = (value["generation"] != receipt["generation"]
                       or self.recheck_target_changed(row, value, receipt)
                       or (mark["version"] != receipt["mark_version"] and not adopted))
            reason = "账号或人工操作已变化，旧复查不再解除标记" if changed else ""
            if not changed and not adopted:
                reason = self.reason(row, mark, recheck=True)
            if reason:
                receipt.update(status="overridden", reason=reason, mark_cleared=adopted)
                self.update(aid, recheck_recovery=receipt)
                return receipt
            if not adopted:
                self.r.capacity_alerts.store.set_mark(
                    aid, False, mark["version"], self.clock(), detection_job_id=receipt["job_id"])
            verified = self.mark(aid)
            state = self.r.capacity_alerts.store.snapshot()["marks"].get(str(aid), {})
            if verified["marked"] or state.get("detection_job_id") != receipt["job_id"]:
                raise ValueError("降智标记取消结果尚未确认")
            now = self.clock().isoformat()
            receipt.update(status="completed", mark_cleared=True, mark_version_after=verified["version"],
                           completed_at=now, reason="检测恢复正常，已取消降智标记")
            with self.store.transaction() as data:
                current = data["accounts"][str(aid)]
                if current["generation"] != receipt["generation"]:
                    raise HTTPException(409, "人工操作已替代复查结果")
                # Keep the existing hold: unmarking must not let another
                # automation silently reopen a stopped account.
                current.update(recheck_recovery=receipt)
                events = data.setdefault("disposition_notifications", {})
                for event in events.values():
                    if (event["account_id"] == aid and event.get("kind") != "recovered"
                            and event.get("status") in {"queued", "retry"}):
                        event.update(status="suppressed", reason="degradation_recovered")
                events.setdefault(receipt["job_id"], {
                    "kind": "recovered", "job_id": receipt["job_id"], "account_id": aid,
                    "account_name": sanitize_error_text(row.get("name"), 120), "account_type": row.get("type"),
                    "generation": receipt["generation"], "mark_version": verified["version"],
                    "model_id": receipt["model_id"], "prediction": receipt["prediction"],
                    "first_group_prediction": receipt["first_group_prediction"],
                    "probability": receipt.get("probability"), "valid_groups": receipt["valid_groups"],
                    "schedulable": row["schedulable"], "triggers": ["scheduled"],
                    "created_at": now, "next_at": now, "attempts": 0, "status": "queued",
                })
            self.s.invalidate()
            write_audit(self.r.settings.audit_path, "model_detection_recovered", {
                "account_id": aid, "job_id": receipt["job_id"], "mark_cleared": True,
                "schedulable": row["schedulable"],
            })
            return receipt
