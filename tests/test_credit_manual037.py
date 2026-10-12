from __future__ import annotations

import copy
import unittest
from datetime import timedelta

from tests.test_auto_reset import ACCOUNT_ID, AutoResetFixture, NOW, quota_result, wham


class ManualCredit037BoundaryTests(AutoResetFixture):
    """Keep manual and legacy uncertain credit episodes outside auto policy."""

    def _seed_unknown(self, *, source: str | None = None, with_evidence: bool = False) -> dict:
        task = {
            "episode": "legacy-manual-episode",
            "stage": "uncertain",
            "attempt_at": NOW.isoformat(),
            "consumed": False,
            "consumption_outcome": "unknown",
            "error_code": "result_uncertain",
            "owns_pause": False,
            "next_at": None,
            "receipt": {"http_status": 502, "business_code": "unrecognized"},
        }
        if source is not None:
            task["source"] = source
        if with_evidence:
            task["evidence_at"] = (NOW - timedelta(seconds=30)).isoformat()
        self.store.update_scheduler({ACCOUNT_ID: {"auto_reset_credit": task}})
        return copy.deepcopy(task)

    def _assert_quarantined_after_restart(self, expected: dict) -> None:
        self.run_once()
        first = self.state()
        self.assertEqual(first, expected)
        self.assertEqual(self.calls, [])

        self.make_controller()
        self.now += timedelta(hours=2)
        self.step()
        self.assertEqual(self.state(), expected)
        self.assertEqual(self.calls, [])
        self.assertEqual(self.store.snapshot()["recovery_history"], {})
        metadata = self.store.snapshot()["scheduler"][str(ACCOUNT_ID)]
        self.assertEqual(metadata.get("credit_result_notifications", {}), {})
        self.assertEqual(self.query_state().get("automatic_attempts", []), [])

    def test_legacy_uncertain_without_evidence_and_manual_source_are_quarantined(self):
        cases = (
            {"source": None, "with_evidence": False},
            {"source": "manual", "with_evidence": True},
        )
        for case in cases:
            with self.subTest(case=case):
                self.fresh_case()
                expected = self._seed_unknown(**case)
                self._assert_quarantined_after_restart(expected)

    def test_manual_unknown_can_only_be_marked_recovered_by_independent_confirmation(self):
        task = self._seed_unknown(source="manual", with_evidence=True)
        self._assert_quarantined_after_restart(task)

        self.now += timedelta(minutes=1)
        self.row.update(
            rate_limited_at=None,
            rate_limit_reset_at=None,
            temp_unschedulable_until=None,
            temp_unschedulable_reason="",
            overload_until=None,
            status="active",
            schedulable=True,
        )
        self.touch()
        self.store.commit(results={
            ACCOUNT_ID: quota_result(wham(self.now, five=10, seven=20), self.row, self.now),
        })

        self.step()
        recovered = self.state()
        self.assertEqual(recovered["episode"], task["episode"])
        self.assertEqual(recovered["stage"], "recovered")
        self.assertFalse(recovered.get("consumed", False))
        self.assertEqual(recovered.get("consumption_outcome"), "unknown")
        self.assertEqual(self.calls, [])
        self.assertEqual(self.store.snapshot()["recovery_history"], {})
        self.assertEqual(self.store.snapshot()["scheduler"][str(ACCOUNT_ID)].get("credit_result_notifications", {}), {})

    def test_newer_depletion_starts_a_fresh_automatic_episode_after_manual_recovery(self):
        task = self._seed_unknown(source=None, with_evidence=False)

        self.now += timedelta(minutes=1)
        self.row.update(
            rate_limited_at=None,
            rate_limit_reset_at=None,
            temp_unschedulable_until=None,
            temp_unschedulable_reason="",
            overload_until=None,
            status="active",
            schedulable=True,
        )
        self.touch()
        self.store.commit(results={
            ACCOUNT_ID: quota_result(wham(self.now, five=10, seven=20), self.row, self.now),
        })
        self.step()
        self.assertEqual(self.state()["stage"], "recovered")

        self.now += timedelta(minutes=1)
        limited_at = self.now
        reset_at = self.now + timedelta(days=5)
        self.row.update(
            rate_limited_at=limited_at.isoformat(),
            rate_limit_reset_at=reset_at.isoformat(),
            schedulable=True,
        )
        self.row["extra"]["codex_reset_credit_snapshot"] = {
            "available_count": 0,
            "fetched_at": self.now.isoformat(),
            "credits": [],
        }
        self.touch()
        self.db.raw.execute("DELETE FROM ops_error_logs")
        self.db.event(record_id=37, at=limited_at)
        self.store.commit(results={
            ACCOUNT_ID: quota_result(
                wham(self.now, five=10, seven=100, seven_reset_at=reset_at),
                self.row,
                self.now,
            ),
        })

        old_episode = self.state()["episode"]
        self.step()
        no_card = self.state()
        self.assertEqual(no_card["episode"], old_episode)
        self.assertEqual(no_card["stage"], "recovered")
        self.assertEqual(self.store.scheduler()[ACCOUNT_ID]["credit_admission"]["reason"], "no_credit")
        self.assertEqual(self.calls, [])

        # A later, trusted positive card can start a new episode inside the
        # original five-minute boundary. Occupy the test slot to inspect the
        # durable waiting state without consuming it in this assertion.
        self.row["extra"]["codex_reset_credit_snapshot"] = {
            "available_count": 1,
            "fetched_at": self.now.isoformat(),
            "credits": [{"expires_at": (self.now + timedelta(days=2)).isoformat()}],
        }
        self.touch()
        self.monitor._active_tests = self.settings.oauth_recovery_test_concurrency
        self.step()
        automatic = self.state()
        self.assertNotEqual(automatic["episode"], task["episode"])
        self.assertEqual(automatic.get("source"), None)
        self.assertEqual(automatic.get("policy_version"), 1)
        self.assertEqual(automatic["stage"], "waiting")
        self.assertEqual(automatic["error_code"], "test_slots")
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
