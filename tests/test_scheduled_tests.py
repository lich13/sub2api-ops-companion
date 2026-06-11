from __future__ import annotations

import unittest
from datetime import datetime, timezone
from pathlib import Path

from app.scheduled_tests import (
    interval_from_cron,
    interval_options,
    next_aligned_run,
    normalize_interval_minutes,
    schedule_cron,
)
from app.sql import (
    SCHEDULED_TEST_CAPABILITY_SQL,
    SCHEDULED_TEST_DELETE_ENDLESS_SQL,
    SCHEDULED_TEST_ENDLESS_PLANS_SQL,
    SCHEDULED_TEST_RECOVERY_ALERTS_SQL,
    SCHEDULED_TEST_UPSERT_SQL,
)

REPO_ROOT = Path(__file__).resolve().parents[1]


class ScheduledTestHelpersTests(unittest.TestCase):
    def test_interval_options_are_fixed_whole_hour_presets(self) -> None:
        self.assertEqual(
            [(item["minutes"], item["label"]) for item in interval_options()],
            [(60, "每小时"), (30, "每30分钟"), (15, "每15分钟"), (5, "每5分钟"), (1, "每1分钟")],
        )

    def test_interval_to_cron_uses_whole_hour_aligned_expressions(self) -> None:
        self.assertEqual(schedule_cron(60), "0 * * * *")
        self.assertEqual(schedule_cron(30), "*/30 * * * *")
        self.assertEqual(schedule_cron(15), "*/15 * * * *")
        self.assertEqual(schedule_cron(5), "*/5 * * * *")
        self.assertEqual(schedule_cron(1), "* * * * *")

    def test_interval_from_cron_round_trips_known_presets(self) -> None:
        self.assertEqual(interval_from_cron("0 * * * *"), 60)
        self.assertEqual(interval_from_cron("*/30 * * * *"), 30)
        self.assertEqual(interval_from_cron("*/15 * * * *"), 15)
        self.assertEqual(interval_from_cron("*/5 * * * *"), 5)
        self.assertEqual(interval_from_cron("* * * * *"), 1)
        self.assertEqual(interval_from_cron("13 * * * *"), 30)

    def test_next_run_is_strictly_after_now_and_aligned_to_hour_grid(self) -> None:
        now = datetime(2026, 5, 14, 10, 29, 40, tzinfo=timezone.utc)

        self.assertEqual(next_aligned_run(now, 30), datetime(2026, 5, 14, 10, 30, tzinfo=timezone.utc))
        self.assertEqual(next_aligned_run(now, 15), datetime(2026, 5, 14, 10, 30, tzinfo=timezone.utc))
        self.assertEqual(next_aligned_run(now, 5), datetime(2026, 5, 14, 10, 30, tzinfo=timezone.utc))
        self.assertEqual(next_aligned_run(now, 1), datetime(2026, 5, 14, 10, 30, tzinfo=timezone.utc))
        self.assertEqual(next_aligned_run(now, 60), datetime(2026, 5, 14, 11, 0, tzinfo=timezone.utc))

    def test_next_run_rolls_to_next_slot_when_exactly_on_boundary(self) -> None:
        now = datetime(2026, 5, 14, 10, 30, 0, tzinfo=timezone.utc)

        self.assertEqual(next_aligned_run(now, 30), datetime(2026, 5, 14, 11, 0, tzinfo=timezone.utc))
        self.assertEqual(next_aligned_run(now, 15), datetime(2026, 5, 14, 10, 45, tzinfo=timezone.utc))
        self.assertEqual(next_aligned_run(now, 1), datetime(2026, 5, 14, 10, 31, tzinfo=timezone.utc))

    def test_invalid_interval_defaults_to_30_minutes(self) -> None:
        self.assertEqual(normalize_interval_minutes("5"), 5)
        self.assertEqual(normalize_interval_minutes("1"), 1)
        self.assertEqual(normalize_interval_minutes("60"), 60)
        self.assertEqual(normalize_interval_minutes("7"), 30)
        self.assertEqual(normalize_interval_minutes("bad"), 30)

    def test_scheduled_test_sql_uses_upstream_tables_and_auto_recover(self) -> None:
        combined = "\n".join(
            [
                SCHEDULED_TEST_CAPABILITY_SQL,
                SCHEDULED_TEST_UPSERT_SQL,
                SCHEDULED_TEST_DELETE_ENDLESS_SQL,
                SCHEDULED_TEST_ENDLESS_PLANS_SQL,
                SCHEDULED_TEST_RECOVERY_ALERTS_SQL,
            ]
        )

        self.assertIn("scheduled_test_plans", combined)
        self.assertIn("scheduled_test_results", combined)
        self.assertIn("auto_recover", combined)
        self.assertIn("next_run_at", combined)
        self.assertIn("WITH existing AS", SCHEDULED_TEST_UPSERT_SQL)
        self.assertIn("INSERT INTO scheduled_test_plans", SCHEDULED_TEST_UPSERT_SQL)
        self.assertIn("account_id = %(account_id)s::bigint", SCHEDULED_TEST_DELETE_ENDLESS_SQL)
        self.assertIn("cron_expression = '* * * * *'", SCHEDULED_TEST_DELETE_ENDLESS_SQL)
        self.assertIn("auto_recover = true", SCHEDULED_TEST_DELETE_ENDLESS_SQL)
        self.assertIn("id AS plan_id", SCHEDULED_TEST_ENDLESS_PLANS_SQL)
        self.assertIn("cron_expression = '* * * * *'", SCHEDULED_TEST_ENDLESS_PLANS_SQL)
        self.assertIn("auto_recover = true", SCHEDULED_TEST_ENDLESS_PLANS_SQL)
        self.assertIn("r.id > %(cursor_id)s::bigint", SCHEDULED_TEST_RECOVERY_ALERTS_SQL)
        self.assertIn("r.created_at <= now() - interval '5 seconds'", SCHEDULED_TEST_RECOVERY_ALERTS_SQL)
        self.assertNotIn("a.updated_at >= r.started_at", SCHEDULED_TEST_RECOVERY_ALERTS_SQL)
        self.assertIn("a.rate_limited_at", SCHEDULED_TEST_RECOVERY_ALERTS_SQL)

    def test_scheduled_recovery_is_internal_guard_capability_only(self) -> None:
        base = (REPO_ROOT / "app" / "templates" / "base.html").read_text(encoding="utf-8")

        self.assertNotIn('active == \'scheduled_tests\'', base)
        self.assertNotIn("定时恢复", base)
        self.assertNotIn("schedule-options.js", base)
        self.assertFalse((REPO_ROOT / "app" / "templates" / "scheduled_tests.html").exists())


if __name__ == "__main__":
    unittest.main()
