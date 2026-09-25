import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts.migrate_oauth_config import migrate
from app.settings import load_settings


class OAuthConfigMigrationTests(unittest.TestCase):
    def fixture(self, root):
        env = {"TELEGRAM_CONFIG_PATH": str(root / "old.json"), "TELEGRAM_STATE_PATH": str(root / "pairing.json"),
               "OAUTH_CONFIG_PATH": str(root / "oauth.json"), "USAGE_QUERY_STATE_PATH": str(root / "state.json"),
               "TELEGRAM_OAUTH_USAGE_REFRESH_CONCURRENCY": "7", "OPS_SESSION_SECRET": "test", "DATABASE_URL": "test"}
        (root / "old.json").write_text(json.dumps({"bot_token": "discard-token", "oauth_daily_test_enabled": False,
            "oauth_daily_test_time": "05:15", "oauth_recovery_monitor_enabled": False, "oauth_recovery_test_model_id": "keep-model"}))
        (root / "pairing.json").write_text('{"paired_user_ids":[1]}')
        (root / "state.json").write_text(json.dumps({"admin_token": "keep-key", "pending_events": {"e": {"id": "keep"}},
            "daily_test": {"time": "05:15"}, "oauth_results": {"1": {"summary": {"telegram_windows": [1], "ui_windows": [2]}}}}))
        (root / ".env").write_text('DATABASE_URL=keep\nTELEGRAM_BOT_TOKEN=discard-token\nTELEGRAM_OAUTH_DAILY_TEST_TIME=05:00\nBARK_DEVICE_KEY=keep-bark\n')
        return env

    def test_migration_preserves_effective_settings_and_state_without_bot_secret(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d); env = self.fixture(root)
            result = migrate(env, root / ".env")
            self.assertTrue(result["legacy_removed"])
            value = json.loads((root / "oauth.json").read_text())
            self.assertEqual(value["oauth_daily_test_time"], "05:15")
            self.assertFalse(value["oauth_daily_test_enabled"])
            self.assertFalse(value["oauth_recovery_monitor_enabled"])
            self.assertEqual(value["oauth_usage_refresh_concurrency"], 7)
            self.assertEqual(value["oauth_recovery_test_model_id"], "keep-model")
            self.assertNotIn("discard-token", (root / ".env").read_text() + (root / "oauth.json").read_text())
            self.assertIn("BARK_DEVICE_KEY=keep-bark", (root / ".env").read_text())
            state = json.loads((root / "state.json").read_text())
            self.assertEqual(state["admin_token"], "keep-key")
            self.assertEqual(state["oauth_results"]["1"]["summary"], {"ui_windows": [2]})
            self.assertEqual(state["pending_events"], {"e": {"id": "keep"}})
            for p in ("oauth.json", "state.json", ".env"):
                self.assertEqual((root / p).stat().st_mode & 0o777, 0o600)
            before = (root / "oauth.json").read_bytes()
            migrate(env, root / ".env")
            self.assertEqual((root / "oauth.json").read_bytes(), before)
            with patch.dict(os.environ, env, clear=True):
                config = load_settings()
            self.assertFalse(hasattr(config, "telegram_bot_token"))
            self.assertEqual(config.oauth_daily_test_time, "05:15")

    def test_new_config_wins_and_invalid_input_never_deletes_old_files(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d); env = self.fixture(root)
            (root / "oauth.json").write_text('{"oauth_daily_test_time":"07:31"}')
            migrate(env, root / ".env")
            self.assertEqual(json.loads((root / "oauth.json").read_text())["oauth_daily_test_time"], "07:31")
        with tempfile.TemporaryDirectory() as d:
            root = Path(d); env = self.fixture(root)
            (root / "old.json").write_text('{"oauth_daily_test_time":"invalid"}')
            before = (root / ".env").read_bytes()
            with self.assertRaises(ValueError): migrate(env, root / ".env")
            self.assertTrue((root / "old.json").exists())
            self.assertTrue((root / "pairing.json").exists())
            self.assertFalse((root / "oauth.json").exists())
            self.assertEqual((root / ".env").read_bytes(), before)

    def test_save_failure_keeps_sources_and_existing_config(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d); env = self.fixture(root)
            with patch("scripts.migrate_oauth_config.os.replace", side_effect=OSError("disk full")):
                with self.assertRaises(OSError): migrate(env, root / ".env")
            self.assertTrue((root / "old.json").exists())
            self.assertTrue((root / "pairing.json").exists())
            self.assertEqual(list(root.glob(".oauth-migration-*")), [])

    def test_runtime_only_reads_neutral_config_and_neutral_env(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d); env = self.fixture(root)
            env["OAUTH_DAILY_TEST_TIME"] = "08:12"
            with patch.dict(os.environ, env, clear=True):
                self.assertEqual(load_settings().oauth_daily_test_time, "08:12")
            self.assertTrue((root / "old.json").exists())
