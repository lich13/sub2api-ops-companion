from __future__ import annotations

import copy
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from fastapi import HTTPException
from app.atomic_config import write_json
from app.bark import BarkPushResult
from app.capacity_alerts import CapacityAlertStore
from app.model_detection import DEFAULT_MODEL, TARGET, ModelDetection
from app.model_tests import ModelTests
from app.model_test_stream import TestFailure
from app.operation_versions import versions


NOW = datetime(2026, 10, 10, 3, tzinfo=timezone.utc)


class Clock:
    def __init__(self, value: datetime = NOW):
        self.value = value

    def __call__(self) -> datetime:
        return self.value

    def advance(self, seconds: int) -> None:
        self.value += timedelta(seconds=seconds)


def account(*, schedulable: bool = False, status: str = "active", account_id: int = 1, **changes) -> dict:
    return {
        "id": account_id,
        "name": "fixture-recheck-account",
        "platform": "openai",
        "type": "apikey",
        "status": status,
        "schedulable": schedulable,
        "deleted_at": None,
        "parent_account_id": None,
        "group_ids": [],
        "model_catalog_version": "fixture-catalog",
        "extra": {},
        "proxy_id": None,
        "credentials": {"api_key": "fixture-key", "base_url": "https://example.invalid"},
        **changes,
    }


class Notifier:
    def __init__(self) -> None:
        self.enabled = True
        self.config_valid = True
        self.sent: list[tuple[str, str, dict]] = []
        self.results: list[BarkPushResult] = []

    def runtime_config(self):
        return SimpleNamespace(enabled=self.enabled, config_valid=self.config_valid)

    def push(self, title, body, *, timeout, options):
        self.sent.append((title, body, options))
        return self.results.pop(0) if self.results else BarkPushResult(True)


class FakeModelTests:
    def __init__(self, row: dict):
        self.row = copy.deepcopy(row)
        self.jobs: dict[str, dict] = {}
        self.started: list[tuple[int, object, dict | None]] = []
        self.start = AsyncMock(side_effect=self._start)
        self.account = AsyncMock(side_effect=lambda _aid: copy.deepcopy(self.row))
        self.latest = Mock(return_value=None)
        self.cancel = AsyncMock()

    async def _start(self, aid, payload, automatic=None):
        job_id = f"{len(self.jobs) + 1:032x}"
        metadata = copy.deepcopy(automatic)
        job = {
            "id": job_id,
            "account_id": aid,
            "status": "queued",
            "automatic": bool(metadata),
            "requested_model": payload.model_id,
            "triggers": list((metadata or {}).get("triggers") or []),
            "detection_generation": (metadata or {}).get("detection_generation"),
            "detection_mark_version": (metadata or {}).get("detection_mark_version"),
            "degradation_recheck": bool((metadata or {}).get("degradation_recheck")),
            "recheck_account_version": (metadata or {}).get("recheck_account_version"),
            "recheck_schedulable": (metadata or {}).get("recheck_schedulable"),
            "first_group_valid": None,
            "first_group_prediction": None,
            "report": None,
            "groups": [],
            "can_retry": False,
        }
        self.jobs[job_id] = job
        self.started.append((aid, payload, metadata))
        return copy.deepcopy(job)

    def get(self, job_id):
        if job_id not in self.jobs:
            raise KeyError(job_id)
        return copy.deepcopy(self.jobs[job_id])

    def set_job(self, job_id: str, **changes) -> dict:
        self.jobs[job_id].update(copy.deepcopy(changes))
        return copy.deepcopy(self.jobs[job_id])


