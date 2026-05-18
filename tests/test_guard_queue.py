from __future__ import annotations

import unittest

from app.guard_queue import auto_queue_plan, queue_tier


def account(**overrides: object) -> dict[str, object]:
    row: dict[str, object] = {
        "id": 1,
        "name": "acct",
        "schedulable": True,
        "status": "active",
        "success_window": 0,
        "account_quality_errors_window": 0,
        "balance_or_quota_window": 0,
        "blocked_403_window": 0,
        "rate_limit_window": 0,
        "unstable_5xx_stream_window": 0,
        "account_priority": 50,
        "concurrency": 3,
        "load_factor": None,
        "temp_unschedulable_until": None,
    }
    row.update(overrides)
    return row


class GuardQueueTests(unittest.TestCase):
    def test_queue_tier_is_explicitly_mapped_from_account_priority(self) -> None:
        self.assertEqual(queue_tier(account(account_priority=1))["key"], "p1")
        self.assertEqual(queue_tier(account(account_priority=2))["key"], "p2")
        self.assertEqual(queue_tier(account(account_priority=50))["key"], "standby")

    def test_auto_queue_plan_promotes_best_healthy_accounts_to_p1_and_p2(self) -> None:
        rows = [
            account(id=1, success_window=1, account_priority=20),
            account(id=2, success_window=8, account_priority=20),
            account(id=3, success_window=5, account_priority=20),
        ]

        plan = auto_queue_plan(rows, p1_count=1, p2_count=1, load_factor_supported=True)

        self.assertEqual([(item["account_id"], item["tier"], item["priority"]) for item in plan], [(2, "p1", 1), (3, "p2", 2), (1, "standby", 50)])
        self.assertTrue(all(item["load_factor"] is None for item in plan))

    def test_auto_queue_plan_deprioritizes_problem_accounts_and_soft_lands_load_factor(self) -> None:
        rows = [
            account(id=1, success_window=10),
            account(id=2, success_window=20, rate_limit_window=1),
            account(id=3, success_window=30, schedulable=False),
        ]

        plan = auto_queue_plan(rows, p1_count=1, p2_count=1, load_factor_supported=True)
        by_id = {item["account_id"]: item for item in plan}

        self.assertEqual(by_id[1]["tier"], "p1")
        self.assertEqual(by_id[2]["tier"], "degraded")
        self.assertEqual(by_id[2]["priority"], 90)
        self.assertEqual(by_id[2]["load_factor"], 1)
        self.assertEqual(by_id[3]["tier"], "degraded")


if __name__ == "__main__":
    unittest.main()
