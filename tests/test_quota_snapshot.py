from __future__ import annotations

import json
import unittest
from datetime import datetime, timedelta, timezone

from app.quota_snapshot import FRESHNESS_SECONDS, latest_openai_result, usage_windows
from app.key_fallback import AVAILABLE, UNKNOWN, classify_oauth_account, latest_completed_oauth_result
from app.oauth_monitor import build_monitor_candidates

NOW = datetime(2026, 9, 24, 12, tzinfo=timezone.utc)


def account(**extra):
    return {"id": 1, "platform": "openai", "type": "oauth", "status": "active", "schedulable": True,
            "credentials": {"plan_type": "plus"}, "extra": {
                "codex_usage_updated_at": NOW.isoformat(), "codex_5h_used_percent": 0,
                "codex_7d_used_percent": 20, "codex_5h_reset_at": (NOW+timedelta(hours=2)).isoformat(),
                "codex_7d_reset_at": (NOW+timedelta(days=2)).isoformat(), **extra}}


class PassiveQuotaTests(unittest.TestCase):
    def test_unknown_and_zero_are_distinct(self):
        windows = usage_windows(account(codex_7d_used_percent=None), NOW)
        self.assertEqual((windows[0]["used_percent"], windows[0]["status"]), (0, "known"))
        self.assertEqual((windows[1]["used_percent"], windows[1]["status"]), (None, "unknown"))

    def test_free_plan_has_only_seven_day_window_and_no_raw_data(self):
        row = account(secret="never-return"); row["quota_plan_type"] = "free"
        windows = usage_windows(row, NOW)
        self.assertEqual([w["key"] for w in windows], ["codex_7d"])
        self.assertNotIn("never-return", json.dumps(windows))

    def test_newer_error_wins_over_old_success_and_passive_snapshot(self):
        row = account(codex_usage_updated_at=(NOW-timedelta(minutes=3)).isoformat())
        old = latest_openai_result(row, None, NOW)
        newest = latest_completed_oauth_result(old, {"last_error_at": NOW.isoformat(), "last_error_code": "http_500"})
        self.assertIs(latest_openai_result(row, newest, NOW), newest)
        self.assertEqual(classify_oauth_account(row, newest, NOW, FRESHNESS_SECONDS), UNKNOWN)
        self.assertTrue(all(w["status"] == "error" for w in usage_windows(row, NOW, newest)))

    def test_new_passive_snapshot_replaces_old_active_result(self):
        row = account()
        newest = latest_openai_result(row, {"success": False, "queried_at": (NOW-timedelta(seconds=1)).isoformat()}, NOW)
        self.assertEqual(classify_oauth_account(row, newest, NOW, FRESHNESS_SECONDS), AVAILABLE)

    def test_stale_incomplete_future_and_expired_snapshots_do_not_enable_fallback(self):
        for fields in ({"codex_usage_updated_at": (NOW-timedelta(seconds=3601)).isoformat()},
                       {"codex_7d_used_percent": None},
                       {"codex_usage_updated_at": (NOW+timedelta(seconds=1)).isoformat()}):
            row = account(**fields)
            self.assertEqual(classify_oauth_account(row, latest_openai_result(row, None, NOW), NOW, 3600), UNKNOWN)
        self.assertEqual(usage_windows(account(codex_5h_reset_at=NOW.isoformat()), NOW)[0]["status"], "stale")

    def test_passive_exhaustion_schedules_active_recovery_at_reset(self):
        row = account(codex_7d_used_percent=100, codex_7d_reset_at=NOW.isoformat())
        candidates = build_monitor_candidates([row], {}, {}, NOW)
        self.assertEqual([(c["account_id"], c["reason"]) for c in candidates], [(1, "exact_reset")])
        self.assertEqual(build_monitor_candidates([account()], {}, {}, NOW), [])

    def test_grok_requests_and_tokens_use_observed_headers_only(self):
        row = {"platform": "grok", "type": "oauth", "extra": {"grok_usage_snapshot": {
            "requests": {"limit": 100, "remaining": 56}, "tokens": {"limit": 100000, "remaining": 75000},
            "headers": {"authorization":"never-return"}, "updated_at":NOW.isoformat(), "status_code":200}}}
        windows = usage_windows(row, NOW)
        self.assertEqual((windows[0]["used_percent"], windows[0]["status"]), (44, "known"))
        self.assertEqual((windows[1]["used_percent"], windows[1]["status"]), (25, "known"))
        self.assertEqual((windows[1]["used"], windows[1]["remaining"], windows[1]["limit"]), (25000,75000,100000))
        self.assertNotIn("never-return", json.dumps(windows))
        self.assertEqual(usage_windows({"platform": "grok", "type": "oauth"}, NOW)[0]["status"], "unknown")

    def test_grok_error_stale_and_incomplete_are_not_fresh(self):
        for kind in ('oauth','apikey'):
            row = {"platform":"grok","type":kind,"extra":{"grok_usage_snapshot":{
                "requests":{"limit":100,"remaining":0},"tokens":{"limit":100},"updated_at":NOW.isoformat(),"status_code":200}}}
            self.assertEqual([(w['used_percent'],w['status']) for w in usage_windows(row,NOW)],[(100,'known'),(None,'unknown')])
            self.assertEqual(usage_windows(row,NOW+timedelta(hours=2))[0]['status'],'stale')
            row['extra']['grok_usage_snapshot']['status_code']=522
            self.assertEqual(usage_windows(row,NOW)[0]['status'],'error')

    def test_billing_and_money_budgets_are_not_usage_windows(self):
        extra={"quota_daily_limit":100,"quota_daily_used":25,"grok_billing_snapshot":{"usage_percent":10}}
        self.assertEqual(usage_windows({"platform":"openai","type":"apikey","extra":extra},NOW),[])
        windows=usage_windows({"platform":"grok","type":"apikey","extra":extra},NOW)
        self.assertTrue(all(w['used_percent'] is None for w in windows))
