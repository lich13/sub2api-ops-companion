from __future__ import annotations

import copy
import unittest
from datetime import timedelta

from app.auto_reset import project_state
from app.oauth_queries import credential_fingerprint
from app.reset_credit_observation import observation_from_usage
from tests.test_auto_reset import ACCOUNT_ID, NOW, native_credit_snapshot, quota_result
from tests.test_auto_reset037 import AutoReset037Fixture, CreditResultNotifier


class AutoReset038Fixture(AutoReset037Fixture):
    """Real loopback fixture for the 0.1.38 credit admission boundary."""

    def fresh_real_http_case(self) -> None:
        self.fresh_case()
        self.server.requests.clear()
        self.server.quota_reads = 0
        self.base_url = self.server.base_url
        self.make_controller(real_http=True)

    def scheduler_meta(self) -> dict:
        return self.store.snapshot()["scheduler"].get(str(ACCOUNT_ID), {})

    def set_snapshot(self, snapshot: dict) -> None:
        self.row["extra"]["codex_reset_credit_snapshot"] = copy.deepcopy(snapshot)

    @staticmethod
    def usage_payload(snapshot: dict | None, *, fetched_at=None) -> dict:
        payload = {
            "five_hour": {
                "utilization": 20,
                "resets_at": (NOW + timedelta(hours=5)).isoformat(),
            },
            "seven_day": {
                "utilization": 100,
                "resets_at": (NOW + timedelta(days=5)).isoformat(),
            },
            "fetched_at": int((fetched_at or NOW).timestamp()),
            "cache_persisted": True,
        }
        if snapshot is not None:
            payload["rate_limit_reset_credits"] = copy.deepcopy(snapshot)
        return payload

    def assert_no_credit_io(self, *, admission_reason: str) -> None:
        meta = self.scheduler_meta()
        self.assertNotIn("auto_reset_credit", meta)
        admission = meta.get("credit_admission") or {}
        self.assertEqual(admission.get("reason"), admission_reason)
        self.assertEqual(self.calls, [])
        self.assertEqual(self.server.requests, [])
        self.assertEqual(self.server.quota_reads, 0)
        self.assertEqual((meta.get("quota_query") or {}).get("automatic_attempts", []), [])
        self.assertEqual(self.store.pending_events(), [])


