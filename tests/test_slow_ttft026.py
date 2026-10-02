from __future__ import annotations

import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace

from app.account_quality import calculate, failure_cause, slow_ttft_sample, slow_ttft_warning
from app.capacity_alerts import CapacityAlerts
from app.error_evidence import ERROR_WHERE, is_local_throttle, match_message


NOW = datetime(2026, 10, 1, 12, tzinfo=timezone.utc)


def sample(i: int, first: float, *, at: datetime | None = None, account_id: int = 1, **changes: object) -> dict:
    row = {
        "id": i,
        "account_id": account_id,
        "created_at": (at or NOW - timedelta(minutes=i)).isoformat(),
        "model": "gpt-6-luna",
        "upstream_model": "gpt-6-luna",
        "stream": True,
        "first_token_ms": first,
        "duration_ms": max(first + 1000, 12000),
        "output_tokens": 64,
        "inbound_endpoint": "/v1/responses",
        "image_count": 0,
        "image_output_tokens": 0,
        "video_count": 0,
        "video_duration_seconds": 0,
    }
    row.update(changes)
    return row


class SlowTTFTTests(unittest.TestCase):
    def test_threshold_and_latest_fast_reset(self) -> None:
        account = {"id": 1, "platform": "openai", "type": "oauth"}
        rows = [slow_ttft_sample(sample(i, 11001 if i < 8 else 9000), account) for i in range(10)]
        self.assertTrue(all(rows))
        warning = slow_ttft_warning(rows, NOW)
        self.assertIsNotNone(warning)
        self.assertEqual(warning["slow_count"], 8)
        self.assertTrue(warning["active"])
        rows[0] = slow_ttft_sample(sample(0, 10000), account)
        warning = slow_ttft_warning(rows, NOW)
        self.assertEqual(warning["slow_count"], 7)
        self.assertFalse(warning["active"])

    def test_exact_eight_requires_latest_slow_and_ten_samples(self) -> None:
        account = {"id": 1, "platform": "openai", "type": "apikey"}
        rows = [slow_ttft_sample(sample(i, 11001 if i < 8 else 9000), account) for i in range(10)]
        self.assertTrue(slow_ttft_warning(rows, NOW)["active"])
        self.assertIsNone(slow_ttft_warning(rows[:9], NOW))
        rows[9] = slow_ttft_sample(sample(9, 11001, at=NOW - timedelta(days=2)), account)
        self.assertIsNone(slow_ttft_warning(rows, NOW))

    def test_valid_sample_exclusions_and_platform_isolation(self) -> None:
        self.assertIsNone(slow_ttft_sample(sample(1, 11001, stream=False), {"id": 1, "platform": "openai", "type": "oauth"}))
        self.assertIsNone(slow_ttft_sample(sample(1, 11001, image_count=1), {"id": 1, "platform": "openai", "type": "oauth"}))
        self.assertIsNone(slow_ttft_sample(sample(1, 11001, duration_ms=10000), {"id": 1, "platform": "openai", "type": "oauth"}))
        self.assertIsNone(slow_ttft_sample(sample(1, 11001), {"id": 1, "platform": "grok", "type": "oauth"}))

    def test_calculate_exposes_only_active_warning(self) -> None:
        accounts = [{"id": 1, "platform": "openai", "type": "oauth", "deleted_at": None}]
        usage = [sample(i, 11001 if i < 8 else 9000) for i in range(10)]
        result = calculate(accounts, usage, [], NOW)[0][1]
        self.assertEqual(result["warnings"][0]["kind"], "slow_ttft")
        self.assertTrue(result["warnings"][0]["active"])


