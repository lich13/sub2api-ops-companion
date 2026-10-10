from __future__ import annotations

import asyncio
import copy
import json
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

from app.config_service import ConfigConflict, ConfigService
from app.oauth_monitor import OAuthMonitor
from app.recovery_policy import CONNECTION_IDS, MODEL_IDS, SEEN_IDS, migrate_recovery_selection
from app.settings import Settings


NOW = datetime(2026, 10, 9, 4, tzinfo=timezone.utc)


def oauth_account(account_id: int, **changes) -> dict:
    return {
        "id": account_id,
        "name": f"fixture-oauth-{account_id}",
        "platform": "openai",
        "type": "oauth",
        "status": "active",
        "schedulable": True,
        "deleted_at": None,
        "parent_account_id": None,
        "credentials": {"plan_type": "plus"},
        "extra": {},
        **changes,
    }


class Runtime:
    def __init__(self, root: Path, payload: dict | None = None):
        self.settings = Settings(
            database_url="fixture",
            base_path="",
            audit_path=str(root / "audit.jsonl"),
            oauth_config_path=str(root / "oauth.json"),
            key_fallback_config_path=str(root / "key.json"),
            usage_query_state_path=str(root / "state.json"),
        )
        for key, default in ((CONNECTION_IDS, None), (MODEL_IDS, None), (SEEN_IDS, None)):
            setattr(self.settings, key, copy.deepcopy(default))
        self.db = Mock()
        self.db.fetch_all.return_value = [oauth_account(31), oauth_account(32)]
        self.key_fallback_controller = None
        self.saved_payloads: list[dict] = []
        self.path = Path(self.settings.oauth_config_path)
        if not self.path.exists():
            self.path.write_text(json.dumps(payload or {}), encoding="utf-8")
        self.apply_oauth_runtime_config(self.oauth_config_file())

    def oauth_config_file(self) -> dict:
        return json.loads(self.path.read_text(encoding="utf-8"))

    def save_oauth_runtime_config(self, payload: dict) -> None:
        self.saved_payloads.append(copy.deepcopy(payload))
        self.path.write_text(json.dumps(payload), encoding="utf-8")

    def apply_oauth_runtime_config(self, payload: dict) -> None:
        for key, value in payload.items():
            if key.startswith("oauth_"):
                setattr(self.settings, key, copy.deepcopy(value))


