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
from app import usage_query as usage_query_module
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
        self.original_execute_oauth_usage_query = getattr(main_module, "execute_oauth_usage_query", None)
        self.original_usage_query_oauth_account_rows = getattr(main_module, "usage_query_oauth_account_rows", None)
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
            sso_config_path=str(data_dir / "sso-config.json"),
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
        if self.original_execute_oauth_usage_query is not None:
            main_module.execute_oauth_usage_query = self.original_execute_oauth_usage_query  # type: ignore[assignment]
        elif hasattr(main_module, "execute_oauth_usage_query"):
            delattr(main_module, "execute_oauth_usage_query")
        if self.original_usage_query_oauth_account_rows is not None:
            main_module.usage_query_oauth_account_rows = self.original_usage_query_oauth_account_rows  # type: ignore[assignment]
        elif hasattr(main_module, "usage_query_oauth_account_rows"):
            delattr(main_module, "usage_query_oauth_account_rows")
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
                base_url="https://stale.example.com",
                api_key="sk-stale",
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
            "credentials": {
                "base_url": "https://account.example.com",
                "api_key": "sk-account",
            },
        }
        queried_configs: list[UsageQueryConfig] = []

        def fake_usage_query(config: UsageQueryConfig) -> dict[str, Any]:
            queried_configs.append(config)
            return {
                "success": True,
                "remaining": 0,
                "actual_available": 0,
                "unit": "USD",
                "upstream_multiplier": 0.5,
                "queried_at": "2026-05-22T08:00:00+00:00",
            }

        main_module.execute_usage_query = fake_usage_query  # type: ignore[assignment]
        main_module.account_ops.guard_pause_account = lambda _db, account_id, reason: paused.append(  # type: ignore[assignment]
            (account_id, reason)
        ) or {"id": account_id, "name": "quota-account", "schedulable": False}

        actions = main_module.run_usage_query_guard("test")

        self.assertEqual([item["account_id"] for item in actions], [9])
        self.assertEqual(paused[0][0], 9)
        self.assertIn("usage query depleted", paused[0][1])
        self.assertEqual(store.result(9)["actual_available"], 0)
        self.assertEqual(queried_configs[0].base_url, "https://account.example.com")
        self.assertEqual(queried_configs[0].api_key, "sk-account")
        self.assertEqual(store.config(9).base_url, "https://stale.example.com")
        self.assertEqual(store.config(9).api_key, "sk-stale")

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
            "credentials": {
                "base_url": "https://account.example.com",
                "api_key": "secret-from-account",
            },
        }
        queried_configs: list[UsageQueryConfig] = []

        def fake_usage_query(config: UsageQueryConfig) -> dict[str, Any]:
            queried_configs.append(config)
            return {
                "success": True,
                "remaining": 3,
                "actual_available": 6,
                "queried_at": "2026-05-22T08:00:00+00:00",
            }

        main_module.execute_usage_query = fake_usage_query  # type: ignore[assignment]

        class FakeRequest:
            async def form(self) -> dict[str, str]:
                return {
                    "return_to": "/sub2ops/speed?group=default#usage-query-9",
                    "template_type": "sub2api",
                    "base_url": "https://stale-form.example.com",
                    "api_key": "stale-form-secret",
                    "upstream_multiplier": "0.5",
                    "timeout_seconds": "10",
                }

        response = asyncio.run(main_module.usage_query_config_save(FakeRequest(), "tester", 9))  # type: ignore[arg-type]

        self.assertEqual(store.result(9)["actual_available"], 6)
        self.assertTrue(store.config(9).enabled)
        self.assertEqual(store.config(9).base_url, "")
        self.assertEqual(store.config(9).api_key, "")
        self.assertEqual(queried_configs[0].base_url, "https://account.example.com")
        self.assertEqual(queried_configs[0].api_key, "secret-from-account")
        self.assertTrue(response.headers["location"].endswith("#usage-query-9"))
        self.assertIn("/sub2ops/speed?group=default&msg=", response.headers["location"])
        audit_text = Path(main_module.settings.audit_path).read_text(encoding="utf-8")
        self.assertIn('"base_url": "https://account.example.com"', audit_text)
        self.assertIn('"api_key_set": true', audit_text)
        self.assertNotIn("stale-form", audit_text)

    def test_usage_query_manual_query_uses_latest_account_credentials(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_config(
            UsageQueryConfig(
                account_id=9,
                enabled=True,
                template_type="sub2api",
                base_url="https://stale-state.example.com",
                api_key="stale-state-secret",
            )
        )
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: {  # type: ignore[assignment]
            "id": 9,
            "name": "quota-account",
            "type": "api",
            "schedulable": True,
            "credentials": {
                "base_url": "https://current-account.example.com",
                "api_key": "current-account-secret",
            },
        }
        queried_configs: list[UsageQueryConfig] = []

        def fake_usage_query(config: UsageQueryConfig) -> dict[str, Any]:
            queried_configs.append(config)
            return {
                "success": True,
                "remaining": 8,
                "actual_available": 8,
                "queried_at": "2026-05-22T08:00:00+00:00",
            }

        main_module.execute_usage_query = fake_usage_query  # type: ignore[assignment]

        class FakeRequest:
            async def form(self) -> dict[str, str]:
                return {
                    "return_to": "/sub2ops/speed#usage-query-9",
                    "template_type": "sub2api",
                    "base_url": "https://stale-form.example.com",
                    "api_key": "stale-form-secret",
                    "upstream_multiplier": "1",
                    "timeout_seconds": "10",
                }

        response = asyncio.run(main_module.usage_query_account_query(FakeRequest(), "tester", 9))  # type: ignore[arg-type]

        self.assertEqual(store.config(9).base_url, "https://stale-state.example.com")
        self.assertEqual(store.config(9).api_key, "stale-state-secret")
        self.assertEqual(queried_configs[0].base_url, "https://current-account.example.com")
        self.assertEqual(queried_configs[0].api_key, "current-account-secret")
        self.assertTrue(response.headers["location"].endswith("#usage-query-9"))

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
                    "base_url": "https://stale-form.example.com",
                    "api_key": "stale-form-secret",
                    "upstream_multiplier": "1",
                    "timeout_seconds": "10",
                }

        response = asyncio.run(main_module.usage_query_fill_credentials(FakeRequest(), "tester", 9))  # type: ignore[arg-type]

        self.assertEqual(store.config(9).base_url, "")
        self.assertEqual(store.config(9).api_key, "")
        self.assertTrue(store.config(9).use_account_credentials)
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
        self.assertEqual(config.base_url, "")
        self.assertEqual(config.api_key, "")
        self.assertEqual(config.access_token, "access-token")
        self.assertEqual(config.user_id, "42")

    def test_usage_query_config_form_preserves_existing_stale_base_url_and_api_key(self) -> None:
        config = main_module.usage_query_config_from_form(
            4,
            {
                "template_type": "sub2api",
                "base_url": "https://submitted.example.com",
                "api_key": "submitted-secret",
                "upstream_multiplier": "1",
                "timeout_seconds": "10",
            },
            UsageQueryConfig(
                account_id=4,
                template_type="sub2api",
                base_url="https://stale-state.example.com",
                api_key="stale-state-secret",
            ),
            "tester",
        )

        self.assertEqual(config.base_url, "https://stale-state.example.com")
        self.assertEqual(config.api_key, "stale-state-secret")

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
                        "sub2api_admin_token": "admin-secret",
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
        self.assertEqual(store.sub2api_admin_token(), "admin-secret")
        self.assertTrue(main_module.usage_query_settings(store)["sub2api_admin_token_saved"])
        audit_text = Path(main_module.settings.audit_path).read_text(encoding="utf-8")
        self.assertNotIn("admin-secret", audit_text)
        self.assertIn('"sub2api_admin_token_set": true', audit_text)
        self.assertIn("/sub2ops/speed?msg=", response.headers["location"])

    def test_sso_config_save_persists_sub2api_admin_key_without_leaking_plaintext(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]

        response = main_module.sso_config_save(
            "tester",
            enabled="1",
            base_url="https://sub2api.example.com",
            verify_base_url="http://sub2api:8080",
            required_role="admin",
            session_ttl_seconds=86400,
            verify_timeout_seconds=5,
            sub2api_admin_token="admin-secret",
        )

        self.assertEqual(store.sub2api_admin_token(), "admin-secret")
        audit_text = Path(main_module.settings.audit_path).read_text(encoding="utf-8")
        self.assertNotIn("admin-secret", audit_text)
        self.assertIn('"sub2api_admin_token_set": true', audit_text)
        self.assertIn('"sub2api_admin_token_saved": true', audit_text)
        self.assertIn("/sub2ops/sso?msg=", response.headers["location"])

        response = main_module.sso_config_save(
            "tester",
            enabled="1",
            base_url="https://sub2api.example.com",
            verify_base_url="http://sub2api:8080",
            required_role="admin",
            session_ttl_seconds=86400,
            verify_timeout_seconds=5,
            sub2api_admin_token="",
        )

        self.assertEqual(store.sub2api_admin_token(), "admin-secret")
        self.assertIn("/sub2ops/sso?msg=", response.headers["location"])

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

    def test_load_speed_quality_projects_read_only_oauth_quota_fields(self) -> None:
        class CaptureDB(FakeCapabilityDB):
            def __init__(self) -> None:
                self.fetch_all_calls: list[tuple[str, dict[str, Any] | None]] = []

            def fetch_all(self, sql: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
                self.fetch_all_calls.append((sql, params))
                return []

        capture_db = CaptureDB()
        main_module.db = capture_db  # type: ignore[assignment]

        rows = main_module.load_speed_quality(["openai-default"], "openai", None, None)

        self.assertEqual(rows, [])
        sql, params = capture_db.fetch_all_calls[0]
        self.assertIn("a.credentials", sql)
        self.assertIn("a.extra", sql)
        self.assertIn("codex_5h_reset_after_seconds", sql)
        self.assertIn("codex_7d_window_minutes", sql)
        self.assertIsNotNone(params)
        assert params is not None
        self.assertEqual(params["group_names"], ["openai-default"])
        self.assertEqual(params["platform"], "openai")

    def test_enrich_usage_query_rows_adds_oauth_quota_from_usage_query_helper(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        calls: list[dict[str, Any]] = []
        missing = object()
        original_helper = getattr(usage_query_module, "oauth_quota_windows", missing)
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]

        def fake_oauth_quota_windows(row: dict[str, Any]) -> dict[str, Any]:
            calls.append(row)
            return {
                "plan_type": "plus",
                "ui_windows": [
                    {"key": "codex_5h", "label": "5h", "used_percent": 100.0, "remaining_percent": 0.0}
                ],
            }

        try:
            usage_query_module.oauth_quota_windows = fake_oauth_quota_windows  # type: ignore[attr-defined]
            enriched = main_module.enrich_usage_query_rows(
                [
                    {
                        "id": 9,
                        "name": "oauth-account",
                        "type": "oauth",
                        "credentials": {"plan_type": "plus"},
                        "extra": {"codex_5h_used_percent": 100},
                    }
                ]
            )
        finally:
            if original_helper is missing:
                delattr(usage_query_module, "oauth_quota_windows")
            else:
                usage_query_module.oauth_quota_windows = original_helper  # type: ignore[attr-defined]

        self.assertEqual(enriched[0]["oauth_quota"]["plan_type"], "plus")
        self.assertEqual(enriched[0]["oauth_quota"]["ui_windows"][0]["remaining_percent"], 0.0)
        self.assertEqual(calls[0]["credentials"]["plan_type"], "plus")

    def test_enrich_usage_query_rows_prefers_empty_success_oauth_query_result(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_result(
            9,
            {
                "account_id": 9,
                "template_type": "oauth",
                "success": True,
                "oauth_quota": {"plan_type": "pro", "ui_windows": [], "telegram_windows": []},
            },
        )
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]

        enriched = main_module.enrich_usage_query_rows(
            [
                {
                    "id": 9,
                    "name": "oauth-account",
                    "type": "oauth",
                    "credentials": {"plan_type": "free"},
                    "extra": {"codex_7d_used_percent": 10},
                }
            ]
        )

        self.assertEqual(enriched[0]["oauth_quota"]["plan_type"], "pro")
        self.assertEqual(enriched[0]["oauth_quota"]["ui_windows"], [])

    def test_speed_template_renders_oauth_plan_badge_window_reset_time_without_admin_token_field(self) -> None:
        rendered = main_module.templates.get_template("speed.html").render(
            {
                "app_name": "Sub2Ops",
                "active": "speed",
                "base_path": "/sub2ops",
                "current_user": "tester",
                "version": {"current_version": "test"},
                "msg": "",
                "rows": [
                    {
                        "id": 9,
                        "name": "oauth-account",
                        "type": "oauth",
                        "concurrency": 1,
                        "platform": "openai",
                        "success_window": 0,
                        "output_tokens_window": 0,
                        "avg_first_token_ms": None,
                        "avg_duration_ms": None,
                        "avg_ms_per_output_token": None,
                        "usage_total_cost": 0,
                        "usage_actual_cost": 0,
                        "usage_request_count": 0,
                        "usage_total_tokens": 0,
                        "last_success_at": None,
                        "rate_limit_reset_at": None,
                        "updated_at": "2026-05-25T08:30:00+00:00",
                        "usage_query": {
                            "configured": False,
                            "config": {"upstream_multiplier": 1},
                            "result": {},
                            "template_options": [],
                            "depleted": False,
                        },
                        "oauth_quota": {
                            "plan_type": "free",
                            "ui_windows": [
                                {
                                    "key": "codex_7d",
                                    "label": "7d",
                                    "used_percent": 100.0,
                                    "remaining_percent": 0.0,
                                    "reset_at": "2026-05-28T22:30:00+00:00",
                                },
                            ],
                        },
                    }
                ],
                "groups": [{"name": "openai-default", "platform": "openai"}],
                "group": "openai-default",
                "group_selection": {
                    "default_value": "openai-default",
                    "label": "默认分组",
                    "options": [
                        {"name": "openai-default", "platform": "openai", "checked": True, "is_default": True}
                    ],
                },
                "platform": "openai",
                "time_range": {
                    "label": "最近 24 小时",
                    "preset": "",
                    "presets": [],
                    "start_date": "",
                    "end_date": "",
                },
                "usage_query_settings": {
                    "usage_query_enabled": True,
                    "guard_disable_on_zero": True,
                    "auto_query_interval_seconds": 3600,
                    "sub2api_admin_token_saved": True,
                },
                "dashboard": {
                    "success_count": 0,
                    "output_tokens": 0,
                    "avg_first_token_ms": None,
                    "avg_duration_ms": None,
                    "avg_ms_per_output_token": None,
                    "usage_total_cost": 0,
                    "usage_total_tokens": 0,
                    "enabled_count": 0,
                    "configured_count": 0,
                    "depleted_count": 0,
                },
                "return_to": "/sub2ops/speed",
            }
        )

        self.assertIn("oauth-quota", rendered)
        self.assertIn("oauth-plan-badge", rendered)
        self.assertIn("free", rendered)
        self.assertNotIn("计划 free", rendered)
        self.assertNotIn(">5h<", rendered)
        self.assertIn("7d", rendered)
        self.assertIn("0%", rendered)
        self.assertIn("恢复 2026-05-29 06:30", rendered)
        self.assertNotIn("更新 2026-05-25 16:30", rendered)
        self.assertNotIn('name="sub2api_admin_token"', rendered)

    def test_usage_query_guard_refreshes_oauth_accounts_without_pausing_them(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_usage_query_settings(
            usage_query_enabled=True,
            guard_disable_on_zero=True,
            auto_query_interval_seconds=1,
            sub2api_admin_token="admin-secret",
        )
        store.save_config(UsageQueryConfig(account_id=9, enabled=True, guard_disable_on_zero=True))
        main_module.settings.sub2api_verify_base_url = "https://verify.example.com"
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda _account_id: {  # type: ignore[assignment]
            "id": 9,
            "name": "oauth-account",
            "platform": "openai",
            "type": "oauth",
            "schedulable": True,
            "credentials": {"plan_type": "plus"},
            "extra": {"codex_7d_used_percent": 10},
        }
        calls: list[tuple[int, str, str]] = []
        paused: list[int] = []

        def fake_oauth_query(account_id: int, base_url: str, admin_token: str, **_kwargs: Any) -> dict[str, Any]:
            calls.append((account_id, base_url, admin_token))
            return {
                "account_id": account_id,
                "template_type": "oauth",
                "success": True,
                "queried_at": "2026-05-25T08:00:00+00:00",
                "oauth_quota": {"plan_type": "plus", "ui_windows": []},
            }

        def unexpected_usage_query(_config: Any) -> dict[str, Any]:
            raise AssertionError("OAuth guard refresh must not use the generic usage query template")

        main_module.execute_oauth_usage_query = fake_oauth_query  # type: ignore[attr-defined]
        main_module.execute_usage_query = unexpected_usage_query  # type: ignore[assignment]
        main_module.account_ops.guard_pause_account = lambda _db, account_id, _reason: paused.append(account_id)  # type: ignore[assignment]

        actions = main_module.run_usage_query_guard("test")

        self.assertEqual(actions, [])
        self.assertEqual(paused, [])
        self.assertEqual(calls, [(9, "https://verify.example.com", "admin-secret")])
        self.assertEqual(store.result(9)["template_type"], "oauth")
        audit_text = Path(main_module.settings.audit_path).read_text(encoding="utf-8")
        self.assertIn('"oauth_queried_count": 1', audit_text)

    def test_oauth_usage_query_base_url_falls_back_to_env_when_sso_file_is_blank(self) -> None:
        data_dir = Path(self.tmpdir.name)
        sso_path = data_dir / "sso-config.json"
        sso_path.write_text('{"base_url": "", "verify_base_url": ""}', encoding="utf-8")
        main_module.settings.sso_config_path = str(sso_path)
        main_module.settings.sub2api_verify_base_url = "https://verify.example.com"
        main_module.settings.sub2api_base_url = "https://sub2api.example.com"

        self.assertEqual(main_module.oauth_usage_query_base_url(), "https://verify.example.com")

    def test_usage_query_guard_refreshes_current_oauth_accounts_without_saved_config(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_usage_query_settings(
            usage_query_enabled=True,
            guard_disable_on_zero=True,
            auto_query_interval_seconds=1,
            sub2api_admin_token="admin-secret",
        )
        main_module.settings.sub2api_base_url = "https://sub2api.example.com"
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_oauth_account_rows = lambda: [  # type: ignore[assignment]
            {
                "id": 12,
                "name": "oauth-current",
                "platform": "openai",
                "type": "oauth",
                "schedulable": True,
                "credentials": {"plan_type": "plus"},
                "extra": {"codex_7d_used_percent": 20},
            }
        ]
        calls: list[int] = []
        paused: list[int] = []

        def fake_oauth_query(account_id: int, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
            calls.append(account_id)
            return {
                "account_id": account_id,
                "template_type": "oauth",
                "success": True,
                "queried_at": "2026-05-25T08:00:00+00:00",
            }

        def unexpected_usage_query(_config: Any) -> dict[str, Any]:
            raise AssertionError("current OAuth guard refresh must not use generic usage query")

        main_module.execute_oauth_usage_query = fake_oauth_query  # type: ignore[attr-defined]
        main_module.execute_usage_query = unexpected_usage_query  # type: ignore[assignment]
        main_module.account_ops.guard_pause_account = lambda _db, account_id, _reason: paused.append(account_id)  # type: ignore[assignment]

        actions = main_module.run_usage_query_guard("test")

        self.assertEqual(actions, [])
        self.assertEqual(paused, [])
        self.assertEqual(calls, [12])
        self.assertEqual(store.result(12)["template_type"], "oauth")

    def test_usage_query_batch_queries_oauth_with_global_admin_token_and_no_skip_message(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_usage_query_settings(
            usage_query_enabled=True,
            sub2api_admin_token="admin-secret",
        )
        store.save_config(
            UsageQueryConfig(
                account_id=9,
                enabled=True,
                template_type="sub2api",
                base_url="https://stale.example.com",
                api_key="sk-stale",
            )
        )
        store.save_config(UsageQueryConfig(account_id=10, enabled=True, template_type="sub2api"))
        main_module.settings.sub2api_base_url = "https://sub2api.example.com"
        rows = {
            9: {
                "id": 9,
                "name": "api-account",
                "type": "api",
                "schedulable": True,
                "credentials": {
                    "base_url": "https://account.example.com",
                    "api_key": "sk-account",
                },
            },
            10: {
                "id": 10,
                "name": "oauth-account",
                "platform": "openai",
                "type": "oauth",
                "schedulable": True,
                "credentials": {"plan_type": "pro"},
                "extra": {"codex_7d_used_percent": 20},
            },
        }
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda account_id: rows.get(account_id)  # type: ignore[assignment]
        generic_calls: list[int] = []
        generic_configs: list[UsageQueryConfig] = []
        oauth_calls: list[int] = []

        def fake_usage_query(config: UsageQueryConfig) -> dict[str, Any]:
            generic_calls.append(config.account_id)
            generic_configs.append(config)
            return {"success": True, "remaining": 3, "actual_available": 3, "queried_at": "2026-05-25T08:00:00+00:00"}

        def fake_oauth_query(account_id: int, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
            oauth_calls.append(account_id)
            return {"account_id": account_id, "template_type": "oauth", "success": True, "queried_at": "2026-05-25T08:00:00+00:00"}

        main_module.execute_usage_query = fake_usage_query  # type: ignore[assignment]
        main_module.execute_oauth_usage_query = fake_oauth_query  # type: ignore[attr-defined]

        response = asyncio.run(main_module.usage_query_query_enabled("tester", return_to="/sub2ops/speed"))
        message = parse_qs(urlsplit(response.headers["location"]).query)["msg"][0]

        self.assertEqual(generic_calls, [9])
        self.assertEqual(generic_configs[0].base_url, "https://account.example.com")
        self.assertEqual(generic_configs[0].api_key, "sk-account")
        self.assertEqual(store.config(9).base_url, "https://stale.example.com")
        self.assertEqual(store.config(9).api_key, "sk-stale")
        self.assertEqual(oauth_calls, [10])
        self.assertEqual(store.result(10)["template_type"], "oauth")
        self.assertIn("已查询 2 个已配置账号", message)
        self.assertIn("失败 0 个", message)
        self.assertNotIn("跳过 OAuth", message)
        audit_text = Path(main_module.settings.audit_path).read_text(encoding="utf-8")
        self.assertIn('"oauth_queried": 1', audit_text)

    def test_usage_query_batch_counts_oauth_missing_admin_token_failure_without_blocking_non_oauth(self) -> None:
        store = UsageQueryStore(main_module.settings.usage_query_state_path)
        store.save_usage_query_settings(usage_query_enabled=True)
        store.save_config(UsageQueryConfig(account_id=9, enabled=True, template_type="sub2api"))
        store.save_config(UsageQueryConfig(account_id=10, enabled=True, template_type="sub2api"))
        rows = {
            9: {"id": 9, "name": "api-account", "type": "api", "schedulable": True},
            10: {"id": 10, "name": "oauth-account", "platform": "openai", "type": "oauth", "schedulable": True},
        }
        main_module.usage_query_store = lambda: store  # type: ignore[assignment]
        main_module.usage_query_account_row = lambda account_id: rows.get(account_id)  # type: ignore[assignment]
        generic_calls: list[int] = []

        def fake_usage_query(config: UsageQueryConfig) -> dict[str, Any]:
            generic_calls.append(config.account_id)
            return {
                "success": True,
                "remaining": 3,
                "actual_available": 3,
                "queried_at": "2026-05-25T08:00:00+00:00",
            }

        main_module.execute_usage_query = fake_usage_query  # type: ignore[assignment]

        response = asyncio.run(main_module.usage_query_query_enabled("tester", return_to="/sub2ops/speed"))
        message = parse_qs(urlsplit(response.headers["location"]).query)["msg"][0]

        self.assertEqual(generic_calls, [9])
        self.assertTrue(store.result(9)["success"])
        self.assertFalse(store.result(10)["success"])
        self.assertIn("已查询 2 个已配置账号", message)
        self.assertIn("失败 1 个", message)

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
        self.assertNotIn("跳过已删除", message)
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
