from __future__ import annotations

import copy
import json
import unittest
from datetime import timedelta
from types import SimpleNamespace

from tests.test_auto_reset import (
    ACCOUNT_ID,
    ADMIN_TOKEN,
    AutoResetFixture,
    LocalCreditServer,
    NOW,
    quota_result,
    wham,
)


class CreditResultNotifier:
    """Small Bark boundary double; it never performs network I/O."""

    def __init__(self, *, enabled: bool = True, config_valid: bool = True, outcomes=()):
        self.enabled = enabled
        self.config_valid = config_valid
        self.outcomes = list(outcomes)
        self.events: list[dict] = []

    def runtime_config(self):
        return SimpleNamespace(enabled=self.enabled, config_valid=self.config_valid)

    def notify_credit_result(self, event, *, config):
        self.events.append(copy.deepcopy(event))
        return bool(self.outcomes.pop(0)) if self.outcomes else True


class AutoReset037Fixture(AutoResetFixture):
    """Use the real controller entry point with a loopback-only HTTP service."""

    def setUp(self):
        super().setUp()
        self.server = LocalCreditServer(self)
        self.addCleanup(self.server.close)
        self.base_url = self.server.base_url
        self.make_controller(real_http=True)

    def move_evidence_inside_window(self):
        self.db.raw.execute("DELETE FROM ops_error_logs")
        self.db.event(record_id=11, at=NOW - timedelta(minutes=1))

    def preload_quota(self, *, observed_age=timedelta(minutes=2)):
        observed = NOW - observed_age
        self.store.commit(results={
            ACCOUNT_ID: quota_result(
                wham(NOW, five=20, seven=100, observed=observed,
                     seven_reset_at=self.reset_seven_reset_at),
                self.row,
                observed,
            )
        })

    def credit_notifications(self):
        scheduler = self.store.snapshot()["scheduler"]
        return scheduler.get(str(ACCOUNT_ID), {}).get("credit_result_notifications", {})


