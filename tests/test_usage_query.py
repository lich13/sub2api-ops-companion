from __future__ import annotations

import unittest
from datetime import datetime, timedelta, timezone

from app.usage_query import (
    execute_oauth_usage_query,
    oauth_quota_from_usage_data,
    oauth_quota_summary_from_result,
    oauth_recovery_transition,
)


NOW = datetime(2026, 2, 1, 8, 0, tzinfo=timezone.utc)


def account(plan: str = "plus") -> dict[str, object]:
    return {
        "id": 7,
        "name": "oauth-7",
        "platform": "openai",
        "type": "oauth",
        "credentials": {"plan_type": plan},
        "extra": {},
    }


def active_usage(five: float = 20, seven: float = 30) -> dict[str, object]:
    return {
        "five_hour": {
            "utilization": five,
            "resets_at": (NOW + timedelta(hours=5)).isoformat(),
        },
        "seven_day": {
            "utilization": seven,
            "resets_at": (NOW + timedelta(days=7)).isoformat(),
        },
    }


class OAuthUsageTests(unittest.TestCase):
    def test_active_usage_uses_x_api_key_without_bearer_header(self) -> None:
        captured: dict[str, object] = {}

        def opener(request: dict[str, object], timeout: int) -> dict[str, object]:
            captured.update(request)
            captured["timeout"] = timeout
            return {"data": active_usage()}

        result = execute_oauth_usage_query(
            7,
            "https://sub2api.example.com",
            "admin-key",
            account_row=account(),
            opener=opener,
            now=NOW,
        )

        self.assertTrue(result["success"])
        headers = captured["headers"]
        self.assertEqual(headers["x-api-key"], "admin-key")
        self.assertNotIn("Authorization", headers)
        self.assertIn("source=active&force=true", captured["url"])

    def test_current_account_plan_overrides_stale_cached_plan(self) -> None:
        cached = {
            "success": True,
            "oauth_quota": {
                "plan_type": "plus",
                "ui_windows": [
                    {"key": "codex_5h", "label": "5h", "used_percent": 20, "remaining_percent": 80},
                    {"key": "codex_7d", "label": "7d", "used_percent": 30, "remaining_percent": 70},
                ],
            },
        }

        quota = oauth_quota_summary_from_result(account("free"), cached)

        self.assertEqual(quota["plan_type"], "free")
        self.assertEqual([item["key"] for item in quota["ui_windows"]], ["codex_7d"])

    def test_active_usage_does_not_infer_plus_from_five_hour_window(self) -> None:
        quota = oauth_quota_from_usage_data(active_usage(), account("free"), now=NOW)

        self.assertEqual(quota["plan_type"], "free")
        self.assertEqual([item["key"] for item in quota["ui_windows"]], ["codex_7d"])

    def test_nonfree_recovery_requires_both_windows_and_seven_day_balance(self) -> None:
        old = oauth_quota_from_usage_data(active_usage(five=100, seven=100), account(), now=NOW)
        only_five = oauth_quota_from_usage_data(active_usage(five=0, seven=100), account(), now=NOW)
        both = oauth_quota_from_usage_data(active_usage(five=0, seven=10), account(), now=NOW)

        self.assertIsNone(oauth_recovery_transition(old, only_five, now=NOW))
        self.assertIsNotNone(oauth_recovery_transition(old, both, now=NOW))

    def test_free_recovery_only_requires_seven_day(self) -> None:
        old = oauth_quota_from_usage_data(active_usage(five=100, seven=100), account("free"), now=NOW)
        refreshed = oauth_quota_from_usage_data(active_usage(five=100, seven=10), account("free"), now=NOW)

        candidate = oauth_recovery_transition(old, refreshed, now=NOW)

        self.assertIsNotNone(candidate)
        self.assertEqual(candidate["window_labels"], ["7d"])


if __name__ == "__main__":
    unittest.main()
