from __future__ import annotations

import unittest

from app.sql import QUALITY_SQL, REQUESTS_SQL
from app.time_range import BEIJING_TZ, build_time_range


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


if __name__ == "__main__":
    unittest.main()
