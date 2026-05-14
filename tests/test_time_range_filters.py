from __future__ import annotations

import unittest
from pathlib import Path

from app.quality_sort import STABILITY_SORT_OPTIONS, normalize_stability_sort, sort_stability_rows
from app.sql import QUALITY_SQL, REQUESTS_SQL
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

    def test_quality_and_requests_sql_use_range_bounds(self) -> None:
        for sql in (QUALITY_SQL, REQUESTS_SQL):
            with self.subTest(sql=sql[:20]):
                self.assertIn("%(range_start)s::timestamptz", sql)
                self.assertIn("%(range_end)s::timestamptz", sql)
                self.assertNotIn("make_interval(hours => %(hours)s)", sql)

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

    def test_stability_sort_by_error_rate(self) -> None:
        rows = [
            {"id": 1, "group_priority": 1, "account_priority": 1, "error_rate_window_pct": 10, "account_quality_errors_window": 3},
            {"id": 2, "group_priority": 1, "account_priority": 2, "error_rate_window_pct": 50, "account_quality_errors_window": 1},
            {"id": 3, "group_priority": 1, "account_priority": 3, "error_rate_window_pct": 50, "account_quality_errors_window": 4},
        ]

        self.assertEqual([row["id"] for row in sort_stability_rows(rows, "error_rate")], [3, 2, 1])
        self.assertEqual(normalize_stability_sort("unknown"), "default")
        self.assertIn("错误率从高到低", {item["label"] for item in STABILITY_SORT_OPTIONS})

    def test_stability_and_speed_templates_split_usage_column(self) -> None:
        stability = (REPO_ROOT / "app" / "templates" / "index.html").read_text(encoding="utf-8")
        speed = (REPO_ROOT / "app" / "templates" / "speed.html").read_text(encoding="utf-8")

        self.assertNotIn("历史消耗</th>", stability)
        self.assertNotIn("历史消耗", speed)
        self.assertIn('name="sort"', stability)
        self.assertIn("sort_options", stability)
        self.assertIn("消耗</th>", speed)
        self.assertIn("首 Token（秒）</th>", speed)
        self.assertIn("平均耗时（秒）</th>", speed)
        self.assertIn("tokens/秒</th>", speed)
        self.assertNotIn("ms/token", speed)

    def test_account_panel_uses_fixed_cooldown_presets(self) -> None:
        stability = (REPO_ROOT / "app" / "templates" / "index.html").read_text(encoding="utf-8")

        self.assertIn("{% for minutes in [5, 15, 30] %}", stability)
        self.assertIn("冷却 {{ minutes }}m", stability)
        self.assertNotIn('name="minutes" value="30" aria-label="冷却分钟"', stability)


if __name__ == "__main__":
    unittest.main()