class LocalThrottleTests(unittest.TestCase):
    def test_structured_local_limiter_is_excluded_but_request_body_is_not(self) -> None:
        text = "您已达到请求数限制：1分钟内最多请求 15 次 (request id: req-1)"
        for field in ("message", "error_message", "upstream_error_message"):
            self.assertTrue(is_local_throttle({field: text}))
            self.assertFalse(is_local_throttle({field: json.dumps({"request": {"error": {"message": text}}})}))
        self.assertTrue(is_local_throttle({"message": "您已达到请求数限制：每分钟最多请求 15 次"}))
        for text in (
            "您 已 超过 请求数量 限制 - 每 1 分钟 至多 请求 15 次",
            "You have reached the rate limit: 15 requests per minute",
            "You've exceeded the request rate limit: maximum 15 requests / minute",
            "maximum number of requests: 15 requests per minute",
        ):
            self.assertTrue(is_local_throttle({"message": text}))
        self.assertTrue(is_local_throttle({"error_body": json.dumps({"error": {"message": text}})}))
        self.assertTrue(is_local_throttle({"upstream_errors": [{"message": text}]}))
        self.assertFalse(is_local_throttle({"message": "Rate limit exceeded", "upstream_status_code": 429}))

    def test_local_limiter_does_not_match_real_capacity_error(self) -> None:
        row = {"account_platform": "openai", "account_type": "oauth", "error_owner": "provider",
               "error_phase": "upstream", "upstream_error_message": "Our servers are currently overloaded. Please try again later."}
        self.assertIsNotNone(match_message(row))
        self.assertIsNotNone(ERROR_WHERE)
        self.assertIsNone(failure_cause({"status_code": 429, "message": "您已达到请求数限制：1分钟内最多请求 15 次"}))
        self.assertEqual(failure_cause({"status_code": 429, "message": "Rate limit exceeded"}), "rate")
        base = {"account_platform": "openai", "account_type": "oauth", "error_owner": "provider", "error_phase": "upstream"}
        self.assertEqual(match_message({**base, "upstream_errors": [{"message": "Our servers are currently overloaded. Please try again later."}]}), "Our servers are currently overloaded. Please try again later.")
        self.assertEqual(match_message({**base, "upstream_errors": [{"message": "您已达到请求数限制：每分钟最多请求 15 次"}, {"message": "Selected model is at capacity. Please try a different model."}]}), "Selected model is at capacity. Please try a different model.")


class SlowAlertIntegrationTests(unittest.TestCase):
    def test_capacity_alert_queues_and_delivers_one_slow_stage(self) -> None:
        class Db:
            def __init__(self) -> None:
                self.usage = [sample(i, 11001 if i < 8 else 9000, account_id=1) | {
                    "account_name": "tmq", "account_platform": "openai", "account_type": "oauth"} for i in range(10)]

            def fetch_one(self, sql, params=None):
                if "coalesce(max(id)" in sql:
                    return {"id": 0}
                if "FROM accounts" in sql:
                    return {"platform": "openai", "type": "oauth", "deleted_at": None}
                return None

            def fetch_all(self, sql, params=None):
                if "FROM usage_logs" in sql:
                    return list(self.usage)
                return []

        class Notifier:
            def __init__(self) -> None:
                self.sent = []

            def runtime_config(self):
                return SimpleNamespace(enabled=True, config_valid=True)

            def push(self, title, body, *, timeout, options):
                self.sent.append((title, body, options))
                return SimpleNamespace(success=True, error_code=None)

        with tempfile.TemporaryDirectory() as root:
            clock_value = NOW
            notifier = Notifier()
            settings = SimpleNamespace(usage_query_state_path=str(Path(root) / "usage.json"), audit_path=str(Path(root) / "audit.jsonl"))
            alerts = CapacityAlerts(settings, Db(), notifier, clock=lambda: clock_value)
            alerts.poll()  # initialize the error cursor without replaying old records
            alerts.poll()
            state = alerts.store.snapshot()
            self.assertEqual(len(state["slow_pending"]), 1)
            alerts.deliver_due()
            self.assertEqual(len(notifier.sent), 1)
            self.assertEqual(alerts.store.snapshot()["slow_pending"], {})


if __name__ == "__main__":
    unittest.main()