class RecheckFixture(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.clock = Clock()
        self.row = account()
        self.db = SimpleNamespace(fetch_one=lambda *args, **kwargs: copy.deepcopy(self.row))
        self.capacity = SimpleNamespace(store=CapacityAlertStore(root / "capacity.json"))
        self.notifier = Notifier()
        self.actions = SimpleNamespace(
            account=AsyncMock(side_effect=lambda _aid, *_args: copy.deepcopy(self.row)),
            model_allowed=AsyncMock(return_value=True),
        )
        self.model_tests = FakeModelTests(self.row)
        self.settings = SimpleNamespace(
            usage_query_state_path=str(root / "usage.json"),
            audit_path=str(root / "audit.jsonl"),
        )
        self.service = SimpleNamespace(
            invalidate=Mock(),
            actions=self.actions,
            model_tests=self.model_tests,
            r=SimpleNamespace(
                db=self.db,
                settings=self.settings,
                capacity_alerts=self.capacity,
                bark_notifier=self.notifier,
                fingerprint_bank=SimpleNamespace(
                    capture=Mock(return_value=({"models": [{"id": TARGET}]}, "fixture-bank"))
                ),
            ),
        )
        self.detection = ModelDetection(self.service, clock=self.clock)
        self.service.model_detection = self.detection
        self.mark_account()
        self.enable_due()

    def mark_account(self) -> dict:
        current = self.detection.mark(1)
        return self.capacity.store.set_mark(1, True, current["version"], self.clock())

    def enable_due(self, *, interval_minutes: int = 15) -> None:
        value = self.detection.control(1)
        self.detection.update(
            1,
            enabled=True,
            interval_minutes=interval_minutes,
            model_id=DEFAULT_MODEL,
            status="waiting",
            reason="",
            next_at=self.clock().isoformat(),
            hold=True,
            disposition={
                "status": "completed",
                "job_id": "fixture-degradation-job",
                "generation": value["generation"],
                "mark_version": self.detection.mark(1)["version"],
                "marked": True,
                "schedule_verified": True,
            },
        )

    def set_manual_close_without_hold(self) -> None:
        self.detection.update(1, hold=False, disposition=None, status="paused", next_at=None)

    def recheck_job(self, *, job_id="fixture-recheck-job", **changes) -> dict:
        value = self.detection.control(1)
        mark = self.detection.mark(1)
        job = {
            "id": job_id,
            "account_id": 1,
            "automatic": True,
            "degradation_recheck": True,
            "status": "completed",
            "requested_model": DEFAULT_MODEL,
            "triggers": ["scheduled"],
            "detection_generation": value["generation"],
            "detection_mark_version": mark["version"],
            "recheck_account_version": versions(self.row)["model_test"],
            "recheck_schedulable": self.row["schedulable"],
            "first_group_valid": True,
            "first_group_prediction": DEFAULT_MODEL,
            "report": {"prediction": DEFAULT_MODEL, "probability": 0.999, "used_outputs": 1},
            "groups": [{"index": 1, "status": "completed", "error": ""}],
            "can_retry": False,
            "error": "",
            "completed_at": self.clock().isoformat(),
            **changes,
        }
        self.model_tests.jobs[job_id] = copy.deepcopy(job)
        return job

    async def complete(self, job: dict | None = None):
        return await self.detection.complete_recheck(job or self.recheck_job())


class RecheckAdmissionTests(RecheckFixture):
    async def test_scheduled_marked_account_is_rechecked_with_immutable_admission_metadata(self):
        await self.detection.tick()

        self.model_tests.start.assert_awaited_once()
        aid, payload, automatic = self.model_tests.started[0]
        self.assertEqual(aid, 1)
        self.assertEqual(payload.model_id, DEFAULT_MODEL)
        self.assertTrue(automatic["degradation_recheck"])
        self.assertEqual(automatic["recheck_account_version"], versions(self.row)["model_test"])
        self.assertEqual(automatic["recheck_schedulable"], self.row["schedulable"])
        self.assertEqual(automatic["detection_generation"], self.detection.control(1)["generation"])
        self.assertEqual(automatic["detection_mark_version"], self.detection.mark(1)["version"])

    async def test_unenabled_marked_account_does_not_recheck_old_clues(self):
        self.detection.update(1, enabled=False, status="disabled", next_at=None)
        with self.capacity.store.transaction() as data:
            data["detection_events"]["error:old"] = {"account_id": 1, "created_at": self.clock().isoformat()}

        await self.detection.tick()

        self.model_tests.start.assert_not_awaited()
        self.assertEqual(self.capacity.store.snapshot()["detection_events"], {})
        self.assertTrue(self.detection.mark(1)["marked"])

    async def test_five_minute_gate_defers_marked_recheck_then_allows_at_exact_boundary(self):
        with self.detection.store.transaction() as data:
            data["automatic_starts"] = {
                "1": {"job_id": "fixture-previous", "started_at": (self.clock() - timedelta(seconds=100)).isoformat()}
            }

        await self.detection.tick()

        self.model_tests.start.assert_not_awaited()
        self.assertEqual(
            datetime.fromisoformat(self.detection.view(1)["next_at"]),
            self.clock() + timedelta(seconds=200),
        )
        self.clock.advance(200)
        await self.detection.tick()
        self.model_tests.start.assert_awaited_once()

    async def test_recheck_keeps_manual_close_status_auth_and_quota_precedence(self):
        cases = (
            ({"status": "disabled"}, "账号停用或认证异常", False),
            ({"schedulable": False, "rate_limit_reset_at": (NOW + timedelta(minutes=5)).isoformat()}, "账号仍在上游限流中", False),
            ({"schedulable": False}, "调度已关闭", True),
        )
        baseline = account()
        for changes, expected, manual_close in cases:
            with self.subTest(changes=changes):
                self.row.clear()
                self.row.update(copy.deepcopy(baseline))
                self.row.update(changes)
                self.actions.account.reset_mock()
                self.model_tests.start.reset_mock()
                if manual_close:
                    self.detection.update(1, hold=False, disposition=None, status="paused", next_at=None)
                self.enable_due()
                if manual_close:
                    self.detection.update(1, hold=False, disposition=None, status="paused", next_at=None)
                self.assertEqual(self.detection.reason(self.row, self.detection.mark(1), recheck=True), expected)
                await self.detection.tick()
                self.model_tests.start.assert_not_awaited()


class RecheckCompletionTests(RecheckFixture):
    async def test_normal_result_clears_mark_persists_receipt_and_notifies_without_schedule_write(self):
        job = self.recheck_job()
        with patch("app.key_fallback.execute_sub2api_set_schedulable") as schedule_write:
            result = await self.complete(job)

        self.assertEqual(result["status"], "completed")
        self.assertTrue(result["mark_cleared"])
        self.assertFalse(self.detection.mark(1)["marked"])
        self.assertTrue(self.detection.held(1))
        self.assertFalse(self.row["schedulable"])
        schedule_write.assert_not_called()
        mark = self.capacity.store.snapshot()["marks"]["1"]
        self.assertEqual(mark["detection_job_id"], job["id"])
        control = self.detection.control(1)
        self.assertEqual(control["recheck_recovery"]["status"], "completed")
        self.assertEqual(control["recheck_recovery"]["job_id"], job["id"])
        event = self.detection.data()["disposition_notifications"][job["id"]]
        self.assertEqual(event["kind"], "recovered")
        self.assertEqual(event["status"], "queued")
        self.assertEqual(self.notifier.sent, [])

        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 1)
        self.assertIn("恢复", self.notifier.sent[0][0] + self.notifier.sent[0][1])
        self.assertEqual(self.detection.data()["disposition_notifications"][job["id"]]["status"], "delivered")

    async def test_pending_recovery_receipt_resumes_after_finish_boundary_failure(self):
        job = self.recheck_job()
        original_finish = self.detection.finish_recheck_recovery
        with patch.object(self.detection, "finish_recheck_recovery", side_effect=OSError("fixture finish failure")):
            with self.assertRaisesRegex(OSError, "fixture finish failure"):
                await self.complete(job)

        pending = self.detection.control(1)["recheck_recovery"]
        self.assertEqual(pending["status"], "pending")
        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})
        self.assertEqual(self.notifier.sent, [])

        completed = original_finish(1)
        self.assertEqual(completed["status"], "completed")
        self.assertFalse(self.detection.mark(1)["marked"])
        self.assertEqual(self.detection.data()["disposition_notifications"][job["id"]]["kind"], "recovered")

    async def test_recovery_notification_retries_after_five_seconds_and_delivers_once(self):
        job = self.recheck_job()
        await self.complete(job)
        self.notifier.results = [BarkPushResult(False, "fixture-retry"), BarkPushResult(True)]

        self.detection.deliver_notifications()
        event = self.detection.data()["disposition_notifications"][job["id"]]
        self.assertEqual((event["status"], event["attempts"]), ("retry", 1))
        self.assertEqual(datetime.fromisoformat(event["next_at"]), self.clock() + timedelta(seconds=5))
        self.assertEqual(len(self.notifier.sent), 1)
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 1)

        self.clock.advance(5)
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 2)
        self.assertEqual(self.detection.data()["disposition_notifications"][job["id"]]["status"], "delivered")
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 2)

    async def test_restart_adopts_mark_clear_when_control_receipt_save_fails_without_replaying_model_or_schedule(self):
        job = self.recheck_job()

        def fail_completed(path, data):
            receipt = data.get("accounts", {}).get("1", {}).get("recheck_recovery") or {}
            if receipt.get("status") == "completed":
                raise OSError("fixture recovery receipt save failure")
            return write_json(path, data)

        with patch("app.policy_store.write_json", side_effect=fail_completed):
            with self.assertRaisesRegex(OSError, "fixture recovery receipt save failure"):
                await self.complete(job)
        self.assertFalse(self.detection.mark(1)["marked"])
        self.assertEqual(self.capacity.store.snapshot()["marks"]["1"]["detection_job_id"], job["id"])
        self.assertEqual(self.detection.control(1)["recheck_recovery"]["status"], "pending")
        self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})

        restarted = ModelDetection(self.service, clock=self.clock)
        with patch("app.key_fallback.execute_sub2api_set_schedulable") as schedule_write:
            await restarted.tick()
        schedule_write.assert_not_called()
        self.model_tests.start.assert_not_awaited()
        self.assertFalse(restarted.mark(1)["marked"])
        self.assertEqual(restarted.control(1)["recheck_recovery"]["status"], "completed")
        self.assertEqual(restarted.data()["disposition_notifications"][job["id"]]["kind"], "recovered")

    async def test_recovery_receipt_and_delivery_survive_restart_and_duplicate_processing_is_once(self):
        job = self.recheck_job()
        await self.complete(job)
        self.detection.deliver_notifications()
        restarted = ModelDetection(self.service, clock=self.clock)

        duplicate = await restarted.complete_recheck(job)
        restarted.deliver_notifications()

        self.assertEqual(duplicate["status"], "completed")
        self.assertEqual(len(self.notifier.sent), 1)
        self.assertEqual(restarted.control(1)["recheck_recovery"]["job_id"], job["id"])
        self.assertEqual(restarted.data()["disposition_notifications"][job["id"]]["status"], "delivered")
        self.assertFalse(restarted.mark(1)["marked"])
        self.assertTrue(restarted.held(1))

    async def test_first_group_target_remains_marked_without_a_recovery_receipt(self):
        job = self.recheck_job(first_group_prediction=TARGET)

        result = await self.detection.verdict(job, {"prediction": TARGET, "used_outputs": 1})

        self.assertEqual(result["kind"], "recheck")
        self.assertEqual(result["status"], "still_degraded")
        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertTrue(self.detection.held(1))
        self.assertEqual(self.detection.control(1).get("recheck_recovery"), None)
        self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})
        self.assertEqual(self.notifier.sent, [])

    async def test_final_target_or_invalid_result_does_not_clear_mark(self):
        cases = (
            {"report": {"prediction": TARGET, "used_outputs": 1}},
            {"report": {"prediction": DEFAULT_MODEL, "used_outputs": 0}},
            {"first_group_valid": False},
            {"can_retry": True},
            {"groups": [{"index": 1, "status": "failed", "error": "fixture"}]},
            {"automatic": False},
        )
        for changes in cases:
            with self.subTest(changes=changes):
                self.capacity.store.set_mark(1, False, self.detection.mark(1)["version"], self.clock())
                self.mark_account()
                self.detection.update(
                    1,
                    hold=True,
                    disposition={
                        "status": "completed",
                        "job_id": "fixture-degradation-job",
                        "generation": self.detection.control(1)["generation"],
                        "mark_version": self.detection.mark(1)["version"],
                        "marked": True,
                        "schedule_verified": True,
                    },
                )
                result = await self.complete(self.recheck_job(**changes))
                self.assertTrue(self.detection.mark(1)["marked"])
                self.assertTrue(self.detection.held(1))
                self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})
                self.assertEqual(self.notifier.sent, [])
                if changes.get("automatic") is False:
                    self.assertIsNone(result)

    async def test_same_job_id_with_new_completed_at_is_processed_after_a_retry(self):
        job = self.recheck_job(
            first_group_valid=False,
            report={"prediction": DEFAULT_MODEL, "used_outputs": 0},
            completed_at=self.clock().isoformat(),
        )
        self.detection.update(
            1,
            job_id=job["id"],
            status="completed",
            handled_job=job["id"],
            handled_completed_at=(self.clock() - timedelta(seconds=1)).isoformat(),
            next_at=None,
        )
        await self.detection.tick()
        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertEqual(self.detection.control(1)["handled_completed_at"], job["completed_at"])

        retried_at = (self.clock() + timedelta(seconds=1)).isoformat()
        self.model_tests.set_job(
            job["id"],
            first_group_valid=True,
            report={"prediction": DEFAULT_MODEL, "used_outputs": 1},
            groups=[{"index": 1, "status": "completed", "error": ""}],
            completed_at=retried_at,
            error_code=None,
            can_retry=False,
        )
        await self.detection.tick()

        self.assertFalse(self.detection.mark(1)["marked"])
        self.assertEqual(self.detection.control(1)["recheck_recovery"]["job_id"], job["id"])
        self.assertEqual(self.detection.data()["disposition_notifications"][job["id"]]["kind"], "recovered")


