from __future__ import annotations

import asyncio
import os
import inspect
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from urllib.parse import parse_qs, urlsplit

os.environ.setdefault("OPS_SESSION_SECRET", "test-session-secret")
os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")

from app import main as main_module
from app.guard_store import GuardStore
from app.usage_query import DEFAULT_NEWAPI_TEMPLATE, DEFAULT_SUB2API_TEMPLATE, UsageQueryConfig, UsageQueryStore


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
        self.original_usage_query_store = getattr(main_module, "usage_query_store", None)
        self.original_usage_query_account_row = getattr(main_module, "usage_query_account_row", None)
        self.original_execute_usage_query = getattr(main_module, "execute_usage_query", None)
        self.original_pause_account_op = main_module.account_ops.pause_account
        self.original_resume_account_op = main_module.account_ops.resume_account
        self.original_guard_pause_account = main_module.account_ops.guard_pause_account
        self.original_guard_state = dict(main_module.guard_state)
        self.tmpdir = tempfile.TemporaryDirectory()
        data_dir = Path(self.tmpdir.name)
        main_module.settings = SimpleNamespace(
            audit_path=str(data_dir / "audit.jsonl"),
            base_path="/sub2ops",
            guard_state_path=str(data_dir / "guard-state.json"),
            usage_query_state_path=str(data_dir / "usage-query-state.json"),
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
        if self.original_usage_query_store is not None:
            main_module.usage_query_store = self.original_usage_query_store  # type: ignore[assignment]
        if self.original_usage_query_account_row is not None:
            main_module.usage_query_account_row = self.original_usage_query_account_row  # type: ignore[assignment]
        if self.original_execute_usage_query is not None:
            main_module.execute_usage_query = self.original_execute_usage_query  # type: ignore[assignment]
        main_module.account_ops.pause_account = self.original_pause_account_op
        main_module.account_ops.resume_account = self.original_resume_account_op
        main_module.account_ops.guard_pause_account = self.original_guard_pause_account
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

    def test_telegram_error_alerts_skip_whitelisted_even_when_hard_disabled(self) -> None:
        policy = main_module.GuardPolicy(whitelist_account_ids=(9,))
        rows = [
            {"error_log_id": 101, "account_id": 9, "schedulable": False, "temp_unschedulable_until": None},
            {"error_log_id": 102, "account_id": 9, "schedulable": True, "temp_unschedulable_until": "2026-05-18T10:05:00+00:00"},
            {"error_log_id": 103, "account_id": 10, "schedulable": False, "temp_unschedulable_until": None},
        ]

        filtered = main_module.filter_telegram_error_alert_rows(rows, policy)

        self.assertEqual([row["error_log_id"] for row in filtered], [103])

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

    def test_usage_query_guard_pauses_depleted_non_oauth_account(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_usage_query_settings(
            usage_query_enabled=True,
            guard_disable_on_zero=True,
            auto_query_interval_seconds=3600,
        )
        store.save_config(
            UsageQueryConfig(
                account_id=9,
                enabled=False,
                guard_disable_on_zero=False,
                upstream_multiplier=0.5,
                auto_query_interval_minutes=60,
            )
        )
        paused: list[tuple[int, str]] = []
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: {  # type: ignore[assignment]
            "id": 9,
            "name": "quota-account",
            "type": "api",
            "schedulable": True,
        }
        main_module.execute_usage_query = lambda _config: {  # type: ignore[assignment]
            "success": True,
            "remaining": 0,
            "actual_available": 0,
            "unit": "USD",
            "upstream_multiplier": 0.5,
            "queried_at": "2026-05-22T08:00:00+00:00",
        }
        main_module.account_ops.guard_pause_account = lambda _db, account_id, reason: paused.append(  # type: ignore[assignment]
            (account_id, reason)
        ) or {"id": account_id, "name": "quota-account", "schedulable": False}

        actions = main_module.run_usage_query_guard("test")

        self.assertEqual([item["account_id"] for item in actions], [9])
        self.assertEqual(paused[0][0], 9)
        self.assertIn("usage query depleted", paused[0][1])
        self.assertEqual(store.result(9)["actual_available"], 0)

    def test_usage_query_guard_uses_global_auto_query_interval_seconds(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_usage_query_settings(auto_query_interval_seconds=0)
        store.save_config(
            UsageQueryConfig(
                account_id=9,
                enabled=False,
                guard_disable_on_zero=False,
                auto_query_interval_minutes=60,
            )
        )
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: {  # type: ignore[assignment]
            "id": 9,
            "name": "quota-account",
            "type": "api",
            "schedulable": True,
        }

        def unexpected_query(_config: Any) -> dict[str, Any]:
            raise AssertionError("global interval 0 should disable automatic usage query refresh")

        main_module.execute_usage_query = unexpected_query  # type: ignore[assignment]

        actions = main_module.run_usage_query_guard("test")

        self.assertEqual(actions, [])
        self.assertEqual(store.result(9), {})

    def test_usage_query_guard_respects_global_query_enabled_switch(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_usage_query_settings(usage_query_enabled=False)
        store.save_config(UsageQueryConfig(account_id=9, enabled=True, guard_disable_on_zero=True))
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: {  # type: ignore[assignment]
            "id": 9,
            "name": "quota-account",
            "type": "api",
            "schedulable": True,
        }

        def unexpected_query(_config: Any) -> dict[str, Any]:
            raise AssertionError("global query switch should disable automatic usage queries")

        main_module.execute_usage_query = unexpected_query  # type: ignore[assignment]

        actions = main_module.run_usage_query_guard("test")

        self.assertEqual(actions, [])
        self.assertEqual(store.result(9), {})

    def test_usage_query_guard_refreshes_but_does_not_pause_when_global_hard_stop_is_off(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_usage_query_settings(
            usage_query_enabled=True,
            guard_disable_on_zero=False,
            auto_query_interval_seconds=1,
        )
        store.save_config(UsageQueryConfig(account_id=9, enabled=True, guard_disable_on_zero=True))
        paused: list[int] = []
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: {  # type: ignore[assignment]
            "id": 9,
            "name": "quota-account",
            "type": "api",
            "schedulable": True,
        }
        main_module.execute_usage_query = lambda _config: {  # type: ignore[assignment]
            "success": True,
            "remaining": 0,
            "actual_available": 0,
            "queried_at": "2026-05-22T08:00:00+00:00",
        }
        main_module.account_ops.guard_pause_account = lambda _db, account_id, _reason: paused.append(account_id)  # type: ignore[assignment]

        actions = main_module.run_usage_query_guard("test")

        self.assertEqual(actions, [])
        self.assertEqual(paused, [])
        self.assertEqual(store.result(9)["actual_available"], 0)

    def test_usage_query_config_save_queries_and_preserves_return_anchor(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: {  # type: ignore[assignment]
            "id": 9,
            "name": "quota-account",
            "type": "api",
            "schedulable": True,
        }
        main_module.execute_usage_query = lambda _config: {  # type: ignore[assignment]
            "success": True,
            "remaining": 3,
            "actual_available": 6,
            "queried_at": "2026-05-22T08:00:00+00:00",
        }

        class FakeRequest:
            async def form(self) -> dict[str, str]:
                return {
                    "return_to": "/sub2ops/speed?group=default#usage-query-9",
                    "template_type": "sub2api",
                    "base_url": "https://quota.example.com",
                    "api_key": "secret",
                    "upstream_multiplier": "0.5",
                    "timeout_seconds": "10",
                }

        response = asyncio.run(main_module.usage_query_config_save(FakeRequest(), "tester", 9))  # type: ignore[arg-type]

        self.assertEqual(store.result(9)["actual_available"], 6)
        self.assertTrue(store.config(9).enabled)
        self.assertTrue(response.headers["location"].endswith("#usage-query-9"))
        self.assertIn("/sub2ops/speed?group=default&msg=", response.headers["location"])

    def test_usage_query_fill_credentials_preserves_return_anchor(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: {  # type: ignore[assignment]
            "id": 9,
            "name": "quota-account",
            "type": "api",
            "credentials": {
                "base_url": "https://quota.example.com",
                "api_key": "secret-from-account",
            },
        }

        class FakeRequest:
            async def form(self) -> dict[str, str]:
                return {
                    "return_to": "/sub2ops/speed?group=default#usage-query-9",
                    "template_type": "sub2api",
                    "base_url": "",
                    "api_key": "",
                    "upstream_multiplier": "1",
                    "timeout_seconds": "10",
                }

        response = asyncio.run(main_module.usage_query_fill_credentials(FakeRequest(), "tester", 9))  # type: ignore[arg-type]

        self.assertEqual(store.config(9).base_url, "https://quota.example.com")
        self.assertEqual(store.config(9).api_key, "secret-from-account")
        self.assertTrue(response.headers["location"].endswith("#usage-query-9"))
        self.assertIn("/sub2ops/speed?group=default&msg=", response.headers["location"])

    def test_usage_query_config_form_switches_default_code_to_selected_template(self) -> None:
        config = main_module.usage_query_config_from_form(
            4,
            {
                "template_type": "newapi",
                "base_url": "https://lt.example.com",
                "access_token": "access-token",
                "user_id": "42",
                "code": DEFAULT_SUB2API_TEMPLATE,
                "upstream_multiplier": "1",
                "timeout_seconds": "10",
            },
            UsageQueryConfig(account_id=4, template_type="sub2api"),
            "tester",
        )

        self.assertEqual(config.template_type, "newapi")
        self.assertEqual(config.code, DEFAULT_NEWAPI_TEMPLATE)
        self.assertEqual(config.access_token, "access-token")
        self.assertEqual(config.user_id, "42")

    def test_usage_query_settings_save_persists_global_switches_and_seconds(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]

        class FakeForm(dict[str, str]):
            def __init__(self) -> None:
                super().__init__(
                    {
                        "return_to": "/sub2ops/speed",
                        "usage_query_enabled": "0",
                        "guard_disable_on_zero": "0",
                        "auto_query_interval_seconds": "30",
                    }
                )

            def getlist(self, name: str) -> list[str]:
                return [self[name]] if name in self else []

        class FakeRequest:
            async def form(self) -> FakeForm:
                return FakeForm()

        response = asyncio.run(main_module.usage_query_settings_save(FakeRequest(), "tester"))  # type: ignore[arg-type]

        self.assertFalse(store.usage_query_enabled())
        self.assertFalse(store.guard_disable_on_zero())
        self.assertEqual(store.auto_query_interval_seconds(), 30)
        self.assertIn("/sub2ops/speed?msg=", response.headers["location"])

    def test_usage_query_view_recalculates_actual_available_with_current_multiplier(self) -> None:
        view = main_module.usage_query_view(
            UsageQueryConfig(account_id=9, enabled=True, upstream_multiplier=0.06),
            {
                "success": True,
                "remaining": 28.6806749,
                "actual_available": 28.6806749,
                "upstream_multiplier": 1.0,
                "unit": "USD",
            },
        )

        self.assertAlmostEqual(view["result"]["actual_available"], 478.01124833333336)
        self.assertEqual(view["result"]["upstream_multiplier"], 0.06)

    def test_usage_query_guard_skips_oauth_accounts(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_config(UsageQueryConfig(account_id=9, enabled=True, guard_disable_on_zero=True))
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: {  # type: ignore[assignment]
            "id": 9,
            "name": "oauth-account",
            "type": "oauth",
            "schedulable": True,
        }
        main_module.execute_usage_query = lambda _config: {  # type: ignore[assignment]
            "success": True,
            "remaining": 0,
            "actual_available": 0,
        }

        actions = main_module.run_usage_query_guard("test")

        self.assertEqual(actions, [])

    def test_usage_query_guard_does_not_pause_failed_queries(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_config(UsageQueryConfig(account_id=9, enabled=True, guard_disable_on_zero=True))
        paused: list[int] = []
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: {  # type: ignore[assignment]
            "id": 9,
            "name": "quota-account",
            "type": "api",
            "schedulable": True,
        }
        main_module.execute_usage_query = lambda _config: {  # type: ignore[assignment]
            "success": False,
            "error": "query failed",
            "actual_available": 0,
        }
        main_module.account_ops.guard_pause_account = lambda _db, account_id, _reason: paused.append(account_id)  # type: ignore[assignment]

        actions = main_module.run_usage_query_guard("test")

        self.assertEqual(actions, [])
        self.assertEqual(paused, [])
        self.assertFalse(store.result(9)["success"])

    def test_usage_query_batch_skips_missing_accounts(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_config(UsageQueryConfig(account_id=9, enabled=True))
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: None  # type: ignore[assignment]

        def unexpected_query(_config: Any) -> dict[str, Any]:
            raise AssertionError("deleted accounts must not be queried")

        main_module.execute_usage_query = unexpected_query  # type: ignore[assignment]

        response = asyncio.run(main_module.usage_query_query_enabled("tester", return_to="/sub2ops/speed"))
        message = parse_qs(urlsplit(response.headers["location"]).query)["msg"][0]

        self.assertEqual(store.result(9), {})
        self.assertIn("已查询 0 个已配置账号", message)
        self.assertIn("跳过已删除 1 个", message)
        audit_text = Path(main_module.settings.audit_path).read_text(encoding="utf-8")
        self.assertIn('"skipped_missing": 1', audit_text)

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
