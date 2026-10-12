from __future__ import annotations

import copy
import json
import threading
import unittest
from datetime import timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

from app.bark import BarkNotifier

from tests.test_auto_reset import ACCOUNT_ID, NOW
from tests.test_auto_reset037 import AutoReset037Fixture


class BarkFailureServer:
    """Loopback Bark endpoint that records payloads and always fails."""

    def __init__(self, fixture):
        self.requests: list[tuple[str, dict[str, str], dict]] = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                size = int(self.headers.get("Content-Length", "0"))
                raw = self.rfile.read(size)
                owner.requests.append((self.path, dict(self.headers), json.loads(raw)))
                body = b'{"code":500}'
                self.send_response(500)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        fixture.allowed_ports.add(self.server.server_port)
        self.thread = threading.Thread(
            target=lambda: self.server.serve_forever(poll_interval=0.01), daemon=True
        )
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(2)


class AutoReset037BoundaryFixture(AutoReset037Fixture):
    def fresh_real_http_case(self):
        self.fresh_case()
        self.server.requests.clear()
        self.server.quota_reads = 0
        self.base_url = self.server.base_url
        self.make_controller(real_http=True)

    def evidence_age(self, seconds: int) -> None:
        self.db.raw.execute("DELETE FROM ops_error_logs")
        limited_at = NOW - timedelta(seconds=seconds)
        self.row["rate_limited_at"] = limited_at.isoformat()
        self.db.event(record_id=37, at=limited_at)

    def prepare_waiting_task(self) -> None:
        self.move_evidence_inside_window()
        self.preload_quota()
        self.monitor._active_tests = self.settings.oauth_recovery_test_concurrency
        self.run_once()
        self.monitor._active_tests = 0
        self.assertEqual(self.state()["stage"], "waiting")
        self.assertFalse(self.state().get("attempt_at"))

    def finish_unknown_episode(self) -> None:
        self.move_evidence_inside_window()
        self.preload_quota()
        self.request_results.append({"data": {"code": "already_redeemed", "windows_reset": 0}})
        self.run_once()
        self.assertEqual(self.state()["stage"], "finished")
        self.assertEqual(self.state()["outcome"], "unknown")


class AutoReset037AdmissionBoundaryTests(AutoReset037BoundaryFixture):
    def test_trigger_at_299_seconds_dispatches_but_exact_300_seconds_does_not(self):
        for age, expected_reset, expected_reason in (
            (299, 1, None),
            (300, 0, "consume_deadline"),
        ):
            with self.subTest(age=age):
                self.fresh_real_http_case()
                self.evidence_age(age)
                self.preload_quota()

                self.run_once()

                self.assertEqual(self.calls.count("reset"), expected_reset)
                if expected_reason:
                    self.assertEqual(self.state()["stage"], "finished")
                    self.assertEqual(self.state()["result_reason"], expected_reason)
                else:
                    self.assertTrue(self.state().get("attempt_at"))
                    self.assertEqual(self.state()["stage"], "recovered")

    def test_policy_clock_never_moves_deadline_forward_after_clock_rollback(self):
        self.prepare_waiting_task()
        checked_at = self.state()["policy_checked_at"]
        deadline = self.state()["consume_deadline_at"]

        self.now = NOW - timedelta(minutes=10)
        self.run_once()

        rolled_back = self.state()
        self.assertEqual(rolled_back["policy_checked_at"], checked_at)
        self.assertEqual(rolled_back["consume_deadline_at"], deadline)
        self.assertEqual(rolled_back["stage"], "finished")
        self.assertEqual(rolled_back["result_reason"], "clock_untrusted")
        self.assertEqual(self.calls, [])

        self.now = NOW + timedelta(seconds=2)
        self.run_once()
        self.assertEqual(self.calls, [])
        self.assertEqual(self.state()["result_reason"], "clock_untrusted")


