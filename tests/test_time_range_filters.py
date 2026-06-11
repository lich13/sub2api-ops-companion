from __future__ import annotations

import unittest
from pathlib import Path

from app.quality_sort import (
    sort_speed_rows,
)
from app.sql import GUARD_BALANCE_CANDIDATES_SQL, GUARD_QUEUE_SQL, QUALITY_ALL_ACCOUNTS_SQL, QUALITY_SQL
from app.time_range import BEIJING_TZ, build_time_range

REPO_ROOT = Path(__file__).resolve().parents[1]


class TimeRangeFilterTests(unittest.TestCase):
    def test_default_time_range_is_today(self) -> None:
        selected = build_time_range()

        self.assertEqual(selected["preset"], "today")
        self.assertEqual(selected["label"], "今天")
        self.assertIsNotNone(selected["start_at"])
        self.assertIsNotNone(selected["end_at"])
        self.assertEqual(selected["start_at"].tzinfo, BEIJING_TZ)
        self.assertEqual(selected["query_args"], {"time_range": "today"})

    def test_custom_time_range_normalizes_reversed_dates(self) -> None:
        selected = build_time_range("custom", "2026-05-12", "2026-05-01")

        self.assertEqual(selected["preset"], "custom")
        self.assertEqual(selected["start_date"], "2026-05-01")
        self.assertEqual(selected["end_date"], "2026-05-12")
        self.assertEqual(selected["label"], "2026/05/01 - 2026/05/12")
        self.assertEqual(
            selected["query_args"],
            {"time_range": "custom", "start_date": "2026-05-01", "end_date": "2026-05-12"},
        )

    def test_all_time_has_open_bounds(self) -> None:
        selected = build_time_range("all")

        self.assertEqual(selected["preset"], "all")
        self.assertEqual(selected["label"], "全部时间")
        self.assertIsNone(selected["start_at"])
        self.assertIsNone(selected["end_at"])

    def test_quality_sql_uses_range_bounds(self) -> None:
        self.assertIn("%(range_start)s::timestamptz", QUALITY_SQL)
        self.assertIn("%(range_end)s::timestamptz", QUALITY_SQL)
        self.assertNotIn("make_interval(hours => %(hours)s)", QUALITY_SQL)

    def test_guard_all_account_sql_keeps_bounded_signal_window(self) -> None:
        for sql in (QUALITY_ALL_ACCOUNTS_SQL, GUARD_QUEUE_SQL):
            with self.subTest(sql=sql[:20]):
                self.assertIn("%(range_start)s::timestamptz", sql)
                self.assertIn("%(range_end)s::timestamptz", sql)
                self.assertNotIn("g.name = ANY(%(group_names)s::text[])", sql)

    def test_balance_candidate_sql_filters_age_before_json_expansion(self) -> None:
        self.assertIn("WITH target_logs AS", GUARD_BALANCE_CANDIDATES_SQL)
        self.assertIn("WHERE created_at >= now() - (%(max_age_hours)s::text || ' hours')::interval", GUARD_BALANCE_CANDIDATES_SQL)
        self.assertIn("FROM target_logs t", GUARD_BALANCE_CANDIDATES_SQL)

    def test_quality_sql_exposes_speed_metrics(self) -> None:
        self.assertIn("AS avg_first_token_ms", QUALITY_SQL)
        self.assertIn("AS avg_duration_ms", QUALITY_SQL)
        self.assertIn("AS avg_ms_per_output_token", QUALITY_SQL)
        self.assertIn("AS output_tokens_window", QUALITY_SQL)

    def test_quality_sql_filters_usage_cost_by_range(self) -> None:
        self.assertNotIn("lifetime_usage AS", QUALITY_SQL)
        self.assertIn("AS usage_total_cost", QUALITY_SQL)
        self.assertIn("AS usage_total_tokens", QUALITY_SQL)
        self.assertIn("COALESCE(s.usage_total_cost,0) AS usage_total_cost", QUALITY_SQL)

    def test_speed_default_sort_prefers_schedulable_then_window_sample_count(self) -> None:
        rows = [
            {"id": 1, "schedulable": True, "success_window": 3, "account_quality_errors_window": 3, "group_priority": 1, "account_priority": 1},
            {"id": 2, "schedulable": True, "success_window": 7, "account_quality_errors_window": 0, "group_priority": 9, "account_priority": 9},
            {"id": 3, "schedulable": False, "success_window": 99, "account_quality_errors_window": 99, "group_priority": 0, "account_priority": 0},
            {"id": 4, "schedulable": True, "success_window": 7, "account_quality_errors_window": 5, "group_priority": 1, "account_priority": 3},
        ]

        self.assertEqual([row["id"] for row in sort_speed_rows(rows)], [4, 2, 1, 3])

    def test_speed_template_keeps_usage_column_without_stability_page(self) -> None:
        speed = (REPO_ROOT / "app" / "templates" / "speed.html").read_text(encoding="utf-8")

        self.assertFalse((REPO_ROOT / "app" / "templates" / "index.html").exists())
        self.assertNotIn("历史消耗", speed)
        self.assertIn("消耗</th>", speed)
        self.assertIn("首 Token（秒）</th>", speed)
        self.assertIn("平均耗时（秒）</th>", speed)
        self.assertIn("tokens/秒</th>", speed)
        self.assertNotIn("ms/token", speed)


if __name__ == "__main__":
    unittest.main()
