from __future__ import annotations

import json
import tempfile
import threading
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

from app.daily_test import DailyTestSchedule, daily_account_eligible, next_daily_time
from app.oauth_monitor import OAuthMonitor, OAuthStateStore
from app.settings import daily_test_time
from test_oauth_monitor import FakeDb, account, result, settings, summary

BEFORE = datetime(2026, 9, 21, 20, 59, 50, tzinfo=timezone.utc)
DUE = BEFORE + timedelta(seconds=10)


def healthy(account_id=1):
    return {**account(account_id), "rate_limited_at": None, "rate_limit_reset_at": None}


class DailyScheduleTests(unittest.TestCase):
    def test_next_future_time_and_beijing_cross_day(self):
        self.assertEqual(next_daily_time(BEFORE, "05:00"), DUE)
        self.assertEqual(next_daily_time(DUE, "05:00"), DUE + timedelta(days=1))
        self.assertEqual(next_daily_time(DUE, "00:00"), datetime(2026, 9, 22, 16, tzinfo=timezone.utc))
        for invalid in ("5:00", "24:00", "00:60", "12:30:00", "", "abc"):
            with self.subTest(value=invalid), self.assertRaises(ValueError):
                daily_test_time(invalid)

    def make_schedule(self, root):
        config = settings(root / "state.json")
        config.telegram_oauth_daily_test_enabled = True
        config.telegram_oauth_daily_test_time = "05:00"
        store = OAuthStateStore(config.usage_query_state_path)
        return DailyTestSchedule(config, store, BEFORE), config, store

    def test_default_custom_time_enable_hot_update_and_dedup(self):
        with tempfile.TemporaryDirectory() as directory:
            schedule, config, store = self.make_schedule(Path(directory))
            self.assertEqual(store.snapshot()["daily_test"]["next_run_at"], DUE.isoformat())
            schedule.tick(DUE)
            batch = schedule.claim(DUE)
            self.assertEqual(batch["date"], "2026-09-22")
            batch["status"] = "completed"
            schedule.save_batch(batch)
            config.telegram_oauth_daily_test_time = "06:00"
            schedule.tick(DUE + timedelta(minutes=1))
            schedule.tick(DUE + timedelta(hours=1))
            self.assertIsNone(schedule.claim(DUE + timedelta(hours=1)))
            config.telegram_oauth_daily_test_enabled = False
            schedule.tick(DUE + timedelta(hours=2))
            self.assertEqual(store.snapshot()["daily_test"]["next_run_at"], "")
            config.telegram_oauth_daily_test_enabled = True
            schedule.tick(DUE + timedelta(days=1, hours=2))
            self.assertEqual(store.snapshot()["daily_test"]["next_run_at"], (DUE + timedelta(days=2, hours=1)).isoformat())

    def test_missed_restart_and_interrupted_batches_do_not_catch_up(self):
        with tempfile.TemporaryDirectory() as directory:
            schedule, config, store = self.make_schedule(Path(directory))
            schedule.tick(DUE + timedelta(minutes=2))
            self.assertIsNone(schedule.claim(DUE + timedelta(minutes=2)))
            self.assertEqual(store.snapshot()["daily_test"]["batches"]["2026-09-22"]["status"], "missed")
            # A newly selected future time is allowed when the prior time was never executed.
            config.telegram_oauth_daily_test_time = "06:00"
            schedule.tick(DUE + timedelta(minutes=3))
            schedule.tick(DUE + timedelta(hours=1))
            self.assertIsNotNone(schedule.claim(DUE + timedelta(hours=1)))
            restarted = DailyTestSchedule(config, store, DUE + timedelta(hours=1, minutes=1))
            self.assertEqual(store.snapshot()["daily_test"]["batches"]["2026-09-22"]["status"], "interrupted")
            self.assertIsNone(restarted.claim(DUE + timedelta(hours=1, minutes=2)))
            self.assertEqual(store.snapshot()["daily_test"]["next_run_at"], (DUE + timedelta(days=1, hours=1)).isoformat())

    def test_legacy_deferred_intent_migrates_without_touching_pending(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            path.write_text(json.dumps({"scheduler": {"1": {"recovery_intent": {
                "status": "deferred", "deferred_until": "2099-01-01T00:00:00Z", "fingerprint": "quota"}}},
                "pending_events": {"existing": {"account_id": 1}}, "daily_test": {"batches": {}}}))
            store = OAuthStateStore(str(path))
            store.migrate()
            self.assertEqual(store.scheduler()[1]["recovery_intent"]["status"], "ready")
            self.assertEqual(store.scheduler()[1]["recovery_intent"]["deferred_until"], "")
            self.assertEqual(len(store.pending_events()), 1)
            self.assertFalse(store.migrate())
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_all_scheduling_gates_and_expired_time_fields(self):
        for changes in (
            {"status": "paused"}, {"schedulable": False}, {"platform": "grok"},
            {"type": "apikey"}, {"deleted_at": DUE.isoformat()},
            {"temp_unschedulable_until": (DUE + timedelta(minutes=1)).isoformat()},
            {"rate_limit_reset_at": (DUE + timedelta(minutes=1)).isoformat()},
            {"overload_until": (DUE + timedelta(minutes=1)).isoformat()},
            {"temp_unschedulable_reason": "telegram pause by user"},
            {"expires_at": BEFORE.isoformat(), "auto_pause_on_expired": True},
            {"overload_until": "invalid"},
        ):
            with self.subTest(changes=changes):
                self.assertFalse(daily_account_eligible({**healthy(), **changes}, DUE))
        self.assertTrue(daily_account_eligible({**healthy(), "rate_limit_reset_at": BEFORE.isoformat(),
                                               "overload_until": BEFORE.isoformat()}, DUE))


class DailyExecutionTests(unittest.TestCase):
    def make_monitor(self, root, rows=None, usage=None, test=None):
        config = settings(root / "state.json")
        config.telegram_oauth_daily_test_enabled = True
        config.telegram_oauth_daily_test_time = "05:00"
        config.telegram_oauth_recovery_monitor_enabled = False
        db = FakeDb(rows or [healthy()])
        calls = []

        def runner(account_id, model, **kwargs):
            calls.append((account_id, model, kwargs["timeout_seconds"]))
            return test(account_id) if test else {"success": True}

        monitor = OAuthMonitor(config, db, base_url_provider=lambda: "http://127.0.0.1",
                               usage_runner=usage or (lambda *_a, **_kw: result(summary(), DUE)),
                               test_runner=runner, clock=lambda: BEFORE)
        monitor.store.save_admin_token("admin-test")
        return monitor, calls

    def test_success_records_without_notification_or_recovery_and_no_repeat(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.make_monitor(Path(directory))
            with patch.object(monitor, "recovery_runner") as recover:
                monitor.run_once(DUE)
                monitor.run_once(DUE + timedelta(seconds=3))
                recover.assert_not_called()
            self.assertEqual(calls, [(1, "gpt-5.6-luna", 30)])
            batch = monitor.store.snapshot()["daily_test"]["batches"]["2026-09-22"]
            self.assertEqual(batch["status"], "completed")
            self.assertEqual(batch["accounts"]["1"]["status"], "success")
            self.assertEqual(monitor.store.pending_events(), [])

    def test_busy_lock_registers_and_later_runs_even_after_due_minute(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.make_monitor(Path(directory))
            with monitor._run_lock:
                monitor.run_once(DUE)
                self.assertEqual(monitor.store.snapshot()["daily_test"]["batches"]["2026-09-22"]["status"], "queued")
            monitor.run_once(DUE + timedelta(minutes=3))
            self.assertEqual(len(calls), 1)

    def test_pause_depleted_missing_window_and_type_change(self):
        rows = [healthy(i) for i in range(1, 5)]
        rows[0]["schedulable"] = False

        def usage(account_id, *_args, **_kwargs):
            if account_id == 2:
                return result(summary(seven_used=100), DUE)
            if account_id == 3:
                return result({"plan_type": "plus", "ui_windows": []}, DUE)
            if account_id == 4:
                rows[3]["type"] = "apikey"
            return result(summary(), DUE)

        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.make_monitor(Path(directory), rows=rows, usage=usage)
            monitor.run_once(DUE)
            records = monitor.store.snapshot()["daily_test"]["batches"]["2026-09-22"]["accounts"]
            self.assertEqual([records[str(i)]["status"] for i in range(1, 5)], ["skipped", "skipped", "failed", "skipped"])
            self.assertFalse(calls)
            self.assertEqual(monitor.store.pending_events()[0]["account_id"], 3)

    def test_failure_uses_reliable_queue_sanitizes_and_never_retests(self):
        for code, expected in (("http_401", "auth_failed"), ("account_test_error", "daily_test_failed")):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                monitor, calls = self.make_monitor(Path(directory), test=lambda _id: {
                    "success": False, "error_code": code, "error": "Authorization: Bearer secret-leak"})
                monitor.run_once(DUE)
                first = monitor.store.pending_events()
                monitor.run_once(DUE + timedelta(minutes=1))
                self.assertEqual(first, monitor.store.pending_events())
                self.assertEqual(len(calls), 1)
                self.assertEqual(first[0]["status"], expected)
                self.assertNotIn("secret-leak", json.dumps(first))
                monitor.store.mark_events_delivered(first)
                self.assertFalse(monitor.store.pending_events())

    def test_quota_failure_has_no_model_request(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.make_monitor(Path(directory), usage=lambda *_a, **_k: {
                "success": False, "error_code": "http_500", "error": "upstream"})
            monitor.run_once(DUE)
            self.assertFalse(calls)
            self.assertEqual(monitor.store.pending_events()[0]["status"], "daily_test_failed")

    def test_recovery_test_in_same_cycle_is_reused(self):
        row = healthy()
        row.update(rate_limited_at=BEFORE.isoformat(), rate_limit_reset_at=DUE.isoformat())
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.make_monitor(Path(directory), rows=[row])
            monitor.settings.telegram_oauth_recovery_monitor_enabled = True
            monitor.store.commit(results={1: result(summary(five_used=100, five_reset=DUE), BEFORE)})
            def recover(*_a, **_kw):
                row.update(rate_limited_at=None, rate_limit_reset_at=None)
                return {"success": True}
            monitor.recovery_runner = recover
            monitor.run_once(DUE)
            self.assertEqual(len(calls), 1)
            self.assertTrue(monitor.store.snapshot()["daily_test"]["batches"]["2026-09-22"]["accounts"]["1"]["reused_recovery_test"])

    def test_recovery_test_is_not_reused_after_account_becomes_unschedulable(self):
        row = healthy()
        row.update(rate_limited_at=BEFORE.isoformat(), rate_limit_reset_at=DUE.isoformat())
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.make_monitor(Path(directory), rows=[row])
            monitor.settings.telegram_oauth_recovery_monitor_enabled = True
            monitor.store.commit(results={1: result(summary(five_used=100, five_reset=DUE), BEFORE)})

            def recover(*_args, **_kwargs):
                row.update(rate_limited_at=None, rate_limit_reset_at=None, schedulable=False)
                return {"success": True}

            monitor.recovery_runner = recover
            monitor.run_once(DUE)
            record = monitor.store.snapshot()["daily_test"]["batches"]["2026-09-22"]["accounts"]["1"]
            self.assertEqual(len(calls), 1)
            self.assertEqual(record["status"], "skipped")
            self.assertFalse(record["reused_recovery_test"])

    def test_interruption_after_test_does_not_retry_on_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.make_monitor(Path(directory))
            monitor.daily_schedule.tick(DUE)
            monitor.daily_schedule.claim(DUE)
            monitor.daily_schedule = DailyTestSchedule(monitor.settings, monitor.store, DUE + timedelta(seconds=2))
            monitor.run_once(DUE + timedelta(seconds=4))
            self.assertFalse(calls)
