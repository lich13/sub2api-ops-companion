from __future__ import annotations

import unittest

from app.guard_classifier import classify_guard_event


def event(**overrides: object) -> dict[str, object]:
    base: dict[str, object] = {
        "account_id": 9,
        "status_code": 403,
        "kind": "http_error:request_body_truncated",
        "error_owner": "provider",
        "error_source": "upstream",
        "message": "",
        "search_text": "",
    }
    base.update(overrides)
    return base


class GuardClassifierTests(unittest.TestCase):
    def test_pre_consume_quota_is_balance_before_rate_limit(self) -> None:
        row = event(
            status_code=403,
            search_text='{"code":"pre_consume_token_quota_failed","message":"token quota is not enough"}',
        )
        self.assertEqual(classify_guard_event(row), "provider_balance_or_quota")

    def test_positive_remain_quota_is_not_balance_signal(self) -> None:
        self.assertEqual(classify_guard_event(event(kind="http_error", search_text="RemainQuota = 12.30")), "account_other_error")

    def test_negative_remain_quota_is_balance_signal(self) -> None:
        self.assertEqual(classify_guard_event(event(search_text="RemainQuota = -0.01")), "provider_balance_or_quota")

    def test_client_bad_request_never_counts_against_account(self) -> None:
        self.assertEqual(classify_guard_event(event(status_code=400, search_text="Input must be a list")), "client_bad_request")

    def test_retryable_provider_categories(self) -> None:
        self.assertEqual(classify_guard_event(event(status_code=429, search_text="rate limit")), "provider_rate_limit")
        self.assertEqual(classify_guard_event(event(status_code=500, search_text="upstream reset")), "upstream_unstable_5xx_stream")
        self.assertEqual(classify_guard_event(event(status_code=403, search_text="blocked by policy")), "provider_blocked_403")


if __name__ == "__main__":
    unittest.main()