class AutoReset038AdmissionTests(AutoReset038Fixture):
    def test_zero_unknown_expired_and_contradictory_cards_skip_without_any_io(self):
        cases = (
            ("zero", native_credit_snapshot(NOW, count=0, expiries=[]), "no_credit"),
            ("unknown", {}, "credit_unconfirmed"),
            ("expired", native_credit_snapshot(NOW, count=1, expiries=[NOW]), "no_credit"),
            ("contradictory", {"available_count": 2, "credits": [{
                "expires_at": (NOW + timedelta(days=2)).isoformat(),
            }]}, "credit_unconfirmed"),
        )
        for name, snapshot, reason in cases:
            with self.subTest(name=name):
                self.fresh_real_http_case()
                self.move_evidence_inside_window()
                self.preload_quota()
                self.set_snapshot(snapshot)

                self.run_once()

                self.assert_no_credit_io(admission_reason=reason)
                notifier = CreditResultNotifier()
                self.controller.deliver_results(notifier)
                self.assertEqual(notifier.events, [])

    def test_card_arriving_before_deadline_reuses_original_depletion_trigger(self):
        self.fresh_real_http_case()
        self.move_evidence_inside_window()
        self.preload_quota()
        self.set_snapshot(native_credit_snapshot(NOW, count=0, expiries=[]))

        self.run_once()
        admission = copy.deepcopy(self.scheduler_meta()["credit_admission"])
        self.assertEqual(self.calls, [])
        self.assertEqual(admission["trigger_at"], (NOW - timedelta(minutes=1)).isoformat())

        # A duplicate rate-limit observation and a restart must retain the
        # original event time. A newly observed positive card is still usable
        # while that original five-minute window remains open.
        self.db.event(record_id=38, at=NOW)
        self.set_snapshot({
            "available_count": 1,
            "fetched_at": NOW.isoformat(),
            "credits": [{"expires_at": (NOW + timedelta(days=2)).isoformat()}],
        })
        self.now = NOW + timedelta(seconds=60)
        self.make_controller(real_http=True)
        self.run_once()

        task = self.state()
        self.assertEqual(task["trigger_at"], admission["trigger_at"])
        self.assertEqual(task["consume_deadline_at"],
                         (NOW + timedelta(minutes=4)).isoformat())
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("query"), 0)
        self.assertEqual(self.calls.count("test"), 1)
        # The reset endpoint itself carries one attached quota read; no
        # separate automatic supplement query is allowed.
        self.assertEqual(self.server.quota_reads, 1)

    def test_card_arriving_after_original_deadline_does_not_reopen_or_extend_window(self):
        self.fresh_real_http_case()
        self.db.raw.execute("DELETE FROM ops_error_logs")
        limited_at = NOW - timedelta(seconds=299)
        self.row["rate_limited_at"] = limited_at.isoformat()
        self.db.event(record_id=39, at=limited_at)
        self.preload_quota()
        self.set_snapshot(native_credit_snapshot(NOW, count=0, expiries=[]))

        self.run_once()
        admission = copy.deepcopy(self.scheduler_meta()["credit_admission"])
        self.assertEqual(admission["trigger_at"], limited_at.isoformat())
        self.assertEqual(admission["reason"], "no_credit")

        self.now = NOW + timedelta(seconds=2)
        self.set_snapshot({
            "available_count": 1,
            "fetched_at": NOW.isoformat(),
            "credits": [{"expires_at": (NOW + timedelta(days=2)).isoformat()}],
        })
        self.make_controller(real_http=True)
        self.run_once()

        self.assert_no_credit_io(admission_reason="consume_deadline")
        self.assertEqual(self.scheduler_meta()["credit_admission"]["trigger_at"],
                         limited_at.isoformat())

    def test_confirmed_consumption_with_zero_balance_still_runs_verification(self):
        self.fresh_real_http_case()
        self.move_evidence_inside_window()
        self.preload_quota()
        self.set_snapshot({
            "available_count": 1,
            "fetched_at": (NOW - timedelta(minutes=2)).isoformat(),
            "credits": [{"expires_at": (NOW + timedelta(days=2)).isoformat()}],
        })

        self.run_once()

        self.assertTrue(self.state()["consumed"])
        self.assertEqual(self.state()["stage"], "recovered")
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)
        self.assertEqual(self.row["extra"]["codex_reset_credit_snapshot"]["available_count"], 0)
        receipt = copy.deepcopy(self.state()["receipt"])
        reset_completed_at = self.state()["reset_completed_at"]

        self.now += timedelta(minutes=1)
        self.run_once()

        self.assertEqual(self.state()["receipt"], receipt)
        self.assertEqual(self.state()["reset_completed_at"], reset_completed_at)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)

    def test_legacy_unissued_zero_card_task_is_hidden_and_pending_notice_suppressed(self):
        self.fresh_real_http_case()
        self.move_evidence_inside_window()
        self.preload_quota()
        self.set_snapshot(native_credit_snapshot(NOW, count=0, expiries=[]))
        episode = "legacy-credit-038"
        legacy = {
            "episode": episode,
            "stage": "waiting",
            "evidence_at": (NOW - timedelta(minutes=1)).isoformat(),
            "evidence_source": "upstream_error",
            "evidence_id": "1",
            "evidence_fingerprint": "fixture-legacy-evidence",
            "reset_at": (NOW + timedelta(days=5)).isoformat(),
            "window_minutes": 10080,
        }
        notice = self.controller._notice(
            ACCOUNT_ID,
            {**legacy, "outcome": "not_executed", "result_reason": "no_credit",
             "error_code": "no_credit"},
            NOW,
            self.row,
        )
        self.store.commit(scheduler_updates={ACCOUNT_ID: {
            "auto_reset_credit": legacy,
            "credit_result_notifications": {episode: notice},
        }})

        self.run_once()

        task = self.state()
        self.assertEqual(task["stage"], "finished")
        self.assertTrue(task["credit_gate_skipped"])
        self.assertEqual(task["outcome"], "not_executed")
        self.assertEqual(task["result_reason"], "no_credit")
        self.assertEqual(self.scheduler_meta()["credit_result_notifications"][episode]["status"], "suppressed")
        self.assertEqual(self.calls, [])
        self.assertEqual(self.server.requests, [])
        self.assertEqual(self.store.pending_events(), [])
        notifier = CreditResultNotifier()
        self.controller.deliver_results(notifier)
        self.assertEqual(notifier.events, [])

    def test_skipped_task_with_unconfirmed_pause_remains_blocked_in_projection(self):
        projected = project_state({
            "episode": "legacy-credit-038-paused",
            "stage": "finished",
            "credit_gate_skipped": True,
            "owns_pause": True,
            "evidence_source": "upstream_error",
            "evidence_at": (NOW - timedelta(minutes=1)).isoformat(),
        })

        self.assertEqual(projected["stage"], "blocked")
        self.assertEqual(projected["label"], "调度暂停待核对")
        self.assertIsNone(projected["next_at"])


