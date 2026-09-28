import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts.migrate_desktop_only import migrate


class MigrationTests(unittest.TestCase):
    def test_retains_effective_urls_and_unrelated_secrets_without_retired_backup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old, new, session, env = (root / name for name in ("sso.json", "connection.json", "sessions.json", ".env"))
            old.write_text(json.dumps({"base_url": "https://public.example/", "verify_base_url": "http://sub2api:8080/", "enabled": False}))
            session.write_text('{"secret":"retire"}')
            env.write_text("DATABASE_URL=keep-private\nOPS_SESSION_SECRET=retire\nSUB2API_SSO_ENABLED=true\nOAUTH_DAILY_TEST_TIME=05:15\nBARK_DEVICE_KEY=keep-bark\n")
            with patch.dict(os.environ, {"OPS_SSO_CONFIG_PATH": str(old), "SUB2API_CONFIG_PATH": str(new), "OPS_SESSION_STORE_PATH": str(session), "SUB2API_BASE_URL": "https://ignored.example"}, clear=True):
                result = migrate(env, remove_menu=False)
                self.assertTrue(result["connection_migrated"])
                self.assertEqual(json.loads(new.read_text()), {"base_url": "https://public.example", "verify_base_url": "http://sub2api:8080"})
                self.assertEqual(new.stat().st_mode & 0o777, 0o600)
                self.assertEqual(env.stat().st_mode & 0o777, 0o600)
                self.assertEqual(env.read_text(), "DATABASE_URL=keep-private\nOAUTH_DAILY_TEST_TIME=05:15\nBARK_DEVICE_KEY=keep-bark\n")
                self.assertFalse(old.exists()); self.assertFalse(session.exists())
                self.assertEqual({p.name for p in root.iterdir()}, {"connection.json", ".env", "group-model-config.json"})
                self.assertEqual(json.loads((root / "group-model-config.json").read_text()), {"schema": 1, "groups": {}})
                self.assertEqual(migrate(env, remove_menu=False)["retired_bytes"], 0)

    def test_invalid_or_overlapping_connection_aborts_before_deleting_state(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); old = root / "sso.json"; session = root / "sessions.json"; env = root / ".env"
            old.write_text('{"base_url":"invalid"}'); session.write_text("keep"); env.write_text("OPS_SESSION_SECRET=keep\n")
            variables = {"OPS_SSO_CONFIG_PATH": str(old), "SUB2API_CONFIG_PATH": str(root / "connection.json"), "OPS_SESSION_STORE_PATH": str(session)}
            with patch.dict(os.environ, variables, clear=True), self.assertRaises(ValueError): migrate(env, remove_menu=False)
            self.assertEqual(session.read_text(), "keep"); self.assertIn("OPS_SESSION", env.read_text())
            variables["SUB2API_CONFIG_PATH"] = str(old)
            with patch.dict(os.environ, variables, clear=True), self.assertRaises(RuntimeError): migrate(env, remove_menu=False)

    def test_menu_removal_matches_path_and_origin_only(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); old = root / "sso.json"
            old.write_text('{"base_url":"https://public.example"}')
            items = [{"id": 1, "url": "https://public.example/sub2ops/sso/start"}, {"id": 2, "url": "https://elsewhere.example/sub2ops/sso/start"}, {"id": 3, "url": "https://public.example/other"}]
            from unittest.mock import MagicMock
            connection = MagicMock(); connection.__enter__.return_value = connection
            connection.execute.return_value.fetchone.return_value = (json.dumps(items),)
            with patch.dict(os.environ, {"OPS_SSO_CONFIG_PATH": str(old), "SUB2API_CONFIG_PATH": str(root / "connection.json"), "OPS_SESSION_STORE_PATH": str(root / "sessions"), "DATABASE_URL": "disposable"}, clear=True), patch("scripts.migrate_desktop_only.psycopg.connect", return_value=connection):
                self.assertEqual(migrate(root / ".env")["menu_items_removed"], 1)
            written = connection.execute.call_args.args[1][0]
            self.assertEqual([i["id"] for i in json.loads(written)], [2, 3])
