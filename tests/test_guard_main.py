from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any

os.environ.setdefault("OPS_SESSION_SECRET", "test-session-secret")
os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")

from app import main as main_module
from app.guard_store import GuardStore


class FakeCapabilityDB:
    def fetch_one(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
        return {"account_priority_column_exists": True, "account_load_factor_column_exists": False}


class GuardMainTests(unittest.TestCase):
    def setUp(self) -> None:
        self.original_settings = main_module.settings
        self.original_db = main_module.db
        self.original_guard_engine = main_module.guard_engine
        self.original_scheduled_test_capability = main_module.scheduled_test_capability
        self.original_load_recovery_rows = main_module.load_scheduled_test_recovery_alert_rows
        self.original_run_guard_balance_fallback = main_module.run_guard_balance_fallback
        self.original_guard_state = dict(main_module.guard_state)
        self.tmpdir = tempfile.TemporaryDirectory()
        data_dir = Path(self.tmpdir.name)
        main_module.settings = SimpleNamespace(
            audit_path=str(data_dir / "audit.jsonl"),
            base_path="/sub2ops",
            guard_state_path=str(data_dir / "guard-state.json"),
            guard_event_batch_size=100,
            guard_lookback_minutes=60,
        )
        main_module.db = FakeCapabilityDB()  # type: ignore[assignment]

    def tearDown(self) -> None:
        main_module.settings = self.original_settings
        main_module.db = self.original_db
        main_module.guard_engine = self.original_guard_engine
        main_module.scheduled_test_capability = self.original_scheduled_test_capability
        main_module.load_scheduled_test_recovery_alert_rows = self.original_load_recovery_rows
        main_module.run_guard_balance_fallback = self.original_run_guard_balance_fallback
        main_module.guard_state.clear()
        main_module.guard_state.update(self.original_guard_state)
        self.tmpdir.cleanup()

    def test_recovery_cursor_write_preserves_recorded_guard_circuit(self) -> None:
        store = GuardStore(main_module.settings.guard_state_path)
        store.set_recovery_cursor(10)
        circuit = store.circuit(9)
        circuit.state = "open"
        circuit.consecutive_failures = 4
        store.save_circuit(circuit)

        main_module.scheduled_test_capability = lambda: {"available": True}  # type: ignore[assignment]
        main_module.load_scheduled_test_recovery_alert_rows = lambda _cursor: [  # type: ignore[assignment]
            {"result_id": 11, "account_id": 9, "model_id": "gpt-test"}
        ]

        main_module.process_guard_recovery_circuits()

        reloaded = GuardStore(main_module.settings.guard_state_path)
        self.assertEqual(reloaded.recovery_cursor(), 11)
        self.assertNotEqual(reloaded.circuit(9).state, "open")

    def test_incremental_guard_failure_returns_fallback_actions_but_marks_error_visible(self) -> None:
        class BrokenEngine:
            def run_once(self, _actor: str) -> list[dict[str, Any]]:
                raise RuntimeError("db cursor failed")

        actions = [{"account_id": 9, "name": "wong", "action": "pause"}]
        main_module.guard_engine = lambda *_args, **_kwargs: BrokenEngine()  # type: ignore[assignment]
        main_module.run_guard_balance_fallback = lambda _actor: list(actions)  # type: ignore[assignment]

        result = main_module.run_auto_guard_once("test")

        self.assertEqual(result, actions)
        self.assertIn("fallback balance scan", main_module.guard_state["last_error"])
        self.assertIn("db cursor failed", main_module.guard_state["last_error"])
        self.assertTrue(main_module.guard_state["last_error_at"])
        self.assertEqual(main_module.guard_state["last_actions"], actions)

    def test_enrich_guard_rows_adds_problem_for_guard_template(self) -> None:
        rows = [
            {
                "id": 9,
                "schedulable": True,
                "blocked_403_window": 0,
                "balance_or_quota_window": 0,
                "unstable_5xx_stream_window": 0,
                "rate_limit_window": 0,
                "account_quality_errors_window": 0,
            }
        ]

        enriched = main_module.enrich_guard_rows(rows)

        self.assertEqual(enriched[0]["problem"]["level"], "good")
        self.assertEqual(enriched[0]["guard_circuit"]["state"], "closed")


if __name__ == "__main__":
    unittest.main()
