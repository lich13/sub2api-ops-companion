from __future__ import annotations

import re
import unittest
from pathlib import Path

from app.sql import GUARD_BALANCE_CANDIDATES_SQL, GUARD_ERROR_EVENTS_SQL, QUALITY_SQL

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

    def test_pre_consume_token_quota_failure_is_balance_guard_signal(self) -> None:
        sample = (
            '{"error":{"code":"pre_consume_token_quota_failed",'
            '"message":"token quota is not enough, token remain quota: ＄0.060062, '
            'need quota: ＄0.198744","type":"new_api_error"}}'
        )
        self.assertIn("pre_consume_token_quota_failed", sample)
        self.assertIn("token quota is not enough", sample)
        for sql in (QUALITY_SQL, GUARD_BALANCE_CANDIDATES_SQL):
            with self.subTest(sql=sql[:20]):
                self.assertIn("search_text ILIKE '%%pre_consume_token_quota_failed%%'", sql)
                self.assertIn("search_text ILIKE '%%token quota is not enough%%'", sql)
                if "provider_balance_or_quota" in sql:
                    balance_index = sql.index("provider_balance_or_quota")
                    rate_index = sql.index("provider_rate_limit")
                    self.assertLess(sql.index("pre_consume_token_quota_failed"), balance_index)
                    self.assertLess(sql.index("token quota is not enough"), balance_index)
                    self.assertLess(balance_index, rate_index)

    def test_user_sample_quota_not_enough_remains_hard_pause_signal(self) -> None:
        sample = """
        {"error":{"code":"pre_consume_token_quota_failed","message":"token quota is not enough, token remain quota: ＄0.060062, need quota: ＄0.198744"}}
        """
        combined = QUALITY_SQL + GUARD_BALANCE_CANDIDATES_SQL
        self.assertIn("pre_consume_token_quota_failed", sample)
        self.assertIn("token quota is not enough", sample)
        self.assertIn("pre_consume_token_quota_failed", combined)
        self.assertIn("token quota is not enough", combined)
        self.assertNotIn("provider_rate_limit' THEN 'provider_balance_or_quota", combined)
        self.assertIn("provider_balance_or_quota", combined)

    def test_positive_remain_quota_is_not_a_standalone_guard_signal(self) -> None:
        sql_terms = QUALITY_SQL + GUARD_BALANCE_CANDIDATES_SQL
        self.assertNotIn("search_text ILIKE '%%RemainQuota%%'", sql_terms)
        self.assertIsNone(re.search(r"RemainQuota\s*%%", sql_terms))

    def test_guard_balance_fallback_includes_already_unschedulable_accounts(self) -> None:
        self.assertNotIn("a.schedulable = true", GUARD_BALANCE_CANDIDATES_SQL)
        self.assertIn("temp_unschedulable_reason", GUARD_BALANCE_CANDIDATES_SQL)
        self.assertIn("a.rate_limited_at IS NOT NULL", GUARD_BALANCE_CANDIDATES_SQL)
        self.assertIn("already_auto_guarded", GUARD_BALANCE_CANDIDATES_SQL)

        main_py = (REPO_ROOT / "app" / "main.py").read_text(encoding="utf-8")
        self.assertNotIn("OR temp_unschedulable_until IS NOT NULL", main_py)

    def test_guard_balance_fallback_ignores_expired_last_error(self) -> None:
        self.assertNotIn("lookback_minutes", GUARD_BALANCE_CANDIDATES_SQL)
        self.assertIn("WHERE created_at >= now() -", GUARD_BALANCE_CANDIDATES_SQL)
        self.assertIn("FROM target_logs t", GUARD_BALANCE_CANDIDATES_SQL)
        self.assertIn("%(max_age_hours)s", GUARD_BALANCE_CANDIDATES_SQL)

    def test_auto_guard_candidate_sql_excludes_oauth_accounts(self) -> None:
        self.assertIn("a.type AS account_type", GUARD_BALANCE_CANDIDATES_SQL)
        self.assertIn("lower(coalesce(a.type, '')) <> 'oauth'", GUARD_BALANCE_CANDIDATES_SQL)
        self.assertIn("a.type AS account_type", GUARD_ERROR_EVENTS_SQL)
        self.assertNotIn("lower(coalesce(a.type, '')) <> 'oauth'", GUARD_ERROR_EVENTS_SQL)

    def test_telegram_error_alert_sql_is_removed_with_public_error_chain_module(self) -> None:
        import app.sql as sql_module

        self.assertFalse(hasattr(sql_module, "TELEGRAM_ERROR_ALERTS_SQL"))


if __name__ == "__main__":
    unittest.main()
