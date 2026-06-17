from __future__ import annotations

import stat
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from app import usage_query as usage_query_module
from app.usage_query import (
    DEFAULT_NEWAPI_TEMPLATE,
    DEFAULT_SUB2API_TEMPLATE,
    UsageQueryConfig,
    UsageQueryStore,
    apply_account_credentials,
    execute_oauth_usage_query,
    execute_usage_query,
    fill_account_credentials,
    is_query_due,
    oauth_account_recovery_candidate,
    oauth_account_recovery_candidate_from_probe,
    oauth_account_recovery_early_probe_due,
    oauth_account_recovery_probe_from_reset_comparison,
    oauth_account_recovery_probe_due,
    oauth_quota_from_usage_data,
    oauth_quota_windows,
    public_config,
    should_pause_for_depleted,
)


class UsageQueryTests(unittest.TestCase):
    def test_default_templates_match_sub2api_and_newapi_shapes(self) -> None:
        self.assertIn("{{baseUrl}}/v1/usage", DEFAULT_SUB2API_TEMPLATE)
        self.assertIn('"Authorization": "Bearer {{apiKey}}"', DEFAULT_SUB2API_TEMPLATE)
        self.assertIn('"User-Agent": "cc-switch/1.0"', DEFAULT_SUB2API_TEMPLATE)
        self.assertIn("response?.remaining ?? response?.quota?.remaining ?? response?.balance", DEFAULT_SUB2API_TEMPLATE)
        self.assertIn("response?.usage?.total?.actual_cost", DEFAULT_SUB2API_TEMPLATE)
        self.assertIn("{{baseUrl}}/api/user/self", DEFAULT_NEWAPI_TEMPLATE)
        self.assertIn('"Authorization": "Bearer {{accessToken}}"', DEFAULT_NEWAPI_TEMPLATE)
        self.assertIn('"New-Api-User": "{{userId}}"', DEFAULT_NEWAPI_TEMPLATE)
        self.assertIn("data?.quota", DEFAULT_NEWAPI_TEMPLATE)
        self.assertIn("data?.used_quota", DEFAULT_NEWAPI_TEMPLATE)
        self.assertNotIn("/api/usage/token/", DEFAULT_NEWAPI_TEMPLATE)
        self.assertNotIn("tokenForUsage", DEFAULT_NEWAPI_TEMPLATE)
        self.assertNotIn("total_available", DEFAULT_NEWAPI_TEMPLATE)

    def test_sub2api_query_computes_actual_available_from_multiplier(self) -> None:
        requests: list[dict[str, Any]] = []

        def fake_opener(request: dict[str, Any], _timeout: int) -> dict[str, Any]:
            requests.append(request)
            return {"is_active": True, "balance": 12.5}

        result = execute_usage_query(
            UsageQueryConfig(
                account_id=9,
                enabled=True,
                template_type="sub2api",
                base_url="https://sub2api.example.com",
                api_key="sk-test",
                upstream_multiplier=0.5,
            ),
            opener=fake_opener,
            now=datetime(2026, 5, 22, 8, 0, tzinfo=timezone.utc),
        )

        self.assertTrue(result["success"])
        self.assertEqual(requests[0]["url"], "https://sub2api.example.com/v1/usage")
        self.assertEqual(requests[0]["headers"]["Authorization"], "Bearer sk-test")
        self.assertEqual(requests[0]["headers"]["User-Agent"], "cc-switch/1.0")
        self.assertEqual(result["remaining"], 12.5)
        self.assertEqual(result["actual_available"], 25.0)
        self.assertEqual(result["unit"], "USD")

    def test_sub2api_template_matches_ciii_usage_response_shape(self) -> None:
        result = execute_usage_query(
            UsageQueryConfig(
                account_id=5,
                enabled=True,
                template_type="sub2api",
                base_url="https://codex.ciii.club",
                api_key="sk-test",
            ),
            opener=lambda _request, _timeout: {
                "balance": 319.8202155,
                "isValid": True,
                "planName": "钱包余额",
                "remaining": 319.8202155,
                "usage": {"total": {"actual_cost": 180.1797845}},
                "unit": "USD",
            },
        )

        self.assertTrue(result["success"])
        self.assertEqual(result["plan_name"], "钱包余额")
        self.assertEqual(result["remaining"], 319.8202155)
        self.assertEqual(result["used"], 180.1797845)
        self.assertEqual(result["total"], 500.0)
        self.assertEqual(result["actual_available"], 319.8202155)

    def test_account_credentials_fill_missing_base_url_and_api_key(self) -> None:
        config = UsageQueryConfig(
            account_id=5,
            enabled=True,
            template_type="sub2api",
            use_account_credentials=True,
        )
        hydrated = apply_account_credentials(
            config,
            {
                "credentials": {
                    "base_url": "https://codex.ciii.club",
                    "api_key": "sk-from-account",
                }
            },
        )

        self.assertEqual(hydrated.base_url, "https://codex.ciii.club")
        self.assertEqual(hydrated.api_key, "sk-from-account")
        self.assertTrue(hydrated.use_account_credentials)

    def test_account_credentials_do_not_fill_access_token_from_account_row(self) -> None:
        config = UsageQueryConfig(
            account_id=5,
            enabled=True,
            template_type="newapi",
            use_account_credentials=True,
        )
        hydrated = apply_account_credentials(
            config,
            {
                "credentials": {
                    "base_url": "https://newapi.example.com",
                    "api_key": "sk-from-account",
                    "access_token": "access-token-from-account",
                }
            },
        )

        self.assertEqual(hydrated.base_url, "https://newapi.example.com")
        self.assertEqual(hydrated.api_key, "sk-from-account")
        self.assertEqual(hydrated.access_token, "")

    def test_account_credentials_override_stale_manual_base_url_and_api_key(self) -> None:
        config = UsageQueryConfig(
            account_id=5,
            enabled=True,
            template_type="sub2api",
            base_url="https://manual.example.com",
            api_key="sk-manual",
            access_token="token-from-config",
            use_account_credentials=True,
        )
        hydrated = apply_account_credentials(
            config,
            {
                "credentials": {
                    "base_url": "https://codex.ciii.club",
                    "api_key": "sk-from-account",
                }
            },
        )

        self.assertEqual(hydrated.base_url, "https://codex.ciii.club")
        self.assertEqual(hydrated.api_key, "sk-from-account")
        self.assertEqual(hydrated.access_token, "token-from-config")

    def test_account_credentials_force_live_sync_even_when_old_config_disabled_it(self) -> None:
        config = UsageQueryConfig(
            account_id=5,
            enabled=True,
            template_type="sub2api",
            base_url="https://codex.ciii.club",
            api_key="sk-from-account",
            use_account_credentials=False,
        )
        hydrated = apply_account_credentials(
            config,
            {
                "credentials": {
                    "base_url": "https://codex.ciii.club",
                    "api_key": "sk-from-account",
                }
            },
        )

        self.assertEqual(hydrated.base_url, "https://codex.ciii.club")
        self.assertEqual(hydrated.api_key, "sk-from-account")
        self.assertTrue(hydrated.use_account_credentials)

    def test_account_credentials_are_rehydrated_from_latest_account_row(self) -> None:
        config = UsageQueryConfig(
            account_id=5,
            enabled=True,
            template_type="sub2api",
            base_url="https://stale.example.com",
            api_key="sk-stale",
            use_account_credentials=True,
        )
        first = apply_account_credentials(
            config,
            {"credentials": {"base_url": "https://old-account.example.com", "api_key": "sk-old-account"}},
        )
        second = apply_account_credentials(
            config,
            {"credentials": {"base_url": "https://new-account.example.com", "api_key": "sk-new-account"}},
        )

        self.assertEqual(first.base_url, "https://old-account.example.com")
        self.assertEqual(first.api_key, "sk-old-account")
        self.assertEqual(second.base_url, "https://new-account.example.com")
        self.assertEqual(second.api_key, "sk-new-account")

    def test_fill_account_credentials_keeps_live_sync_instead_of_static_copy(self) -> None:
        config = UsageQueryConfig(
            account_id=5,
            enabled=True,
            template_type="sub2api",
            base_url="https://old.example.com",
            api_key="sk-old",
            access_token="token-from-config",
            use_account_credentials=False,
        )
        filled = fill_account_credentials(
            config,
            {
                "credentials": {
                    "base_url": "https://codex.ciii.club",
                    "api_key": "sk-from-account",
                }
            },
        )

        self.assertEqual(filled.base_url, "https://old.example.com")
        self.assertEqual(filled.api_key, "sk-old")
        self.assertNotEqual(filled.base_url, "https://codex.ciii.club")
        self.assertNotEqual(filled.api_key, "sk-from-account")
        self.assertEqual(filled.access_token, "token-from-config")
        self.assertTrue(filled.use_account_credentials)

    def test_public_config_ignores_stale_static_base_url_and_api_key(self) -> None:
        payload = public_config(
            UsageQueryConfig(
                account_id=5,
                enabled=True,
                template_type="sub2api",
                base_url="https://stale.example.com",
                api_key="sk-stale",
                access_token="token-from-config",
            )
        )

        self.assertEqual(payload["base_url"], "")
        self.assertEqual(payload["api_key"], "")
        self.assertFalse(payload["api_key_saved"])
        self.assertTrue(payload["access_token_saved"])

    def test_legacy_default_sub2api_template_is_upgraded(self) -> None:
        legacy_template = """({
    request: {
      url: "{{baseUrl}}/v1/usage",
      method: "GET",
      headers: { "Authorization": "Bearer {{apiKey}}" }
    },
    extractor: function(response) {
      const remaining = response?.remaining ?? response?.quota?.remaining ?? response?.balance;
      const unit = response?.unit ?? response?.quota?.unit ?? "USD";
      return {
        isValid: response?.is_active ?? response?.isValid ?? true,
        remaining,
        unit
      };
    }
  })"""
        config = UsageQueryConfig.from_dict(
            5,
            {
                "template_type": "sub2api",
                "code": legacy_template,
            },
        )

        self.assertIn("response?.usage?.total?.actual_cost", config.code)
        self.assertEqual(config.code, DEFAULT_SUB2API_TEMPLATE)

    def test_legacy_default_newapi_template_is_upgraded(self) -> None:
        legacy_template = """({
  request: {
    url: "{{baseUrl}}/api/user/self",
    method: "GET",
    headers: {
      "Content-Type": "application/json",
      "Authorization": "Bearer {{accessToken}}",
      "User-Agent": "cc-switch/1.0",
      "New-Api-User": "{{userId}}"
    },
  },
  extractor: function (response) {
    if (response.success && response.data) {
      return {
        planName: response.data.group || "默认套餐",
        remaining: response.data.quota / 500000,
        used: response.data.used_quota / 500000,
        total: (response.data.quota + response.data.used_quota) / 500000,
        unit: "USD",
      };
    }
    return {
      isValid: false,
      invalidMessage: response.message || "查询失败"
    };
  },
})"""

        config = UsageQueryConfig.from_dict(
            10,
            {
                "template_type": "newapi",
                "code": legacy_template,
            },
        )

        self.assertEqual(config.code, DEFAULT_NEWAPI_TEMPLATE)

    def test_legacy_newapi_token_usage_template_is_upgraded(self) -> None:
        legacy_token_template = """({
  request: { url: "{{baseUrl}}/api/usage/token/", method: "GET" },
  extractor: function(response) {
    return { remaining: response?.data?.total_available / 500000, unit: "USD" };
  },
})"""

        config = UsageQueryConfig.from_dict(
            10,
            {
                "template_type": "newapi",
                "code": legacy_token_template,
            },
        )

        self.assertEqual(config.code, DEFAULT_NEWAPI_TEMPLATE)

    def test_oauth_quota_windows_parse_plan_type_and_percent_windows(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": {
                    "plan_type": "pro",
                    "chatgpt_plan_type": "plus",
                },
                "extra": {
                    "plan_type": "team",
                    "codex_5h_used_percent": "42.5%",
                    "codex_5h_reset_at": "2026-05-25T14:30:00Z",
                    "codex_5h_window_minutes": 300,
                    "codex_7d_used_percent": 100,
                    "codex_7d_reset_at": "2026-05-29T06:00:00Z",
                    "codex_7d_reset_after_seconds": 3600,
                },
            }
        )

        self.assertEqual(summary["plan_type"], "pro")
        self.assertEqual(
            summary["ui_windows"],
            [
                {
                    "key": "codex_5h",
                    "label": "5h",
                    "used_percent": 42.5,
                    "remaining_percent": 57.5,
                    "depleted": False,
                    "reset_at": "2026-05-25T14:30:00Z",
                    "window_minutes": 300,
                },
                {
                    "key": "codex_7d",
                    "label": "7d",
                    "used_percent": 100.0,
                    "remaining_percent": 0.0,
                    "depleted": True,
                    "reset_at": "2026-05-29T06:00:00Z",
                    "reset_after_seconds": 3600,
                },
            ],
        )
        self.assertEqual(summary["telegram_windows"], [summary["ui_windows"][0]])

    def test_oauth_quota_windows_hide_free_five_hour_and_keep_reset_time(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": {"plan_type": "free"},
                "extra": {
                    "codex_5h_used_percent": 0,
                    "codex_5h_reset_at": "2026-05-25T10:00:00Z",
                    "codex_7d_used_percent": 25,
                    "codex_7d_reset_at": "2026-05-28T22:30:00Z",
                },
            }
        )

        self.assertEqual(summary["plan_type"], "free")
        self.assertEqual([window["label"] for window in summary["ui_windows"]], ["7d"])
        self.assertEqual(summary["ui_windows"][0]["remaining_percent"], 75.0)
        self.assertEqual(summary["ui_windows"][0]["reset_at"], "2026-05-28T22:30:00Z")

    def test_oauth_active_usage_does_not_promote_free_plan_from_five_hour_window(self) -> None:
        summary = oauth_quota_from_usage_data(
            {
                "five_hour": {
                    "utilization": 19,
                    "resets_at": "2026-05-29T11:24:06+08:00",
                    "remaining_seconds": 17714,
                    "window_stats": {"window_minutes": 300},
                },
                "seven_day": {
                    "utilization": 51,
                    "resets_at": "2026-06-04T11:30:55+08:00",
                    "remaining_seconds": 536523,
                    "window_stats": {"window_minutes": 10080},
                },
            },
            {"credentials": {"plan_type": "free"}, "extra": {}},
            now=datetime(2026, 5, 29, 6, 28, 51, tzinfo=timezone.utc),
        )

        self.assertEqual(summary["plan_type"], "free")
        self.assertEqual([window["label"] for window in summary["ui_windows"]], ["7d"])
        self.assertEqual(summary["ui_windows"][0]["remaining_percent"], 49.0)
        self.assertEqual(summary["ui_windows"][0]["reset_at"], "2026-06-04T11:30:55+08:00")

    def test_oauth_active_usage_keeps_five_hour_when_current_account_plan_is_plus(self) -> None:
        summary = oauth_quota_from_usage_data(
            {
                "five_hour": {
                    "utilization": 19,
                    "resets_at": "2026-05-29T11:24:06+08:00",
                    "remaining_seconds": 17714,
                    "window_stats": {"window_minutes": 300},
                },
                "seven_day": {
                    "utilization": 51,
                    "resets_at": "2026-06-04T11:30:55+08:00",
                    "remaining_seconds": 536523,
                    "window_stats": {"window_minutes": 10080},
                },
            },
            {"credentials": {"plan_type": "plus"}, "extra": {}},
            now=datetime(2026, 5, 29, 6, 28, 51, tzinfo=timezone.utc),
        )

        self.assertEqual(summary["plan_type"], "plus")
        self.assertEqual([window["label"] for window in summary["ui_windows"]], ["5h", "7d"])
        self.assertEqual(summary["ui_windows"][0]["remaining_percent"], 81.0)
        self.assertEqual(summary["ui_windows"][0]["reset_at"], "2026-05-29T11:24:06+08:00")

    def test_oauth_quota_windows_derives_reset_at_from_usage_update_and_reset_after_seconds(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": {"plan_type": "plus"},
                "extra": {
                    "codex_usage_updated_at": "2026-05-25T00:00:00+00:00",
                    "codex_5h_used_percent": 12,
                    "codex_5h_reset_after_seconds": 1800,
                    "codex_7d_used_percent": 30,
                    "codex_7d_reset_after_seconds": 3600,
                },
            }
        )

        self.assertEqual([window["label"] for window in summary["ui_windows"]], ["5h", "7d"])
        self.assertEqual(summary["ui_windows"][0]["remaining_percent"], 88.0)
        self.assertEqual(summary["ui_windows"][0]["reset_at"], "2026-05-25T00:30:00+00:00")
        self.assertEqual(summary["ui_windows"][1]["reset_at"], "2026-05-25T01:00:00+00:00")

    def test_oauth_quota_windows_fall_back_plan_type_and_clamp_remaining_percent(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": '{"plan_type": "   ", "chatgpt_plan_type": "plus"}',
                "extra": '{"plan_type": "team", "codex_5h_used_percent": 150, "codex_7d_used_percent": -5}',
            }
        )

        self.assertEqual(summary["plan_type"], "plus")
        self.assertEqual(summary["ui_windows"][0]["used_percent"], 100.0)
        self.assertEqual(summary["ui_windows"][0]["remaining_percent"], 0.0)
        self.assertEqual(summary["ui_windows"][1]["used_percent"], 0.0)
        self.assertEqual(summary["ui_windows"][1]["remaining_percent"], 100.0)
        self.assertEqual(summary["telegram_windows"], [summary["ui_windows"][1]])

    def test_oauth_quota_windows_default_plan_type_and_skip_invalid_percent_values(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": {},
                "extra": {
                    "plan_type": "",
                    "codex_5h_used_percent": "",
                    "codex_7d_used_percent": "not-a-number",
                },
            }
        )

        self.assertEqual(summary["plan_type"], "oauth")
        self.assertEqual(summary["ui_windows"], [])
        self.assertEqual(summary["telegram_windows"], [])

    def test_oauth_quota_windows_use_extra_plan_type_before_oauth_default(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": {},
                "extra": {
                    "plan_type": "team",
                    "codex_5h_used_percent": 0,
                },
            }
        )

        self.assertEqual(summary["plan_type"], "team")

    def test_oauth_recovery_candidate_requires_plus_five_hour_and_seven_day_available(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": {"plan_type": "plus"},
                "extra": {
                    "codex_5h_used_percent": 50,
                    "codex_5h_reset_at": "2026-05-25T00:00:00+00:00",
                    "codex_7d_used_percent": 100,
                    "codex_7d_reset_at": "2026-05-25T00:00:00+00:00",
                },
            }
        )

        candidate = oauth_account_recovery_candidate(
            summary,
            now=datetime(2026, 5, 25, 0, 0, 1, tzinfo=timezone.utc),
        )

        self.assertIsNone(candidate)

    def test_oauth_recovery_probe_does_not_wait_for_available_seven_day_future_reset(self) -> None:
        probe = oauth_quota_windows(
            {
                "credentials": {"plan_type": "pro"},
                "extra": {
                    "codex_5h_used_percent": 100,
                    "codex_5h_reset_at": "2026-05-25T00:00:00+00:00",
                    "codex_7d_used_percent": 10,
                    "codex_7d_reset_at": "2026-05-26T00:00:00+00:00",
                },
            }
        )

        before = oauth_account_recovery_probe_due(
            probe,
            now=datetime(2026, 5, 24, 23, 59, 59, tzinfo=timezone.utc),
        )
        after = oauth_account_recovery_probe_due(
            probe,
            now=datetime(2026, 5, 25, 0, 0, 1, tzinfo=timezone.utc),
        )

        self.assertIsNone(before)
        self.assertIsNotNone(after)
        self.assertEqual(after["window_labels"], ["5h", "7d"])
        self.assertEqual(after["trigger_window_labels"], ["5h"])
        self.assertEqual(after["fingerprint"], "2026-05-25T00:00:00+00:00")

    def test_oauth_recovery_probe_does_not_trigger_before_five_hour_reset(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": {"plan_type": "pro"},
                "extra": {
                    "codex_5h_used_percent": 100,
                    "codex_5h_reset_at": "2026-05-25T01:00:00+00:00",
                    "codex_7d_used_percent": 10,
                    "codex_7d_reset_at": "2026-05-26T00:00:00+00:00",
                },
            }
        )

        candidate = oauth_account_recovery_probe_due(
            summary,
            now=datetime(2026, 5, 25, 0, 0, 1, tzinfo=timezone.utc),
        )

        self.assertIsNone(candidate)

    def test_oauth_recovery_early_probe_checks_depleted_seven_day_before_reset_after_interval(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": {"plan_type": "pro"},
                "extra": {
                    "codex_5h_used_percent": 0,
                    "codex_5h_reset_at": "2026-05-25T00:00:00+00:00",
                    "codex_7d_used_percent": 100,
                    "codex_7d_reset_at": "2026-05-26T00:00:00+00:00",
                },
            }
        )
        recent_result = {"queried_at": "2026-05-25T00:00:30+00:00"}
        stale_result = {"queried_at": "2026-05-25T00:00:00+00:00"}

        before_interval = oauth_account_recovery_early_probe_due(
            summary,
            recent_result,
            now=datetime(2026, 5, 25, 0, 1, 0, tzinfo=timezone.utc),
            interval_seconds=60,
        )
        after_interval = oauth_account_recovery_early_probe_due(
            summary,
            stale_result,
            now=datetime(2026, 5, 25, 0, 1, 1, tzinfo=timezone.utc),
            interval_seconds=3600,
        )

        self.assertIsNone(before_interval)
        self.assertIsNotNone(after_interval)
        self.assertTrue(after_interval["early_probe"])
        self.assertEqual(after_interval["fingerprint"], "2026-05-26T00:00:00+00:00")

    def test_oauth_recovery_early_probe_does_not_skip_unrecovered_five_hour(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": {"plan_type": "pro"},
                "extra": {
                    "codex_5h_used_percent": 100,
                    "codex_5h_reset_at": "2026-05-25T02:00:00+00:00",
                    "codex_7d_used_percent": 100,
                    "codex_7d_reset_at": "2026-05-26T00:00:00+00:00",
                },
            }
        )

        candidate = oauth_account_recovery_early_probe_due(
            summary,
            {"queried_at": "2026-05-25T00:00:00+00:00"},
            now=datetime(2026, 5, 25, 0, 1, 1, tzinfo=timezone.utc),
            interval_seconds=60,
        )

        self.assertIsNone(candidate)

    def test_oauth_recovery_candidate_from_probe_uses_probe_reset_after_active_refresh(self) -> None:
        probe = oauth_quota_windows(
            {
                "credentials": {"plan_type": "plus"},
                "extra": {
                    "codex_5h_used_percent": 100,
                    "codex_5h_reset_at": "2026-05-25T00:00:00+00:00",
                    "codex_7d_used_percent": 100,
                    "codex_7d_reset_at": "2026-05-25T00:00:00+00:00",
                },
            }
        )
        refreshed = oauth_quota_windows(
            {
                "credentials": {"plan_type": "plus"},
                "extra": {
                    "codex_5h_used_percent": 0,
                    "codex_5h_reset_at": "2026-05-25T05:00:00+00:00",
                    "codex_7d_used_percent": 0,
                    "codex_7d_reset_at": "2026-06-01T00:00:00+00:00",
                },
            }
        )

        candidate = oauth_account_recovery_candidate_from_probe(
            refreshed,
            probe,
            now=datetime(2026, 5, 25, 0, 0, 1, tzinfo=timezone.utc),
        )

        self.assertIsNotNone(candidate)
        self.assertEqual(candidate["window_labels"], ["5h", "7d"])
        self.assertEqual(candidate["fingerprint"], "2026-05-25T00:00:00+00:00|2026-05-25T00:00:00+00:00")

    def test_oauth_recovery_probe_from_reset_comparison_detects_early_seven_day_reset(self) -> None:
        previous = oauth_quota_windows(
            {
                "credentials": {"plan_type": "plus"},
                "extra": {
                    "codex_5h_used_percent": 0,
                    "codex_5h_reset_at": "2026-05-25T05:00:00+00:00",
                    "codex_7d_used_percent": 100,
                    "codex_7d_reset_at": "2026-06-01T00:00:00+00:00",
                },
            }
        )
        refreshed = oauth_quota_windows(
            {
                "credentials": {"plan_type": "plus"},
                "extra": {
                    "codex_5h_used_percent": 0,
                    "codex_5h_reset_at": "2026-05-25T05:00:00+00:00",
                    "codex_7d_used_percent": 30,
                    "codex_7d_reset_at": "2026-06-01T00:00:00+00:00",
                },
            }
        )

        probe = oauth_account_recovery_probe_from_reset_comparison(
            previous,
            refreshed,
            now=datetime(2026, 5, 25, 21, 0, 0, tzinfo=timezone.utc),
        )

        self.assertIsNotNone(probe)
        assert probe is not None
        self.assertTrue(probe["early_reset_detected"])
        self.assertEqual(probe["trigger_window_labels"], ["7d"])
        self.assertEqual(probe["fingerprint"], "early:2026-06-01T00:00:00+00:00")
        self.assertEqual(probe["old_7d_used_percent"], 100)
        self.assertEqual(probe["new_7d_used_percent"], 30)

    def test_oauth_recovery_probe_from_reset_comparison_ignores_five_hour_only_reset(self) -> None:
        previous = oauth_quota_windows(
            {
                "credentials": {"plan_type": "plus"},
                "extra": {
                    "codex_5h_used_percent": 100,
                    "codex_5h_reset_at": "2026-06-01T00:00:00+00:00",
                    "codex_7d_used_percent": 100,
                    "codex_7d_reset_at": "2026-06-01T00:00:00+00:00",
                },
            }
        )
        refreshed = oauth_quota_windows(
            {
                "credentials": {"plan_type": "plus"},
                "extra": {
                    "codex_5h_used_percent": 0,
                    "codex_5h_reset_at": "2026-06-01T00:00:00+00:00",
                    "codex_7d_used_percent": 100,
                    "codex_7d_reset_at": "2026-06-01T00:00:00+00:00",
                },
            }
        )

        probe = oauth_account_recovery_probe_from_reset_comparison(
            previous,
            refreshed,
            now=datetime(2026, 5, 25, 21, 0, 0, tzinfo=timezone.utc),
        )

        self.assertIsNone(probe)

    def test_oauth_recovery_candidate_free_account_requires_only_seven_day(self) -> None:
        summary = oauth_quota_windows(
            {
                "credentials": {"plan_type": "free"},
                "extra": {
                    "codex_5h_used_percent": 100,
                    "codex_5h_reset_at": "2026-05-25T00:00:00+00:00",
                    "codex_7d_used_percent": 0,
                    "codex_7d_reset_at": "2026-05-25T01:00:00+00:00",
                },
            }
        )

        candidate = oauth_account_recovery_candidate(
            summary,
            now=datetime(2026, 5, 25, 1, 0, 1, tzinfo=timezone.utc),
        )

        self.assertIsNone(candidate)

    def test_newapi_query_prefers_user_self_when_access_token_and_user_id_are_set(self) -> None:
        requests: list[dict[str, Any]] = []

        def fake_opener(request: dict[str, Any], _timeout: int) -> dict[str, Any]:
            requests.append(request)
            return {
                "success": True,
                "data": {
                    "username": "lt",
                    "group": "default",
                    "quota": 487908,
                    "used_quota": 34934908,
                },
            }

        result = execute_usage_query(
            UsageQueryConfig(
                account_id=10,
                enabled=True,
                template_type="newapi",
                base_url="https://newapi.example.com",
                api_key="sk-newapi",
                access_token="access-token",
                user_id="42",
                upstream_multiplier=2,
            ),
            opener=fake_opener,
        )

        self.assertTrue(result["success"])
        self.assertEqual(requests[0]["url"], "https://newapi.example.com/api/user/self")
        self.assertEqual(requests[0]["headers"]["Authorization"], "Bearer access-token")
        self.assertEqual(requests[0]["headers"]["New-Api-User"], "42")
        self.assertEqual(result["plan_name"], "default")
        self.assertEqual(result["remaining"], 0.975816)
        self.assertEqual(result["used"], 69.869816)
        self.assertEqual(result["total"], 70.845632)
        self.assertEqual(result["actual_available"], 0.487908)
        self.assertEqual(result["extra"], "user_self")

    def test_oauth_active_query_calls_sub2api_admin_usage_endpoint_and_normalizes_windows(self) -> None:
        requests: list[dict[str, Any]] = []

        def fake_opener(request: dict[str, Any], _timeout: int) -> dict[str, Any]:
            requests.append(request)
            return {
                "code": 0,
                "message": "success",
                "data": {
                    "five_hour": {
                        "utilization": 12,
                        "resets_at": "2026-05-25T10:30:00Z",
                        "remaining_seconds": 1800,
                    },
                    "seven_day": {
                        "utilization": 100,
                        "resets_at": "2026-05-28T22:30:00Z",
                        "window_stats": {"window_minutes": 10080},
                    },
                },
            }

        result = execute_oauth_usage_query(
            8,
            "https://sub2api.example.com/",
            "admin-token",
            account_row={"id": 8, "credentials": {"plan_type": "plus"}, "extra": {}},
            opener=fake_opener,
            now=datetime(2026, 5, 25, 8, 0, tzinfo=timezone.utc),
        )

        self.assertTrue(result["success"])
        self.assertEqual(
            requests[0]["url"],
            "https://sub2api.example.com/api/v1/admin/accounts/8/usage?source=active&force=true",
        )
        self.assertEqual(requests[0]["headers"]["x-api-key"], "admin-token")
        self.assertNotIn("Authorization", requests[0]["headers"])
        self.assertEqual(result["template_type"], "oauth")
        self.assertEqual(result["source"], "sub2api_admin_usage")
        self.assertEqual(result["oauth_quota"]["plan_type"], "plus")
        self.assertEqual(result["oauth_quota"]["ui_windows"][0]["label"], "5h")
        self.assertEqual(result["oauth_quota"]["ui_windows"][0]["remaining_percent"], 88.0)
        self.assertEqual(result["oauth_quota"]["ui_windows"][0]["reset_at"], "2026-05-25T10:30:00Z")
        self.assertEqual(result["oauth_quota"]["telegram_windows"], [result["oauth_quota"]["ui_windows"][0]])

    def test_oauth_active_query_derives_reset_at_from_remaining_seconds(self) -> None:
        result = execute_oauth_usage_query(
            8,
            "https://sub2api.example.com",
            "admin-token",
            account_row={"id": 8, "credentials": {"plan_type": "plus"}, "extra": {}},
            opener=lambda _request, _timeout: {
                "data": {
                    "five_hour": {"utilization": 50, "remaining_seconds": 1800},
                    "seven_day": {"utilization": 50, "remaining_seconds": 3600},
                }
            },
            now=datetime(2026, 5, 25, 8, 0, tzinfo=timezone.utc),
        )

        self.assertTrue(result["success"])
        self.assertEqual(result["oauth_quota"]["ui_windows"][0]["reset_at"], "2026-05-25T08:30:00+00:00")
        self.assertEqual(result["oauth_quota"]["ui_windows"][1]["reset_at"], "2026-05-25T09:00:00+00:00")

    def test_oauth_active_query_requires_base_url_and_admin_token(self) -> None:
        result = execute_oauth_usage_query(
            8,
            "",
            "",
            now=datetime(2026, 5, 25, 8, 0, tzinfo=timezone.utc),
        )

        self.assertFalse(result["success"])
        self.assertEqual(result["template_type"], "oauth")
        self.assertIn("Sub2API", result["error"])

    def test_oauth_active_query_normalizes_http_error_code(self) -> None:
        result = execute_oauth_usage_query(
            8,
            "https://sub2api.example.com",
            "admin-token",
            opener=lambda _request, _timeout: (_ for _ in ()).throw(
                usage_query_module.UsageQueryError('HTTP 401: {"code":"INVALID_TOKEN"}')
            ),
            now=datetime(2026, 5, 25, 8, 0, tzinfo=timezone.utc),
        )

        self.assertFalse(result["success"])
        self.assertEqual(result["error_code"], "http_401")

    def test_newapi_query_rejects_token_usage_shape_without_user_quota(self) -> None:
        requests: list[dict[str, Any]] = []

        def fake_opener(request: dict[str, Any], _timeout: int) -> dict[str, Any]:
            requests.append(request)
            return {
                "success": True,
                "data": {
                    "name": "lt-token",
                    "total_granted": 5000000,
                    "total_used": 1250000,
                    "total_available": 3750000,
                    "expires_at": 1776556800,
                },
            }

        result = execute_usage_query(
            UsageQueryConfig(
                account_id=10,
                enabled=True,
                template_type="newapi",
                base_url="https://newapi.example.com",
                api_key="sk-newapi",
                access_token="access-token",
                user_id="42",
                upstream_multiplier=2,
            ),
            opener=fake_opener,
        )

        self.assertFalse(result["success"])
        self.assertEqual(requests[0]["url"], "https://newapi.example.com/api/user/self")
        self.assertEqual(requests[0]["headers"]["Authorization"], "Bearer access-token")
        self.assertEqual(requests[0]["headers"]["New-Api-User"], "42")
        self.assertIn("响应缺少 NewAPI 用户额度字段", result["error"])
        self.assertIsNone(result["remaining"])
        self.assertIsNone(result["actual_available"])

    def test_newapi_base_url_strips_openai_v1_suffix(self) -> None:
        requests: list[dict[str, Any]] = []

        def fake_opener(request: dict[str, Any], _timeout: int) -> dict[str, Any]:
            requests.append(request)
            return {"success": True, "data": {"quota": 500000, "used_quota": 250000, "group": "default"}}

        result = execute_usage_query(
            UsageQueryConfig(
                account_id=10,
                enabled=True,
                template_type="newapi",
                base_url="https://newapi.example.com/v1/",
                access_token="access-token",
                user_id="42",
            ),
            opener=fake_opener,
        )

        self.assertTrue(result["success"])
        self.assertEqual(requests[0]["url"], "https://newapi.example.com/api/user/self")
        self.assertEqual(result["remaining"], 1.0)
        self.assertEqual(result["used"], 0.5)

    def test_custom_template_runs_request_and_extractor(self) -> None:
        code = """({
  request: {
    url: "https://quota.example.com/custom",
    method: "POST",
    headers: {"X-Test": "1"},
    body: JSON.stringify({account: "a"})
  },
  extractor: function(response) {
    return {
      planName: response.plan,
      remaining: response.left,
      unit: "USD"
    };
  }
})"""

        result = execute_usage_query(
            UsageQueryConfig(account_id=11, enabled=True, template_type="custom", code=code),
            opener=lambda _request, _timeout: {"plan": "custom-plan", "left": 3},
        )

        self.assertTrue(result["success"])
        self.assertEqual(result["plan_name"], "custom-plan")
        self.assertEqual(result["actual_available"], 3.0)

    def test_invalid_extractor_result_is_saved_as_failed_query(self) -> None:
        code = """({
  request: {url: "https://quota.example.com/custom", method: "GET", headers: {}},
  extractor: function(response) { return {remaining: "bad"}; }
})"""

        result = execute_usage_query(
            UsageQueryConfig(account_id=12, enabled=True, template_type="custom", code=code),
            opener=lambda _request, _timeout: {"ok": True},
        )

        self.assertFalse(result["success"])
        self.assertIn("remaining", result["error"])
        self.assertIsNone(result["actual_available"])

    def test_store_round_trips_configs_and_restricts_file_permissions(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "usage-query-state.json"
            store = UsageQueryStore(str(path))
            store.save_usage_query_settings(
                usage_query_enabled=False,
                guard_disable_on_zero=False,
                auto_query_interval_seconds=15,
                sub2api_admin_token="admin-secret",
            )
            store.save_config(
                UsageQueryConfig(
                    account_id=9,
                    enabled=True,
                    template_type="sub2api",
                    base_url="https://sub2api.example.com",
                    api_key="secret",
                    upstream_multiplier=0.5,
                    guard_disable_on_zero=True,
                    auto_query_interval_minutes=30,
                )
            )
            store.save_result(9, {"success": True, "remaining": 1, "actual_available": 2})

            reloaded = UsageQueryStore(str(path))

            self.assertEqual(reloaded.config(9).api_key, "secret")
            self.assertEqual(reloaded.config(9).upstream_multiplier, 0.5)
            self.assertFalse(reloaded.usage_query_enabled())
            self.assertFalse(reloaded.guard_disable_on_zero())
            self.assertEqual(reloaded.auto_query_interval_seconds(), 15)
            self.assertEqual(reloaded.sub2api_admin_token(), "admin-secret")
            self.assertEqual(reloaded.result(9)["actual_available"], 2)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

            reloaded.save_usage_query_settings(sub2api_admin_token="")
            self.assertEqual(UsageQueryStore(str(path)).sub2api_admin_token(), "admin-secret")

    def test_store_migrates_legacy_global_auto_query_minutes_to_seconds(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "usage-query-state.json"
            path.write_text(
                '{"configs": {}, "results": {}, "settings": {"auto_query_interval_minutes": 15}}',
                encoding="utf-8",
            )

            reloaded = UsageQueryStore(str(path))

            self.assertTrue(reloaded.usage_query_enabled())
            self.assertTrue(reloaded.guard_disable_on_zero())
            self.assertEqual(reloaded.auto_query_interval_seconds(), 900)

    def test_store_preserves_existing_results_when_saving_config_from_stale_instance(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "usage-query-state.json"
            first = UsageQueryStore(str(path))
            second = UsageQueryStore(str(path))

            first.save_result(9, {"success": True, "actual_available": 8})
            second.save_config(UsageQueryConfig(account_id=9, enabled=True))

            reloaded = UsageQueryStore(str(path))

            self.assertEqual(reloaded.result(9)["actual_available"], 8)
            self.assertTrue(reloaded.config(9).enabled)

    def test_due_and_depleted_helpers_only_pause_successful_zero_available_results(self) -> None:
        config = UsageQueryConfig(account_id=9, enabled=True, auto_query_interval_minutes=30)
        now = datetime(2026, 5, 22, 8, 0, tzinfo=timezone.utc)

        self.assertTrue(is_query_due(config, {}, now))
        self.assertFalse(is_query_due(config, {}, now, interval_seconds=0))
        self.assertFalse(
            is_query_due(
                config,
                {"queried_at": "2026-05-22T07:45:00+00:00"},
                now,
                interval_seconds=1800,
            )
        )
        self.assertTrue(
            is_query_due(
                UsageQueryConfig(account_id=9, enabled=True, auto_query_interval_minutes=1440),
                {"queried_at": "2026-05-22T07:44:59+00:00"},
                now,
                interval_seconds=900,
            )
        )
        self.assertTrue(
            is_query_due(
                config,
                {"queried_at": "2026-05-22T07:29:59+00:00"},
                now,
            )
        )
        self.assertTrue(should_pause_for_depleted({"success": True, "actual_available": 0}))
        self.assertTrue(should_pause_for_depleted({"success": True, "actual_available": -0.1}))
        self.assertFalse(should_pause_for_depleted({"success": True, "actual_available": 0.1}))
        self.assertFalse(should_pause_for_depleted({"success": False, "actual_available": 0}))


if __name__ == "__main__":
    unittest.main()
