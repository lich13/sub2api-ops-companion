from __future__ import annotations

import stat
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from app.usage_query import (
    DEFAULT_NEWAPI_TEMPLATE,
    DEFAULT_SUB2API_TEMPLATE,
    UsageQueryConfig,
    UsageQueryStore,
    apply_account_credentials,
    execute_usage_query,
    is_query_due,
    should_pause_for_depleted,
)


class UsageQueryTests(unittest.TestCase):
    def test_default_templates_match_sub2api_and_newapi_shapes(self) -> None:
        self.assertIn("{{baseUrl}}/v1/usage", DEFAULT_SUB2API_TEMPLATE)
        self.assertIn('"Authorization": "Bearer {{apiKey}}"', DEFAULT_SUB2API_TEMPLATE)
        self.assertIn("response?.remaining ?? response?.quota?.remaining ?? response?.balance", DEFAULT_SUB2API_TEMPLATE)
        self.assertIn("response?.usage?.total?.actual_cost", DEFAULT_SUB2API_TEMPLATE)
        self.assertIn("{{baseUrl}}/api/user/self", DEFAULT_NEWAPI_TEMPLATE)
        self.assertIn('"New-Api-User": "{{userId}}"', DEFAULT_NEWAPI_TEMPLATE)
        self.assertIn("response.data.quota / 500000", DEFAULT_NEWAPI_TEMPLATE)

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

    def test_account_credentials_do_not_override_manual_values(self) -> None:
        config = UsageQueryConfig(
            account_id=5,
            enabled=True,
            template_type="sub2api",
            base_url="https://manual.example.com",
            api_key="sk-manual",
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

        self.assertEqual(hydrated.base_url, "https://manual.example.com")
        self.assertEqual(hydrated.api_key, "sk-manual")

    def test_legacy_default_sub2api_template_is_upgraded(self) -> None:
        legacy_template = """({
  request: {
    url: "{{baseUrl}}/v1/usage",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}"
    }
  },
  extractor: function(response) {
    const remaining = response?.remaining ?? response?.quota?.remaining ?? response?.balance;
    const total = response?.total ?? response?.quota?.total;
    const used = response?.used ?? response?.quota?.used;
    const unit = response?.unit ?? response?.quota?.unit ?? "USD";
    return {
      isValid: response?.is_active ?? response?.isValid ?? true,
      planName: response?.planName ?? response?.plan_name ?? response?.quota?.planName ?? "",
      remaining,
      total,
      used,
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

    def test_newapi_query_uses_access_token_user_id_and_converts_quota(self) -> None:
        requests: list[dict[str, Any]] = []

        def fake_opener(request: dict[str, Any], _timeout: int) -> dict[str, Any]:
            requests.append(request)
            return {"success": True, "data": {"group": "vip", "quota": 500000, "used_quota": 250000}}

        result = execute_usage_query(
            UsageQueryConfig(
                account_id=10,
                enabled=True,
                template_type="newapi",
                base_url="https://newapi.example.com",
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
        self.assertEqual(result["plan_name"], "vip")
        self.assertEqual(result["remaining"], 1.0)
        self.assertEqual(result["used"], 0.5)
        self.assertEqual(result["total"], 1.5)
        self.assertEqual(result["actual_available"], 0.5)

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
            self.assertEqual(reloaded.result(9)["actual_available"], 2)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

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
        self.assertFalse(
            is_query_due(
                config,
                {"queried_at": "2026-05-22T07:45:00+00:00"},
                now,
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
