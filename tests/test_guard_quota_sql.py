from __future__ import annotations

import re
import unittest
from pathlib import Path

from app.sql import GUARD_BALANCE_CANDIDATES_SQL, QUALITY_SQL, TELEGRAM_ERROR_ALERTS_SQL

REPO_ROOT = Path(__file__).resolve().parents[1]


class GuardQuotaSqlTests(unittest.TestCase):
    def test_quality_and_guard_search_full_error_payload(self) -> None:
        for sql in (QUALITY_SQL, GUARD_BALANCE_CANDIDATES_SQL):
            with self.subTest(sql=sql[:20]):
                self.assertIn("AS search_text", sql)
                self.assertIn("x.elem::text", sql)
                self.assertIn("e.upstream_errors::text", sql)
                self.assertIn("search_text ILIKE '%%额度已用尽%%'", sql)
                self.assertIn("search_text ~* 'RemainQuota[[:space:]]*=[[:space:]]*-'", sql)

    def test_new_api_quota_exhausted_sample_matches_guard_terms(self) -> None:
        sample = (
            '{"error":{"code":"","message":"[sk-Di1***uE8] 该令牌额度已用尽 '
            '!token.UnlimitedQuota && token.RemainQuota = -56679 '
            '(request id: 2026051103345959094769452hyagit)","type":"new_api_error"}}'
        )
        self.assertIn("额度已用尽", sample)
        self.assertRegex(sample, r"RemainQuota\s*=\s*-")

    def test_positive_remain_quota_is_not_a_standalone_guard_signal(self) -> None:
        sql_terms = QUALITY_SQL + GUARD_BALANCE_CANDIDATES_SQL
        self.assertNotIn("search_text ILIKE '%%RemainQuota%%'", sql_terms)
        self.assertIsNone(re.search(r"RemainQuota\s*%%", sql_terms))

    def test_guard_promotes_temporary_cooldowns_to_permanent_pause(self) -> None:
        self.assertIn("OR a.temp_unschedulable_until IS NOT NULL", GUARD_BALANCE_CANDIDATES_SQL)

        main_py = (REPO_ROOT / "app" / "main.py").read_text(encoding="utf-8")
        self.assertIn("OR temp_unschedulable_until IS NOT NULL", main_py)

    def test_telegram_error_alerts_scan_incrementally_by_error_log_id(self) -> None:
        self.assertIn("WHERE id > %(cursor_id)s::bigint", TELEGRAM_ERROR_ALERTS_SQL)
        self.assertIn("LIMIT %(limit)s", TELEGRAM_ERROR_ALERTS_SQL)
        self.assertIn("e.upstream_errors::text", TELEGRAM_ERROR_ALERTS_SQL)
        self.assertIn("WITH ORDINALITY AS x(elem, ordinality)", TELEGRAM_ERROR_ALERTS_SQL)
        self.assertIn("COALESCE(c.account_name, a.name) AS account_name", TELEGRAM_ERROR_ALERTS_SQL)


if __name__ == "__main__":
    unittest.main()
