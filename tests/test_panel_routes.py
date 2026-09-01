from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

os.environ.setdefault("OPS_SESSION_SECRET", "test-session-secret")
os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")

from starlette.requests import Request
from starlette.datastructures import FormData

from app import main as main_module
from app.settings import load_settings


REPO_ROOT = Path(__file__).resolve().parents[1]


def request(path: str = "/") -> Request:
    return Request(
        {
            "type": "http",
            "method": "GET",
            "path": path,
            "headers": [],
            "query_string": b"",
            "client": ("127.0.0.1", 12345),
        }
    )


class PanelRouteTests(unittest.TestCase):
    def test_root_redirects_to_telegram(self) -> None:
        original = main_module.settings
        main_module.settings = SimpleNamespace(base_path="/sub2ops")
        try:
            response = main_module.index(request(), "sub2api:1:admin")
        finally:
            main_module.settings = original

        self.assertEqual(response.status_code, 303)
        self.assertEqual(response.headers["location"], "/sub2ops/telegram")

    def test_removed_routes_are_not_registered(self) -> None:
        paths = {getattr(route, "path", "") for route in main_module.app.routes}

        for removed in (
            "/speed",
            "/guard",
            "/guard/run",
            "/guard/policy",
            "/guard/apply",
            "/telegram/guard-run",
            "/usage-query/settings",
            "/usage-query/query-enabled",
            "/usage-query/accounts/{account_id}",
            "/usage-query/accounts/{account_id}/editor",
            "/usage-query/accounts/{account_id}/query",
            "/usage-query/accounts/{account_id}/delete",
        ):
            self.assertNotIn(removed, paths)

        self.assertFalse(any(path.startswith("/guard") for path in paths))
        self.assertFalse(any(path.startswith("/usage-query") for path in paths))

        self.assertIn("/telegram", paths)
        self.assertIn("/sso", paths)
        self.assertIn("/sso/start", paths)

    def test_removed_templates_and_scripts_are_absent(self) -> None:
        for path in (
            "app/templates/speed.html",
            "app/templates/guard.html",
            "app/templates/guard_queue_section.html",
            "app/templates/guard_suggestions_section.html",
            "app/templates/guard_routing_section.html",
            "app/templates/guard_audit_section.html",
            "app/templates/usage_query_editor.html",
            "app/static/usage-query.js",
            "app/static/guard-queue.js",
            "app/static/guard-sections.js",
            "app/static/group-picker.js",
            "app/static/time-range.js",
            "app/static/table-columns.js",
        ):
            self.assertFalse((REPO_ROOT / path).exists(), path)

    def test_navigation_only_contains_sso_and_telegram(self) -> None:
        base = (REPO_ROOT / "app/templates/base.html").read_text(encoding="utf-8")
        telegram = (REPO_ROOT / "app/templates/telegram.html").read_text(encoding="utf-8")

        self.assertIn(" Sub2API 接入".strip(), base)
        self.assertIn("Telegram", base)
        self.assertNotIn("账号速度", base)
        self.assertNotIn("自动 Guard", base)
        self.assertNotIn("/telegram/guard-run", telegram)
        self.assertNotIn("/whitelist", telegram)
        self.assertNotIn("/endless", telegram)

    def test_settings_ignore_legacy_fast_probe_and_default_to_luna(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config_path = Path(directory) / "telegram-config.json"
            config_path.write_text(
                '{"oauth_early_probe_interval_seconds": 15}',
                encoding="utf-8",
            )
            with patch.dict(
                os.environ,
                {
                    "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                    "OPS_SESSION_SECRET": "secret",
                    "TELEGRAM_CONFIG_PATH": str(config_path),
                },
                clear=True,
            ):
                loaded = load_settings()

        self.assertEqual(loaded.telegram_oauth_regular_refresh_interval_seconds, 3600)
        self.assertEqual(loaded.telegram_oauth_7d_probe_interval_seconds, 3600)
        self.assertEqual(loaded.telegram_oauth_recovery_test_model_id, "gpt-5.6-luna")
        self.assertFalse(hasattr(loaded, "telegram_oauth_early_probe_interval_seconds"))
        self.assertFalse(hasattr(loaded, "guard_enabled"))


class OAuthSettingsRouteTests(unittest.IsolatedAsyncioTestCase):
    async def test_oauth_settings_persist_new_intervals_and_remove_legacy_probe(self) -> None:
        class FormRequest:
            async def form(self) -> FormData:
                return FormData(
                    [
                        ("oauth_usage_refresh_enabled", "1"),
                        ("oauth_recovery_monitor_enabled", "1"),
                        ("oauth_recovery_push_enabled", "1"),
                        ("oauth_usage_refresh_concurrency", "5"),
                        ("oauth_recovery_test_concurrency", "3"),
                        ("oauth_early_probe_batch_size", "9"),
                        ("oauth_regular_refresh_interval_seconds", "5400"),
                        ("oauth_7d_probe_interval_seconds", "7200"),
                        ("oauth_recovery_test_model_id", "gpt-5.6-luna"),
                    ]
                )

        original = main_module.settings
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config_path = root / "telegram-config.json"
            config_path.write_text('{"oauth_early_probe_interval_seconds": 15}', encoding="utf-8")
            main_module.settings = SimpleNamespace(
                telegram_config_path=str(config_path),
                audit_path=str(root / "audit.jsonl"),
                base_path="/sub2ops",
                telegram_oauth_usage_refresh_enabled=True,
                telegram_oauth_recovery_monitor_enabled=True,
                telegram_oauth_recovery_push_enabled=True,
                telegram_oauth_usage_refresh_concurrency=4,
                telegram_oauth_recovery_test_concurrency=2,
                telegram_oauth_early_probe_batch_size=8,
                telegram_oauth_regular_refresh_interval_seconds=3600,
                telegram_oauth_7d_probe_interval_seconds=3600,
                telegram_oauth_recovery_test_model_id="gpt-5.6-luna",
            )
            try:
                response = await main_module.telegram_oauth_settings_save(FormRequest(), "admin")
                persisted = json.loads(config_path.read_text(encoding="utf-8"))
            finally:
                main_module.settings = original

        self.assertEqual(response.status_code, 303)
        self.assertEqual(persisted["oauth_regular_refresh_interval_seconds"], 5400)
        self.assertEqual(persisted["oauth_7d_probe_interval_seconds"], 7200)
        self.assertEqual(persisted["oauth_recovery_test_model_id"], "gpt-5.6-luna")
        self.assertNotIn("oauth_early_probe_interval_seconds", persisted)


if __name__ == "__main__":
    unittest.main()
