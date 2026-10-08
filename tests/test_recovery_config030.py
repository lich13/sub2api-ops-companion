from __future__ import annotations

import copy
import json
import os
import tempfile
import unittest
from contextlib import nullcontext
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from app.auto_reset import project_state
from app.config_service import ConfigConflict, ConfigService
from app.desktop_api import DesktopService, ScheduleRequest, account_dto
from app.key_fallback import KeyFallbackConfigError, KeyFallbackController
from app.oauth_monitor import OAuthStateStore
from app.settings import Settings, load_settings


NOW = datetime(2026, 10, 8, 4, tzinfo=timezone.utc)
CONNECTION = "oauth_recovery_connection_account_ids"
MODEL = "oauth_recovery_model_account_ids"


def oauth_account(account_id=31, **changes):
    return {
        "id": account_id, "name": f"fixture-oauth-{account_id}", "platform": "openai", "type": "oauth",
        "status": "active", "schedulable": True, "deleted_at": None, "parent_account_id": None,
        "credentials": {"plan_type": "plus"}, "extra": {}, "updated_at": NOW.isoformat(), **changes,
    }


def key_account(account_id=41, **changes):
    return {
        "id": account_id, "name": f"fixture-key-{account_id}", "platform": "openai", "type": "apikey",
        "status": "active", "schedulable": False, "deleted_at": None, "credentials": {}, "extra": {},
        "updated_at": NOW.isoformat(), "group_ids": [], **changes,
    }


def available_quota(*, at=NOW):
    return {
        "success": True, "queried_at": at.isoformat(),
        "oauth_quota": {"plan_type": "plus", "ui_windows": [
            {"key": "codex_5h", "used_percent": 20, "remaining_percent": 80},
            {"key": "codex_7d", "used_percent": 30, "remaining_percent": 70},
        ]},
    }


class ConfigRuntime:
    def __init__(self, root, payload=None):
        self.settings = Settings(
            database_url="fixture", base_path="", audit_path=str(root / "audit.jsonl"),
            oauth_config_path=str(root / "oauth.json"), key_fallback_config_path=str(root / "key.json"),
            usage_query_state_path=str(root / "state.json"),
        )
        setattr(self.settings, CONNECTION, [])
        setattr(self.settings, MODEL, [])
        self.saved_payloads = []
        self.db = Mock()
        self.db.fetch_all.return_value = [oauth_account(31), oauth_account(32)]
        self.key_fallback_controller = None
        self.oauth_monitor = None
        self.path = Path(self.settings.oauth_config_path)
        self.path.write_text(json.dumps(payload or {}), encoding="utf-8")
        self.apply_oauth_runtime_config(payload or {})

    def oauth_config_file(self):
        return json.loads(self.path.read_text(encoding="utf-8"))

    def save_oauth_runtime_config(self, payload):
        self.saved_payloads.append(copy.deepcopy(payload))
        self.path.write_text(json.dumps(payload), encoding="utf-8")

    def apply_oauth_runtime_config(self, payload):
        for key, value in payload.items():
            if key.startswith("oauth_"):
                setattr(self.settings, key, copy.deepcopy(value))


