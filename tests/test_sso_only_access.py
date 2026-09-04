from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

os.environ.setdefault("OPS_SESSION_SECRET", "test-session-secret")
os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")

from fastapi import HTTPException
from starlette.requests import Request

from app import main as main_module
from app.settings import load_settings

REPO_ROOT = Path(__file__).resolve().parents[1]


def protected_request(path: str = "/telegram") -> Request:
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


class SSOOnlyAccessTests(unittest.TestCase):
    def setUp(self) -> None:
        self.original_settings = main_module.settings
        self.tmpdir = tempfile.TemporaryDirectory()
        data_dir = Path(self.tmpdir.name)
        main_module.settings = SimpleNamespace(
            audit_path=str(data_dir / "audit.jsonl"),
            base_path="/sub2ops",
            session_secret="very-long-session-secret",
            session_store_path=str(data_dir / "sessions.json"),
            session_ttl_seconds=31536000,
            sso_config_path=str(data_dir / "sso-config.json"),
            sub2api_base_url="https://661313.xyz",
            sub2api_verify_base_url="http://sub2api:8080",
            sub2api_sso_enabled=True,
            sub2api_sso_required_role="Admin",
            sub2api_sso_session_ttl_seconds=600,
            sub2api_sso_verify_timeout_seconds=3,
        )

    def tearDown(self) -> None:
        main_module.settings = self.original_settings
        self.tmpdir.cleanup()

    def test_unauthenticated_panel_access_rejects_without_login_redirect(self) -> None:
        with self.assertRaises(HTTPException) as raised:
            main_module.require_auth(protected_request())

        error = raised.exception
        self.assertEqual(error.status_code, 403)
        self.assertEqual(error.detail, "Sub2API SSO required")
        self.assertEqual(error.headers["Cache-Control"], "no-store")
        self.assertEqual(error.headers["Referrer-Policy"], "no-referrer")
        self.assertNotIn("Location", error.headers)

    def test_deleted_login_and_turnstile_artifacts_are_absent(self) -> None:
        template_dir = REPO_ROOT / "app" / "templates"
        app_dir = REPO_ROOT / "app"
        main_py = (app_dir / "main.py").read_text(encoding="utf-8")
        settings_py = (app_dir / "settings.py").read_text(encoding="utf-8")
        base = (template_dir / "base.html").read_text(encoding="utf-8")
        style = (app_dir / "static" / "style.css").read_text(encoding="utf-8")

        self.assertFalse((template_dir / "login.html").exists())
        self.assertFalse((template_dir / "turnstile.html").exists())
        self.assertFalse((app_dir / "turnstile.py").exists())

        for old_entrypoint in (
            '@app.get("/login"',
            '@app.post("/login"',
            '@app.get("/turnstile"',
            '@app.post("/turnstile"',
            "login_redirect",
            "cf-turnstile-response",
            "verify_turnstile_token",
            "load_turnstile_runtime_config",
            "OPS_TURNSTILE",
            "turnstile_config_path",
            "turnstile_verify_timeout_seconds",
            "basic_user",
            "basic_password",
        ):
            self.assertNotIn(old_entrypoint, main_py)
            self.assertNotIn(old_entrypoint, settings_py)

        self.assertIn('href="{{ base_path }}/sso"', base)
        self.assertIn("Sub2API 接入", base)
        self.assertNotIn("登录防护", base)
        self.assertNotIn("turnstile", style.lower())
        self.assertNotIn("login-shell", style)
        self.assertNotIn("login-form", style)

    def test_sso_config_page_contains_only_sso_controls(self) -> None:
        template = (REPO_ROOT / "app" / "templates" / "sso.html").read_text(encoding="utf-8")

        self.assertIn("Sub2API 免二次登录", template)
        self.assertIn('action="{{ base_path }}/sso-config"', template)
        self.assertIn("verify_base_url", template)
        self.assertIn("Sub2API 管理员 API Key", template)
        self.assertIn('name="sub2api_admin_token"', template)
        self.assertIn("已保存，留空保留", template)
        self.assertIn("管理员 API Key", template)
        self.assertNotIn("Sub2API 源码和镜像不会被修改", template)
        self.assertNotIn("Ops Companion 登录", template)
        self.assertNotIn("Turnstile", template)
        self.assertNotIn("Cloudflare", template)
        self.assertNotIn("cf-turnstile", template)
        self.assertNotIn("/turnstile", template)

    def test_brand_icon_is_static_svg_asset(self) -> None:
        icon = (REPO_ROOT / "app" / "static" / "sub2ops-icon.svg").read_text(encoding="utf-8")
        base = (REPO_ROOT / "app" / "templates" / "base.html").read_text(encoding="utf-8")

        self.assertIn("<svg", icon)
        self.assertIn('viewBox="0 0 64 64"', icon)
        self.assertIn("<title>Sub2API Ops Companion</title>", icon)
        self.assertIn('href="{{ base_path }}/static/sub2ops-icon.svg', base)
        self.assertIn('src="{{ base_path }}/static/sub2ops-icon.svg', base)
        self.assertNotIn('<span class="brand-mark">S</span>', base)

    def test_settings_load_without_legacy_basic_password(self) -> None:
        with patch.dict(
            os.environ,
            {
                "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                "OPS_SESSION_SECRET": "session-secret-only",
            },
            clear=True,
        ):
            loaded = load_settings()

        self.assertEqual(loaded.session_secret, "session-secret-only")
        self.assertFalse(hasattr(loaded, "basic_user"))
        self.assertFalse(hasattr(loaded, "basic_password"))

    def test_settings_loads_oauth_switches_and_ignores_legacy_push_switch(self) -> None:
        with patch.dict(
            os.environ,
            {
                "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                "OPS_SESSION_SECRET": "session-secret-only",
                "TELEGRAM_OAUTH_USAGE_REFRESH_ENABLED": "0",
                "TELEGRAM_OAUTH_RECOVERY_MONITOR_ENABLED": "false",
                "TELEGRAM_OAUTH_NIGHT_RECOVERY_COOLDOWN_ENABLED": "off",
                "TELEGRAM_OAUTH_RECOVERY_PUSH_ENABLED": "off",
            },
            clear=True,
        ):
            loaded = load_settings()

        self.assertFalse(loaded.telegram_oauth_usage_refresh_enabled)
        self.assertFalse(loaded.telegram_oauth_recovery_monitor_enabled)
        self.assertFalse(loaded.telegram_oauth_night_recovery_cooldown_enabled)
        self.assertFalse(hasattr(loaded, "telegram_oauth_recovery_push_enabled"))

    def test_removed_guard_settings_are_absent(self) -> None:
        with patch.dict(
            os.environ,
            {
                "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                "OPS_SESSION_SECRET": "session-secret-only",
                "GUARD_ENABLED": "true",
                "GUARD_INTERVAL_SECONDS": "2",
            },
            clear=True,
        ):
            loaded = load_settings()

        self.assertFalse(hasattr(loaded, "guard_enabled"))
        self.assertFalse(hasattr(loaded, "guard_interval_seconds"))


if __name__ == "__main__":
    unittest.main()
