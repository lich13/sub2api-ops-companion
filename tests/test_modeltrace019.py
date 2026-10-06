from __future__ import annotations

import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock

from app.model_test_stream import Collector, TestFailure, failure
from app.modeltrace import analyze, bank, validate
from app.model_tests import ModelTests


class ModelTrace019Tests(unittest.TestCase):
    def test_bank_is_pinned_and_valid_sequence_scores(self):
        _, version = bank()
        self.assertRegex(version["sha256"], r"^[0-9a-f]{64}$")
        values = ",".join(str((i * 71) % 355 + 1) for i in range(80))
        self.assertEqual(len(validate(values)), 80)
        self.assertIsNotNone(analyze([values]))

    def test_thought_tags_and_fences_are_removed_without_scoring_prose(self):
        values = " ".join(str((i * 13) % 355 + 1) for i in range(80))
        self.assertEqual(validate(f"<think>not scored 123</think>```text\n{values}\n```"),
                         validate(values))
        self.assertEqual(validate("1, two, 3"), [])

    def test_collector_merges_delta_snapshot_and_return_model(self):
        collector = Collector(80)
        collector.accept({"type": "response.output_item.added", "output_index": 0,
                          "item": {"phase": "final_answer"}})
        collector.accept({"type": "response.output_text.delta", "output_index": 0, "delta": "1,2"})
        collector.accept({"type": "response.output_text.done", "output_index": 0, "text": "1,2"})
        collector.accept({"type": "response.completed", "response": {"model": "gpt-6-luna", "status": "completed"}})
        self.assertEqual(collector.text, "1,2")
        self.assertEqual(collector.returned_model, "gpt-6-luna")
        self.assertTrue(collector.completed)

    def test_reasoning_is_not_scored_as_final_answer(self):
        collector = Collector(80)
        collector.accept({"type": "response.output_item.added", "output_index": 0,
                          "item": {"type": "reasoning", "phase": "commentary"}})
        collector.accept({"type": "reasoning.delta", "output_index": 0, "delta": "1 2 3"})
        self.assertEqual(collector.text, "")

    def test_integer_and_bytes_guards(self):
        collector = Collector(2)
        with self.assertRaises(TestFailure) as ctx:
            collector.accept({"type": "response.output_text.delta", "delta": "1 2 3 4 5 "})
        self.assertEqual(ctx.exception.code, "output_limit")
        self.assertEqual(len(Collector(80).text), 0)

    def test_hard_errors_are_not_retryable(self):
        self.assertFalse(failure(401).retryable)
        self.assertFalse(failure(429).retryable)
        self.assertTrue(failure(503).retryable)

    def test_target_bypasses_account_model_mapping(self):
        service = SimpleNamespace(actions=SimpleNamespace(model_allowed=AsyncMock(return_value=True)),
                                  r=SimpleNamespace(db=SimpleNamespace(fetch_one=AsyncMock())))
        tests = ModelTests.__new__(ModelTests)
        tests.s = service
        row = {
            "id": 387, "name": "fixture-slow", "platform": "openai", "type": "apikey",
            "parent_account_id": None, "credentials": {
                "api_key": "secret", "base_url": "https://api.example.test/v1",
                "model_mapping": {"gpt-6-luna": "gpt-6-astra"},
            }, "extra": {}, "proxy_id": None,
        }
        import asyncio
        target, _ = asyncio.run(tests.target(row, "gpt-6-luna"))
        self.assertEqual(target["model"], "gpt-6-luna")

    def test_oauth_target_uses_shared_codex_user_agent_and_version(self):
        class Db:
            def fetch_all(self, sql, params):
                return [
                    {"key": "openai_codex_user_agent", "value": "codex-tui/0.146.1 (macOS 15.6; arm64) iTerm2 (codex-tui; 0.146.1)"},
                    {"key": "openai_codex_client_version", "value": "0.160.0"},
                ]

        service = SimpleNamespace(actions=SimpleNamespace(model_allowed=AsyncMock(return_value=True)),
                                  r=SimpleNamespace(db=Db()))
        tests = ModelTests.__new__(ModelTests)
        tests.s = service
        row = {
            "id": 387, "name": "fixture-slow", "platform": "openai", "type": "oauth",
            "parent_account_id": None, "credentials": {"access_token": "secret"},
            "extra": {}, "proxy_id": None,
        }
        import asyncio
        target, _ = asyncio.run(tests.target(row, "gpt-6-luna"))
        self.assertEqual(target["headers"]["Version"], "0.160.0")
        self.assertEqual(target["headers"]["User-Agent"], "codex-tui/0.160.0 (macOS 15.6; arm64) iTerm2 (codex-tui; 0.160.0)")


if __name__ == "__main__":
    unittest.main()
