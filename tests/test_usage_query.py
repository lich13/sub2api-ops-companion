from __future__ import annotations

import unittest
from datetime import datetime, timedelta, timezone

from app.usage_query import (
    execute_oauth_usage_query,
    oauth_quota_from_usage_data,
    oauth_quota_summary_from_result,
    oauth_recovery_transition,
    oauth_reset_at,
    oauth_windows_by_key,
    parse_iso_datetime,
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
    def test_reset_time_parser_normalizes_rfc3339_and_epoch_to_utc(self) -> None:
        epoch_seconds = int(NOW.timestamp())

        self.assertEqual(parse_iso_datetime("2026-02-01T16:00:00+08:00"), NOW)
        self.assertEqual(parse_iso_datetime(epoch_seconds), NOW)
        self.assertEqual(parse_iso_datetime(str(epoch_seconds)), NOW)
        self.assertEqual(parse_iso_datetime(epoch_seconds * 1000), NOW)
        self.assertEqual(parse_iso_datetime(str(epoch_seconds * 1000)), NOW)

    def test_reset_time_parser_rejects_non_finite_and_out_of_range_epoch(self) -> None:
        for value in (
            True,
            float("nan"),
            float("inf"),
            "-inf",
            10**100,
            "0001-01-01T00:00:00+14:00",
        ):
            with self.subTest(value=value):
                self.assertIsNone(parse_iso_datetime(value))

    def test_oauth_reset_at_keeps_legacy_interface_and_falls_back_from_invalid_explicit(self) -> None:
        self.assertEqual(
            oauth_reset_at("invalid", 90, NOW.isoformat()),
            (NOW + timedelta(seconds=90)).isoformat(),
        )

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

    def test_cached_free_summary_filters_label_only_five_hour_window(self) -> None:
        cached = {
            "success": True,
            "oauth_quota": {
                "plan_type": "plus",
                "ui_windows": [
                    {"label": "5h", "used_percent": 20},
                    {"label": "7d", "used_percent": 30},
                ],
            },
        }

        quota = oauth_quota_summary_from_result(account("free"), cached)

        self.assertEqual([item["key"] for item in quota["ui_windows"]], ["codex_7d"])

    def test_exact_server_reset_accepts_epoch_and_exposes_source(self) -> None:
        reset_at = NOW + timedelta(hours=5)
        quota = oauth_quota_from_usage_data(
            {
                "five_hour": {
                    "utilization": 100,
                    "resets_at": int(reset_at.timestamp() * 1000),
                    "remaining_seconds": 99,
                },
                "seven_day": {
                    "utilization": 20,
                    "resets_at": (NOW + timedelta(days=7)).isoformat(),
                },
            },
            account(),
            now=NOW,
        )

        five_hour = oauth_windows_by_key(quota["ui_windows"])["codex_5h"]
        self.assertEqual(five_hour["reset_at"], reset_at.isoformat())
        self.assertEqual(five_hour["reset_source"], "server_exact")
        self.assertEqual(quota["recovery_due_at"], reset_at.isoformat())
        self.assertEqual(
            quota["recovery_fingerprint"],
            f"codex_5h@{reset_at.isoformat()}",
        )
        self.assertEqual(quota["recovery_reset_source"], "server_exact")

    def test_invalid_explicit_reset_uses_remaining_seconds_from_query_time(self) -> None:
        quota = oauth_quota_from_usage_data(
            {
                "five_hour": {
                    "utilization": 100,
                    "resets_at": "not-a-reset",
                    "remaining_seconds": "120",
                },
                "seven_day": {
                    "utilization": 20,
                    "resets_at": (NOW + timedelta(days=7)).isoformat(),
                },
            },
            account(),
            now=NOW,
        )

        five_hour = oauth_windows_by_key(quota["ui_windows"])["codex_5h"]
        expected = (NOW + timedelta(seconds=120)).isoformat()
        self.assertEqual(five_hour["reset_at"], expected)
        self.assertEqual(five_hour["reset_source"], "estimated_from_remaining")
        self.assertEqual(quota["recovery_due_at"], expected)
        self.assertEqual(quota["recovery_reset_source"], "estimated_from_remaining")

    def test_invalid_remaining_seconds_uses_reset_after_seconds_alias(self) -> None:
        quota = oauth_quota_from_usage_data(
            {
                "five_hour": {
                    "utilization": 100,
                    "resets_at": "invalid",
                    "remaining_seconds": "invalid",
                    "reset_after_seconds": 45,
                }
            },
            account(),
            now=NOW,
        )

        five_hour = oauth_windows_by_key(quota["ui_windows"])["codex_5h"]
        self.assertEqual(five_hour["reset_at"], (NOW + timedelta(seconds=45)).isoformat())
        self.assertEqual(five_hour["reset_source"], "estimated_from_remaining")

    def test_fresh_window_clears_stale_reset_and_window_fields(self) -> None:
        row = account()
        row["extra"] = {
            "codex_5h_used_percent": 100,
            "codex_5h_reset_at": (NOW + timedelta(hours=5)).isoformat(),
            "codex_5h_reset_after_seconds": 18000,
            "codex_5h_window_minutes": 300,
            "codex_usage_updated_at": NOW.isoformat(),
        }

        quota = oauth_quota_from_usage_data(
            {"five_hour": {"utilization": 10, "resets_at": "invalid"}},
            row,
            now=NOW + timedelta(minutes=1),
        )

        five_hour = oauth_windows_by_key(quota["ui_windows"])["codex_5h"]
        self.assertEqual(five_hour["used_percent"], 10)
        for field in ("reset_at", "reset_source", "reset_after_seconds", "window_minutes"):
            self.assertNotIn(field, five_hour)
        self.assertNotIn("recovery_due_at", quota)
        self.assertNotIn("recovery_fingerprint", quota)

    def test_fresh_response_does_not_inherit_an_entire_missing_window(self) -> None:
        row = account()
        row["extra"] = {
            "codex_5h_used_percent": 100,
            "codex_5h_reset_at": (NOW + timedelta(hours=5)).isoformat(),
            "codex_7d_used_percent": 100,
            "codex_7d_reset_at": (NOW + timedelta(days=7)).isoformat(),
            "codex_usage_updated_at": NOW.isoformat(),
        }

        quota = oauth_quota_from_usage_data(
            {"seven_day": {"utilization": 20, "resets_at": (NOW + timedelta(days=7)).isoformat()}},
            row,
            now=NOW + timedelta(minutes=1),
        )

        self.assertEqual(set(oauth_windows_by_key(quota["ui_windows"])), {"codex_7d"})
        self.assertNotIn("recovery_due_at", quota)

    def test_recovery_metadata_uses_depleted_required_keys_and_datetime_max(self) -> None:
        five_reset = datetime(2026, 2, 1, 17, 30, tzinfo=timezone(timedelta(hours=9)))
        seven_reset = datetime(2026, 2, 1, 8, 15, tzinfo=timezone.utc)
        quota = oauth_quota_from_usage_data(
            {
                "five_hour": {"utilization": 100, "resets_at": five_reset.isoformat()},
                "seven_day": {"utilization": 100, "resets_at": seven_reset.timestamp()},
            },
            account(),
            now=NOW,
        )

        five_utc = five_reset.astimezone(timezone.utc).isoformat()
        seven_utc = seven_reset.isoformat()
        self.assertEqual(quota["recovery_due_at"], five_utc)
        self.assertEqual(
            quota["recovery_fingerprint"],
            f"codex_5h@{five_utc}|codex_7d@{seven_utc}",
        )
        self.assertEqual(quota["recovery_window_keys"], ["codex_5h", "codex_7d"])

    def test_free_recovery_metadata_ignores_depleted_five_hour_window(self) -> None:
        quota = oauth_quota_from_usage_data(
            active_usage(five=100, seven=100),
            account("free"),
            now=NOW,
        )

        seven_reset = (NOW + timedelta(days=7)).isoformat()
        self.assertEqual(quota["recovery_due_at"], seven_reset)
        self.assertEqual(quota["recovery_fingerprint"], f"codex_7d@{seven_reset}")
        self.assertEqual(quota["recovery_window_keys"], ["codex_7d"])

    def test_available_fresh_windows_remove_stale_recovery_metadata(self) -> None:
        cached = {
            "success": True,
            "oauth_quota": {
                "plan_type": "plus",
                "recovery_due_at": "2099-01-01T00:00:00+00:00",
                "recovery_fingerprint": "stale",
                "recovery_reset_source": "server_exact",
                "recovery_window_keys": ["codex_7d"],
                "ui_windows": [
                    {"key": "codex_5h", "label": "5h", "used_percent": 20},
                    {"key": "codex_7d", "label": "7d", "used_percent": 30},
                ],
            },
        }

        quota = oauth_quota_summary_from_result(account(), cached)

        for field in (
            "recovery_due_at",
            "recovery_fingerprint",
            "recovery_reset_source",
            "recovery_window_keys",
        ):
            self.assertNotIn(field, quota)

    def test_cached_invalid_reset_falls_back_to_remaining_and_updated_at(self) -> None:
        cached = {
            "success": True,
            "oauth_quota": {
                "plan_type": "plus",
                "updated_at": NOW.timestamp(),
                "ui_windows": [
                    {
                        "key": "codex_5h",
                        "label": "5h",
                        "used_percent": 100,
                        "reset_at": "invalid",
                        "reset_after_seconds": 30,
                    },
                    {"key": "codex_7d", "label": "7d", "used_percent": 30},
                ],
            },
        }

        quota = oauth_quota_summary_from_result(account(), cached)
        five_hour = oauth_windows_by_key(quota["ui_windows"])["codex_5h"]

        self.assertEqual(quota["updated_at"], NOW.isoformat())
        self.assertEqual(five_hour["reset_at"], (NOW + timedelta(seconds=30)).isoformat())
        self.assertEqual(five_hour["reset_source"], "estimated_from_remaining")

    def test_nonfree_recovery_requires_both_windows_and_seven_day_balance(self) -> None:
        old = oauth_quota_from_usage_data(active_usage(five=100, seven=100), account(), now=NOW)
        only_five = oauth_quota_from_usage_data(active_usage(five=0, seven=100), account(), now=NOW)
        both = oauth_quota_from_usage_data(active_usage(five=0, seven=10), account(), now=NOW)

        self.assertIsNone(oauth_recovery_transition(old, only_five, now=NOW))
        self.assertIsNotNone(oauth_recovery_transition(old, both, now=NOW))

    def test_recovery_transition_fingerprint_is_keyed_canonical_and_uses_time_max(self) -> None:
        previous = oauth_quota_from_usage_data(
            {
                "five_hour": {
                    "utilization": 100,
                    "resets_at": "2026-02-01T17:30:00+09:00",
                },
                "seven_day": {
                    "utilization": 100,
                    "resets_at": "2026-02-01T08:15:00Z",
                },
            },
            account(),
            now=NOW,
        )
        refreshed = oauth_quota_from_usage_data(active_usage(five=0, seven=10), account(), now=NOW)

        candidate = oauth_recovery_transition(previous, refreshed, now=NOW + timedelta(hours=1))

        self.assertIsNotNone(candidate)
        assert candidate is not None
        self.assertEqual(candidate["reset_at"], "2026-02-01T08:30:00+00:00")
        self.assertEqual(
            candidate["fingerprint"],
            "codex_5h@2026-02-01T08:30:00+00:00|codex_7d@2026-02-01T08:15:00+00:00",
        )
        self.assertEqual(candidate["window_keys"], ["codex_5h", "codex_7d"])
        self.assertEqual(candidate["reset_source"], "server_exact")

    def test_free_recovery_only_requires_seven_day(self) -> None:
        old = oauth_quota_from_usage_data(active_usage(five=100, seven=100), account("free"), now=NOW)
        refreshed = oauth_quota_from_usage_data(active_usage(five=100, seven=10), account("free"), now=NOW)

        candidate = oauth_recovery_transition(old, refreshed, now=NOW)

        self.assertIsNotNone(candidate)
        self.assertEqual(candidate["window_labels"], ["7d"])
        self.assertEqual(candidate["window_keys"], ["codex_7d"])


if __name__ == "__main__":
    unittest.main()
