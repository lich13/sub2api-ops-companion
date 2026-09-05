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

from starlette.datastructures import FormData
from starlette.requests import Request

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
    def test_oauth_monitor_prefers_internal_sso_verify_base_url(self) -> None:
        config = SimpleNamespace(
            base_url="https://public.example.com/",
            verify_base_url="http://sub2api:8080/",
        )
        with patch.object(main_module, "current_sso_config", return_value=config):
            self.assertEqual(main_module.oauth_base_url(), "http://sub2api:8080")

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
        self.assertIn("/key-fallback/config", paths)

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

    def test_telegram_panel_config_exposes_night_recovery_cooldown(self) -> None:
        with (
            patch.object(main_module, "telegram_state", return_value={}),
            patch.object(main_module, "telegram_config_file", return_value={}),
            patch.object(main_module, "ensure_telegram_pairing_code", return_value=""),
            patch.object(
                main_module.settings,
                "telegram_oauth_night_recovery_cooldown_enabled",
                False,
            ),
        ):
            panel = main_module.build_telegram_config()

        self.assertFalse(panel["oauth_night_recovery_cooldown_enabled"])

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
        self.assertTrue(loaded.telegram_oauth_night_recovery_cooldown_enabled)
        self.assertFalse(hasattr(loaded, "telegram_oauth_early_probe_interval_seconds"))
        self.assertFalse(hasattr(loaded, "guard_enabled"))

    def test_night_recovery_cooldown_prefers_json_config_over_environment(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config_path = Path(directory) / "telegram-config.json"
            config_path.write_text(
                '{"oauth_night_recovery_cooldown_enabled": false}',
                encoding="utf-8",
            )
            with patch.dict(
                os.environ,
                {
                    "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                    "OPS_SESSION_SECRET": "secret",
                    "TELEGRAM_CONFIG_PATH": str(config_path),
                    "TELEGRAM_OAUTH_NIGHT_RECOVERY_COOLDOWN_ENABLED": "true",
                },
                clear=True,
            ):
                loaded = load_settings()

        self.assertFalse(loaded.telegram_oauth_night_recovery_cooldown_enabled)


class OAuthSettingsRouteTests(unittest.IsolatedAsyncioTestCase):
    async def test_oauth_settings_persist_new_intervals_and_remove_legacy_probe(self) -> None:
        class FormRequest:
            async def form(self) -> FormData:
                return FormData(
                    [
                        ("oauth_usage_refresh_enabled", "1"),
                        ("oauth_recovery_monitor_enabled", "1"),
                        ("oauth_night_recovery_cooldown_enabled", "1"),
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
            config_path.write_text(
                '{"oauth_early_probe_interval_seconds": 15, "oauth_recovery_push_enabled": true}',
                encoding="utf-8",
            )
            main_module.settings = SimpleNamespace(
                telegram_config_path=str(config_path),
                audit_path=str(root / "audit.jsonl"),
                base_path="/sub2ops",
                telegram_oauth_usage_refresh_enabled=True,
                telegram_oauth_recovery_monitor_enabled=True,
                telegram_oauth_night_recovery_cooldown_enabled=True,
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
                applied_night_cooldown = (
                    main_module.settings.telegram_oauth_night_recovery_cooldown_enabled
                )
            finally:
                main_module.settings = original

        self.assertEqual(response.status_code, 303)
        self.assertEqual(persisted["oauth_regular_refresh_interval_seconds"], 5400)
        self.assertEqual(persisted["oauth_7d_probe_interval_seconds"], 7200)
        self.assertEqual(persisted["oauth_recovery_test_model_id"], "gpt-5.6-luna")
        self.assertTrue(persisted["oauth_night_recovery_cooldown_enabled"])
        self.assertTrue(applied_night_cooldown)
        self.assertNotIn("oauth_early_probe_interval_seconds", persisted)
        self.assertNotIn("oauth_recovery_push_enabled", persisted)

    async def test_oauth_settings_can_disable_night_recovery_cooldown(self) -> None:
        class FormRequest:
            async def form(self) -> FormData:
                return FormData()

        original = main_module.settings
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config_path = root / "telegram-config.json"
            main_module.settings = SimpleNamespace(
                telegram_config_path=str(config_path),
                audit_path=str(root / "audit.jsonl"),
                base_path="/sub2ops",
                telegram_oauth_night_recovery_cooldown_enabled=True,
            )
            try:
                response = await main_module.telegram_oauth_settings_save(FormRequest(), "admin")
                persisted = json.loads(config_path.read_text(encoding="utf-8"))
                applied_night_cooldown = (
                    main_module.settings.telegram_oauth_night_recovery_cooldown_enabled
                )
            finally:
                main_module.settings = original

        self.assertEqual(response.status_code, 303)
        self.assertFalse(persisted["oauth_night_recovery_cooldown_enabled"])
        self.assertFalse(applied_night_cooldown)

    def test_telegram_template_renders_night_recovery_cooldown_checkbox(self) -> None:
        template = (REPO_ROOT / "app/templates/telegram.html").read_text(encoding="utf-8")

        self.assertIn('name="oauth_night_recovery_cooldown_enabled"', template)
        self.assertIn("telegram.oauth_night_recovery_cooldown_enabled", template)

    def test_telegram_template_contains_bark_controls_without_secret_value(self) -> None:
        template = (REPO_ROOT / "app/templates/telegram.html").read_text(encoding="utf-8")

        self.assertIn('action="{{ base_path }}/bark/config"', template)
        self.assertIn('action="{{ base_path }}/bark/push-test"', template)
        self.assertIn('name="bark_device_key"', template)
        self.assertNotIn('value="{{ bark.device_key', template)
        self.assertNotIn("bark_server_url", template)
        self.assertNotIn("服务 URL", template)
        self.assertNotIn("api.day.app", template)
        self.assertNotIn("OAuth 恢复成功、测活失败、自动恢复失败和认证异常只通过 Bark 主动推送。", template)
        self.assertIn("Telegram Bot 消息测试", template)
        self.assertIn('action="{{ base_path }}/key-fallback/config"', template)
        self.assertIn('name="managed_account_ids"', template)
        self.assertIn("启用 Key 回退", template)

    def test_telegram_view_html_omits_bark_server_and_device_key(self) -> None:
        original = main_module.settings
        secret = "never-render-this-device-key"
        custom_url = "https://custom-bark.example.com/root"
        main_module.settings = SimpleNamespace(
            app_name="Sub2API Ops Companion",
            base_path="/sub2ops",
            session_secret="secret",
            session_store_path="/tmp/missing-sessions.json",
            session_ttl_seconds=60,
        )
        try:
            with (
                patch.object(
                    main_module,
                    "build_telegram_config",
                    return_value={
                        "configured": False,
                        "bot_token_set": False,
                        "bot_token_preview": "",
                        "pairing_code": "",
                        "binding_status": "未启用",
                        "paired_chat_ids": [],
                        "paired_user_ids": [],
                        "push_target_count": 0,
                        "control_user_count": 0,
                        "config_updated_at": None,
                        "oauth_usage_refresh_enabled": True,
                        "oauth_recovery_monitor_enabled": True,
                        "oauth_night_recovery_cooldown_enabled": True,
                        "oauth_usage_refresh_concurrency": 4,
                        "oauth_recovery_test_concurrency": 2,
                        "oauth_early_probe_batch_size": 8,
                        "oauth_regular_refresh_interval_seconds": 3600,
                        "oauth_7d_probe_interval_seconds": 3600,
                        "oauth_recovery_test_model_id": "gpt-5.6-luna",
                    },
                ),
                patch.object(
                    main_module,
                    "build_bark_config",
                    return_value={
                        "configured": True,
                        "config_valid": True,
                        "enabled": True,
                        "device_key_set": True,
                        "device_key_status": "已设置",
                        "server_url_valid": True,
                        "config_updated_at": None,
                    },
                ),
                patch.object(
                    main_module,
                    "build_key_fallback_panel",
                    return_value={
                        "enabled": True,
                        "managed_account_ids": [4],
                        "config_valid": True,
                        "config_updated_at": None,
                        "accounts": [{"id": 4, "name": "panel-key"}],
                    },
                ),
            ):
                response = main_module.telegram_view(request("/telegram"), "admin")
        finally:
            main_module.settings = original

        html = bytes(response.body).decode("utf-8")
        self.assertNotIn("bark_server_url", html)
        self.assertNotIn("服务 URL", html)
        self.assertNotIn("api.day.app", html)
        self.assertNotIn(custom_url, html)
        self.assertNotIn(secret, html)
        self.assertNotIn('type="hidden"', html)
        self.assertIn("name=\"bark_device_key\"", html)
        self.assertIn("Bark 事件推送", html)


if __name__ == "__main__":
    unittest.main()