class RecoverySelection034Tests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_upgrade_records_seen_baseline_without_adding_to_explicit_selection(self):
        runtime = Runtime(self.root, {CONNECTION_IDS: [], MODEL_IDS: []})
        rows = [oauth_account(31), oauth_account(32), oauth_account(33, platform="grok")]

        changed = migrate_recovery_selection(runtime, rows)

        self.assertTrue(changed)
        saved = runtime.oauth_config_file()
        self.assertEqual(saved[CONNECTION_IDS], [])
        self.assertEqual(saved[MODEL_IDS], [])
        self.assertEqual(saved[SEEN_IDS], [31, 32])
        self.assertEqual(runtime.saved_payloads[-1][CONNECTION_IDS], [])

    def test_first_new_eligible_account_is_added_once_but_removal_and_reappearance_are_sticky(self):
        runtime = Runtime(self.root, {"oauth_daily_test_time": "06:15"})
        initial_rows = [oauth_account(31), oauth_account(32)]
        migrate_recovery_selection(runtime, initial_rows)
        self.assertEqual(runtime.oauth_config_file()[CONNECTION_IDS], [31, 32])

        new_rows = [*initial_rows, oauth_account(39)]
        migrate_recovery_selection(runtime, new_rows)
        self.assertEqual(runtime.oauth_config_file()[CONNECTION_IDS], [31, 32, 39])
        self.assertEqual(runtime.oauth_config_file()[SEEN_IDS], [31, 32, 39])

        # An administrator removal is preserved by the seen watermark.
        service = ConfigService(runtime)
        current = service.snapshot("oauth")["oauth"]
        saved = asyncio.run(service.save(
            "oauth", {CONNECTION_IDS: [32, 39], MODEL_IDS: []}, "fixture-user", current["revision"]
        ))
        self.assertEqual(runtime.oauth_config_file()[SEEN_IDS], [31, 32, 39])
        migrate_recovery_selection(runtime, new_rows)
        self.assertEqual(runtime.oauth_config_file()[CONNECTION_IDS], [32, 39])

        # Deleting and reintroducing the removed account cannot select it again.
        migrate_recovery_selection(runtime, [oauth_account(32), oauth_account(39)])
        migrate_recovery_selection(runtime, new_rows)
        self.assertEqual(runtime.oauth_config_file()[CONNECTION_IDS], [32, 39])

    def test_moving_account_to_model_mode_is_not_reversed_by_inventory_reconcile(self):
        runtime = Runtime(self.root, {CONNECTION_IDS: [31, 32], MODEL_IDS: [], SEEN_IDS: [31, 32]})
        service = ConfigService(runtime)
        current = service.snapshot("oauth")["oauth"]
        saved = asyncio.run(service.save(
            "oauth", {CONNECTION_IDS: [32], MODEL_IDS: [31]}, "fixture-user", current["revision"]
        ))
        self.assertEqual((saved[CONNECTION_IDS], saved[MODEL_IDS]), ([32], [31]))

        changed = runtime.saved_payloads[-1]
        migrate_recovery_selection(runtime, [oauth_account(31), oauth_account(32)])
        self.assertEqual(runtime.oauth_config_file()[CONNECTION_IDS], [32])
        self.assertEqual(runtime.oauth_config_file()[MODEL_IDS], [31])
        self.assertEqual(runtime.oauth_config_file()[SEEN_IDS], [31, 32])
        self.assertEqual(runtime.oauth_config_file()[CONNECTION_IDS], changed[CONNECTION_IDS])

    def test_failed_selection_write_preserves_both_lists_seen_watermark_and_runtime_settings(self):
        runtime = Runtime(self.root, {CONNECTION_IDS: [31], MODEL_IDS: [], SEEN_IDS: [31]})
        before = runtime.path.read_bytes()
        runtime.save_oauth_runtime_config = Mock(side_effect=OSError("fixture write failed"))

        with self.assertRaises(OSError):
            ConfigService(runtime).reconcile_recovery_accounts([oauth_account(31), oauth_account(39)])

        self.assertEqual(runtime.path.read_bytes(), before)
        self.assertEqual(runtime.oauth_config_file(), {CONNECTION_IDS: [31], MODEL_IDS: [], SEEN_IDS: [31]})
        self.assertEqual(getattr(runtime.settings, CONNECTION_IDS), [31])
        self.assertEqual(getattr(runtime.settings, MODEL_IDS), [])
        self.assertEqual(getattr(runtime.settings, SEEN_IDS), [31])

    def test_corrupt_or_deleted_existing_config_stops_reconcile_without_replacing_it(self):
        runtime = Runtime(self.root, {CONNECTION_IDS: [31], MODEL_IDS: [], SEEN_IDS: [31]})
        service = ConfigService(runtime)
        service.reconcile_recovery_accounts([oauth_account(31)])

        runtime.path.write_text("{", encoding="utf-8")
        with self.assertRaises(json.JSONDecodeError):
            service.reconcile_recovery_accounts([oauth_account(31), oauth_account(39)])
        self.assertEqual(runtime.path.read_text(encoding="utf-8"), "{")

        runtime.path.write_text(json.dumps({CONNECTION_IDS: [31], MODEL_IDS: [], SEEN_IDS: [31]}), encoding="utf-8")
        service.reconcile_recovery_accounts([oauth_account(31)])
        runtime.path.unlink()
        with self.assertRaises(ValueError):
            service.reconcile_recovery_accounts([oauth_account(31), oauth_account(39)])
        self.assertFalse(runtime.path.exists())

    def test_restart_keeps_seen_watermark_and_does_not_reselect_old_account(self):
        runtime = Runtime(self.root, {CONNECTION_IDS: [32], MODEL_IDS: [], SEEN_IDS: [31, 32]})
        runtime2 = Runtime(self.root)

        migrate_recovery_selection(runtime2, [oauth_account(31), oauth_account(32)])

        self.assertEqual(runtime2.oauth_config_file()[CONNECTION_IDS], [32])
        self.assertEqual(runtime2.oauth_config_file()[SEEN_IDS], [31, 32])

    def test_old_revision_cannot_overwrite_a_new_automatic_account_and_partial_save_keeps_it(self):
        runtime = Runtime(self.root, {CONNECTION_IDS: [31, 32], MODEL_IDS: [], SEEN_IDS: [31, 32]})
        service = ConfigService(runtime)
        old_revision = service.snapshot("oauth")["oauth"]["revision"]
        service.reconcile_recovery_accounts([oauth_account(31), oauth_account(32), oauth_account(39)])
        with self.assertRaises(ConfigConflict):
            asyncio.run(service.save("oauth", {"oauth_daily_test_time": "08:00"}, "fixture-user", old_revision))
        self.assertEqual(runtime.oauth_config_file()[CONNECTION_IDS], [31, 32, 39])
        asyncio.run(service.save("oauth", {"oauth_daily_test_time": "08:00"}, "fixture-user"))
        self.assertEqual(runtime.oauth_config_file()[CONNECTION_IDS], [31, 32, 39])

    def test_monitor_run_once_refreshes_inventory_without_network_when_recovery_is_disabled(self):
        state_path = self.root / "monitor-state.json"
        settings = SimpleNamespace(
            usage_query_state_path=str(state_path),
            audit_path=str(self.root / "monitor-audit.jsonl"),
            oauth_recovery_monitor_enabled=False,
            oauth_daily_test_enabled=False,
            oauth_usage_refresh_concurrency=1,
            oauth_recovery_test_concurrency=1,
            oauth_early_probe_batch_size=8,
            oauth_recovery_test_model_id="gpt-5.6-luna",
            oauth_recovery_connection_account_ids=[],
            oauth_recovery_model_account_ids=[],
        )
        inventory = Mock(return_value=[oauth_account(31)])
        observer = Mock()
        usage = Mock(side_effect=AssertionError("network usage query must not run"))
        test = Mock(side_effect=AssertionError("model test must not run"))
        recovery = Mock(side_effect=AssertionError("recovery request must not run"))
        monitor = OAuthMonitor(
            settings,
            Mock(),
            base_url_provider=lambda: "https://example.invalid",
            inventory_loader=inventory,
            usage_runner=usage,
            test_runner=test,
            recovery_runner=recovery,
            inventory_observer=observer,
            clock=lambda: NOW,
        )

        monitor.run_once(now=NOW)

        inventory.assert_called_once()
        observer.assert_called_once_with([oauth_account(31)])
        usage.assert_not_called()
        test.assert_not_called()
        recovery.assert_not_called()

    def test_concurrent_reconcile_and_partial_save_preserve_all_fields_without_inventory_fetch(self):
        runtime = Runtime(self.root, {CONNECTION_IDS: [31, 32], MODEL_IDS: [], SEEN_IDS: [31, 32]})
        service = ConfigService(runtime)
        rows_with_39 = [oauth_account(31), oauth_account(32), oauth_account(39)]
        rows_with_40 = [oauth_account(31), oauth_account(32), oauth_account(40)]

        async def run() -> None:
            await asyncio.gather(
                asyncio.to_thread(service.reconcile_recovery_accounts, rows_with_39),
                asyncio.to_thread(service.reconcile_recovery_accounts, rows_with_40),
                service.save("oauth", {"oauth_daily_test_time": "07:30"}, "fixture-user"),
            )

        asyncio.run(run())
        saved = runtime.oauth_config_file()
        self.assertEqual(saved["oauth_daily_test_time"], "07:30")
        self.assertEqual(saved[CONNECTION_IDS], [31, 32, 39, 40])
        self.assertEqual(saved[MODEL_IDS], [])
        self.assertEqual(saved[SEEN_IDS], [31, 32, 39, 40])
        runtime.db.fetch_all.assert_not_called()


if __name__ == "__main__":
    unittest.main()