class AutoReset037BoundedWindowTests(AutoReset037Fixture):
    def test_original_evidence_starts_fixed_five_minute_consumption_window(self):
        self.move_evidence_inside_window()
        self.preload_quota()
        # The upstream receipt is intentionally ambiguous. The controller must
        # persist an attempt and close the episode instead of retrying a card.
        self.request_results.append({"data": {"code": "already_redeemed", "windows_reset": 0}})

        self.run_once()

        first = self.state()
        self.assertEqual(first["policy_version"], 1)
        trigger = first["trigger_at"]
        deadline = first["consume_deadline_at"]
        self.assertEqual(deadline, (NOW - timedelta(minutes=1) + timedelta(minutes=5)).isoformat())
        self.assertEqual(first["stage"], "finished")
        self.assertEqual(first["outcome"], "unknown")
        self.assertEqual(first["result_reason"], "already_redeemed")
        self.assertEqual(self.calls.count("reset"), 1)

        # A restart and a duplicate 429 observation retain the original
        # trigger. They cannot reopen a consumed or uncertain episode.
        self.make_controller(real_http=True)
        self.now = NOW + timedelta(minutes=2)
        self.db.event(record_id=12, at=NOW + timedelta(minutes=1))
        self.run_once()
        second = self.state()
        self.assertEqual(second["episode"], first["episode"])
        self.assertEqual(second["trigger_at"], trigger)
        self.assertEqual(second["consume_deadline_at"], deadline)
        self.assertEqual(self.calls.count("reset"), 1)

        self.now = NOW + timedelta(minutes=6)
        self.run_once()
        self.assertEqual(self.state()["consume_deadline_at"], deadline)
        self.assertEqual(self.calls.count("reset"), 1)

    def test_card_supplement_is_one_request_and_unique_consumption_is_budgeted(self):
        self.move_evidence_inside_window()
        self.row["extra"]["codex_reset_credit_snapshot"] = {}
        # A stale saved quota permits one explicit supplement before the
        # bounded consumption request. The supplement itself still consumes
        # the normal one-hour query interval.
        self.preload_quota(observed_age=timedelta(hours=2))
        self.row["extra"]["codex_reset_credit_snapshot"] = {}

        self.run_once()

        # The missing card snapshot is repaired once, then the same run may
        # consume it. The consume read bypasses only the hourly interval.
        self.assertEqual(self.calls.count("query"), 1)
        self.assertEqual(self.calls.count("reset"), 1)
        query_state = self.query_state()
        self.assertEqual(len(query_state["automatic_attempts"]), 2)
        self.assertEqual(self.server.quota_reads, 2)

        # A second supplement is refused even after a restart and cannot emit
        # another upstream read.
        self.row["extra"]["codex_reset_credit_snapshot"] = {}
        self.make_controller(real_http=True)
        self.now = NOW + timedelta(minutes=2)
        self.run_once()
        self.assertEqual(self.calls.count("query"), 1)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.server.quota_reads, 2)

    def test_recent_quota_blocks_card_supplement_without_consumption(self):
        self.move_evidence_inside_window()
        self.preload_quota(observed_age=timedelta(minutes=2))
        self.row["extra"]["codex_reset_credit_snapshot"] = {}

        self.run_once()

        self.assertEqual(self.calls, [])
        self.assertEqual(self.server.quota_reads, 0)
        self.assertEqual(self.state()["stage"], "finished")
        self.assertEqual(self.state()["result_reason"], "query_cooldown")

    def test_failed_verification_retries_after_sixty_seconds_and_stops_at_two_rounds(self):
        self.move_evidence_inside_window()
        self.preload_quota()
        self.test_results.extend([
            {"success": False, "error_code": "http_502"},
            {"success": False, "error_code": "http_502"},
        ])

        self.run_once()
        first = self.state()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)
        self.assertEqual(first["verification_deadline_at"],
                         (NOW + timedelta(minutes=5)).isoformat())
        retry_at = first["next_at"]
        self.assertGreaterEqual(retry_at, (NOW + timedelta(seconds=60)).isoformat())

        self.now = NOW + timedelta(seconds=59)
        self.run_once()
        self.assertEqual(self.calls.count("test"), 1)
        self.now = NOW + timedelta(seconds=60)
        self.run_once()
        self.assertEqual(self.calls.count("test"), 2)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.state()["stage"], "finished")
        self.assertEqual(self.state()["result_reason"], "verification_limit")


class AutoReset037NotificationTests(AutoReset037Fixture):
    def _finish_unknown_episode(self):
        self.move_evidence_inside_window()
        self.preload_quota()
        self.request_results.append({"data": {"code": "already_redeemed", "windows_reset": 0}})
        self.run_once()
        self.assertEqual(self.state()["stage"], "finished")
        self.assertEqual(self.state()["outcome"], "unknown")
        self.assertEqual(len(self.credit_notifications()), 1)

    def test_notification_retry_is_atomic_bounded_and_never_replays_reset(self):
        self._finish_unknown_episode()
        notifier = CreditResultNotifier(outcomes=[False, False, False, False, False])

        for attempt in range(5):
            self.controller.deliver_results(notifier)
            if attempt < 4:
                self.now += timedelta(seconds=(5, 30, 120, 600, 600)[attempt])

        notice = next(iter(self.credit_notifications().values()))
        self.assertEqual(notice["status"], "failed")
        self.assertEqual(notice["attempts"], 5)
        self.assertEqual(len(notifier.events), 5)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.server.quota_reads, 1)

        # A later monitor cycle only delivers the terminal notice; it cannot
        # repeat the business operation.
        self.now += timedelta(minutes=30)
        self.run_once()
        self.controller.deliver_results(notifier)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(notice["status"], "failed")

    def test_disabled_bark_suppresses_notice_without_business_replay(self):
        self._finish_unknown_episode()
        notifier = CreditResultNotifier(enabled=False)
        self.controller.deliver_results(notifier)
        notice = next(iter(self.credit_notifications().values()))
        self.assertEqual(notice["status"], "suppressed")
        self.assertEqual(notice["attempts"], 0)
        self.assertEqual(notifier.events, [])
        self.assertEqual(self.calls.count("reset"), 1)


if __name__ == "__main__":
    unittest.main()
