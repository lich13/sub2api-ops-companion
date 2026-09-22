from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

from app.model_guard import (
    ModelGuard,
    PriceCatalog,
    classify_model_event,
    normalize_model,
    precise_mapping_key,
)


class ModelGuardUnitTests(unittest.TestCase):
    def setUp(self) -> None:
        self.catalog = PriceCatalog.from_payload(
            {
                "source_sha256": "price-sha",
                "prices": {
                    "gpt-6-astra": {"input_price": 10, "output_price": 50, "unit": "token"},
                    "gpt-5.6-sol": {"input_price": 5, "output_price": 30, "unit": "token"},
                    "gpt-5.6-luna": {"input_price": 2, "output_price": 12, "unit": "token"},
                },
            }
        )

    def test_date_aliases_are_normalized(self) -> None:
        self.assertEqual(normalize_model("gpt-6-astra@2026-09-21"), "gpt-6-astra")
        self.assertEqual(normalize_model("gpt-6-astra:20260921"), "gpt-6-astra")

    def test_further_downgrade_is_confirmed(self) -> None:
        result = classify_model_event(
            {"id": 10, "account_id": 7, "requested_model": "gpt-6-astra", "upstream_model": "gpt-5.6-sol", "upstream_response_model": "gpt-5.6-luna"},
            self.catalog,
        )
        self.assertEqual(result["status"], "confirmed")

    def test_intended_mapping_is_not_anomaly(self) -> None:
        result = classify_model_event(
            {"id": 10, "account_id": 7, "requested_model": "gpt-6-astra", "upstream_model": "gpt-5.6-sol", "upstream_response_model": "gpt-5.6-sol"},
            self.catalog,
        )
        self.assertEqual(result["status"], "ok")

    def test_unknown_and_missing_response_are_distinct(self) -> None:
        unknown = classify_model_event(
            {"id": 1, "account_id": 7, "model": "gpt-6-astra", "upstream_model": "gpt-5.6-sol", "upstream_response_model": "mystery"},
            self.catalog,
        )
        missing = classify_model_event(
            {"id": 2, "account_id": 7, "model": "gpt-6-astra", "upstream_model": "gpt-5.6-sol"},
            self.catalog,
        )
        self.assertEqual(unknown["status"], "unconfirmed")
        self.assertEqual(missing["status"], "missing")

    def test_missing_upstream_model_never_confirms_downgrade(self) -> None:
        result = classify_model_event(
            {"id": 3, "account_id": 7, "model": "gpt-6-astra", "upstream_response_model": "gpt-5.6-luna"},
            self.catalog,
        )
        self.assertEqual(result["status"], "unconfirmed")

    def test_precise_mapping_fails_closed_for_wildcard_or_last_entry(self) -> None:
        row = {"credentials": {"model_mapping": {"gpt-6-astra": "gpt-5.6-sol", "gpt-5.6-sol": "gpt-5.6-sol"}}}
        self.assertEqual(precise_mapping_key(row, "gpt-6-astra", "gpt-5.6-sol"), "gpt-6-astra")
        self.assertIsNone(precise_mapping_key({"credentials": {"model_mapping": {"gpt-*": "gpt-5.6-sol", "x": "y"}}}, "gpt-6-astra", "gpt-5.6-sol"))
        self.assertIsNone(precise_mapping_key({"credentials": {"model_mapping": {"gpt-6-astra": "gpt-5.6-sol"}}}, "gpt-6-astra", "gpt-5.6-sol"))

    def test_disabled_guard_does_not_query_logs(self) -> None:
        class DB:
            def fetch_all(self, *_args, **_kwargs):
                raise AssertionError("disabled guard queried logs")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            settings = SimpleNamespace(
                model_guard_config_path=str(root / "config.json"),
                model_guard_state_path=str(root / "state.json"),
                model_guard_pricing_path=str(root / "pricing.json"),
                audit_path=str(root / "audit.jsonl"),
            )
            controller = ModelGuard(settings, DB(), catalog=self.catalog)
            self.assertTrue(controller.run_once()["skipped"])


if __name__ == "__main__":
    unittest.main()
