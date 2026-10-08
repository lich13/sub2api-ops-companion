from __future__ import annotations

import asyncio
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
from app.capacity_alerts import CapacityAlerts, MESSAGES
from app.model_detection import DEFAULT_MODEL, TARGET, DetectionRequest, ModelDetection
from app.model_test_stream import TestFailure
from app.model_tests import ModelTestRequest, ModelTests


NOW = datetime(2026, 10, 8, 3, tzinfo=timezone.utc)
OUTPUT = " ".join(str(i * 13 % 355 + 1) for i in range(80))


class _Clock:
    def __init__(self):
        self.value = NOW

    def __call__(self):
        return self.value

    def advance(self, seconds):
        self.value += timedelta(seconds=seconds)


def _account(aid, *, parent=None):
    return {
        "id": aid,
        "name": "fixture-account",
        "platform": "openai",
        "type": "apikey",
        "status": "active",
        "schedulable": True,
        "deleted_at": None,
        "parent_account_id": parent,
        "group_ids": [],
        "model_catalog_version": "fixture-catalog",
        "extra": {},
        "proxy_id": None,
        "credentials": {"api_key": "fixture-key", "base_url": "https://example.invalid"},
    }


def _error(error_id, at, *, aid=1):
    return {
        "id": error_id,
        "account_id": aid,
        "created_at": at.isoformat(),
        "error_owner": "provider",
        "error_phase": "upstream",
        "requested_model": DEFAULT_MODEL,
        "model": DEFAULT_MODEL,
        "upstream_model": DEFAULT_MODEL,
        "upstream_error_message": MESSAGES[0],
        "upstream_status_code": 503,
        "account_name": "fixture-account",
        "account_platform": "openai",
        "account_type": "apikey",
        "account_deleted_at": None,
    }


def _usage(sample_id, at, *, aid=1, first=11001):
    return {
        "id": sample_id,
        "account_id": aid,
        "created_at": at.isoformat(),
        "model": DEFAULT_MODEL,
        "upstream_model": DEFAULT_MODEL,
        "stream": True,
        "first_token_ms": first,
        "duration_ms": 15000,
        "output_tokens": 64,
        "inbound_endpoint": "/v1/responses",
        "image_count": 0,
        "image_output_tokens": 0,
        "video_count": 0,
        "video_duration_seconds": 0,
        "account_name": "fixture-account",
        "account_platform": "openai",
        "account_type": "apikey",
    }


class _Db:
    def __init__(self):
        self.accounts = {1: _account(1), 2: _account(2), 3: _account(3, parent=1)}
        self.errors = []
        self.usage = []

    def fetch_one(self, sql, params=None):
        if "coalesce(max(id)" in sql:
            return {"id": max((row["id"] for row in self.errors), default=0)}
        return copy.deepcopy(self.accounts.get((params or {}).get("id")))

    def fetch_all(self, sql, params=None):
        params = params or {}
        if "FROM usage_logs" in sql:
            return copy.deepcopy(self.usage)
        if "FROM ops_error_logs" in sql:
            after = params.get("cursor", params.get("after", 0))
            rows = [row for row in self.errors if row["id"] > after]
            if "since" in params:
                rows = [row for row in rows if datetime.fromisoformat(row["created_at"]) >= params["since"]]
            return copy.deepcopy(sorted(rows, key=lambda row: row["id"])[:200])
        return []


class _Notifier:
    def __init__(self):
        self.enabled = True
        self.config_valid = True
        self.results = []
        self.sent = []

    def runtime_config(self):
        return SimpleNamespace(enabled=self.enabled, config_valid=self.config_valid)

    def push(self, title, body, *, timeout, options):
        self.sent.append((title, body, options))
        return self.results.pop(0) if self.results else BarkPushResult(True)


class DetectionClueTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.clock, self.db, self.notifier = _Clock(), _Db(), _Notifier()
        self.settings = SimpleNamespace(
            usage_query_state_path=str(Path(self.tmp.name) / "usage.json"),
            audit_path=str(Path(self.tmp.name) / "audit.jsonl"),
        )
        self.alerts = CapacityAlerts(self.settings, self.db, self.notifier, clock=self.clock)

    def _new_evidence(self):
        self.alerts.poll()
        self.clock.advance(1)
        self.db.errors.append(_error(1, self.clock()))
        self.db.usage = [_usage(20 - i, self.clock() - timedelta(seconds=i), first=11001 if i < 8 else 9000)
                         for i in range(10)]
        self.alerts.poll()

    def test_capacity_and_slow_ttft_only_create_detection_clues(self):
        self._new_evidence()
        state = self.alerts.store.snapshot()
        self.assertEqual(set(state["detection_events"]), {"error:1", "slow:1:20"})
        self.assertEqual(state["pending"], {})
        self.assertEqual(state["slow_pending"], {})
        self.alerts.deliver_due()
        self.assertEqual(self.notifier.sent, [])
        self.alerts.poll()
        self.assertEqual(self.alerts.store.snapshot()["detection_events"], state["detection_events"])

    def test_disabled_bark_does_not_suppress_detection_clues(self):
        self.notifier.enabled = False
        self._new_evidence()
        self.assertEqual(set(self.alerts.store.snapshot()["detection_events"]), {"error:1", "slow:1:20"})
        self.alerts.deliver_due()
        self.assertEqual(self.notifier.sent, [])

    def test_invalid_bark_config_does_not_suppress_detection_clues(self):
        self.notifier.config_valid = False
        self._new_evidence()
        self.assertEqual(set(self.alerts.store.snapshot()["detection_events"]), {"error:1", "slow:1:20"})
        self.assertEqual(self.notifier.sent, [])

    def test_upgrade_discards_legacy_pending_notifications_without_replaying_history(self):
        self.alerts.poll()
        legacy = {"id": 99, "account_id": 1, "account_name": "fixture-account", "account_type": "apikey",
                  "requested_model": DEFAULT_MODEL, "upstream_model": DEFAULT_MODEL, "message": MESSAGES[0],
                  "created_at": NOW.isoformat(), "next_at": NOW.isoformat(), "attempts": 2}
        with self.alerts.store.transaction() as data:
            data.pop("detection_since", None)
            data.pop("clue_mode_since", None)
            data["pending"]["99"] = legacy
            data["slow_pending"]["slow:1:99"] = {
                **legacy, "kind": "slow_ttft", "sample_count": 10, "slow_count": 8,
                "threshold_ms": 10000, "latest_first_token_ms": 11001,
            }
        self.db.errors.append(_error(99, NOW - timedelta(seconds=1)))
        restarted = CapacityAlerts(self.settings, self.db, self.notifier, clock=self.clock)
        restarted.poll()
        restarted.deliver_due()
        state = restarted.store.snapshot()
        self.assertEqual(state["pending"], {})
        self.assertEqual(state["slow_pending"], {})
        self.assertEqual(state["detection_events"], {})
        self.assertEqual(self.notifier.sent, [])


