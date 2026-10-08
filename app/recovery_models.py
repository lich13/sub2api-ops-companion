"""Nonblocking bridge from quota recovery to one-group ModelTrace jobs."""
from __future__ import annotations

import asyncio
from datetime import timedelta

from fastapi import HTTPException

from .operation_versions import digest, versions
from .recovery_policy import recovery_method
from .usage_query import parse_iso_datetime


class RecoveryModels:
    def bind_recovery(self, monitor, loop):
        self.recovery_loop = loop
        monitor.model_verifier = self.poll_recovery

    def poll_recovery(self, aid, *, kind, fingerprint, attempt, consume=False):
        future = asyncio.run_coroutine_threadsafe(
            self.recovery_job(aid, kind=kind, fingerprint=fingerprint, attempt=attempt, consume=consume), self.recovery_loop)
        try:
            return future.result(timeout=15)
        except TimeoutError:
            # Submission is idempotent. A late enqueue is polled next time,
            # never replaced by a second consumption or model request.
            return self.recovery_wait("验证任务正在登记")

    def recovery_wait(self, reason, *, until=None, job_id=None):
        return {"success": False, "deferred": True, "error_code": "model_verification_pending", "error": reason,
                "next_at": until or (self.clock() + timedelta(seconds=5)).isoformat(), "job_id": job_id}

    def recovery_reason(self, row, context, *, before_reset=False):
        from .oauth_monitor import automatic_recovery_eligible, control_allows_recovery, recovery_quota_ready, recovery_block_signature
        from .quota_snapshot import latest_openai_result
        monitor = self.r.oauth_monitor
        aid, now = context["account_id"], self.clock()
        if not row or row.get("platform") != "openai" or row.get("type") != "oauth" or row.get("deleted_at"):
            return "账号不符合恢复验证条件"
        if recovery_method(self.r.settings, aid) != "model" or not self.r.settings.oauth_recovery_monitor_enabled:
            return "恢复验证设置已变化"
        meta = monitor.store.snapshot()["scheduler"].get(str(aid), {})
        if (monitor.store.control_generation(aid) != context["control_generation"]
                or int(meta.get("recovery_method_generation", 0)) != context["method_generation"]):
            return "人工操作或恢复方式已变化"
        if self.held(aid) or self.mark(aid)["marked"]:
            return "账号已标记降智"
        if context["kind"] == "credit":
            controller = monitor.auto_reset
            task = controller.state(aid)
            if task.get("episode") != context["fingerprint"]:
                return "用卡任务已变化"
            if before_reset and not task.get("attempt_at"):
                if (not self.r.settings.oauth_auto_reset_credit_enabled or not controller._depletion(row, now)
                        or task.get("stage") in {"manual", "closed", "blocked"}):
                    return "账号不再符合自动用卡条件"
            elif not (controller._owned(row, task, same_version=False)
                      and controller._processing_eligible(row, task, now) and controller._available(row, now, task)):
                return "用卡后的额度或暂停归属未确认"
        else:
            intent = meta.get("recovery_intent") or {}
            result = latest_openai_result(row, monitor.store.result(aid), now)
            if intent.get("fingerprint") != context["fingerprint"]:
                return "恢复事件已变化"
            if not automatic_recovery_eligible(row, exhausted_window_keys=intent.get("window_keys"), now=now):
                return "账号状态不允许自动恢复"
            if recovery_block_signature(row) != context["block_signature"]:
                return "账号阻断状态已变化"
            if not control_allows_recovery(row, result, meta, now) or not recovery_quota_ready(row, result, meta, now):
                return "恢复所需的新鲜额度证据不足"
        return ""

    async def recovery_job(self, aid, *, kind, fingerprint, attempt, consume=False):
        from .model_tests import ModelTestRequest
        monitor, value = self.r.oauth_monitor, self.control(aid)
        meta = monitor.store.snapshot()["scheduler"].get(str(aid), {})
        context = {"account_id": aid, "kind": kind, "fingerprint": fingerprint,
                   "control_generation": monitor.store.control_generation(aid),
                   "method_generation": int(meta.get("recovery_method_generation", 0)),
                   "attempt": attempt, "consume": consume}
        # Whether a reset is still pending can change before the poll. It must
        # not change the identity of the first verification after that reset.
        request_id = digest(["recovery", {k: v for k, v in context.items() if k != "consume"}, value["generation"], value["model_id"]])[:32]
        jobs = self.s.model_tests.store.read()
        previous = jobs["requests"].get(request_id)
        if previous:
            job = self.s.model_tests.get(previous)
            if job["status"] in {"queued", "running", "retrying"}:
                return self.recovery_wait("模型验证排队中" if job["status"] == "queued" else "模型验证中", job_id=job["id"], until=job.get("waiting_until"))
            if job.get("preflight_result"):
                result = job['preflight_result']
                due = parse_iso_datetime(result.get('next_at'))
                task = monitor.auto_reset.state(aid) if kind == 'credit' else {}
                if result.get('deferred') and due and self.clock() >= due and not task.get('attempt_at'):
                    # Only pre-dispatch admission failures can be resubmitted.
                    # The immutable request receipt still prevents consumption replay.
                    with self.s.model_tests.store.transaction() as data:
                        if data['requests'].get(request_id) == previous:
                            del data['requests'][request_id]
                    return self.recovery_wait('重新核对用卡条件')
                return result
            report = job.get("report") or {}
            valid = (job["status"] == "completed" and report.get("used_outputs", 0) >= 1
                     and isinstance(report.get("prediction"), str) and bool(report["prediction"].strip()))
            if valid and report.get('prediction') != 'gpt-5.6-luna':
                try:
                    await self.guard(job)
                except HTTPException as exc:
                    return {'success': False, 'error_code': 'recovery_state_changed', 'error': str(exc.detail)}
            failure = job.get('error_code') or 'model_analysis_insufficient'
            if failure == 'auth_or_quota':
                status = next((g.get('diagnostics', {}).get('http_status') for g in job.get('groups', [])
                               if g.get('diagnostics', {}).get('http_status') in {401, 402}), 401)
                failure = f'http_{status}'
            return {"success": valid and report.get("prediction") != "gpt-5.6-luna",
                    "error_code": "detection_held" if report.get("prediction") == "gpt-5.6-luna" else "" if valid else failure,
                    "error": job.get("error") or ("账号已判定降智" if report.get("prediction") == "gpt-5.6-luna" else "" if valid else "模型验证未获得有效样本"),
                    "model_id": job["requested_model"], "completed_at": job.get("completed_at"), "job_id": job["id"]}
        allowed = parse_iso_datetime(self.next_allowed_at(aid))
        if allowed and self.clock() < allowed:
            return self.recovery_wait("等待自动检测冷却", until=allowed.isoformat())
        current = self.s.model_tests.latest(aid)
        if current and current["status"] in {"queued", "running", "retrying"}:
            return self.recovery_wait("等待当前模型测试结束", job_id=current["id"])
        row = await self.s.actions.account(aid)
        from .oauth_monitor import recovery_block_signature
        context["block_signature"] = recovery_block_signature(row)
        reason = self.recovery_reason(row, context, before_reset=consume)
        if reason:
            return {"success": False, "error": reason, "error_code": "recovery_ineligible"}
        if not await self.s.actions.model_allowed(row, value["model_id"], candidate_required=True):
            return {"success": False, "error": "所选模型不在当前分组白名单中", "error_code": "model_unavailable"}
        bank = self.r.fingerprint_bank.capture()
        if not any(model["id"] == "gpt-5.6-luna" for model in bank[0]["models"]):
            return {"success": False, "error": "指纹库缺少降智判定模型", "error_code": "bank_unavailable"}
        mark = self.mark(aid)
        payload = ModelTestRequest(model_id=value["model_id"], expected_version="0" * 64,
                                   expected_operation_version=versions(row)["model_test"], request_id=request_id, concurrency=1)
        job = await self.s.model_tests.start(aid, payload, automatic={"detection_generation": value["generation"],
            "detection_mark_version": mark["version"], "triggers": [kind + ":" + fingerprint],
            "recovery_context": context, "planned_groups": 1})
        self.update(aid, job_id=job['id'], triggers=job.get('triggers', []), status=job['status'])
        return self.recovery_wait("模型验证排队中", job_id=job["id"])

    async def prepare_recovery(self, job):
        """Called with account lease and a model slot; consumption never queues twice."""
        context = job.get("recovery_context")
        if not context or not context.get("consume"):
            return None
        monitor, aid = self.r.oauth_monitor, job["account_id"]
        def consume():
            controller = monitor.auto_reset
            row, task = monitor._read_account(aid), controller.state(aid)
            if task.get("attempt_at"):
                # Restart/uncertain consumption is reconciled by the card state
                # machine, never replayed by the queued model worker.
                if task.get("consumed") and task.get("stage") == "testing":
                    return None
                return {"success": False, "error_code": "reset_uncertain", "error": "重置结果待确认"}
            reason = self.recovery_reason(row, context, before_reset=True)
            if reason:
                return {"success": False, "error_code": "recovery_ineligible", "error": reason}
            result = controller._request(row, "reset", task, self.clock())
            if result.get("consumed") and not result.get("uncertain") and controller.state(aid).get("stage") == "testing":
                return None
            return {"success": False, "deferred": bool(result.get("next_query_at")),
                    "error_code": result.get("error_code") or "reset_uncertain", "error": "重置尚未确认成功",
                    "next_at": result.get("next_query_at")}
        return await asyncio.to_thread(consume)
