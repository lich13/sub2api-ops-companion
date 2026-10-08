from __future__ import annotations

import asyncio
import copy
import sqlite3
import unittest
from datetime import datetime, timedelta
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from app.account_locks import AccountLease
from app.capacity_alerts import CapacityAlertStore
from app.model_detection import DEFAULT_MODEL, TARGET, ModelDetection
from app.model_test_stream import TestFailure
from app.model_tests import ModelTests
from tests.test_auto_reset import ACCOUNT_ID, AutoResetFixture
from tests.test_recovery019 import passive_account, recovery_intent


OUTPUT = " ".join(str(i * 13 % 355 + 1) for i in range(80))


class _RecoveryModelFixture(unittest.IsolatedAsyncioTestCase):
    """Keep the real schedulers, stores and leases; isolate upstream operations."""

    async def asyncSetUp(self):
        self.fixture = AutoResetFixture()
        self.fixture.addCleanup = self.addCleanup
        self.fixture.setUp()
        fixture = self.fixture
        fixture.settings.oauth_recovery_connection_account_ids = []
        fixture.settings.oauth_recovery_model_account_ids = [ACCOUNT_ID]
        fixture.row.update(group_ids=[], proxy_id=None, deleted_at=None)

        evidence_read = fixture.db.fetch_one

        def read(sql, params=None):
            if "FROM accounts" in sql:
                return fixture.read_account(fixture.db, int((params or {}).get("id") or 0))
            return evidence_read(sql, params)

        fixture.db.fetch_one = read
        fixture.db.fetch_all = lambda *_args, **_kwargs: []
        self.capacity = SimpleNamespace(store=CapacityAlertStore(fixture.root / "capacity.json"))
        self.service = SimpleNamespace(
            invalidate=Mock(),
            actions=SimpleNamespace(
                account=AsyncMock(side_effect=lambda aid, *_args: fixture.read_account(fixture.db, aid)),
                model_allowed=AsyncMock(return_value=True),
            ),
            r=SimpleNamespace(
                db=fixture.db,
                settings=fixture.settings,
                capacity_alerts=self.capacity,
                fingerprint_bank=SimpleNamespace(capture=Mock(return_value=(
                    {"models": [{"id": TARGET}, {"id": DEFAULT_MODEL}]}, "fixture-bank"
                ))),
                oauth_state_store=lambda: fixture.store,
                oauth_base_url=lambda: "https://example.invalid",
            ),
        )
        self.detection = ModelDetection(self.service, clock=lambda: fixture.now)
        self.service.model_detection = self.detection
        self.tests = ModelTests(self.service)
        self.service.model_tests = self.tests
        self._bind_monitor()
        self.analysis_report = {"prediction": DEFAULT_MODEL, "probability": .75, "used_outputs": 1}
        self.model_calls = []
        self.model_hook = None
        self.model_failure = None
        self.tests.execute = self._execute
        analyzer = patch("app.model_tests.analyze", side_effect=lambda *_args, **_kwargs: copy.deepcopy(self.analysis_report))
        self.analyzer = analyzer.start()
        self.addCleanup(analyzer.stop)
        schedule = patch("app.key_fallback.execute_sub2api_set_schedulable", side_effect=fixture.schedule_runner)
        schedule.start()
        self.addCleanup(schedule.stop)
        self.addAsyncCleanup(self.tests.close)

    def _bind_monitor(self):
        fixture = self.fixture
        fixture.monitor.auto_reset = fixture.controller
        self.service.r.oauth_monitor = fixture.monitor
        fixture.monitor.detection_gate = lambda aid: self.detection.held(aid) or self.detection.mark(aid)["marked"]
        self.detection.bind_recovery(fixture.monitor, asyncio.get_running_loop())

    async def _execute(self, **request):
        self.fixture.calls.append("model")
        self.model_calls.append({"at": self.fixture.now, "model": request["model"], "prompt": request["prompt"]})
        if self.model_hook:
            await self.model_hook()
        if self.model_failure:
            raise self.model_failure
        return OUTPUT, DEFAULT_MODEL

    def _natural_ready(self):
        fixture = self.fixture
        fixture.settings.oauth_auto_reset_credit_enabled = False
        fixture.row = {
            **passive_account(fixture.now), "id": ACCOUNT_ID, "name": "fixture-recovery",
            "parent_account_id": None, "group_ids": [], "proxy_id": None, "deleted_at": None,
            "credentials": {"plan_type": "plus", "access_token": "fixture-access"},
        }
        fixture.store.update_scheduler({ACCOUNT_ID: {"recovery_intent": recovery_intent(now=fixture.now)}})

    async def _monitor_cycle(self):
        return await asyncio.wait_for(
            asyncio.to_thread(self.fixture.monitor.run_once, now=self.fixture.now), 10
        )

    async def _card_cycle(self):
        fixture = self.fixture
        await asyncio.wait_for(
            asyncio.to_thread(fixture.controller.run, [copy.deepcopy(fixture.row)], fixture.now), 10
        )

    def _latest(self):
        job = self.tests.latest(ACCOUNT_ID)
        self.assertIsNotNone(job, "The actual scheduler did not enqueue a model job")
        return job

    async def _finish(self, job=None):
        job = job or self._latest()
        task = self.tests.tasks.get(job["id"])
        if task:
            await asyncio.wait_for(asyncio.shield(task), 10)
        return self.tests.get(job["id"])

    async def _wait_queued(self, job):
        async def wait():
            while job["id"] not in self.tests.waiting_leases:
                self.assertEqual(self.tests.get(job["id"])["status"], "queued")
                await asyncio.sleep(.001)

        await asyncio.wait_for(wait(), 5)
        self.assertEqual(self.tests.get(job["id"])["status"], "queued")

    def _assert_account_available(self):
        lease = AccountLease(self.fixture.db, copy.deepcopy(self.fixture.row))
        try:
            self.assertTrue(lease.acquire(), "Waiting recovery retained the account lease")
        finally:
            lease.release()

    def _occupy_model_slots(self):
        self.tests.job_slots = asyncio.Semaphore(0)

    def _start_cooldown(self):
        self.detection.register_dispatch({
            "id": "fixture-earlier-automatic", "account_id": ACCOUNT_ID,
            "automatic": True, "requested_model": DEFAULT_MODEL,
            "detection_generation": self.detection.control(ACCOUNT_ID)["generation"],
            "detection_mark_version": self.detection.mark(ACCOUNT_ID)["version"], "triggers": ["scheduled"],
        })
        return datetime.fromisoformat(self.detection.next_allowed_at(ACCOUNT_ID))

    async def _completed_natural_job(self):
        self._natural_ready()
        await self._monitor_cycle()
        job = await self._finish()
        self.assertNotIn("recover", self.fixture.calls)
        self.assertNotIn("test", self.fixture.calls)
        self.fixture.now += timedelta(seconds=5)
        return job

    def _intent(self):
        return self.fixture.store.snapshot()["scheduler"][str(ACCOUNT_ID)]["recovery_intent"]

    def _change_after_success(self, change):
        verifier = self.fixture.monitor.model_verifier
        changed = Mock()

        def delayed_result(*args, **kwargs):
            result = verifier(*args, **kwargs)
            if result.get("success"):
                change()
                changed()
            return result

        self.fixture.monitor.model_verifier = delayed_result
        return changed

    async def _assert_changed_selection_skips_dispatch(self, change):
        self._natural_ready()
        fixture = self.fixture
        budget = {
            "last_query_at": (fixture.now - timedelta(seconds=30)).isoformat(),
            "automatic_attempts": [(fixture.now - timedelta(hours=hour)).isoformat() for hour in (1, 2)],
        }
        fixture.store.update_scheduler({ACCOUNT_ID: {"quota_query": budget}})
        before = copy.deepcopy(fixture.row)
        recovery_test = fixture.monitor._recovery_test
        results = []

        def change_before_execution(*args, **kwargs):
            self.assertEqual(kwargs["verification_method"], "model")
            self.assertEqual(kwargs["verification_generation"], 0)
            self._assert_account_available()
            change()
            result = recovery_test(*args, **kwargs)
            results.append(result)
            return result

        with patch.object(fixture.monitor, "_recovery_test", side_effect=change_before_execution) as dispatch:
            events = await self._monitor_cycle()

        dispatch.assert_called_once()
        self.assertEqual(len(results), 1)
        self.assertFalse(results[0]["success"])
        self.assertTrue(results[0]["skipped"])
        self.assertEqual(results[0]["error_code"], "recovery_method_changed")
        self.assertEqual(fixture.calls, [])
        self.assertEqual(self.model_calls, [])
        self.assertIsNone(self.tests.latest(ACCOUNT_ID))
        self.analyzer.assert_not_called()
        self.assertEqual(fixture.row, before)
        self.assertNotEqual(self._intent()["status"], "recovered")
        self.assertFalse(any(event["status"] == "recovered" for event in events))
        self.assertEqual(fixture.store.snapshot()["recovery_history"], {})
        self.assertEqual(fixture.query_state()["automatic_attempts"], budget["automatic_attempts"])
        self.assertEqual(fixture.query_state()["last_query_at"], budget["last_query_at"])
        self.assertFalse(fixture.state().get("attempt_at"))
        self._assert_account_available()


