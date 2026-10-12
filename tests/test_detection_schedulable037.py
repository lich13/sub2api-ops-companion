from __future__ import annotations

import unittest
from datetime import timedelta
from unittest.mock import patch

from tests.test_detection_rechecks034 import RecheckFixture


class SchedulableGateTests(RecheckFixture):
    def configure_scheduled_state(self, *, marked: bool) -> None:
        current = self.detection.mark(1)
        if marked:
            if not current["marked"]:
                self.capacity.store.set_mark(1, True, current["version"], self.clock())
            self.enable_due()
            return
        if current["marked"]:
            self.capacity.store.set_mark(1, False, current["version"], self.clock())
        self.detection.update(1, hold=False, disposition=None, status="waiting",
                              reason="", next_at=self.clock().isoformat())

    async def test_closed_oauth_and_key_accounts_do_not_dispatch_marked_or_unmarked(self):
        for account_type in ("oauth", "apikey"):
            for marked in (False, True):
                with self.subTest(account_type=account_type, marked=marked):
                    self.row.update(type=account_type, status="active", schedulable=False)
                    self.model_tests.start.reset_mock()
                    self.configure_scheduled_state(marked=marked)

                    await self.detection.tick()

                    self.model_tests.start.assert_not_awaited()
                    state = self.detection.control(1)
                    self.assertTrue(state["enabled"])
                    self.assertEqual((state["status"], state["next_at"], state["reason"]),
                                     ("paused", None, "调度已关闭"))
                    self.assertEqual(self.detection.mark(1)["marked"], marked)

    async def test_reopening_after_a_long_pause_keeps_settings_and_waits_a_full_interval(self):
        self.row.update(schedulable=False)
        self.configure_scheduled_state(marked=False)
        await self.detection.tick()
        paused = self.detection.control(1)
        self.assertTrue(paused["enabled"])
        self.assertEqual(paused["interval_minutes"], 15)
        self.assertEqual((paused["status"], paused["next_at"]), ("paused", None))

        self.clock.advance(3600)
        self.row["schedulable"] = True
        await self.detection.tick()
        resumed = self.detection.control(1)
        self.assertEqual(resumed["status"], "waiting")
        self.assertEqual(resumed["next_at"], (self.clock() + timedelta(minutes=15)).isoformat())
        self.model_tests.start.assert_not_awaited()

        self.clock.advance(14 * 60 + 59)
        await self.detection.tick()
        self.model_tests.start.assert_not_awaited()
        self.clock.advance(1)
        await self.detection.tick()
        self.model_tests.start.assert_awaited_once()

    async def test_closing_queued_or_running_job_cancels_it_without_a_new_request(self):
        for active_status in ("queued", "running"):
            with self.subTest(active_status=active_status):
                self.row.update(type="apikey", status="active", schedulable=True)
                self.configure_scheduled_state(marked=False)
                self.model_tests.start.reset_mock()
                self.model_tests.cancel.reset_mock()
                self.model_tests.cancel.side_effect = lambda job_id: self.model_tests.set_job(job_id, status="cancelled")

                await self.detection.tick()
                self.model_tests.start.assert_awaited_once()
                job_id = self.detection.control(1)["job_id"]
                self.model_tests.set_job(job_id, status=active_status)
                self.row["schedulable"] = False

                await self.detection.tick()
                self.model_tests.cancel.assert_awaited_once_with(job_id)
                self.model_tests.start.assert_awaited_once()
                self.model_tests.cancel.reset_mock()
                await self.detection.tick()
                state = self.detection.control(1)
                self.assertEqual((state["status"], state["next_at"], state["reason"]),
                                 ("paused", None, "调度已关闭"))

                self.model_tests.start.assert_awaited_once()
                self.model_tests.cancel.assert_not_awaited()

                self.detection.update(1, job_id=None, status="waiting", next_at=self.clock().isoformat())

    async def test_reopening_marked_account_still_waits_for_shared_cooldown(self):
        self.enable_due(interval_minutes=1)
        with self.detection.store.transaction() as data:
            data["automatic_starts"] = {
                "1": {"job_id": "fixture-previous", "started_at": self.clock().isoformat()}
            }
        self.row["schedulable"] = False
        await self.detection.tick()
        self.clock.advance(120)
        self.row["schedulable"] = True
        await self.detection.tick()
        self.assertEqual(self.detection.control(1)["next_at"],
                         (self.clock() + timedelta(seconds=180)).isoformat())
        self.model_tests.start.assert_not_awaited()
        self.clock.advance(179)
        await self.detection.tick()
        self.model_tests.start.assert_not_awaited()
        self.clock.advance(1)
        await self.detection.tick()
        self.model_tests.start.assert_awaited_once()
        self.assertTrue(self.model_tests.started[0][2]["degradation_recheck"])

    async def test_legacy_recheck_admitted_while_closed_cannot_clear_the_mark(self):
        self.row["schedulable"] = False
        job = self.recheck_job()
        self.detection.update(1, job_id=job["id"], status="completed", next_at=None)
        await self.detection.tick()
        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertIsNone(self.detection.control(1).get("recheck_recovery"))
        self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})
        self.model_tests.start.assert_not_awaited()

    async def test_closed_target_cannot_apply_a_late_recheck_result(self):
        job = self.recheck_job()
        self.detection.update(1, job_id=job["id"], status="completed", next_at=None)
        self.row["schedulable"] = False

        with patch("app.key_fallback.execute_sub2api_set_schedulable") as schedule_write:
            await self.detection.tick()

        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertIsNone(self.detection.control(1).get("recheck_recovery"))
        self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})
        self.assertEqual(self.detection.control(1)["reason"], "调度已关闭")
        schedule_write.assert_not_called()


if __name__ == "__main__":
    unittest.main()
