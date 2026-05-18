from __future__ import annotations

import unittest

from app.guard_queue import auto_queue_plan, queue_position, reorder_queue_plan


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
        "group_id": 101,
        "group_sort_order": 1,
        "group_name": "openai",
        "group_priority": 50,
        "concurrency": 3,
        "load_factor": None,
        "temp_unschedulable_until": None,
    }
    row.update(overrides)
    return row


class GuardQueueTests(unittest.TestCase):
    def test_queue_position_labels_priority_as_unbounded_p_sequence(self) -> None:
        self.assertEqual(queue_position(account(group_priority=1))["label"], "P1")
        self.assertEqual(queue_position(account(group_priority=2))["label"], "P2")
        self.assertEqual(queue_position(account(group_priority=42))["label"], "P42")

    def test_auto_queue_plan_assigns_dense_order_for_each_group(self) -> None:
        rows = [
            account(id=1, group_id=101, group_name="openai", success_window=1, group_priority=20),
            account(id=2, group_id=101, group_name="openai", success_window=8, group_priority=20),
            account(id=3, group_id=202, group_name="claude", success_window=5, group_priority=20),
        ]

        plan = auto_queue_plan(rows, load_factor_supported=True)

        self.assertEqual(
            [(item["account_id"], item["group_name"], item["position"], item["group_priority"]) for item in plan],
            [(2, "openai", 1, 1), (1, "openai", 2, 2), (3, "claude", 1, 1)],
        )
        self.assertTrue(all(item["load_factor"] is None for item in plan))

    def test_auto_queue_plan_moves_problem_accounts_to_group_tail_and_soft_lands_load_factor(self) -> None:
        rows = [
            account(id=1, group_id=101, group_name="openai", success_window=10),
            account(id=2, group_id=101, group_name="openai", success_window=20, rate_limit_window=1),
            account(id=3, group_id=101, group_name="openai", success_window=30, schedulable=False),
        ]

        plan = auto_queue_plan(rows, load_factor_supported=True)
        by_id = {item["account_id"]: item for item in plan}

        self.assertEqual(by_id[1]["position"], 1)
        self.assertEqual(by_id[2]["position"], 2)
        self.assertEqual(by_id[2]["group_priority"], 2)
        self.assertEqual(by_id[2]["load_factor"], 1)
        self.assertEqual(by_id[3]["position"], 3)

    def test_reorder_queue_plan_uses_submitted_order_inside_each_group_membership(self) -> None:
        rows = [account(id=index, group_id=101, group_name="openai", group_priority=index, load_factor=None) for index in range(1, 103)]
        rows[1]["load_factor"] = 1

        plan = reorder_queue_plan(rows, ["101:2", "101:1"], load_factor_supported=True)

        self.assertEqual(
            [(item["account_id"], item["membership_key"], item["group_name"], item["position"], item["group_priority"], item["load_factor"]) for item in plan[:3]],
            [(2, "101:2", "openai", 1, 1, 1), (1, "101:1", "openai", 2, 2, None), (3, "101:3", "openai", 3, 3, None)],
        )
        self.assertEqual(plan[-1]["position"], 102)
        self.assertEqual(plan[-1]["group_priority"], 102)

    def test_same_account_can_have_different_positions_in_different_groups(self) -> None:
        rows = [
            account(id=9, group_id=101, group_name="openai", group_priority=1),
            account(id=10, group_id=101, group_name="openai", group_priority=2),
            account(id=9, group_id=202, group_name="backup", group_priority=2),
            account(id=11, group_id=202, group_name="backup", group_priority=1),
        ]

        plan = reorder_queue_plan(rows, ["101:10", "101:9", "202:9", "202:11"], load_factor_supported=True)

        self.assertEqual(
            [(item["membership_key"], item["position"], item["group_priority"]) for item in plan],
            [("101:10", 1, 1), ("101:9", 2, 2), ("202:9", 1, 1), ("202:11", 2, 2)],
        )


if __name__ == "__main__":
    unittest.main()