class NaturalModelRecovery030Tests(_RecoveryModelFixture):
    async def test_valid_normal_fingerprint_runs_one_group_and_really_recovers(self):
        job = await self._completed_natural_job()
        self.assertEqual(job["status"], "completed")
        self.assertEqual((job["planned_groups"], len(job["groups"]), job["attempts"]), (1, 1, 1))
        self.assertEqual((job["completed_groups"], job["valid_groups"]), (1, 1))
        self.assertEqual(len(self.model_calls), 1)
        self.assertEqual(self.model_calls[0]["model"], DEFAULT_MODEL)
        events = await self._monitor_cycle()
        self.assertEqual(self.fixture.calls.count("recover"), 1)
        self.assertEqual(self._intent()["status"], "recovered")
        self.assertIsNone(self.fixture.row["rate_limit_reset_at"])
        self.assertTrue(any(event["status"] == "recovered" for event in events))
        self.assertEqual(len(self.fixture.store.snapshot()["recovery_history"]), 1)
        self._assert_account_available()

    async def test_failed_model_request_does_not_recover(self):
        self.model_failure = TestFailure("invalid_model_or_request")
        job = await self._completed_natural_job()
        self.assertEqual(job["status"], "failed")
        await self._monitor_cycle()
        self.assertEqual(len(self.model_calls), 1)
        self.analyzer.assert_not_called()
        self.assertNotIn("recover", self.fixture.calls)
        self.assertEqual(self._intent()["status"], "retry")
        self.assertIsNotNone(self.fixture.row["rate_limit_reset_at"])

    async def test_empty_fingerprint_analysis_does_not_recover(self):
        self.analysis_report = None
        job = await self._completed_natural_job()
        self.assertEqual(job["valid_groups"], 0)
        await self._monitor_cycle()
        self.assertNotIn("recover", self.fixture.calls)
        self.assertEqual(self._intent()["last_error_code"], "model_analysis_insufficient")

    async def test_auth_failure_enters_auth_pause_without_recovery(self):
        self.model_failure = TestFailure("auth_or_quota")
        job = await self._completed_natural_job()
        self.assertEqual(job["error_code"], "auth_or_quota")
        await self._monitor_cycle()
        self.assertEqual(self._intent()["status"], "auth_failed")
        self.assertEqual(self._intent()["last_error_code"], "http_401")
        self.assertTrue(self.fixture.query_state()["auth_fingerprint"])
        self.assertEqual(len(self.model_calls), 1)
        self.assertNotIn("recover", self.fixture.calls)

    async def test_missing_prediction_does_not_authorize_recovery(self):
        self.analysis_report = {"probability": .99, "used_outputs": 1}
        await self._completed_natural_job()
        await self._monitor_cycle()
        self.assertNotIn("recover", self.fixture.calls)
        self.assertNotEqual(self._intent()["status"], "recovered")

    async def test_degraded_fingerprint_stops_scheduling_without_recovery(self):
        self.analysis_report = {"prediction": TARGET, "probability": .05, "used_outputs": 1}
        job = await self._completed_natural_job()
        self.assertEqual(len(self.model_calls), 1)
        self.assertEqual(job["first_group_prediction"], TARGET)
        self.assertTrue(self.detection.mark(ACCOUNT_ID)["marked"])
        self.assertFalse(self.fixture.row["schedulable"])
        await self._monitor_cycle()
        self.assertNotIn("recover", self.fixture.calls)

    async def test_model_to_connection_before_dispatch_skips_stale_task(self):
        def select_connection():
            self.fixture.settings.oauth_recovery_model_account_ids = []
            self.fixture.settings.oauth_recovery_connection_account_ids = [ACCOUNT_ID]

        await self._assert_changed_selection_skips_dispatch(select_connection)

    async def test_model_reselected_before_dispatch_skips_old_method_generation(self):
        def reselect_model():
            self.fixture.settings.oauth_recovery_model_account_ids = []
            self.fixture.settings.oauth_recovery_connection_account_ids = [ACCOUNT_ID]
            self.fixture.store.update_scheduler({ACCOUNT_ID: {"recovery_method_generation": 1}})
            self.fixture.settings.oauth_recovery_connection_account_ids = []
            self.fixture.settings.oauth_recovery_model_account_ids = [ACCOUNT_ID]
            self.fixture.store.update_scheduler({ACCOUNT_ID: {"recovery_method_generation": 2}})

        await self._assert_changed_selection_skips_dispatch(reselect_model)
        self.assertEqual(self.fixture.settings.oauth_recovery_model_account_ids, [ACCOUNT_ID])
        self.assertEqual(self.fixture.store.scheduler()[ACCOUNT_ID]["recovery_method_generation"], 2)

    async def test_selected_method_change_rejects_success_before_recovery_write(self):
        await self._completed_natural_job()

        def select_connection():
            self.fixture.settings.oauth_recovery_model_account_ids = []
            self.fixture.settings.oauth_recovery_connection_account_ids = [ACCOUNT_ID]

        changed = self._change_after_success(select_connection)
        await self._monitor_cycle()
        changed.assert_called_once()
        self.assertNotIn("recover", self.fixture.calls)
        self.assertNotEqual(self._intent()["status"], "recovered")

    async def test_method_generation_change_rejects_success_even_if_model_is_reselected(self):
        await self._completed_natural_job()

        def change_generation():
            self.fixture.store.update_scheduler({ACCOUNT_ID: {"recovery_method_generation": 2}})

        changed = self._change_after_success(change_generation)
        await self._monitor_cycle()
        changed.assert_called_once()
        self.assertEqual(self.fixture.settings.oauth_recovery_model_account_ids, [ACCOUNT_ID])
        self.assertNotIn("recover", self.fixture.calls)
        self.assertNotEqual(self._intent()["status"], "recovered")

    async def test_manual_control_generation_rejects_success_before_recovery_write(self):
        await self._completed_natural_job()

        def manual_disable():
            self.fixture.store.manual_control(self.fixture.row, False, self.fixture.now)
            self.fixture.row["schedulable"] = False
            self.fixture.touch()

        changed = self._change_after_success(manual_disable)
        await self._monitor_cycle()
        changed.assert_called_once()
        self.assertEqual(self.fixture.store.control_generation(ACCOUNT_ID), 1)
        self.assertFalse(self.fixture.row["schedulable"])
        self.assertEqual(self._intent()["status"], "cancelled")
        self.assertNotIn("recover", self.fixture.calls)

    async def test_cooldown_has_no_model_job_or_account_lease_and_opens_at_boundary(self):
        self._natural_ready()
        due = self._start_cooldown()
        await self._monitor_cycle()
        self.assertIsNone(self.tests.latest(ACCOUNT_ID))
        self.assertEqual(self.model_calls, [])
        self._assert_account_available()
        self.assertEqual(datetime.fromisoformat(self._intent()["next_retry_at"]), due)
        self.fixture.now = due - timedelta(microseconds=1)
        await self._monitor_cycle()
        self.assertIsNone(self.tests.latest(ACCOUNT_ID))
        self._assert_account_available()
        self.fixture.now = due
        await self._monitor_cycle()
        await self._finish()
        self.assertEqual(len(self.model_calls), 1)
        self.assertEqual(self.model_calls[0]["at"], due)

    async def test_waiting_for_model_slot_does_not_hold_account_lease(self):
        self._natural_ready()
        self._occupy_model_slots()
        await self._monitor_cycle()
        job = self._latest()
        await self._wait_queued(job)
        self.assertEqual(self.model_calls, [])
        self._assert_account_available()
        self.assertIsNone(self.detection.next_allowed_at(ACCOUNT_ID))
        self.tests.job_slots.release()
        await self._finish(job)
        self.assertEqual(len(self.model_calls), 1)
        self._assert_account_available()

    async def test_manual_generation_changed_while_queued_prevents_dispatch(self):
        self._natural_ready()
        self._occupy_model_slots()
        await self._monitor_cycle()
        job = self._latest()
        await self._wait_queued(job)
        self.fixture.store.manual_control(self.fixture.row, False, self.fixture.now)
        self.fixture.row["schedulable"] = False
        self.tests.job_slots.release()
        finished = await self._finish(job)
        self.assertEqual(finished["status"], "needs_confirmation")
        self.assertEqual(self.model_calls, [])
        self.assertNotIn("recover", self.fixture.calls)
        self.assertIsNone(self.detection.next_allowed_at(ACCOUNT_ID))
        self._assert_account_available()