class AutoReset037TerminationTests(AutoReset037BoundaryFixture):
    def test_missing_card_finishes_without_pause_or_upstream_request(self):
        self.move_evidence_inside_window()
        self.preload_quota()
        self.row["extra"]["codex_reset_credit_snapshot"] = {
            "available_count": 0,
            "fetched_at": (NOW - timedelta(minutes=2)).isoformat(),
            "credits": [],
        }

        self.run_once()

        self.assertEqual(self.calls, [])
        self.assertEqual(self.server.requests, [])
        self.assertEqual(self.state()["stage"], "finished")
        self.assertEqual(self.state()["result_reason"], "no_credit")
        self.assertTrue(self.row["schedulable"])

    def test_manual_stop_disabled_account_and_deleted_account_finish_waiting_episode(self):
        cases = (
            ("manual", lambda: self.controller.cancel(ACCOUNT_ID), "manual_intervention"),
            ("disabled", lambda: self.row.update(status="disabled"), "account_disabled"),
            ("deleted", lambda: self.row.update(deleted_at=NOW.isoformat()), "account_deleted"),
        )
        for name, change, reason in cases:
            with self.subTest(name=name):
                self.fresh_real_http_case()
                self.prepare_waiting_task()
                change()
                self.run_once()

                self.assertEqual(self.calls, [])
                self.assertEqual(self.server.requests, [])
                self.assertEqual(self.state()["stage"], "finished")
                self.assertEqual(self.state()["result_reason"], reason)

    def test_save_failure_before_admission_never_dispatches_business_request(self):
        self.move_evidence_inside_window()
        self.preload_quota()

        with patch.object(self.store, "_write", side_effect=OSError("fixture state write failed")):
            self.run_once()

        self.assertEqual(self.calls, [])
        self.assertEqual(self.server.requests, [])
        self.assertEqual(self.state(), {})
        self.assertTrue(self.row["schedulable"])


class AutoReset037BudgetTests(AutoReset037BoundaryFixture):
    def test_supplement_plus_unique_consumption_uses_exactly_six_query_budget_slots(self):
        self.move_evidence_inside_window()
        self.preload_quota(observed_age=timedelta(hours=2))
        self.row["extra"]["codex_reset_credit_snapshot"] = {}
        attempts = [(NOW - timedelta(hours=hour)).isoformat() for hour in (1, 2, 3, 4)]
        self.store.update_scheduler({ACCOUNT_ID: {
            "quota_query": {
                "automatic_attempts": attempts,
                "last_query_at": (NOW - timedelta(hours=2)).isoformat(),
            }
        }})

        self.run_once()

        self.assertEqual(self.calls.count("query"), 1)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(len(self.query_state()["automatic_attempts"]), 6)
        self.assertEqual(self.server.quota_reads, 2)

    def test_second_supplement_is_budgeted_out_without_any_upstream_request(self):
        self.move_evidence_inside_window()
        self.preload_quota(observed_age=timedelta(hours=2))
        self.row["extra"]["codex_reset_credit_snapshot"] = {}
        attempts = [(NOW - timedelta(hours=hour)).isoformat() for hour in (1, 2, 3, 4, 5)]
        self.store.update_scheduler({ACCOUNT_ID: {
            "quota_query": {
                "automatic_attempts": attempts,
                "last_query_at": (NOW - timedelta(hours=2)).isoformat(),
            }
        }})

        self.run_once()

        self.assertEqual(self.calls, [])
        self.assertEqual(self.server.requests, [])
        self.assertEqual(self.state()["stage"], "finished")
        self.assertEqual(self.state()["result_reason"], "query_budget")
        self.assertEqual(self.query_state()["automatic_attempts"], attempts)