class RecheckStaleResultTests(RecheckFixture):
    async def test_generation_change_makes_late_recheck_a_noop(self):
        job = self.recheck_job()
        self.detection.human_control(1)

        with self.assertRaises(HTTPException):
            await self.complete(job)

        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertTrue(self.detection.held(1))
        self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})

    async def test_mark_version_change_makes_late_recheck_a_noop(self):
        job = self.recheck_job()
        current = self.detection.mark(1)
        self.clock.advance(1)
        self.capacity.store.set_mark(1, True, current["version"], self.clock())

        with self.assertRaises(HTTPException):
            await self.complete(job)

        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})

    async def test_manual_release_hold_invalidates_late_recheck_and_preserves_mark(self):
        job = self.recheck_job()
        self.detection.human_control(1, release_hold=True)

        with self.assertRaises(HTTPException):
            await self.complete(job)

        self.assertFalse(self.detection.held(1))
        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})

    async def test_retrying_failed_first_group_scores_first_sample_separately_from_aggregate(self):
        tests = ModelTests(self.service)
        self.service.model_tests = tests
        self.addAsyncCleanup(tests.close)
        self.detection.verdict = AsyncMock()
        tests.execute = AsyncMock(side_effect=[
            TestFailure("network_error", True),
            TestFailure("network_error", True),
            TestFailure("network_error", True),
            ("sample-two", DEFAULT_MODEL),
            ("sample-three", DEFAULT_MODEL),
            ("sample-one", DEFAULT_MODEL),
        ])
        reports = [
            {"prediction": DEFAULT_MODEL, "probability": 0.5, "used_outputs": 1},
            {"prediction": DEFAULT_MODEL, "probability": 0.5, "used_outputs": 2},
            {"prediction": TARGET, "probability": 0.5, "used_outputs": 3},
            {"prediction": DEFAULT_MODEL, "probability": 0.5, "used_outputs": 1},
        ]
        with patch("app.model_tests.asyncio.sleep", new=AsyncMock()), \
                patch("app.model_tests.challenges", return_value=[("one", "prompt"), ("two", "prompt"), ("three", "prompt")]), \
                patch("app.model_tests.analyze", side_effect=reports):
            await self.detection.tick()
            job_id = self.detection.control(1)["job_id"]
            first_task = tests.tasks.get(job_id)
            if first_task:
                await first_task
            first = tests.get(job_id)
            self.assertTrue(first["can_retry"])
            self.assertTrue(any(group.get("retryable") and group["status"] == "failed" for group in first["groups"]))

            self.clock.advance(300)
            await tests.retry_failed(job_id, "recheck-first-group-retry-034")
            retry_task = tests.tasks.get(job_id)
            if retry_task:
                await retry_task
            saved = tests.get(job_id)

        self.assertEqual(saved["status"], "completed")
        self.assertEqual(saved["report"]["prediction"], TARGET)
        self.assertEqual(saved["report"]["used_outputs"], 3)
        self.assertEqual(saved["first_group_prediction"], DEFAULT_MODEL)
        self.assertTrue(saved["first_group_valid"])
        self.detection.verdict.assert_not_awaited()
        await self.detection.tick()
        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertIsNone(self.detection.control(1).get("recheck_recovery"))

    async def test_real_model_tests_persists_recheck_metadata_and_first_group_validity(self):
        tests = ModelTests(self.service)
        self.service.model_tests = tests
        self.addAsyncCleanup(tests.close)
        tests.execute = AsyncMock(return_value=("fixture output", DEFAULT_MODEL))
        with patch("app.model_tests.challenges", return_value=[("one", "prompt"), ("two", "prompt"), ("three", "prompt")]), \
                patch("app.model_tests.analyze", return_value={"prediction": DEFAULT_MODEL, "probability": 0.999, "used_outputs": 1}):
            await self.detection.tick()
            job_id = self.detection.control(1)["job_id"]
            task = tests.tasks.get(job_id)
            if task:
                await task
            await self.detection.tick()

        saved = tests.get(job_id)
        self.assertTrue(saved["degradation_recheck"])
        self.assertEqual(saved["recheck_account_version"], versions(self.row)["model_test"])
        self.assertEqual(saved["recheck_schedulable"], self.row["schedulable"])
        self.assertTrue(saved["first_group_valid"])
        self.assertEqual(saved["first_group_prediction"], DEFAULT_MODEL)
        self.assertEqual(saved["report"]["prediction"], DEFAULT_MODEL)
        self.assertEqual(saved["report"]["used_outputs"], 1)
        self.assertEqual(saved["status"], "completed")
        self.assertTrue(all(group["status"] in {"completed", "skipped"} for group in saved["groups"]))
        self.assertFalse(self.detection.mark(1)["marked"])
        self.assertTrue(self.detection.held(1))
        self.assertFalse(self.row["schedulable"])
        self.assertEqual(self.detection.data()["disposition_notifications"][job_id]["kind"], "recovered")


if __name__ == "__main__":
    unittest.main()