class CreditModelRecovery030Tests(_RecoveryModelFixture):
    async def test_structured_local_limiters_never_authorize_credit_consumption(self):
        message = "You have reached the request limit: 10 requests per minute"
        cases = (
            {"upstream_error_message": "您已达到请求数限制：1分钟内最多请求 10 次"},
            {"error_body": {"error": {"message": message}}},
            {"upstream_errors": [{"message": message}]},
            {"error_owner": "client", "error_phase": "concurrency",
             "upstream_error_message": "Too many concurrent requests for user"},
        )
        for fields in cases:
            with self.subTest(fields=fields):
                self.fixture.db.raw.execute("DELETE FROM ops_error_logs")
                self.fixture.db.event(**fields)
                self.fixture.now += timedelta(seconds=30)
                await self._card_cycle()
                self.assertEqual(self.fixture.calls, [])
                self.assertIsNone(self.tests.latest(ACCOUNT_ID))
                self.assertTrue(self.fixture.row["schedulable"])
                self.assertFalse(self.fixture.state().get("attempt_at"))
                self._assert_account_available()

    async def test_local_limiter_is_filtered_before_latest_real_upstream_evidence_is_selected(self):
        self.fixture.db.event(
            2, at=self.fixture.now - timedelta(minutes=1),
            upstream_error_message="You have reached the request limit: 10 requests per minute",
        )
        await self._card_cycle()
        await self._finish()
        self.assertEqual(self.fixture.state()["evidence_id"], "1")
        self.assertEqual(self.fixture.calls, ["schedule:False", "reset", "model"])

    async def test_failed_evidence_query_never_falls_back_or_authorizes_consumption(self):
        with patch.object(self.fixture.db, "fetch_one", side_effect=sqlite3.OperationalError("fixture query unavailable")) as read:
            await self._card_cycle()
        read.assert_called_once()
        self.assertEqual(self.fixture.calls, [])
        self.assertIsNone(self.tests.latest(ACCOUNT_ID))
        self.assertTrue(self.fixture.row["schedulable"])
        self.assertFalse(self.fixture.state().get("attempt_at"))

    async def test_credit_waits_for_slot_consumes_once_and_verifies_immediately(self):
        fixture = self.fixture
        self._occupy_model_slots()
        await self._card_cycle()
        job = self._latest()
        await self._wait_queued(job)
        self.assertEqual(fixture.calls, [])
        self.assertTrue(fixture.row["schedulable"])
        self.assertFalse(fixture.state().get("attempt_at"))
        self._assert_account_available()
        admitted_at = fixture.now
        consumed_with_slot = []

        def check_admission(action):
            if action != "reset":
                return
            self.assertTrue(self.tests.job_slots.locked())
            other = AccountLease(fixture.db, copy.deepcopy(fixture.row))
            try:
                self.assertFalse(other.acquire(), "Card consumption must own the account lease")
            finally:
                other.release()
            consumed_with_slot.append(fixture.now)

        fixture.request_hook = check_admission
        self.tests.job_slots.release()
        finished = await self._finish(job)
        self.assertEqual(finished["status"], "completed")
        self.assertEqual(fixture.calls, ["schedule:False", "reset", "model"])
        self.assertEqual(consumed_with_slot, [admitted_at])
        self.assertEqual([call["at"] for call in self.model_calls], [admitted_at])
        self.assertEqual((finished["planned_groups"], len(finished["groups"])), (1, 1))
        self.assertTrue(fixture.state()["consumed"])
        self.assertFalse(fixture.row["schedulable"])
        fixture.now += timedelta(seconds=30)
        await self._card_cycle()
        self.assertEqual(fixture.state()["stage"], "recovered")
        self.assertEqual(fixture.calls, ["schedule:False", "reset", "model", "recover", "schedule:True"])
        self.assertTrue(fixture.row["schedulable"])
        self.assertEqual(len(fixture.query_state()["automatic_attempts"]), 1)
        self._assert_account_available()

    async def test_credit_cooldown_does_not_pause_consume_or_hold_account(self):
        due = self._start_cooldown()
        await self._card_cycle()
        self.assertEqual(self.fixture.calls, [])
        self.assertIsNone(self.tests.latest(ACCOUNT_ID))
        self.assertTrue(self.fixture.row["schedulable"])
        self.assertFalse(self.fixture.state().get("attempt_at"))
        self.assertEqual(datetime.fromisoformat(self.fixture.state()["next_at"]), due)
        self._assert_account_available()
        self.fixture.now = due
        await self._card_cycle()
        await self._finish()
        self.assertEqual(self.fixture.calls, ["schedule:False", "reset", "model"])
        self.assertEqual(self.model_calls[0]["at"], due)

    async def test_credit_query_cooldown_prevents_consumption_even_with_model_capacity(self):
        fixture = self.fixture
        last_query = fixture.now - timedelta(seconds=30)
        fixture.store.update_scheduler({ACCOUNT_ID: {"quota_query": {
            "last_query_at": last_query.isoformat(), "automatic_attempts": [last_query.isoformat()],
        }}})
        await self._card_cycle()
        job = await self._finish()
        self.assertEqual(job["status"], "failed")
        self.assertTrue(job["preflight_result"]["deferred"])
        self.assertEqual(fixture.calls, [])
        self.assertTrue(fixture.row["schedulable"])
        self.assertFalse(fixture.state().get("attempt_at"))
        self.assertIsNone(self.detection.next_allowed_at(ACCOUNT_ID))
        self._assert_account_available()

    async def test_failed_credit_verification_retries_only_model_after_cooldown(self):
        fixture = self.fixture
        self.model_failure = TestFailure("invalid_model_or_request")
        await self._card_cycle()
        first = await self._finish()
        self.assertEqual(first["status"], "failed")
        self.assertEqual(fixture.calls, ["schedule:False", "reset", "model"])
        fixture.now += timedelta(seconds=30)
        await self._card_cycle()
        self.assertEqual((fixture.state()["stage"], fixture.state()["test_attempts"]), ("retry", 1))
        fixture.now = datetime.fromisoformat(fixture.state()["next_at"])
        await self._card_cycle()
        self.assertEqual(len(self.model_calls), 1)
        self.assertEqual(fixture.calls.count("reset"), 1)
        self.assertEqual(fixture.state()["test_attempts"], 1)
        self.assertEqual(fixture.state()["next_at"], self.detection.next_allowed_at(ACCOUNT_ID))
        self._assert_account_available()
        fixture.now = datetime.fromisoformat(fixture.state()["next_at"])
        self.model_failure = None
        await self._card_cycle()
        second = await self._finish()
        self.assertNotEqual(first["id"], second["id"])
        self.assertFalse(second["recovery_context"]["consume"])
        self.assertEqual(len(self.model_calls), 2)
        self.assertEqual(fixture.calls.count("reset"), 1)
        fixture.now += timedelta(seconds=30)
        await self._card_cycle()
        self.assertEqual(fixture.state()["stage"], "recovered")
        self.assertEqual(fixture.calls.count("reset"), 1)
        self.assertEqual(fixture.calls.count("recover"), 1)
        self.assertNotIn("test", fixture.calls)

    async def test_uncertain_consumption_is_not_replayed_after_restart(self):
        fixture = self.fixture
        fixture.request_results.append({
            "success": False, "consumed": False, "uncertain": True, "error_code": "result_uncertain",
        })
        await self._card_cycle()
        job = await self._finish()
        self.assertEqual(job["status"], "failed")
        self.assertEqual(fixture.calls, ["schedule:False", "reset"])
        self.assertEqual(self.model_calls, [])
        episode, attempted = fixture.state()["episode"], fixture.state()["attempt_at"]
        self.assertEqual(fixture.state()["stage"], "uncertain")
        self.assertFalse(fixture.row["schedulable"])
        await self.tests.close()
        fixture.make_controller()
        self._bind_monitor()
        self.tests = ModelTests(self.service)
        self.tests.execute = self._execute
        self.service.model_tests = self.tests
        self.addAsyncCleanup(self.tests.close)
        for _ in range(2):
            fixture.now += timedelta(hours=2)
            await self._card_cycle()
        self.assertEqual(fixture.state()["episode"], episode)
        self.assertEqual(fixture.state()["attempt_at"], attempted)
        self.assertEqual(fixture.calls.count("reset"), 1)
        self.assertEqual(fixture.calls.count("query"), 2)
        self.assertEqual(self.model_calls, [])
        self.assertNotIn("recover", fixture.calls)
        self.assertNotIn("schedule:True", fixture.calls)
        self._assert_account_available()
