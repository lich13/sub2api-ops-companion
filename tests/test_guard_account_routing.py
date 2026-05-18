from __future__ import annotations

import unittest
from pathlib import Path

from app.account_ops import normalize_load_factor_value, normalize_priority_value
from app.sql import (
    ACCOUNT_ROUTING_CAPABILITY_SQL,
    GUARD_ACCOUNT_PRIORITY_UPDATE_SQL,
    GUARD_ACCOUNT_ROUTING_UPDATE_SQL,
    QUALITY_ALL_ACCOUNTS_SQL,
    QUALITY_ALL_ACCOUNTS_SQL_COMPAT_NO_LOAD_FACTOR,
    QUALITY_SQL,
    QUALITY_SQL_COMPAT_NO_LOAD_FACTOR,
)

REPO_ROOT = Path(__file__).resolve().parents[1]


class GuardAccountRoutingTests(unittest.TestCase):
    def test_account_routing_sql_uses_existing_sub2api_columns(self) -> None:
        self.assertIn("table_name = 'accounts'", ACCOUNT_ROUTING_CAPABILITY_SQL)
        self.assertIn("column_name = 'priority'", ACCOUNT_ROUTING_CAPABILITY_SQL)
        self.assertIn("column_name = 'load_factor'", ACCOUNT_ROUTING_CAPABILITY_SQL)
        self.assertIn("UPDATE accounts", GUARD_ACCOUNT_ROUTING_UPDATE_SQL)
        self.assertIn("priority = %(priority)s::int", GUARD_ACCOUNT_ROUTING_UPDATE_SQL)
        self.assertIn("load_factor = CASE", GUARD_ACCOUNT_ROUTING_UPDATE_SQL)
        self.assertIn("ELSE %(load_factor)s::int", GUARD_ACCOUNT_ROUTING_UPDATE_SQL)
        self.assertIn("deleted_at IS NULL", GUARD_ACCOUNT_ROUTING_UPDATE_SQL)
        self.assertIn("UPDATE accounts", GUARD_ACCOUNT_PRIORITY_UPDATE_SQL)
        self.assertNotIn("load_factor =", GUARD_ACCOUNT_PRIORITY_UPDATE_SQL)

    def test_quality_sql_exposes_account_priority_and_load_factor(self) -> None:
        self.assertIn("a.priority AS account_priority", QUALITY_SQL)
        self.assertIn("a.load_factor", QUALITY_SQL)
        self.assertIn("effective_load_factor", QUALITY_SQL)

    def test_quality_compat_sql_does_not_require_load_factor_column(self) -> None:
        self.assertIn("NULL::integer AS load_factor", QUALITY_SQL_COMPAT_NO_LOAD_FACTOR)
        self.assertNotIn("a.load_factor", QUALITY_SQL_COMPAT_NO_LOAD_FACTOR)

    def test_guard_all_accounts_quality_sql_has_no_group_platform_or_time_filters(self) -> None:
        self.assertIn("LEFT JOIN account_groups", QUALITY_ALL_ACCOUNTS_SQL)
        self.assertIn("LEFT JOIN groups", QUALITY_ALL_ACCOUNTS_SQL)
        self.assertNotIn("g.name = ANY(%(group_names)s::text[])", QUALITY_ALL_ACCOUNTS_SQL)
        self.assertNotIn("a.platform = %(platform)s", QUALITY_ALL_ACCOUNTS_SQL)
        self.assertNotIn("e.platform = %(platform)s", QUALITY_ALL_ACCOUNTS_SQL)
        self.assertNotIn("created_at >= %(range_start)s", QUALITY_ALL_ACCOUNTS_SQL)
        self.assertNotIn("created_at < %(range_end)s", QUALITY_ALL_ACCOUNTS_SQL)
        self.assertIn("NULL::integer AS load_factor", QUALITY_ALL_ACCOUNTS_SQL_COMPAT_NO_LOAD_FACTOR)
        self.assertNotIn("a.load_factor", QUALITY_ALL_ACCOUNTS_SQL_COMPAT_NO_LOAD_FACTOR)

    def test_priority_validation_matches_sub2api_scheduler_range(self) -> None:
        self.assertEqual(normalize_priority_value("1"), 1)
        self.assertEqual(normalize_priority_value("50"), 50)
        self.assertEqual(normalize_priority_value("999"), 100)
        self.assertEqual(normalize_priority_value("bad"), 50)

    def test_load_factor_validation_allows_clear_and_positive_values(self) -> None:
        self.assertIsNone(normalize_load_factor_value(""))
        self.assertIsNone(normalize_load_factor_value("0"))
        self.assertIsNone(normalize_load_factor_value("-1"))
        self.assertEqual(normalize_load_factor_value("1"), 1)
        self.assertEqual(normalize_load_factor_value("20"), 20)

    def test_guard_template_exposes_policy_and_account_routing_controls(self) -> None:
        template = (REPO_ROOT / "app" / "templates" / "guard.html").read_text(encoding="utf-8")

        self.assertIn('action="{{ base_path }}/guard/policy"', template)
        self.assertIn('name="failure_threshold"', template)
        self.assertIn('name="hard_pause_enabled"', template)
        self.assertIn('name="rate_limit_enabled"', template)
        self.assertIn('name="unstable_enabled"', template)
        self.assertIn('action="{{ base_path }}/guard/account-routing"', template)
        self.assertIn('action="{{ base_path }}/guard/queue/auto"', template)
        self.assertIn('action="{{ base_path }}/guard/queue-tier"', template)
        self.assertIn('name="queue_group"', template)
        self.assertIn('name="priority"', template)
        self.assertIn('name="load_factor"', template)
        self.assertIn("accounts.load_factor", template)

    def test_guard_template_does_not_add_guard_scan_platform_or_hours_filters(self) -> None:
        template = (REPO_ROOT / "app" / "templates" / "guard.html").read_text(encoding="utf-8")

        self.assertNotIn('name="platform"', template)
        self.assertNotIn('name="hours"', template)


if __name__ == "__main__":
    unittest.main()
