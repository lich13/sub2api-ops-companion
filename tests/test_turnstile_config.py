from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from typing import Any

from app.turnstile import (
    TurnstileRuntimeConfig,
    build_turnstile_panel_config,
    load_turnstile_runtime_config,
    save_turnstile_config,
    verify_turnstile_token,
)

REPO_ROOT = Path(__file__).resolve().parents[1]


class FakeHTTPResponse:
    def __init__(self, payload: dict[str, Any]) -> None:
        self.payload = payload

    def __enter__(self) -> "FakeHTTPResponse":
        return self

    def __exit__(self, *_: object) -> None:
        return None

    def read(self) -> bytes:
        return json.dumps(self.payload).encode("utf-8")


class TurnstileConfigTests(unittest.TestCase):
    def test_panel_config_hides_secret_value(self) -> None:
        panel = build_turnstile_panel_config(
            TurnstileRuntimeConfig(
                enabled=True,
                site_key="0x4AAAAAADPfCPB_O-N3j6ON",
                secret_key="secret-do-not-render",
                updated_at="2026-05-15T00:00:00Z",
            )
        )

        self.assertTrue(panel["enabled"])
        self.assertEqual(panel["site_key"], "0x4AAAAAADPfCPB_O-N3j6ON")
        self.assertTrue(panel["secret_key_set"])
        self.assertNotIn("secret_key", panel)
        self.assertNotIn("secret-do-not-render", str(panel))

    def test_save_preserves_existing_secret_when_blank(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            path = str(Path(tmpdir) / "turnstile.json")
            save_turnstile_config(
                path,
                enabled=True,
                site_key="site-key",
                secret_key="secret-key",
                updated_by="admin",
            )

            save_turnstile_config(
                path,
                enabled=True,
                site_key="new-site-key",
                secret_key=None,
                updated_by="admin",
            )
            runtime = load_turnstile_runtime_config(path)

            self.assertTrue(runtime.enabled)
            self.assertEqual(runtime.site_key, "new-site-key")
            self.assertEqual(runtime.secret_key, "secret-key")

    def test_verifier_blocks_enabled_login_without_token(self) -> None:
        result = verify_turnstile_token(
            TurnstileRuntimeConfig(enabled=True, site_key="site", secret_key="secret"),
            token="",
            remote_ip="127.0.0.1",
            opener=lambda *_args, **_kwargs: FakeHTTPResponse({"success": True}),
        )

        self.assertFalse(result.ok)
        self.assertEqual(result.reason, "missing_token")

    def test_verifier_accepts_cloudflare_success_response(self) -> None:
        calls: list[Any] = []

        def opener(request: Any, **kwargs: Any) -> FakeHTTPResponse:
            calls.append((request, kwargs))
            return FakeHTTPResponse({"success": True})

        result = verify_turnstile_token(
            TurnstileRuntimeConfig(enabled=True, site_key="site", secret_key="secret"),
            token="token",
            remote_ip="127.0.0.1",
            opener=opener,
        )

        self.assertTrue(result.ok)
        self.assertEqual(result.reason, "verified")
        self.assertEqual(len(calls), 1)
        body = calls[0][0].data.decode("utf-8")
        self.assertIn("secret=secret", body)
        self.assertIn("response=token", body)
        self.assertIn("remoteip=127.0.0.1", body)

    def test_login_and_panel_templates_are_ops_local(self) -> None:
        base = (REPO_ROOT / "app" / "templates" / "base.html").read_text(encoding="utf-8")
        login = (REPO_ROOT / "app" / "templates" / "login.html").read_text(encoding="utf-8")
        panel = (REPO_ROOT / "app" / "templates" / "turnstile.html").read_text(encoding="utf-8")
        main_py = (REPO_ROOT / "app" / "main.py").read_text(encoding="utf-8")

        self.assertIn('href="{{ base_path }}/turnstile"', base)
        self.assertIn("登录防护", base)
        self.assertIn("cf-turnstile", login)
        self.assertIn("cf-turnstile-response", main_py)
        self.assertIn("verify_turnstile_token", main_py)
        self.assertIn("Ops Companion 登录", panel)
        self.assertNotIn("window.__APP_CONFIG__", panel)
        self.assertNotIn("Sub2API 的 Turnstile", panel)
        self.assertNotIn("settings</dd>", panel)


if __name__ == "__main__":
    unittest.main()