class AutoReset038ObservationTests(AutoReset038Fixture):
    def test_observation_from_usage_requires_fresh_explicit_matching_card_data(self):
        snapshot = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=2)])
        before = copy.deepcopy(self.row)
        after = copy.deepcopy(self.row)
        after["extra"]["codex_reset_credit_snapshot"] = copy.deepcopy(snapshot)
        result = {
            "success": True,
            "queried_at": NOW.isoformat(),
            "data": self.usage_payload(snapshot),
        }

        observation = observation_from_usage(before, after, result, NOW, NOW + timedelta(seconds=2))

        self.assertIsNotNone(observation)
        self.assertEqual(observation["available_count"], 1)
        self.assertEqual(observation["observed_at"], NOW.isoformat())
        self.assertEqual(observation["credential_fingerprint"], credential_fingerprint(after))

        no_card = copy.deepcopy(result)
        no_card["data"].pop("rate_limit_reset_credits")
        self.assertIsNone(observation_from_usage(
            before, after, no_card, NOW, NOW + timedelta(seconds=2)
        ))

        stale = copy.deepcopy(result)
        stale["data"]["fetched_at"] = int((NOW - timedelta(seconds=2)).timestamp())
        self.assertIsNone(observation_from_usage(
            before, after, stale, NOW, NOW + timedelta(seconds=2)
        ))

        changed_after = copy.deepcopy(after)
        changed_after["extra"]["codex_reset_credit_snapshot"]["credits"][0]["expires_at"] = (
            NOW + timedelta(days=3)
        ).isoformat()
        self.assertIsNone(observation_from_usage(
            before, changed_after, result, NOW, NOW + timedelta(seconds=2)
        ))

    def test_coordinator_persists_only_current_explicit_card_observation(self):
        snapshot = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=2)])
        self.set_snapshot({
            "available_count": 1,
            "fetched_at": (NOW - timedelta(hours=2)).isoformat(),
            "credits": [{"expires_at": (NOW + timedelta(days=2)).isoformat()}],
        })
        calls = []

        def runner(account_id, _base_url, _token, **_kwargs):
            calls.append(account_id)
            self.row["extra"]["codex_reset_credit_snapshot"] = copy.deepcopy(snapshot)
            return {
                "success": True,
                "queried_at": NOW.isoformat(),
                "data": self.usage_payload(snapshot),
            }

        self.monitor.usage_runner = runner
        result = self.monitor.queries.query(
            copy.deepcopy(self.row),
            "fixture-admin",
            source="manual",
            reason="fixture-current-card",
            now=NOW,
        )

        self.assertTrue(result["success"], result)
        self.assertEqual(calls, [ACCOUNT_ID])
        self.assertEqual(self.server.requests, [])
        observation = self.scheduler_meta().get("reset_credit_observation")
        self.assertIsNotNone(observation)
        self.assertEqual(observation["observed_at"], NOW.isoformat())
        self.assertEqual(observation["available_count"], 1)

        # A normal response without an explicit card field cannot retimestamp
        # or replace the previously observed card snapshot.
        self.fresh_case()
        self.base_url = self.server.base_url
        self.make_controller(real_http=True)
        old_snapshot = {
            "available_count": 1,
            "fetched_at": (NOW - timedelta(hours=2)).isoformat(),
            "credits": [{"expires_at": (NOW + timedelta(days=2)).isoformat()}],
        }
        self.set_snapshot(old_snapshot)
        no_card_calls = []

        def no_card_runner(account_id, _base_url, _token, **_kwargs):
            no_card_calls.append(account_id)
            return {
                "success": True,
                "queried_at": NOW.isoformat(),
                "data": self.usage_payload(None),
            }

        self.monitor.usage_runner = no_card_runner
        no_card_result = self.monitor.queries.query(
            copy.deepcopy(self.row),
            "fixture-admin",
            source="manual",
            reason="fixture-no-card",
            now=NOW,
        )

        self.assertTrue(no_card_result["success"], no_card_result)
        self.assertEqual(no_card_calls, [ACCOUNT_ID])
        self.assertEqual(self.server.requests, [])
        self.assertNotIn("reset_credit_observation", self.scheduler_meta())
        self.assertEqual(self.row["extra"]["codex_reset_credit_snapshot"]["fetched_at"], old_snapshot["fetched_at"])

    def test_coordinator_keeps_quota_success_when_card_readback_fails(self):
        snapshot = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=2)])
        self.set_snapshot(snapshot)

        def runner(_account_id, _base_url, _token, **_kwargs):
            self.row["extra"]["codex_reset_credit_snapshot"] = copy.deepcopy(snapshot)
            payload = self.usage_payload(snapshot)
            return {**quota_result(payload, self.row, NOW), "data": payload}

        reads = []
        original_reader = self.monitor.queries.account_reader

        def flaky_reader(account_id):
            reads.append(account_id)
            if len(reads) >= 2:
                raise OSError("fixture account readback unavailable")
            return original_reader(account_id)

        self.monitor.usage_runner = runner
        self.monitor.queries.account_reader = flaky_reader
        result = self.monitor.queries.query(
            copy.deepcopy(self.row), "fixture-admin", source="manual",
            reason="fixture-readback-failure", now=NOW,
        )

        self.assertTrue(result["success"], result)
        self.assertGreaterEqual(len(reads), 2)
        self.assertTrue(self.store.snapshot()["oauth_results"][str(ACCOUNT_ID)]["success"])
        self.assertNotIn("reset_credit_observation", self.scheduler_meta())
        self.assertEqual((self.scheduler_meta().get("quota_query") or {}).get("automatic_attempts", []), [])
        self.assertEqual(self.server.requests, [])


if __name__ == "__main__":
    unittest.main()
