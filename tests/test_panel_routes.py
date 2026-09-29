from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")
from fastapi.testclient import TestClient
from app import main as main_module
from app.settings import load_settings


class DesktopOnlyTests(unittest.TestCase):
    def test_retired_entries_are_404_without_cookies_or_admin_authority(self):
        with _client(main_module.app) as client:
            for method, path in [(m,p) for m in ("GET","POST") for p in ("/", "/ops", "/sso", "/sso/start", "/oauth/settings", "/key-fallback/config", "/bark/config", "/bark/push-test", "/system/version", "/system/update", "/system/restart", "/logout", "/static/style.css", "/docs", "/redoc", "/openapi.json")]:
                self.assertEqual(client.request(method,path).status_code,404,(method,path))

    def test_connection_json_precedes_environment_and_uses_internal_url(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/"connection.json"
            path.write_text(json.dumps({"base_url":"https://public.example/", "verify_base_url":"http://sub2api:8080/"}))
            with patch.dict(os.environ,{"DATABASE_URL":"unused","SUB2API_CONFIG_PATH":str(path),"SUB2API_BASE_URL":"https://ignored.example"},clear=True),patch.object(main_module,"settings",load_settings()):
                self.assertEqual(main_module.oauth_base_url(),"http://sub2api:8080")

    def test_settings_ignore_legacy_fast_probe_and_default_to_luna(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config_path = Path(directory) / "oauth-config.json"
            config_path.write_text(
                '{"oauth_early_probe_interval_seconds": 15}',
                encoding="utf-8",
            )
            with patch.dict(
                os.environ,
                {
                    "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                    "OAUTH_CONFIG_PATH": str(config_path),
                },
                clear=True,
            ):
                loaded = load_settings()

        self.assertFalse(hasattr(loaded, "oauth_regular_refresh_interval_seconds"))
        self.assertFalse(hasattr(loaded, "oauth_7d_probe_interval_seconds"))
        self.assertEqual(loaded.oauth_recovery_test_model_id, "gpt-5.6-luna")
        self.assertTrue(loaded.oauth_daily_test_enabled)
        self.assertFalse(hasattr(loaded, "oauth_early_probe_interval_seconds"))
        self.assertFalse(hasattr(loaded, "guard_enabled"))

    def test_daily_test_prefers_json_config_over_environment(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config_path = Path(directory) / "oauth-config.json"
            config_path.write_text(
                '{"oauth_daily_test_enabled": false}',
                encoding="utf-8",
            )
            with patch.dict(
                os.environ,
                {
                    "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                    "OAUTH_CONFIG_PATH": str(config_path),
                    "OAUTH_DAILY_TEST_ENABLED": "true",
                },
                clear=True,
            ):
                loaded = load_settings()

        self.assertFalse(loaded.oauth_daily_test_enabled)


class _client:
    # Route-only checks intentionally do not start production background work.
    def __init__(self, app): self.client = TestClient(app)
    def __enter__(self): return self.client
    def __exit__(self, *_): self.client.close()
