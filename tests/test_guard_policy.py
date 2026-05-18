from __future__ import annotations

import unittest
from datetime import datetime, timezone

from app.guard_policy import GuardCircuit, GuardPolicy, GuardSignal, apply_signal


def now() -> datetime:
    return datetime(2026, 5, 18, 10, 0, tzinfo=timezone.utc)


class GuardPolicyTests(unittest.TestCase):
    def test_balance_quota_immediately_hard_pauses(self) -> None:
        action, updated = apply_signal(
            GuardPolicy(),
            GuardCircuit(account_id=9),
            GuardSignal(account_id=9, category="provider_balance_or_quota", event_key="error:101:1", event_id=101, created_at=now()),
            now(),
        )
        self.assertEqual(action.kind, "pause")
        self.assertTrue(action.hard)
        self.assertEqual(updated.state, "open")

    def test_rate_limit_escalates_to_allowed_cooldown_slots(self) -> None:
        policy = GuardPolicy()
        circuit = GuardCircuit(account_id=9)
        action1, circuit = apply_signal(policy, circuit, GuardSignal(9, "provider_rate_limit", "error:101:1", now(), event_id=101), now())
        action2, circuit = apply_signal(policy, circuit, GuardSignal(9, "provider_rate_limit", "error:102:1", now(), event_id=102), now())
        action3, circuit = apply_signal(policy, circuit, GuardSignal(9, "provider_rate_limit", "error:103:1", now(), event_id=103), now())
        self.assertEqual([action1.minutes, action2.minutes, action3.minutes], [5, 15, 30])

    def test_unstable_errors_open_after_failure_threshold(self) -> None:
        policy = GuardPolicy(failure_threshold=4)
        circuit = GuardCircuit(account_id=9)
        action = None
        for index in range(4):
            action, circuit = apply_signal(
                policy,
                circuit,
                GuardSignal(9, "upstream_unstable_5xx_stream", f"error:{200 + index}:1", now(), event_id=200 + index),
                now(),
            )
        self.assertIsNotNone(action)
        self.assertEqual(action.kind, "cooldown")
        self.assertEqual(action.minutes, 5)
        self.assertEqual(circuit.state, "open")

    def test_client_errors_are_ignored(self) -> None:
        action, updated = apply_signal(
            GuardPolicy(),
            GuardCircuit(account_id=9),
            GuardSignal(9, "client_bad_request", "error:101:1", now(), event_id=101),
            now(),
        )
        self.assertEqual(action.kind, "none")
        self.assertEqual(updated.state, "closed")

    def test_successes_close_half_open_after_threshold(self) -> None:
        policy = GuardPolicy(success_threshold=2)
        circuit = GuardCircuit(account_id=9, state="half_open", consecutive_failures=0)
        _, circuit = apply_signal(policy, circuit, GuardSignal(9, "success", "success:9:1", now()), now())
        self.assertEqual(circuit.state, "half_open")
        _, circuit = apply_signal(policy, circuit, GuardSignal(9, "success", "success:9:2", now()), now())
        self.assertEqual(circuit.state, "closed")

    def test_recovery_success_closes_open_circuit_after_threshold(self) -> None:
        policy = GuardPolicy(success_threshold=1)
        circuit = GuardCircuit(account_id=9, state="open", consecutive_failures=4)
        _, circuit = apply_signal(policy, circuit, GuardSignal(9, "success", "recovery:999", now(), event_id=999), now())
        self.assertEqual(circuit.state, "closed")


if __name__ == "__main__":
    unittest.main()
