from __future__ import annotations

import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from app.guard_policy import GuardCircuit
from app.guard_store import GuardStore


class GuardStoreTests(unittest.TestCase):
    def test_store_round_trips_cursors_and_circuits(self) -> None:
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "guard-state.json"
            store = GuardStore(str(path))
            store.set_error_cursor(42)
            store.set_success_cursor("2026-05-18T10:00:00+00:00")
            store.set_recovery_cursor(77)
            store.save_circuit(GuardCircuit(account_id=9, state="open", consecutive_failures=4))

            reloaded = GuardStore(str(path))
            self.assertEqual(reloaded.error_cursor(), 42)
            self.assertEqual(reloaded.success_cursor(), "2026-05-18T10:00:00+00:00")
            self.assertEqual(reloaded.recovery_cursor(), 77)
            self.assertEqual(reloaded.circuit(9).state, "open")
            self.assertEqual(reloaded.circuit(9).consecutive_failures, 4)

    def test_store_handles_missing_file_as_empty_state(self) -> None:
        with TemporaryDirectory() as tmp:
            store = GuardStore(str(Path(tmp) / "missing.json"))
            self.assertEqual(store.error_cursor(), 0)
            self.assertEqual(store.success_cursor(), "")
            self.assertEqual(store.recovery_cursor(), 0)
            self.assertEqual(store.circuit(9).state, "closed")

    def test_store_round_trips_policy_config(self) -> None:
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "guard-state.json"
            store = GuardStore(str(path))
            store.save_policy({"failure_threshold": 8, "success_threshold": 3, "rate_limit_cooldowns": [5, 15, 30]})
            reloaded = GuardStore(str(path))
            self.assertEqual(reloaded.policy_config()["failure_threshold"], 8)
            self.assertEqual(reloaded.policy_config()["success_threshold"], 3)
            self.assertEqual(reloaded.policy_config()["rate_limit_cooldowns"], [5, 15, 30])

    def test_whitelist_helpers_preserve_policy_config(self) -> None:
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "guard-state.json"
            store = GuardStore(str(path))
            store.save_policy(
                {
                    "failure_threshold": 8,
                    "success_threshold": 3,
                    "rate_limit_cooldowns": [5, 15, 30],
                    "whitelist_account_ids": [9],
                }
            )

            policy, changed = store.add_whitelist_account(7)
            self.assertTrue(changed)
            self.assertEqual(policy["failure_threshold"], 8)
            self.assertEqual(policy["success_threshold"], 3)
            self.assertEqual(policy["rate_limit_cooldowns"], [5, 15, 30])
            self.assertEqual(policy["whitelist_account_ids"], [7, 9])

            policy, changed = GuardStore(str(path)).add_whitelist_account(7)
            self.assertFalse(changed)
            self.assertEqual(policy["whitelist_account_ids"], [7, 9])

            policy, changed = GuardStore(str(path)).remove_whitelist_account(9)
            self.assertTrue(changed)
            self.assertEqual(policy["failure_threshold"], 8)
            self.assertEqual(policy["whitelist_account_ids"], [7])

    def test_whitelist_and_endless_helpers_are_mutually_exclusive(self) -> None:
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "guard-state.json"
            store = GuardStore(str(path))
            store.save_policy(
                {
                    "failure_threshold": 8,
                    "success_threshold": 3,
                    "whitelist_account_ids": [9],
                    "endless_account_ids": [7],
                }
            )

            policy, changed = store.add_endless_account(9)
            self.assertTrue(changed)
            self.assertEqual(policy["failure_threshold"], 8)
            self.assertEqual(policy["whitelist_account_ids"], [])
            self.assertEqual(policy["endless_account_ids"], [7, 9])

            policy, changed = GuardStore(str(path)).add_whitelist_account(7)
            self.assertTrue(changed)
            self.assertEqual(policy["whitelist_account_ids"], [7])
            self.assertEqual(policy["endless_account_ids"], [9])

            policy, changed = GuardStore(str(path)).remove_endless_account(9)
            self.assertTrue(changed)
            self.assertEqual(policy["whitelist_account_ids"], [7])
            self.assertEqual(policy["endless_account_ids"], [])

    def test_clear_whitelist_preserves_other_policy_fields_and_endless_accounts(self) -> None:
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "guard-state.json"
            store = GuardStore(str(path))
            store.save_policy(
                {
                    "failure_threshold": 8,
                    "success_threshold": 3,
                    "whitelist_account_ids": [9],
                    "endless_account_ids": [7],
                }
            )

            policy, changed = store.clear_whitelist_accounts()

            self.assertTrue(changed)
            self.assertEqual(policy["failure_threshold"], 8)
            self.assertEqual(policy["success_threshold"], 3)
            self.assertEqual(policy["whitelist_account_ids"], [])
            self.assertEqual(policy["endless_account_ids"], [7])


if __name__ == "__main__":
    unittest.main()
