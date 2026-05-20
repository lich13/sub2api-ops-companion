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

    def test_hard_pause_switch_disables_balance_and_blocked_actions(self) -> None:
        policy = GuardPolicy(hard_pause_enabled=False)

        balance_action, balance_circuit = apply_signal(
            policy,
            GuardCircuit(account_id=9),
            GuardSignal(9, "provider_balance_or_quota", "error:101:1", now(), event_id=101),
            now(),
        )
        blocked_action, blocked_circuit = apply_signal(
            policy,
            GuardCircuit(account_id=10),
            GuardSignal(10, "provider_blocked_403", "error:102:1", now(), event_id=102),
            now(),
        )

        self.assertEqual(balance_action.kind, "none")
        self.assertEqual(blocked_action.kind, "none")
        self.assertEqual(balance_circuit.last_category, "provider_balance_or_quota")
        self.assertEqual(blocked_circuit.last_category, "provider_blocked_403")

    def test_rate_limit_uses_short_realtime_cooldown_slots(self) -> None:
        policy = GuardPolicy()
        circuit = GuardCircuit(account_id=9)
        action1, circuit = apply_signal(policy, circuit, GuardSignal(9, "provider_rate_limit", "error:101:1", now(), event_id=101), now())
        action2, circuit = apply_signal(policy, circuit, GuardSignal(9, "provider_rate_limit", "error:102:1", now(), event_id=102), now())
        action3, circuit = apply_signal(policy, circuit, GuardSignal(9, "provider_rate_limit", "error:103:1", now(), event_id=103), now())
        self.assertEqual([action1.minutes, action2.minutes, action3.minutes], [1, 3, 5])

    def test_rate_limit_switch_disables_cooldown_action(self) -> None:
        action, updated = apply_signal(
            GuardPolicy(rate_limit_enabled=False),
            GuardCircuit(account_id=9),
            GuardSignal(9, "provider_rate_limit", "error:101:1", now(), event_id=101),
            now(),
        )

        self.assertEqual(action.kind, "none")
        self.assertEqual(updated.last_category, "provider_rate_limit")

    def test_whitelisted_rate_limit_records_without_cooldown_action(self) -> None:
        action, updated = apply_signal(
            GuardPolicy(whitelist_account_ids=(9,)),
            GuardCircuit(account_id=9),
            GuardSignal(9, "provider_rate_limit", "error:101:1", now(), event_id=101),
            now(),
        )

        self.assertEqual(action.kind, "none")
        self.assertEqual(updated.state, "closed")
        self.assertEqual(updated.last_category, "provider_rate_limit")
        self.assertEqual(updated.consecutive_balance_quota_failures, 0)

    def test_whitelisted_blocked_403_records_without_pause_action(self) -> None:
        action, updated = apply_signal(
            GuardPolicy(whitelist_account_ids=(9,)),
            GuardCircuit(account_id=9),
            GuardSignal(9, "provider_blocked_403", "error:102:1", now(), event_id=102),
            now(),
        )

        self.assertEqual(action.kind, "none")
        self.assertEqual(updated.state, "closed")
        self.assertEqual(updated.last_category, "provider_blocked_403")

    def test_whitelisted_quota_pauses_after_ten_consecutive_quota_signals(self) -> None:
        policy = GuardPolicy(whitelist_account_ids=(9,))
        circuit = GuardCircuit(account_id=9)
        action = None

        for index in range(9):
            action, circuit = apply_signal(
                policy,
                circuit,
                GuardSignal(9, "provider_balance_or_quota", f"error:{201 + index}:1", now(), event_id=201 + index),
                now(),
            )

        self.assertIsNotNone(action)
        self.assertEqual(action.kind, "none")
        self.assertEqual(circuit.state, "closed")
        self.assertEqual(circuit.consecutive_balance_quota_failures, 9)

        action, circuit = apply_signal(
            policy,
            circuit,
            GuardSignal(9, "provider_balance_or_quota", "error:210:1", now(), event_id=210),
            now(),
        )

        self.assertEqual(action.kind, "pause")
        self.assertTrue(action.hard)
        self.assertEqual(circuit.state, "open")
        self.assertEqual(circuit.consecutive_balance_quota_failures, 10)

    def test_whitelisted_non_quota_signal_resets_quota_streak(self) -> None:
        policy = GuardPolicy(whitelist_account_ids=(9,))
        circuit = GuardCircuit(account_id=9)
        for index in range(9):
            _, circuit = apply_signal(
                policy,
                circuit,
                GuardSignal(9, "provider_balance_or_quota", f"error:{301 + index}:1", now(), event_id=301 + index),
                now(),
            )

        action, circuit = apply_signal(
            policy,
            circuit,
            GuardSignal(9, "provider_rate_limit", "error:400:1", now(), event_id=400),
            now(),
        )
        self.assertEqual(action.kind, "none")
        self.assertEqual(circuit.consecutive_balance_quota_failures, 0)

        for index in range(9):
            action, circuit = apply_signal(
                policy,
                circuit,
                GuardSignal(9, "provider_balance_or_quota", f"error:{401 + index}:1", now(), event_id=401 + index),
                now(),
            )

        self.assertIsNotNone(action)
        self.assertEqual(action.kind, "none")
        self.assertEqual(circuit.state, "closed")
        self.assertEqual(circuit.consecutive_balance_quota_failures, 9)

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
        self.assertEqual(action.minutes, 1)
        self.assertEqual(circuit.state, "open")

    def test_unstable_switch_disables_cooldown_action(self) -> None:
        policy = GuardPolicy(failure_threshold=1, unstable_enabled=False)
        action, updated = apply_signal(
            policy,
            GuardCircuit(account_id=9),
            GuardSignal(9, "upstream_unstable_5xx_stream", "error:201:1", now(), event_id=201),
            now(),
        )

        self.assertEqual(action.kind, "none")
        self.assertEqual(updated.last_category, "upstream_unstable_5xx_stream")

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
