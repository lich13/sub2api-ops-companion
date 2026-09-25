from __future__ import annotations

import unittest
from datetime import datetime, timedelta, timezone

from pydantic import ValidationError

from app.desktop_actions import PriorityRequest, TestRequest, billing_is_fresh


class DesktopActionContractTests(unittest.TestCase):
    def test_priority_is_strict_and_bounded(self) -> None:
        base = {"expected_version": "a" * 64}
        self.assertEqual(PriorityRequest(priority=0, **base).priority, 0)
        with self.assertRaises(ValidationError):
            PriorityRequest(priority=True, **base)
        with self.assertRaises(ValidationError):
            PriorityRequest(priority=-1, **base)

    def test_test_request_requires_explicit_confirmation_for_route_layer(self) -> None:
        request = TestRequest(expected_version="a" * 64, mode="image", model_id="gpt-image-1")
        self.assertFalse(request.confirmed)
        with self.assertRaises(ValidationError):
            TestRequest(expected_version="a" * 64, mode="unknown")

    def test_grok_batch_accepts_only_fresh_complete_billing(self) -> None:
        queried = datetime(2026, 9, 25, 8, 0, tzinfo=timezone.utc)
        payload = {"grok_billing": {"fetched_at": queried.isoformat(), "partial": False}}
        self.assertTrue(billing_is_fresh(payload, queried.isoformat()))
        self.assertFalse(billing_is_fresh({"grok_billing": {"fetched_at": (queried - timedelta(seconds=1)).isoformat()}}, queried.isoformat()))
        self.assertFalse(billing_is_fresh({"grok_billing": {"fetched_at": queried.isoformat(), "partial": True}}, queried.isoformat()))


if __name__ == "__main__":
    unittest.main()