class RecoveryConfig030Tests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.runtime = ConfigRuntime(self.root, {CONNECTION: [31], MODEL: [32]})
        self.service = ConfigService(self.runtime)

    async def test_oauth_lists_are_exposed_and_partial_old_client_save_preserves_them(self):
        initial = self.service.snapshot("oauth")["oauth"]
        self.assertEqual(initial[CONNECTION], [31])
        self.assertEqual(initial[MODEL], [32])
        saved = await self.service.save("oauth", {"oauth_daily_test_time": "06:15"}, "fixture-user", initial["revision"])
        self.assertEqual(saved[CONNECTION], [31])
        self.assertEqual(saved[MODEL], [32])
        self.assertEqual(self.runtime.oauth_config_file()[CONNECTION], [31])
        self.assertEqual(self.runtime.oauth_config_file()[MODEL], [32])
        with self.assertRaises(ConfigConflict):
            await self.service.save("oauth", {CONNECTION: [], MODEL: []}, "fixture-user", initial["revision"])

    async def test_recovery_modes_can_be_switched_in_one_atomic_save(self):
        result = await self.service.save("oauth", {CONNECTION: [32], MODEL: [31]}, "fixture-user")
        self.assertEqual(result[CONNECTION], [32])
        self.assertEqual(result[MODEL], [31])
        self.assertEqual(len(self.runtime.saved_payloads), 1)
        self.assertEqual(getattr(self.runtime.settings, CONNECTION), [32])
        self.assertEqual(getattr(self.runtime.settings, MODEL), [31])

    async def test_overlapping_modes_reject_the_entire_save_without_mutating_state(self):
        before = self.runtime.path.read_bytes()
        for changes in ({CONNECTION: [31, 32]}, {MODEL: [31, 32]}, {CONNECTION: [31], MODEL: [31]}):
            with self.subTest(changes=changes):
                with self.assertRaises(ValueError):
                    await self.service.save("oauth", changes, "fixture-user")
                self.assertEqual(self.runtime.path.read_bytes(), before)
                self.assertEqual(getattr(self.runtime.settings, CONNECTION), [31])
                self.assertEqual(getattr(self.runtime.settings, MODEL), [32])
        self.assertEqual(self.runtime.saved_payloads, [])

    async def test_invalid_account_ids_cannot_change_recovery_selection(self):
        before = self.runtime.path.read_bytes()
        for invalid in ([True], [0], [-1], [31.5], "31"):
            with self.subTest(invalid=invalid):
                with self.assertRaises(ValueError):
                    await self.service.save("oauth", {CONNECTION: invalid, MODEL: []}, "fixture-user")
                self.assertEqual(self.runtime.path.read_bytes(), before)

    async def test_explicit_empty_lists_remain_empty_and_survive_settings_reload(self):
        saved = await self.service.save("oauth", {CONNECTION: [], MODEL: []}, "fixture-user")
        self.assertEqual((saved[CONNECTION], saved[MODEL]), ([], []))
        with patch.dict(os.environ, {"DATABASE_URL": "fixture", "OAUTH_CONFIG_PATH": str(self.runtime.path)}, clear=True):
            reloaded = load_settings()
        self.assertEqual(getattr(reloaded, CONNECTION), [])
        self.assertEqual(getattr(reloaded, MODEL), [])


class RecoveryMigration030Tests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_legacy_accounts_migrate_once_and_new_accounts_are_not_auto_selected(self):
        from app.recovery_policy import migrate_recovery_selection

        runtime = ConfigRuntime(self.root, {"oauth_daily_test_time": "06:15"})
        runtime.db.fetch_all.return_value = [
            oauth_account(31), oauth_account(32),
            oauth_account(33, deleted_at=NOW.isoformat()),
            oauth_account(34, platform="grok"),
            oauth_account(35, type="apikey"),
            oauth_account(36, parent_account_id=31),
        ]
        migrate_recovery_selection(runtime)
        saved = runtime.oauth_config_file()
        self.assertEqual(saved[CONNECTION], [31, 32])
        self.assertEqual(saved[MODEL], [])
        self.assertEqual(saved["oauth_daily_test_time"], "06:15")
        self.assertEqual(getattr(runtime.settings, CONNECTION), [31, 32])
        before = runtime.path.read_bytes()
        runtime.db.fetch_all.return_value.append(oauth_account(39))
        migrate_recovery_selection(runtime)
        self.assertEqual(runtime.path.read_bytes(), before)
        self.assertEqual(len(runtime.saved_payloads), 1)

    def test_existing_empty_or_selected_lists_are_not_replaced_by_inventory(self):
        from app.recovery_policy import migrate_recovery_selection

        for connection, model in (([], []), ([31], [32])):
            with self.subTest(connection=connection, model=model):
                runtime = ConfigRuntime(self.root, {CONNECTION: connection, MODEL: model})
                before = runtime.path.read_bytes()
                migrate_recovery_selection(runtime)
                self.assertEqual(runtime.path.read_bytes(), before)
                self.assertEqual(runtime.saved_payloads, [])
                runtime.db.fetch_all.assert_not_called()

    def test_failed_legacy_inventory_read_does_not_mark_migration_complete(self):
        from app.recovery_policy import migrate_recovery_selection

        runtime = ConfigRuntime(self.root, {"oauth_daily_test_time": "06:15"})
        runtime.db.fetch_all.side_effect = RuntimeError("fixture inventory unavailable")
        before = runtime.path.read_bytes()
        with self.assertRaises(RuntimeError):
            migrate_recovery_selection(runtime)
        self.assertEqual(runtime.path.read_bytes(), before)
        self.assertEqual(runtime.saved_payloads, [])