class AutoReset037RuntimeSafetyTests(AutoReset037BoundaryFixture):
    def test_detection_control_version_change_invalidates_waiting_episode(self):
        version = ["0" * 64]
        self.monitor.credit_control_version = lambda _aid: version[0]
        self.prepare_waiting_task()
        version[0] = "1" * 64

        self.run_once()

        self.assertEqual(self.calls, [])
        self.assertEqual(self.server.requests, [])
        self.assertEqual(self.state()["stage"], "finished")
        self.assertEqual(self.state()["result_reason"], "manual_intervention")

    def test_unreliable_detection_version_callback_keeps_episode_waiting_without_dispatch(self):
        def unreliable(_aid):
            raise OSError("fixture detection state unavailable")

        self.monitor.credit_control_version = unreliable
        self.move_evidence_inside_window()
        self.preload_quota()

        self.run_once()

        self.assertEqual(self.calls, [])
        self.assertEqual(self.server.requests, [])
        self.assertEqual(self.state()["stage"], "waiting")
        self.assertFalse(self.state().get("policy_version"))

    def test_late_receipt_after_watchdog_stop_keeps_consumption_and_never_replays_reset(self):
        self.evidence_age(299)
        self.preload_quota()

        def finish_from_watchdog(action):
            if action == "reset":
                self.now = NOW + timedelta(seconds=2)
                self.store.manual_control(self.row, False, self.now)
                self.controller.sweep([copy.deepcopy(self.row)], self.now)

        self.request_hook = finish_from_watchdog
        self.run_once()

        finished = self.state()
        self.assertEqual(finished["stage"], "finished")
        self.assertTrue(finished["consumed"])
        self.assertTrue(finished["receipt"])
        self.assertEqual(finished["result_reason"], "manual_intervention")
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 0)

        self.make_controller(real_http=True)
        self.now = NOW + timedelta(minutes=6)
        self.run_once()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 0)

    def test_manual_control_releases_finished_pause_without_changing_receipt(self):
        self.prepare_waiting_task()
        task = self.state()
        task = self.controller._save(ACCOUNT_ID, task, owns_pause=True)
        finished = self.controller._finish(ACCOUNT_ID, task, "consume_deadline", NOW, self.row)
        before = copy.deepcopy(finished)

        self.store.manual_control(self.row, False, NOW + timedelta(seconds=1))

        after = self.state()
        self.assertEqual(after["stage"], "finished")
        self.assertFalse(after["owns_pause"])
        for key in ("episode", "trigger_at", "consume_deadline_at", "outcome", "result_reason", "completed_at"):
            self.assertEqual(after.get(key), before.get(key))
        self.assertEqual(self.calls, [])

    def test_degradation_disposition_finishes_with_notice_in_one_persisted_state(self):
        self.prepare_waiting_task()
        task = self.controller._save(ACCOUNT_ID, self.state(), model_test_job_id="fixture-job-037")

        merged = self.controller.merge_disposition(
            ACCOUNT_ID,
            "fixture-job-037",
            {
                "marked": True,
                "schedule_verified": True,
                "model_id": "fixture-model",
                "prediction": "gpt-5.6-luna",
                "probability": 0.97,
            },
            self.row,
        )

        self.assertTrue(merged)
        self.assertEqual(self.state()["stage"], "finished")
        self.assertEqual(self.state()["result_reason"], "detection_held")
        notice = next(iter(self.credit_notifications().values()))
        self.assertTrue(notice["degradation_confirmed"])
        self.assertEqual(notice["model_id"], "fixture-model")
        self.assertEqual(notice["prediction"], "gpt-5.6-luna")
        self.assertEqual(notice["probability"], 0.97)

    def test_clear_request_is_persisted_once_and_unknown_write_is_only_reconciled(self):
        self.move_evidence_inside_window()
        self.preload_quota()
        calls = []

        def unknown_clear(*_args, **_kwargs):
            calls.append(1)
            raise OSError("fixture clear result unknown")

        with patch.object(self.monitor, "recovery_runner", side_effect=unknown_clear):
            self.run_once()

        self.assertEqual(calls, [1])
        first = self.state()
        self.assertTrue(first.get("clear_requested_at"))
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)

        self.now = NOW + timedelta(seconds=2)
        self.run_once()

        self.assertEqual(calls, [1])
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("recover"), 0)


class AutoReset037BarkTests(AutoReset037BoundaryFixture):
    def setUp(self):
        super().setUp()
        self.bark_server = BarkFailureServer(self)
        self.addCleanup(self.bark_server.close)

    def test_real_loopback_bark_retries_five_times_with_parameters_without_replaying_card(self):
        self.finish_unknown_episode()
        self.settings.bark_enabled = True
        self.settings.bark_config_valid = True
        self.settings.bark_device_key = "fixture-device-key"
        self.settings.bark_server_url = self.bark_server.base_url
        notifier = BarkNotifier(self.settings)

        delays = (5, 30, 120, 600, 600)
        for attempt in range(5):
            self.controller.deliver_results(notifier)
            if attempt < 4:
                self.now += timedelta(seconds=delays[attempt])
        self.controller.deliver_results(notifier)

        notice = next(iter(self.credit_notifications().values()))
        self.assertEqual(notice["status"], "failed")
        self.assertEqual(notice["attempts"], 5)
        self.assertEqual(len(self.bark_server.requests), 5)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.server.quota_reads, 1)
        for path, headers, payload in self.bark_server.requests:
            self.assertEqual(path, "/push")
            self.assertEqual(headers.get("Content-Type"), "application/json")
            self.assertEqual(payload["device_key"], "fixture-device-key")
            self.assertEqual(payload["level"], "critical")
            self.assertEqual(payload["sound"], "alarm")
            self.assertEqual(payload["group"], "oauth-reset-credit")


if __name__ == "__main__":
    unittest.main()
