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

from app.capacity_alerts import CapacityAlertStore
from app.model_detection import DEFAULT_MODEL, TARGET, DetectionRequest, ModelDetection
from app.model_tests import ModelTestRequest, ModelTests


NOW = datetime(2026, 10, 6, 3, 0, tzinfo=timezone.utc)
OUTPUT = " ".join(str(i * 13 % 355 + 1) for i in range(80))


class _Clock:
    def __init__(self, value=NOW):
        self.value = value

    def __call__(self):
        return self.value


def _row(account_id=1):
    return {
        "id": account_id,
        "name": "fixture-account",
        "platform": "openai",
        "type": "apikey",
        "status": "active",
        "schedulable": True,
        "parent_account_id": None,
        "group_ids": [],
        "model_catalog_version": "fixture-catalog",
        "extra": {},
        "proxy_id": None,
        "credentials": {"api_key": "fixture-key", "base_url": "http://unused.invalid"},
    }


class _DetectionFixture(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.clock = _Clock()
        self.row = _row()
        self.db = SimpleNamespace(fetch_one=lambda *args, **kwargs: copy.deepcopy(self.row))
        self.capacity = SimpleNamespace(store=CapacityAlertStore(root / "capacity.json"))
        self.jobs = {}
        self.job_index = 0

        async def start(_aid, _payload, automatic=None):
            self.job_index += 1
            job_id = f"{self.job_index:032x}"
            job = {
                "id": job_id,
                "account_id": _aid,
                "status": "queued",
                "automatic": bool(automatic),
                "triggers": sorted(set((automatic or {}).get("triggers") or [])),
                "detection_generation": (automatic or {}).get("detection_generation"),
                "detection_mark_version": (automatic or {}).get("detection_mark_version"),
            }
            self.jobs[job_id] = job
            return copy.deepcopy(job)

        self.model_tests = SimpleNamespace(
            account=AsyncMock(return_value=copy.deepcopy(self.row)),
            get=lambda job_id: copy.deepcopy(self.jobs[job_id]),
            latest=lambda aid: next(
                (copy.deepcopy(job) for job in reversed(list(self.jobs.values())) if job["account_id"] == aid),
                None,
            ),
            start=AsyncMock(side_effect=start),
            cancel=AsyncMock(),
        )
        self.actions = SimpleNamespace(
            account=AsyncMock(return_value=copy.deepcopy(self.row)),
            model_allowed=AsyncMock(return_value=True),
        )
        self.service = SimpleNamespace(
            invalidate=Mock(),
            model_tests=self.model_tests,
            actions=self.actions,
            r=SimpleNamespace(
                db=self.db,
                settings=SimpleNamespace(
                    usage_query_state_path=str(root / "usage.json"),
                    audit_path=str(root / "audit.jsonl"),
                ),
                capacity_alerts=self.capacity,
                fingerprint_bank=SimpleNamespace(
                    capture=Mock(return_value=({"models": [{"id": TARGET}]}, "fixture-bank"))
                ),
            ),
        )
        self.detection = ModelDetection(self.service, clock=self.clock)

    def _event(self, key, account_id=1):
        with self.capacity.store.transaction() as data:
            data["detection_events"][key] = {"account_id": account_id}

    async def _enable(self, interval=15):
        view = self.detection.view(1)
        return await self.detection.save(
            1,
            DetectionRequest(
                expected_version=view["version"],
                enabled=True,
                interval_minutes=interval,
                model_id=DEFAULT_MODEL,
            ),
        )

    async def test_default_is_disabled_and_save_schedules_from_injected_now(self):
        default = self.detection.view(1)
        self.assertEqual(
            {key: default[key] for key in ("enabled", "interval_minutes", "next_at", "status")},
            {"enabled": False, "interval_minutes": 15, "next_at": None, "status": "disabled"},
        )
        saved = await self._enable(interval=20)
        self.assertEqual(datetime.fromisoformat(saved["next_at"]), NOW + timedelta(minutes=20))
        self.assertEqual(saved["status"], "waiting")

    async def test_pause_and_resume_starts_a_fresh_wait_without_replaying_missed_time(self):
        await self._enable()
        mark = self.detection.mark(1)
        self.capacity.store.set_mark(1, True, mark["version"], self.clock())
        await self.detection.tick()
        paused = self.detection.view(1)
        self.assertEqual((paused["status"], paused["next_at"]), ("paused", None))

        mark = self.detection.mark(1)
        self.capacity.store.set_mark(1, False, mark["version"], self.clock())
        self.clock.value = NOW + timedelta(hours=4)
        await self.detection.tick()
        resumed = self.detection.view(1)
        self.assertEqual(resumed["status"], "waiting")
        self.assertEqual(datetime.fromisoformat(resumed["next_at"]), self.clock.value + timedelta(minutes=15))
        self.model_tests.start.assert_not_awaited()

    async def test_completed_job_anchors_next_run_from_current_time_without_catch_up(self):
        await self._enable()
        self.clock.value = NOW + timedelta(hours=1)
        await self.detection.tick()
        self.assertEqual(self.model_tests.start.await_count, 1)
        job_id = self.detection.control(1)["job_id"]
        self.jobs[job_id]["status"] = "completed"
        self.clock.value = NOW + timedelta(hours=4)
        await self.detection.tick()
        state = self.detection.view(1)
        self.assertEqual(self.model_tests.start.await_count, 1)
        self.assertEqual(datetime.fromisoformat(state["next_at"]), self.clock.value + timedelta(minutes=15))

    async def test_duplicate_detection_events_are_ignored_while_job_is_active(self):
        self._event("error:one")
        await self.detection.tick()
        first_job_id = self.detection.control(1)["job_id"]
        self._event("slow:one")
        await self.detection.tick()
        self.assertEqual(self.model_tests.start.await_count, 1)
        state = self.detection.control(1)
        self.assertEqual(state["job_id"], first_job_id)
        self.assertEqual(state["triggers"], ["error:one"])
        self.assertEqual(self.jobs[first_job_id]["triggers"], ["error:one"])
        snapshot = self.capacity.store.snapshot()
        self.assertEqual(snapshot["detection_events"], {})
        self.assertEqual(snapshot["detection_clues"]["error:one"]["status"], "testing")
        self.assertEqual(snapshot["detection_clues"]["slow:one"]["status"], "ignored")
        self.assertEqual(set(self.detection.data()["consumed"]), {"error:one", "slow:one"})

    async def test_manual_generation_overrides_an_older_automatic_job(self):
        self.detection.update(1, enabled=True, status="handling", hold=True, disposition={"status": "pending"})
        old = self.detection.control(1)
        job = {"account_id": 1, "detection_generation": old["generation"], "detection_mark_version": self.detection.mark(1)["version"]}
        self.detection.human_control(1, release_hold=True)
        current = self.detection.control(1)
        self.assertEqual(current["generation"], old["generation"] + 1)
        self.assertFalse(current["hold"])
        self.assertEqual(current["disposition"]["status"], "overridden")
        with self.assertRaisesRegex(HTTPException, "新的人工操作"):
            await self.detection.guard(job)

    async def test_failed_persistence_fails_closed_and_unreliable_state_is_rejected(self):
        self.detection.store.read = Mock(side_effect=OSError("fixture persistence failure"))
        self.assertTrue(self.detection.held(1))

        self.detection.store.read = Mock(
            return_value={
                "version": 1,
                "accounts": {"1": {"enabled": "yes", "interval_minutes": 15, "model_id": DEFAULT_MODEL, "generation": 0, "hold": False}},
                "consumed": {},
            }
        )
        with self.assertRaises(ValueError):
            self.detection.data()

    async def test_missing_candidate_stays_paused_without_dispatch_or_repeated_directory_read(self):
        await self._enable()
        self.actions.model_allowed.reset_mock()
        self.actions.model_allowed.return_value = False
        self.clock.value += timedelta(minutes=15)
        await self.detection.tick()
        first = self.detection.view(1)
        self.assertTrue(first["candidate_blocked"])
        self.assertEqual(first["status"], "paused")
        await self.detection.tick()
        self.assertEqual(self.detection.view(1)["reason"], first["reason"])
        self.actions.model_allowed.assert_awaited_once_with(self.row, DEFAULT_MODEL, candidate_required=True)
        self.model_tests.start.assert_not_awaited()
        self.clock.value += timedelta(minutes=1)
        self.actions.model_allowed.return_value = True
        await self.detection.tick()
        self.assertFalse(self.detection.view(1)["candidate_blocked"])
        self.assertEqual(datetime.fromisoformat(self.detection.view(1)["next_at"]), self.clock.value+timedelta(minutes=15))

    async def test_missing_target_bank_never_dispatches_automatic_requests(self):
        self.service.r.fingerprint_bank.capture.return_value = ({"models": [{"id": "fixture-model"}]}, "fixture-bank")
        await self.detection.trigger(1, ["warning:fixture"])
        self.model_tests.start.assert_not_awaited()
        self.assertTrue(self.detection.view(1)["candidate_blocked"])

    async def test_detection_state_survives_restart(self):
        saved = await self._enable(interval=17)
        restarted = ModelDetection(self.service, clock=self.clock)
        self.assertEqual(restarted.view(1), saved)


class _ModelTestFixture(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.row = _row()
        self.detection = SimpleNamespace(
            guard=AsyncMock(return_value=copy.deepcopy(self.row)),
            next_allowed_at=Mock(return_value=None),
            register_dispatch=Mock(),
            verdict=AsyncMock(return_value={"status": "completed"}),
        )
        db = SimpleNamespace(
            fetch_one=lambda *args, **kwargs: copy.deepcopy(self.row),
            fetch_all=lambda *args, **kwargs: [],
        )
        self.service = SimpleNamespace(
            r=SimpleNamespace(
                db=db,
                settings=SimpleNamespace(
                    usage_query_state_path=str(root / "usage.json"),
                    audit_path=str(root / "audit.jsonl"),
                ),
                fingerprint_bank=SimpleNamespace(
                    capture=Mock(return_value=({"models": [{"id": TARGET}]}, "fixture-bank"))
                ),
            ),
            actions=SimpleNamespace(
                account=AsyncMock(return_value=copy.deepcopy(self.row)),
                model_allowed=AsyncMock(return_value=True),
            ),
            model_detection=self.detection,
        )
        self.tests = ModelTests(self.service)
        self.tests.execute = AsyncMock(return_value=(OUTPUT, DEFAULT_MODEL))
        self.addAsyncCleanup(self.tests.close)

    async def test_automatic_first_group_target_is_actionable_without_probability_threshold(self):
        request = ModelTestRequest(model_id=DEFAULT_MODEL, expected_version="a" * 64, request_id="automatic-fixture-1")
        with patch("app.model_tests.analyze", return_value={"prediction": TARGET, "probability": 0.01, "used_outputs": 1}):
            job = await self.tests.start(
                1,
                request,
                automatic={"detection_generation": 0, "detection_mark_version": "fixture-mark", "triggers": ["scheduled"]},
            )
            await self.tests.tasks[job["id"]]
        self.detection.verdict.assert_awaited_once()
        result = self.tests.get(job["id"])
        self.assertEqual(result["completion_reason"], "automatic_degradation")
        self.assertEqual(result["automatic_disposition"], {"status": "completed"})
        self.assertEqual(self.tests.execute.await_count, 1)

    async def test_manual_model_test_never_invokes_automatic_disposition(self):
        request = ModelTestRequest(model_id=DEFAULT_MODEL, expected_version="a" * 64, request_id="manual-fixture-1")
        with patch("app.model_tests.analyze", return_value={"prediction": TARGET, "probability": 0.01, "used_outputs": 1}):
            job = await self.tests.start(1, request)
            await self.tests.tasks[job["id"]]
        self.detection.verdict.assert_not_awaited()
        self.assertEqual(self.tests.get(job["id"])["status"], "completed")
        self.assertEqual(self.tests.execute.await_count, 3)


if __name__ == "__main__":
    unittest.main()