class KeyCoexist030Tests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.runtime = ConfigRuntime(self.root)
        self.keys = {41: key_account(41), 42: key_account(42, schedulable=True), 43: key_account(43)}
        self.oauth = [oauth_account()]
        self.snapshot = {"oauth_results": {"31": available_quota()}, "scheduler": {}, "pending_events": {}}
        self.calls = []
        monitor = SimpleNamespace(committed_snapshot=lambda: copy.deepcopy(self.snapshot))
        monitor.evaluation_guard = lambda: nullcontext(monitor)
        self.controller = KeyFallbackController(
            self.runtime.settings, self.runtime.db, oauth_monitor=monitor,
            base_url_provider=lambda: "http://127.0.0.1:1", admin_token_provider=lambda: "fixture-admin",
            oauth_inventory=lambda _db: copy.deepcopy(self.oauth), grok_oauth_inventory=lambda _db: [],
            key_inventory=lambda _db: copy.deepcopy(list(self.keys.values())),
            account_reader=lambda _db, aid: copy.deepcopy(self.keys.get(aid)),
            schedulable_runner=self.write_schedulable,
        )
        self.runtime.key_fallback_controller = self.controller

    def write_schedulable(self, aid, enabled, **_connection):
        self.calls.append((aid, enabled))
        self.keys[aid]["schedulable"] = enabled
        return {"success": True}

    def save(self, **changes):
        return self.controller.save_user_config(
            **{"openai_enabled": True, "grok_enabled": False, "managed_account_ids": [41, 42],
               "coexist_account_ids": [41], "user": "fixture-user", **changes})

    def test_coexist_is_persisted_and_legacy_save_preserves_remaining_selection(self):
        self.save(coexist_account_ids=[41, 42])
        config = self.controller.load_config()
        self.assertEqual(config.coexist_account_ids, (41, 42))
        self.assertEqual(self.controller.panel_snapshot()["coexist_account_ids"], [41, 42])
        self.controller.save_user_config(openai_enabled=True, grok_enabled=False, managed_account_ids=[41, 42], user="legacy-client")
        self.assertEqual(self.controller.load_config().coexist_account_ids, (41, 42))
        self.controller.save_user_config(openai_enabled=True, grok_enabled=False, managed_account_ids=[42], user="legacy-client")
        self.assertEqual(self.controller.load_config().coexist_account_ids, (42,))

    async def test_partial_desktop_key_config_save_preserves_coexist_members(self):
        self.save()
        service = ConfigService(self.runtime)
        current = service.snapshot("key_fallback")["key_fallback"]
        saved = await service.save("key_fallback", {"grok_enabled": True}, "fixture-user", current["revision"])
        self.assertEqual(saved["coexist_account_ids"], [41])
        self.assertEqual(saved["managed_account_ids"], [41, 42])
        self.assertTrue(saved["grok_enabled"])

    def test_coexist_must_be_subset_of_managed_ids_and_invalid_save_is_atomic(self):
        self.save()
        path = Path(self.runtime.settings.key_fallback_config_path)
        before = path.read_bytes()
        for invalid in ([43], [True], [0], [-1], [41.5]):
            with self.subTest(invalid=invalid):
                with self.assertRaises(KeyFallbackConfigError):
                    self.save(coexist_account_ids=invalid)
                self.assertEqual(path.read_bytes(), before)

    def test_legacy_config_defaults_to_no_coexist_selection(self):
        path = Path(self.runtime.settings.key_fallback_config_path)
        path.write_text(json.dumps({"enabled": True, "managed_account_ids": [41, 42], "config_version": 1}))
        self.assertTrue(self.controller.load_config().valid)
        self.assertEqual(self.controller.load_config().coexist_account_ids, ())
        self.controller.run_once(now=NOW)
        self.assertEqual(self.calls, [(42, False)])

    def test_invalid_persisted_coexist_subset_disables_policy_without_changes(self):
        self.save()
        path = Path(self.runtime.settings.key_fallback_config_path)
        payload = json.loads(path.read_text())
        payload["coexist_account_ids"] = [43]
        path.write_text(json.dumps(payload))
        self.assertFalse(self.controller.load_config().valid)
        result = self.controller.run_once(now=NOW)
        self.assertTrue(result["skipped"])
        self.assertEqual(self.calls, [])

    def test_available_oauth_enables_coexist_and_disables_only_regular_managed_key(self):
        self.save()
        result = self.controller.run_once(now=NOW)
        self.assertEqual(self.calls, [(41, True), (42, False)])
        self.assertEqual(result["changed_ids"], [41, 42])
        self.assertFalse(self.keys[43]["schedulable"])
        self.calls.clear()
        self.controller.run_once(now=NOW)
        self.assertEqual(self.calls, [])

    def test_unknown_or_stale_oauth_preserves_both_existing_key_states(self):
        self.save()
        for result in (None, {"success": False, "error_code": "network_error", "queried_at": NOW.isoformat()},
                       available_quota(at=NOW - timedelta(hours=2))):
            with self.subTest(result=result):
                self.snapshot["oauth_results"] = {} if result is None else {"31": result}
                self.controller.run_once(now=NOW)
                self.assertEqual(self.calls, [])
                self.assertFalse(self.keys[41]["schedulable"])
                self.assertTrue(self.keys[42]["schedulable"])

    def test_all_unavailable_oauth_still_enables_regular_and_coexist_keys(self):
        self.save()
        self.keys[42]["schedulable"] = False
        self.oauth[0]["schedulable"] = False
        self.snapshot["oauth_results"] = {"31": {"success": False, "error_code": "http_401", "queried_at": NOW.isoformat()}}
        self.controller.run_once(now=NOW)
        self.assertEqual(self.calls, [(41, True), (42, True)])

    def test_disabled_account_and_automatic_degradation_hold_prevent_coexist_enable(self):
        self.save(coexist_account_ids=[41, 42])
        self.keys[41]["status"] = "disabled"
        self.keys[42]["schedulable"] = False
        self.controller.detection_gate = lambda aid: aid == 42
        self.controller.run_once(now=NOW)
        self.assertEqual(self.calls, [])
        self.assertFalse(self.keys[41]["schedulable"])
        self.assertFalse(self.keys[42]["schedulable"])

    def test_manual_disable_detaches_only_target_and_cannot_be_reopened_by_coexist(self):
        self.save(coexist_account_ids=[41, 42])
        self.keys[41]["schedulable"] = True
        self.runtime.oauth_base_url = lambda: "http://127.0.0.1:1"
        self.runtime.db.fetch_one.side_effect = lambda _sql, params: copy.deepcopy(self.keys.get(params["id"]))
        service = DesktopService(self.runtime)
        service._model_detection = SimpleNamespace(human_control=Mock())
        payload = ScheduleRequest(schedulable=False, detach_managed=True,
                                  expected_version=account_dto(self.keys[41], NOW, {41, 42})["version"])

        def manual_write(aid, enabled, **connection):
            config = self.controller.load_config()
            self.assertEqual(config.managed_account_ids, (42,))
            self.assertEqual(config.coexist_account_ids, (42,))
            return self.write_schedulable(aid, enabled, **connection)

        with patch("app.desktop_api.execute_sub2api_set_schedulable", side_effect=manual_write):
            result = service.set_schedulable(41, payload, "fixture-admin")
        self.assertTrue(result["verified"])
        self.assertTrue(result["detached"])
        self.calls.clear()
        self.controller.run_once(now=NOW)
        self.assertEqual(self.calls, [])
        self.assertFalse(self.keys[41]["schedulable"])


