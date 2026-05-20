from __future__ import annotations

import unittest

from app.sql import GUARD_ERROR_EVENTS_SQL, GUARD_ERROR_EVENTS_SQL_COMPAT_NO_LOAD_FACTOR, GUARD_SUCCESS_EVENTS_SQL


class GuardEventSqlTests(unittest.TestCase):
    def test_guard_error_events_are_cursor_based_and_expand_attempts(self) -> None:
        self.assertIn("WITH target_logs AS", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("id > %(cursor_id)s::bigint", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("jsonb_array_elements", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("error_log_id", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("attempt_no", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("a.type AS account_type", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("account_priority", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("load_factor", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("LIMIT %(limit)s::int", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("ORDER BY e.id ASC", GUARD_ERROR_EVENTS_SQL)

    def test_guard_error_events_include_full_payload_search_text(self) -> None:
        self.assertIn("x.elem::text", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("e.upstream_errors::text", GUARD_ERROR_EVENTS_SQL)
        self.assertIn("upstream_response_body", GUARD_ERROR_EVENTS_SQL)

    def test_guard_success_events_are_incremental(self) -> None:
        self.assertIn("usage_logs", GUARD_SUCCESS_EVENTS_SQL)
        self.assertIn("u.created_at > %(cursor_created_at)s::timestamptz", GUARD_SUCCESS_EVENTS_SQL)
        self.assertIn("success_event_key", GUARD_SUCCESS_EVENTS_SQL)
        self.assertIn("u.account_id IS NOT NULL", GUARD_SUCCESS_EVENTS_SQL)
        self.assertIn("a.type AS account_type", GUARD_SUCCESS_EVENTS_SQL)

    def test_guard_incremental_events_are_not_time_window_filtered(self) -> None:
        combined = GUARD_ERROR_EVENTS_SQL + GUARD_SUCCESS_EVENTS_SQL
        self.assertNotIn("lookback_minutes", combined)
        self.assertNotIn("created_at >= now() -", combined)

    def test_compat_guard_error_sql_does_not_select_missing_load_factor_column(self) -> None:
        self.assertIn("NULL::integer AS load_factor", GUARD_ERROR_EVENTS_SQL_COMPAT_NO_LOAD_FACTOR)
        self.assertNotIn("a.load_factor", GUARD_ERROR_EVENTS_SQL_COMPAT_NO_LOAD_FACTOR)


if __name__ == "__main__":
    unittest.main()
