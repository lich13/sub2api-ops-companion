from __future__ import annotations

import os
import inspect
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
        return {
            "account_priority_column_exists": True,
            "account_load_factor_column_exists": False,
            "account_group_priority_column_exists": True,
        }

    def fetch_all(self, *_args: Any, **_kwargs: Any) -> list[dict[str, Any]]:
        return []


class GuardMainTests(unittest.TestCase):
    def setUp(self) -> None:
        self.original_settings = main_module.settings
        self.original_db = main_module.db
        self.original_guard_engine = main_module.guard_engine
        self.original_scheduled_test_capability = main_module.scheduled_test_capability
        self.original_load_recovery_rows = main_module.load_scheduled_test_recovery_alert_rows
        self.original_run_guard_balance_fallback = main_module.run_guard_balance_fallback
        self.original_pause_account_op = main_module.account_ops.pause_account
        self.original_resume_account_op = main_module.account_ops.resume_account
        self.original_guard_state = dict(main_module.guard_state)
        self.tmpdir = tempfile.TemporaryDirectory()
        data_dir = Path(self.tmpdir.name)
        main_module.settings = SimpleNamespace(
            audit_path=str(data_dir / "audit.jsonl"),
            base_path="/sub2ops",
            guard_state_path=str(data_dir / "guard-state.json"),
            guard_enabled=True,
            guard_interval_seconds=5,
            guard_balance_error_threshold=1,
            guard_balance_error_max_age_hours=24,
            guard_quality_hours=24,
            guard_event_batch_size=100,
        )
        main_module.db = FakeCapabilityDB()  # type: ignore[assignment]

    def tearDown(self) -> None:
        main_module.settings = self.original_settings
        main_module.db = self.original_db
        main_module.guard_engine = self.original_guard_engine
        main_module.scheduled_test_capability = self.original_scheduled_test_capability
        main_module.load_scheduled_test_recovery_alert_rows = self.original_load_recovery_rows
        main_module.run_guard_balance_fallback = self.original_run_guard_balance_fallback
        main_module.account_ops.pause_account = self.original_pause_account_op
        main_module.account_ops.resume_account = self.original_resume_account_op
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
            {"result_id": 11, "account_id": 9, "model_id": "gpt-test", "schedulable": False}
        ]

        main_module.process_guard_recovery_circuits()

        reloaded = GuardStore(main_module.settings.guard_state_path)
        self.assertEqual(reloaded.recovery_cursor(), 11)
        self.assertNotEqual(reloaded.circuit(9).state, "open")

    def test_recovery_processing_only_resumes_accounts_with_recoverable_state(self) -> None:
        store = GuardStore(main_module.settings.guard_state_path)
        store.set_recovery_cursor(10)
        recorded: list[tuple[int, int]] = []

        class CaptureEngine:
            def __init__(self, engine_store: GuardStore) -> None:
                self.store = engine_store

            def record_recovery_success(self, account_id: int, result_id: int, _message: str) -> None:
                recorded.append((account_id, result_id))

        main_module.scheduled_test_capability = lambda: {"available": True}  # type: ignore[assignment]
        main_module.load_scheduled_test_recovery_alert_rows = lambda _cursor: [  # type: ignore[assignment]
            {"result_id": 11, "account_id": 8, "model_id": "gpt-test", "schedulable": True},
            {"result_id": 12, "account_id": 9, "model_id": "gpt-test", "schedulable": False},
        ]
        main_module.guard_engine = lambda engine_store=None: CaptureEngine(engine_store or store)  # type: ignore[assignment]

        main_module.process_guard_recovery_circuits()

        reloaded = GuardStore(main_module.settings.guard_state_path)
        self.assertEqual(recorded, [(9, 12)])
        self.assertEqual(reloaded.recovery_cursor(), 12)

    def test_scheduled_test_needs_recovery_includes_account_status(self) -> None:
        self.assertTrue(main_module.scheduled_test_needs_recovery({"account_status": "error", "schedulable": True}))
        self.assertFalse(main_module.scheduled_test_needs_recovery({"account_status": "active", "schedulable": True}))
        self.assertFalse(main_module.scheduled_test_needs_recovery({"account_status": "error", "schedulable": False, "type": "oauth"}))

    def test_guard_suggestion_skips_oauth_accounts(self) -> None:
        self.assertIsNone(
            main_module.guard_suggestion(
                {
                    "id": 9,
                    "name": "oauth-account",
                    "type": "oauth",
                    "schedulable": True,
                    "balance_or_quota_window": 3,
                }
            )
        )

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

    def test_successful_incremental_guard_also_runs_balance_sweep(self) -> None:
        class EmptyEngine:
            def run_once(self, _actor: str) -> list[dict[str, Any]]:
                return []

        actions = [{"account_id": 9, "name": "wong", "action": "pause"}]
        main_module.guard_engine = lambda *_args, **_kwargs: EmptyEngine()  # type: ignore[assignment]
        main_module.run_guard_balance_fallback = lambda _actor: list(actions)  # type: ignore[assignment]

        result = main_module.run_auto_guard_once("test")

        self.assertEqual(result, actions)
        self.assertEqual(main_module.guard_state["last_actions"], actions)
        self.assertEqual(main_module.guard_state["last_error"], "")

    def test_balance_sweep_passes_max_age_hours(self) -> None:
        class CaptureDB(FakeCapabilityDB):
            def __init__(self) -> None:
                self.calls: list[tuple[str, dict[str, Any] | None]] = []

            def fetch_all(self, sql: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
                self.calls.append((sql, params))
                return []

        capture_db = CaptureDB()
        main_module.db = capture_db  # type: ignore[assignment]
        main_module.settings.guard_balance_error_max_age_hours = 12

        actions = main_module.run_guard_balance_fallback("test")

        self.assertEqual(actions, [])
        params = capture_db.calls[0][1]
        assert params is not None
        self.assertEqual(params["threshold"], 1)
        self.assertEqual(params["max_age_hours"], 12)

    def test_balance_sweep_respects_disabled_hard_pause_policy(self) -> None:
        class CaptureDB(FakeCapabilityDB):
            def __init__(self) -> None:
                self.called = False

            def fetch_all(self, _sql: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
                self.called = True
                return []

        capture_db = CaptureDB()
        main_module.db = capture_db  # type: ignore[assignment]
        GuardStore(main_module.settings.guard_state_path).save_policy({"hard_pause_enabled": False})

        actions = main_module.run_guard_balance_fallback("test")

        self.assertEqual(actions, [])
        self.assertFalse(capture_db.called)

    def test_guard_policy_from_store_parses_whitelist(self) -> None:
        GuardStore(main_module.settings.guard_state_path).save_policy(
            {
                "whitelist_account_ids": ["9", "10", "bad", "9"],
                "whitelist_balance_pause_threshold": 12,
            }
        )

        policy = main_module.guard_policy_from_store()

        self.assertEqual(policy.whitelist_account_ids, (9, 10))
        self.assertEqual(policy.whitelist_balance_pause_threshold, 12)

    def test_guard_policy_save_persists_whitelist_settings(self) -> None:
        main_module.guard_policy_save(
            "tester",
            hard_pause_enabled="1",
            rate_limit_enabled="1",
            unstable_enabled="1",
            whitelist_account_ids="9, 10, bad, 9",
            whitelist_balance_pause_threshold=11,
        )

        saved = GuardStore(main_module.settings.guard_state_path).policy_config()

        self.assertEqual(saved["whitelist_account_ids"], [9, 10])
        self.assertEqual(saved["whitelist_balance_pause_threshold"], 11)

    def test_guard_policy_save_accepts_checkbox_whitelist_values(self) -> None:
        main_module.guard_policy_save(
            "tester",
            hard_pause_enabled="1",
            rate_limit_enabled="1",
            unstable_enabled="1",
            whitelist_account_ids=["9", "10", "9"],
            whitelist_balance_pause_threshold=10,
        )

        saved = GuardStore(main_module.settings.guard_state_path).policy_config()

        self.assertEqual(saved["whitelist_account_ids"], [9, 10])

    def test_guard_whitelist_options_preserve_selected_missing_accounts(self) -> None:
        options = main_module.guard_whitelist_options(
            [{"id": 9, "name": "primary", "type": "api", "platform": "openai"}],
            main_module.GuardPolicy(whitelist_account_ids=(9, 12)),
        )

        self.assertEqual([item["id"] for item in options], [9, 12])
        self.assertTrue(options[0]["checked"])
        self.assertEqual(options[0]["label"], "#9 primary")
        self.assertEqual(options[0]["meta"], "api / openai")
        self.assertEqual(options[1]["label"], "#12 当前列表未返回")
        self.assertTrue(options[1]["checked"])

    def test_telegram_error_alerts_skip_whitelisted_schedulable_accounts(self) -> None:
        policy = main_module.GuardPolicy(whitelist_account_ids=(9,))
        rows = [
            {"error_log_id": 101, "account_id": 9, "schedulable": True, "temp_unschedulable_until": None},
            {"error_log_id": 102, "account_id": 10, "schedulable": True, "temp_unschedulable_until": None},
        ]

        filtered = main_module.filter_telegram_error_alert_rows(rows, policy)

        self.assertEqual([row["account_id"] for row in filtered], [10])

    def test_telegram_error_alerts_keep_whitelisted_hard_disabled_accounts(self) -> None:
        policy = main_module.GuardPolicy(whitelist_account_ids=(9,))
        rows = [
            {"error_log_id": 101, "account_id": 9, "schedulable": False, "temp_unschedulable_until": None},
            {"error_log_id": 102, "account_id": 9, "schedulable": True, "temp_unschedulable_until": "2026-05-18T10:05:00+00:00"},
        ]

        filtered = main_module.filter_telegram_error_alert_rows(rows, policy)

        self.assertEqual([row["error_log_id"] for row in filtered], [101])

    def test_balance_sweep_skips_whitelisted_candidates(self) -> None:
        class CaptureDB(FakeCapabilityDB):
            def __init__(self) -> None:
                self.updated_account_ids: list[int] = []

            def fetch_all(self, _sql: str, _params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
                return [
                    {
                        "id": 9,
                        "name": "white",
                        "balance_error_count": 99,
                        "last_message": "token quota is not enough",
                    },
                    {
                        "id": 10,
                        "name": "normal",
                        "balance_error_count": 1,
                        "last_message": "token quota is not enough",
                    },
                ]

            def fetch_one(self, _sql: str, params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                assert params is not None
                self.updated_account_ids.append(int(params["account_id"]))
                return {"id": params["account_id"], "name": "normal", "schedulable": False}

        capture_db = CaptureDB()
        main_module.db = capture_db  # type: ignore[assignment]
        GuardStore(main_module.settings.guard_state_path).save_policy({"whitelist_account_ids": [9]})

        actions = main_module.run_guard_balance_fallback("test")

        self.assertEqual([item["account_id"] for item in actions], [10])
        self.assertEqual(capture_db.updated_account_ids, [10])

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

    def test_guard_view_does_not_accept_group_platform_or_hours_filters(self) -> None:
        params = inspect.signature(main_module.guard_view).parameters

        self.assertNotIn("group", params)
        self.assertNotIn("platform", params)
        self.assertNotIn("hours", params)

    def test_load_guard_quality_uses_all_accounts_sql_with_bounded_signal_window(self) -> None:
        class CaptureDB(FakeCapabilityDB):
            def __init__(self) -> None:
                self.fetch_all_calls: list[tuple[str, dict[str, Any] | None]] = []

            def fetch_all(self, sql: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
                self.fetch_all_calls.append((sql, params))
                return []

        capture_db = CaptureDB()
        main_module.db = capture_db  # type: ignore[assignment]

        rows = main_module.load_guard_quality()

        self.assertEqual(rows, [])
        sql, params = capture_db.fetch_all_calls[0]
        self.assertIn("LEFT JOIN account_groups", sql)
        self.assertNotIn("g.name = ANY", sql)
        self.assertIsNotNone(params)
        assert params is not None
        self.assertEqual(params["platform"], "")
        self.assertIsNotNone(params["range_start"])
        self.assertIsNone(params["range_end"])

    def test_load_guard_queue_quality_preserves_group_membership_rows_with_bounded_signal_window(self) -> None:
        class CaptureDB(FakeCapabilityDB):
            def __init__(self) -> None:
                self.fetch_all_calls: list[tuple[str, dict[str, Any] | None]] = []

            def fetch_all(self, sql: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
                self.fetch_all_calls.append((sql, params))
                return []

        capture_db = CaptureDB()
        main_module.db = capture_db  # type: ignore[assignment]

        rows = main_module.load_guard_queue_quality()

        self.assertEqual(rows, [])
        sql, params = capture_db.fetch_all_calls[0]
        self.assertIn("ag.group_id", sql)
        self.assertNotIn("SELECT DISTINCT ON (a.id)", sql)
        self.assertIsNotNone(params)
        assert params is not None
        self.assertEqual(params["platform"], "")
        self.assertIsNotNone(params["range_start"])
        self.assertIsNone(params["range_end"])

    def test_request_view_limits_error_logs_before_expanding_attempts(self) -> None:
        class CaptureDB(FakeCapabilityDB):
            def __init__(self) -> None:
                self.calls: list[tuple[str, dict[str, Any] | None]] = []

            def fetch_all(self, sql: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
                self.calls.append((sql, params))
                return []

        captured: dict[str, Any] = {}
        original_render = main_module.render
        capture_db = CaptureDB()

        def capture_render(_request: Any, _template: str, context: dict[str, Any]) -> Any:
            captured["context"] = context
            return context

        try:
            main_module.db = capture_db  # type: ignore[assignment]
            main_module.render = capture_render  # type: ignore[assignment]

            main_module.requests_view(object(), "tester", limit=200)  # type: ignore[arg-type]
        finally:
            main_module.render = original_render

        request_params = next(params for sql, params in capture_db.calls if "WITH target_logs AS" in sql)
        assert request_params is not None
        self.assertEqual(request_params["limit"], 200)
        self.assertEqual(request_params["scan_limit"], 4000)

    def test_guard_view_uses_all_accounts_loader_and_no_filter_context(self) -> None:
        sentinel_rows = [
            {
                "id": 9,
                "name": "wong",
                "schedulable": True,
                "blocked_403_window": 0,
                "balance_or_quota_window": 0,
                "unstable_5xx_stream_window": 0,
                "rate_limit_window": 0,
                "account_quality_errors_window": 0,
            }
        ]
        captured: dict[str, Any] = {}
        original_load_guard_quality = main_module.load_guard_quality
        original_load_guard_queue_quality = main_module.load_guard_queue_quality
        original_load_quality = main_module.load_quality
        original_render = main_module.render

        def fail_load_quality(*_args: Any, **_kwargs: Any) -> list[dict[str, Any]]:
            raise AssertionError("guard_view must not use filtered load_quality")

        def capture_render(_request: Any, template: str, context: dict[str, Any]) -> Any:
            captured["template"] = template
            captured["context"] = context
            return context

        try:
            main_module.load_guard_quality = lambda: list(sentinel_rows)  # type: ignore[assignment]
            main_module.load_guard_queue_quality = lambda: list(sentinel_rows)  # type: ignore[assignment]
            main_module.load_quality = fail_load_quality  # type: ignore[assignment]
            main_module.render = capture_render  # type: ignore[assignment]

            result = main_module.guard_view(object(), "tester")  # type: ignore[arg-type]
        finally:
            main_module.load_guard_quality = original_load_guard_quality
            main_module.load_guard_queue_quality = original_load_guard_queue_quality
            main_module.load_quality = original_load_quality
            main_module.render = original_render

        self.assertIs(result, captured["context"])
        self.assertEqual(captured["template"], "guard.html")
        self.assertEqual(captured["context"]["rows"][0]["id"], 9)
        self.assertNotIn("group", captured["context"])
        self.assertNotIn("platform", captured["context"])
        self.assertNotIn("hours", captured["context"])

    def test_account_pause_can_return_to_guard_panel(self) -> None:
        calls: list[tuple[int, str]] = []
        main_module.account_ops.pause_account = lambda _db, _audit, account_id, _user, reason: calls.append(  # type: ignore[assignment]
            (account_id, reason)
        )

        response = main_module.pause_account(
            object(), 7, "tester", reason="manual switch pause from guard queue", return_to="guard"
        )

        self.assertEqual(calls, [(7, "manual switch pause from guard queue")])
        self.assertEqual(response.status_code, 303)
        self.assertEqual(response.headers["location"], "/sub2ops/guard?msg=paused+account+7")

    def test_account_resume_can_return_to_guard_panel(self) -> None:
        calls: list[int] = []
        main_module.account_ops.resume_account = lambda _db, _audit, account_id, _user: calls.append(account_id)  # type: ignore[assignment]

        response = main_module.resume_account(object(), 8, "tester", return_to="guard")

        self.assertEqual(calls, [8])
        self.assertEqual(response.status_code, 303)
        self.assertEqual(response.headers["location"], "/sub2ops/guard?msg=resumed+account+8")


if __name__ == "__main__":
    unittest.main()