class AutoResetProjection030Tests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name) / "oauth-state.json"
        self.store = OAuthStateStore(str(self.path))

    def project_persisted(self, state):
        self.store.update_scheduler({31: {"auto_reset_credit": state}})
        before = self.path.read_bytes()
        value = self.store.scheduler()[31]["auto_reset_credit"]
        original = copy.deepcopy(value)
        projected = project_state(value)
        self.assertEqual(value, original)
        self.assertEqual(self.path.read_bytes(), before)
        reloaded = OAuthStateStore(str(self.path))
        self.assertEqual(reloaded.scheduler()[31]["auto_reset_credit"], original)
        return projected

    def test_completed_or_relinquished_states_are_hidden_without_deleting_history(self):
        for stage in ("manual", "recovered", "closed"):
            with self.subTest(stage=stage):
                value = self.project_persisted({"episode": "fixture-episode", "stage": stage,
                    "consumed": True, "attempt_at": NOW.isoformat(), "owns_pause": False,
                    "reset_at": NOW.isoformat(), "next_at": None})
                self.assertIsNone(value)

    def test_manual_state_without_any_consumption_attempt_is_hidden(self):
        value = self.project_persisted({"episode": "fixture-episode", "stage": "manual",
            "consumed": None, "attempt_at": None, "owns_pause": False, "next_at": None})
        self.assertIsNone(value)

    def test_unknown_consumption_remains_visible_after_manual_control(self):
        for stage in ("uncertain", "manual"):
            with self.subTest(stage=stage):
                value = self.project_persisted({"episode": "fixture-episode", "stage": stage,
                    "consumed": None, "attempt_at": NOW.isoformat(), "owns_pause": False,
                    "next_at": None, "error_code": "result_uncertain"})
                self.assertIsNotNone(value)
                self.assertEqual(value["stage"], "uncertain")
                self.assertEqual(value["attempt_at"], NOW.isoformat())
                self.assertTrue(value["error"])

    def test_active_recovery_steps_remain_visible(self):
        for stage in ("waiting", "pausing", "resetting", "testing", "retry", "confirming", "releasing", "blocked"):
            with self.subTest(stage=stage):
                value = self.project_persisted({"episode": "fixture-episode", "stage": stage,
                    "consumed": False, "owns_pause": False, "next_at": (NOW + timedelta(minutes=1)).isoformat()})
                self.assertIsNotNone(value)
                self.assertEqual(value["stage"], stage)


if __name__ == "__main__":
    unittest.main()