class _DetectionFixture(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.clock, self.db, self.notifier = _Clock(), _Db(), _Notifier()
        self.settings = SimpleNamespace(
            usage_query_state_path=str(Path(self.tmp.name) / "usage.json"),
            audit_path=str(Path(self.tmp.name) / "audit.jsonl"),
        )
        self.capacity = CapacityAlerts(self.settings, self.db, self.notifier, clock=self.clock)
        self.actions = SimpleNamespace(
            account=AsyncMock(side_effect=lambda aid, *_args: copy.deepcopy(self.db.accounts[aid])),
            model_allowed=AsyncMock(return_value=True),
        )
        self.service = SimpleNamespace(
            invalidate=Mock(),
            actions=self.actions,
            r=SimpleNamespace(
                db=self.db,
                settings=self.settings,
                capacity_alerts=self.capacity,
                bark_notifier=self.notifier,
                oauth_state_store=lambda: SimpleNamespace(admin_token=lambda: "fixture-admin-token"),
                oauth_base_url=lambda: "https://example.invalid",
                fingerprint_bank=SimpleNamespace(capture=Mock(return_value=({"models": [{"id": TARGET}]}, "fixture-bank"))),
            ),
        )
        self.detection = ModelDetection(self.service, clock=self.clock)
        self.service.model_detection = self.detection
        self.tests = ModelTests(self.service)
        self.service.model_tests = self.tests
        self.tests.execute = AsyncMock(return_value=(OUTPUT, DEFAULT_MODEL))
        self.addAsyncCleanup(self.tests.close)
        self.serial = 0
        analyzer = patch("app.model_tests.analyze", return_value={"prediction": DEFAULT_MODEL, "probability": .999, "used_outputs": 1})
        analyzer.start()
        self.addCleanup(analyzer.stop)

    def _job(self, *, aid=1, job_id="fixture-disposition-job"):
        return {"id": job_id, "account_id": aid, "automatic": True, "requested_model": DEFAULT_MODEL,
                "detection_generation": self.detection.control(aid)["generation"],
                "detection_mark_version": self.detection.mark(aid)["version"], "triggers": ["scheduled"]}

    async def _start(self, *, aid=1, automatic=True, causes=None, model=DEFAULT_MODEL):
        self.serial += 1
        request = ModelTestRequest(model_id=model, expected_version="a" * 64, request_id=f"fixture-request-{self.serial:08d}")
        metadata = self._job(aid=aid) if automatic else None
        if metadata is not None:
            metadata["triggers"] = causes or ["scheduled"]
        return await self.tests.start(aid, request, automatic=metadata)

    async def _run(self, **kwargs):
        until = self.detection.next_allowed_at(kwargs.get("aid", 1)) if kwargs.get("automatic", True) else None
        cooling = until is not None and self.clock() < datetime.fromisoformat(until)
        execute_count = self.tests.execute.await_count
        available_slots = self.tests.job_slots._value
        try:
            job = await self._start(**kwargs)
        except HTTPException as exc:
            self.assertEqual(exc.status_code, 409)
            self.assertFalse(cooling, "Automatic cooldown requests must enter the queue")
            return {"status": "rejected", "error": exc.detail}
        task = self.tests.tasks.get(job["id"])
        if cooling:
            self.assertIsNotNone(task, "Automatic cooldown requests must retain a queue task")
        if task:
            if cooling:
                async def wait_for_cooldown():
                    while not task.done():
                        queued = self.tests.get(job["id"])
                        if queued.get("waiting_until"):
                            return queued
                        await asyncio.sleep(.01)
                    await task
                    self.fail("Automatic request ended before entering the cooldown queue")

                queued = await asyncio.wait_for(wait_for_cooldown(), timeout=3)
                self.assertEqual(queued["status"], "queued")
                self.assertEqual(queued["waiting_until"], until)
                self.assertEqual(self.tests.execute.await_count, execute_count)
                lease = self.tests.waiting_leases[job["id"]]
                self.assertFalse(lease.held)
                self.assertTrue(all(not lock.locked() for lock in lease.locks))
                self.assertEqual(self.tests.job_slots._value, available_slots)
                cancelled = await self.tests.cancel(job["id"])
                self.assertEqual(cancelled["status"], "cancelled")
            else:
                await task
        return self.tests.get(job["id"])

    async def _restart(self):
        execute = self.tests.execute
        await self.tests.close()
        self.detection = ModelDetection(self.service, clock=self.clock)
        self.service.model_detection = self.detection
        self.tests = ModelTests(self.service)
        self.service.model_tests = self.tests
        self.tests.execute = execute
        self.addAsyncCleanup(self.tests.close)

    def _stop_schedule(self, aid, schedulable, **_kwargs):
        self.db.accounts[aid]["schedulable"] = schedulable

    def _outbox(self):
        return self.detection.data().get("disposition_notifications", {})


class AutomaticCooldownTests(_DetectionFixture):
    async def test_all_automatic_sources_and_models_share_exact_300_second_boundary(self):
        self.assertIsNone(self.detection.next_allowed_at(1))
        first = await self._run()
        self.assertEqual(first["status"], "completed")
        self.assertEqual(self.tests.execute.await_count, 1)
        started = self.detection.data()["automatic_starts"]["1"]
        self.assertEqual(started["job_id"], first["id"])
        self.assertEqual(datetime.fromisoformat(started["started_at"]), NOW)
        self.assertEqual(datetime.fromisoformat(self.detection.next_allowed_at(1)), NOW + timedelta(seconds=300))

        self.clock.advance(299)
        for causes in (["scheduled"], ["error:fixture"], ["slow:1:fixture"], ["recovery"], ["credit"], ["automatic-api"]):
            with self.subTest(causes=causes):
                await self._run(causes=causes, model="fixture-other-model")
                self.assertEqual(self.tests.execute.await_count, 1)
        self.assertEqual(self.detection.data()["automatic_starts"]["1"], started)
        self.clock.advance(1)
        await self._run()
        self.assertEqual(self.tests.execute.await_count, 2)

    async def test_manual_jobs_are_exempt_and_do_not_extend_automatic_cooldown(self):
        await self._run()
        started = self.detection.data()["automatic_starts"]["1"]
        self.clock.advance(1)
        manual = await self._run(automatic=False)
        self.assertEqual(manual["status"], "completed")
        self.assertEqual(self.tests.execute.await_count, 2)
        self.assertEqual(self.detection.data()["automatic_starts"]["1"], started)
        await self._run()
        self.assertEqual(self.tests.execute.await_count, 2)

    async def test_actual_account_ids_have_independent_cooldowns_even_with_shared_parent(self):
        for aid in (1, 2, 3):
            result = await self._run(aid=aid)
            self.assertEqual(result["status"], "completed")
        self.assertEqual(self.tests.execute.await_count, 3)
        self.assertEqual(set(self.detection.data()["automatic_starts"]), {"1", "2", "3"})

    async def test_restart_keeps_dispatch_time_even_when_result_summaries_are_pruned(self):
        await self._run()
        with self.tests.store.transaction() as data:
            data.update(jobs={}, accounts={}, requests={})
        await self._restart()
        self.clock.advance(299)
        await self._run()
        self.assertEqual(self.tests.execute.await_count, 1)
        self.clock.advance(1)
        await self._run()
        self.assertEqual(self.tests.execute.await_count, 2)

    async def test_failed_dispatched_job_still_consumes_cooldown(self):
        self.tests.execute.side_effect = TestFailure("invalid_model_or_request")
        result = await self._run()
        self.assertEqual(result["status"], "failed")
        self.assertEqual(self.tests.execute.await_count, 1)
        self.tests.execute.side_effect = None
        self.clock.advance(299)
        await self._run()
        self.assertEqual(self.tests.execute.await_count, 1)
        self.clock.advance(1)
        await self._run()
        self.assertEqual(self.tests.execute.await_count, 2)

    async def test_cancelled_dispatched_job_still_consumes_cooldown_after_restart(self):
        entered, release = asyncio.Event(), asyncio.Event()

        async def blocked_execute(**_kwargs):
            entered.set()
            await release.wait()
            return OUTPUT, DEFAULT_MODEL

        self.tests.execute.side_effect = blocked_execute
        job = await self._start()
        await asyncio.wait_for(entered.wait(), timeout=3)
        result = await self.tests.cancel(job["id"])
        self.assertEqual(result["status"], "cancelled")
        await self._restart()
        self.tests.execute.side_effect = None
        self.clock.advance(299)
        await self._run()
        self.assertEqual(self.tests.execute.await_count, 1)

    async def test_queued_cancellation_before_dispatch_does_not_consume_cooldown(self):
        with patch.object(self.tests, "launch"):
            job = await self._start()
        await self.tests.cancel(job["id"])
        self.assertIsNone(self.detection.next_allowed_at(1))
        await self._run()
        self.assertEqual(self.tests.execute.await_count, 1)

    async def test_dispatch_registration_is_idempotent_for_retries_of_same_job(self):
        job = self._job()
        self.detection.register_dispatch(job)
        started = copy.deepcopy(self.detection.data()["automatic_starts"]["1"])
        self.clock.advance(30)
        self.detection.register_dispatch(job)
        self.assertEqual(self.detection.data()["automatic_starts"]["1"], started)
        with self.assertRaises(HTTPException) as raised:
            self.detection.register_dispatch(self._job(job_id="fixture-different-job"))
        self.assertEqual(raised.exception.status_code, 409)

    async def test_automatic_transport_retry_keeps_original_dispatch_time(self):
        self.tests.execute.side_effect = [TestFailure("network_error", True), (OUTPUT, DEFAULT_MODEL)]
        result = await self._run()
        self.assertEqual(result["status"], "completed")
        self.assertEqual(self.tests.execute.await_count, 2)
        self.assertEqual(self.detection.data()["automatic_starts"]["1"],
                         {"job_id": result["id"], "started_at": NOW.isoformat()})
        self.assertEqual(datetime.fromisoformat(self.detection.next_allowed_at(1)), NOW + timedelta(seconds=300))

    async def test_upgrade_imports_latest_automatic_start_and_ignores_manual_jobs(self):
        with self.tests.store.transaction() as data:
            data["jobs"] = {
                "a" * 32: {"id": "a" * 32, "account_id": 1, "automatic": True, "status": "cancelled",
                           "started_at": (NOW - timedelta(seconds=30)).isoformat()},
                "b" * 32: {"id": "b" * 32, "account_id": 1, "automatic": True, "status": "failed",
                           "started_at": (NOW - timedelta(seconds=10)).isoformat()},
                "c" * 32: {"id": "c" * 32, "account_id": 1, "automatic": False, "status": "completed",
                           "started_at": NOW.isoformat()},
                "d" * 32: {"id": "d" * 32, "account_id": 2, "automatic": True, "status": "queued",
                           "started_at": None},
            }
        self.detection.migrate_dispatches()
        self.assertEqual(self.detection.data()["automatic_starts"]["1"]["job_id"], "b" * 32)
        self.assertEqual(datetime.fromisoformat(self.detection.next_allowed_at(1)), NOW + timedelta(seconds=290))
        self.assertIsNone(self.detection.next_allowed_at(2))

    async def test_failed_dispatch_persistence_never_reaches_upstream(self):
        self.detection.update(1)
        with patch("app.policy_store.write_json", side_effect=OSError("fixture save failure")):
            await self._run()
        self.tests.execute.assert_not_awaited()
        self.assertIsNone(self.detection.next_allowed_at(1))

    async def test_cooling_clues_are_consumed_without_later_replay(self):
        await self._run()
        self.clock.advance(1)
        with self.capacity.store.transaction() as data:
            data["detection_events"].update({
                "error:fixture": {"account_id": 1, "created_at": self.clock().isoformat()},
                "slow:1:fixture": {"account_id": 1, "created_at": self.clock().isoformat()},
            })
        await self.detection.tick()
        self.assertEqual(self.capacity.store.snapshot()["detection_events"], {})
        self.assertEqual(set(self.detection.data()["consumed"]), {"error:fixture", "slow:1:fixture"})
        self.assertEqual(self.tests.execute.await_count, 1)
        self.clock.advance(300)
        await self.detection.tick()
        self.assertEqual(self.tests.execute.await_count, 1)

    async def test_due_schedule_defers_to_cooldown_without_queuing_a_request(self):
        await self._run()
        view = self.detection.view(1)
        await self.detection.save(1, DetectionRequest(expected_version=view["version"], enabled=True, interval_minutes=1))
        self.clock.advance(60)
        await self.detection.tick()
        self.assertEqual(self.tests.execute.await_count, 1)
        self.assertEqual(datetime.fromisoformat(self.detection.view(1)["next_at"]), NOW + timedelta(seconds=300))
        self.clock.advance(240)
        await self.detection.tick()
        await asyncio.gather(*list(self.tests.tasks.values()))
        self.assertEqual(self.tests.execute.await_count, 2)


class DispositionNotificationTests(_DetectionFixture):
    async def _complete(self, job=None):
        job = job or self._job()
        with patch("app.key_fallback.execute_sub2api_set_schedulable", side_effect=self._stop_schedule):
            result = await self.detection.verdict(job, {"prediction": TARGET, "used_outputs": 1})
        self.assertEqual(result["status"], "completed")
        return job

    async def test_verified_disposition_persists_one_outbox_before_bark_and_survives_restart(self):
        job = await self._complete()
        self.assertEqual(self.notifier.sent, [])
        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertFalse(self.db.accounts[1]["schedulable"])
        self.assertEqual(set(self._outbox()), {job["id"]})
        await self._restart()
        self.detection.finish_disposition(1)
        self.assertEqual(set(self._outbox()), {job["id"]})
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 1)
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 1)

    async def test_result_bark_bypasses_suppression_from_its_new_degradation_mark(self):
        await self._complete()
        self.assertTrue(self.detection.mark(1)["marked"])
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 1)
        title, body, options = self.notifier.sent[0]
        self.assertIn("fixture-account", body)
        self.assertIn("1", body)
        self.assertIn("降智", title + body)
        self.assertIn("调度", title + body)
        self.assertEqual(options["level"], "critical")

    async def test_unverified_schedule_never_creates_result_notification(self):
        with patch("app.key_fallback.execute_sub2api_set_schedulable", return_value=None):
            result = await self.detection.verdict(self._job(), {"prediction": TARGET, "used_outputs": 1})
        self.assertFalse(result["schedule_verified"])
        self.assertEqual(self._outbox(), {})
        self.detection.deliver_notifications()
        self.assertEqual(self.notifier.sent, [])
        self.db.accounts[1]["schedulable"] = False
        self.detection.finish_disposition(1)
        self.assertEqual(len(self._outbox()), 1)

    async def test_missing_admin_authorization_preserves_mark_without_notification(self):
        self.service.r.oauth_state_store = lambda: SimpleNamespace(admin_token=lambda: "")
        result = await self.detection.verdict(self._job(), {"prediction": TARGET, "used_outputs": 1})
        self.assertTrue(result["marked"])
        self.assertFalse(result["schedule_verified"])
        self.assertEqual(self._outbox(), {})
        self.assertEqual(self.notifier.sent, [])

    async def test_mark_save_failure_cannot_stop_scheduling_or_enqueue_result(self):
        with patch("app.capacity_alerts.write_json", side_effect=OSError("fixture mark save failure")):
            with patch("app.key_fallback.execute_sub2api_set_schedulable") as stop:
                with self.assertRaisesRegex(OSError, "fixture mark save failure"):
                    await self.detection.verdict(self._job(), {"prediction": TARGET, "used_outputs": 1})
        stop.assert_not_called()
        self.assertTrue(self.db.accounts[1]["schedulable"])
        self.assertEqual(self._outbox(), {})
        self.assertEqual(self.notifier.sent, [])

    async def test_non_degraded_and_empty_reports_do_not_create_notifications(self):
        for report in ({"prediction": DEFAULT_MODEL, "used_outputs": 1}, {"prediction": TARGET, "used_outputs": 0}):
            self.assertIsNone(await self.detection.verdict(self._job(), report))
        self.assertEqual(self._outbox(), {})
        self.assertFalse(self.detection.mark(1)["marked"])
        self.assertTrue(self.db.accounts[1]["schedulable"])

    async def test_result_delivery_retries_on_persisted_schedule_and_only_delivers_once(self):
        job = await self._complete()
        self.notifier.results = [BarkPushResult(False, "fixture-retry")] * 4 + [BarkPushResult(True)]
        for attempts, delay in enumerate((5, 30, 120, 600), start=1):
            self.detection.deliver_notifications()
            self.assertEqual(len(self.notifier.sent), attempts)
            queued = self._outbox()[job["id"]]
            self.assertEqual(queued["attempts"], attempts)
            self.assertEqual(datetime.fromisoformat(queued["next_at"]), self.clock() + timedelta(seconds=delay))
            await self._restart()
            self.detection.deliver_notifications()
            self.assertEqual(len(self.notifier.sent), attempts)
            self.clock.advance(delay)
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 5)
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 5)

    async def test_attempt_save_failure_blocks_bark_until_durable_retry(self):
        job = await self._complete()
        with patch("app.policy_store.write_json", side_effect=OSError("fixture save failure")):
            with self.assertRaisesRegex(OSError, "fixture save failure"):
                self.detection.deliver_notifications()
        self.assertEqual(self.notifier.sent, [])
        self.assertEqual(self._outbox()[job["id"]]["attempts"], 0)
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 1)

    async def test_disabled_bark_suppresses_confirmed_result_and_invalid_config_waits(self):
        job = await self._complete()
        self.notifier.config_valid = False
        self.detection.deliver_notifications()
        self.assertEqual(self.notifier.sent, [])
        self.assertEqual(self._outbox()[job["id"]]["attempts"], 0)
        self.notifier.config_valid = True
        self.notifier.enabled = False
        self.detection.deliver_notifications()
        self.assertEqual(self.notifier.sent, [])
        self.assertEqual(self._outbox()[job["id"]]["status"], "suppressed")
        self.notifier.enabled = True
        self.detection.deliver_notifications()
        self.assertEqual(self.notifier.sent, [])

    async def test_delivery_receipt_save_failure_keeps_durable_backoff(self):
        job = await self._complete()

        def fail_receipt(path, data):
            event = data.get("disposition_notifications", {}).get(job["id"], {})
            if event.get("status") == "delivered":
                raise OSError("fixture receipt save failure")
            return write_json(path, data)

        with patch("app.policy_store.write_json", side_effect=fail_receipt):
            with self.assertRaisesRegex(OSError, "fixture receipt save failure"):
                self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 1)
        self.assertEqual(self._outbox()[job["id"]]["attempts"], 1)
        await self._restart()
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 1)
        self.clock.advance(5)
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 2)
        self.assertEqual(self._outbox()[job["id"]]["status"], "delivered")

    async def test_completion_and_outbox_save_are_atomic_and_restart_can_recover(self):
        job = self._job()

        def fail_completed(path, data):
            disposition = data.get("accounts", {}).get("1", {}).get("disposition", {})
            if disposition.get("status") == "completed":
                self.assertIn(job["id"], data["disposition_notifications"])
                raise OSError("fixture completion save failure")
            return write_json(path, data)

        with patch("app.key_fallback.execute_sub2api_set_schedulable", side_effect=self._stop_schedule):
            with patch("app.policy_store.write_json", side_effect=fail_completed):
                with self.assertRaisesRegex(OSError, "fixture completion save failure"):
                    await self.detection.verdict(job, {"prediction": TARGET, "used_outputs": 1})
        self.assertFalse(self.db.accounts[1]["schedulable"])
        self.assertEqual(self._outbox(), {})
        self.assertEqual(self.notifier.sent, [])
        await self._restart()
        with patch("app.key_fallback.execute_sub2api_set_schedulable") as stop:
            self.detection.finish_disposition(1)
        stop.assert_not_called()
        self.assertEqual(set(self._outbox()), {job["id"]})
        self.detection.deliver_notifications()
        self.assertEqual(len(self.notifier.sent), 1)

    async def test_manual_override_before_verdict_prevents_mark_schedule_and_bark(self):
        job = self._job()
        self.detection.human_control(1, release_hold=True)
        with self.assertRaises(HTTPException) as raised:
            await self.detection.verdict(job, {"prediction": TARGET, "used_outputs": 1})
        self.assertEqual(raised.exception.status_code, 409)
        self.assertEqual(self._outbox(), {})
        self.assertFalse(self.detection.mark(1)["marked"])
        self.assertTrue(self.db.accounts[1]["schedulable"])
        self.assertEqual(self.notifier.sent, [])

    async def test_manual_override_after_verified_disposition_suppresses_queued_result(self):
        await self._complete()
        self.detection.human_control(1, release_hold=True)
        self.detection.deliver_notifications()
        self.assertEqual(self.notifier.sent, [])
        self.clock.advance(600)
        self.detection.deliver_notifications()
        self.assertEqual(self.notifier.sent, [])

    async def test_manual_override_during_schedule_write_prevents_completed_receipt(self):
        def stopped_then_overridden(aid, schedulable, **kwargs):
            self._stop_schedule(aid, schedulable, **kwargs)
            self.detection.human_control(aid, release_hold=True)

        with patch("app.key_fallback.execute_sub2api_set_schedulable", side_effect=stopped_then_overridden):
            with self.assertRaises(HTTPException) as raised:
                await self.detection.verdict(self._job(), {"prediction": TARGET, "used_outputs": 1})
        self.assertEqual(raised.exception.status_code, 409)
        self.assertEqual(self._outbox(), {})
        self.assertEqual(self.notifier.sent, [])


if __name__ == "__main__":
    unittest.main()
