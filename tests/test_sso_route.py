from __future__ import annotations

import os
import tempfile
import unittest
from http.cookies import SimpleCookie
from pathlib import Path
from types import SimpleNamespace

os.environ.setdefault("OPS_SESSION_SECRET", "test-session-secret")
os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")

from starlette.requests import Request

from app import main as main_module
from app.sub2api_sso import Sub2APISSOError, Sub2APISSOUser


def fake_request() -> Request:
    return Request(
        {
            "type": "http",
            "method": "GET",
            "path": "/sso/start",
            "headers": [],
            "query_string": b"src_host=https%3A%2F%2F661313.xyz",
            "client": ("127.0.0.1", 12345),
        }
    )


class Sub2APISSORouteTests(unittest.TestCase):
    def setUp(self) -> None:
        self.original_settings = main_module.settings
        self.original_validator = main_module.validate_sub2api_token
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
        main_module.validate_sub2api_token = self.original_validator
        self.tmpdir.cleanup()

    def test_successful_sso_sets_opaque_ops_session_and_cleans_redirect(self) -> None:
        calls: list[dict[str, object]] = []

        def fake_validator(*args: object, **kwargs: object) -> Sub2APISSOUser:
            calls.append({"args": args, "kwargs": kwargs})
            return Sub2APISSOUser(id=7, username="admin", role="admin")

        main_module.validate_sub2api_token = fake_validator

        response = main_module.sub2api_sso_start(
            fake_request(),
            token="jwt-token",
            user_id="7",
            next="/speed",
        )

        self.assertEqual(response.status_code, 303)
        self.assertEqual(response.headers["location"], "/sub2ops/speed")
        self.assertEqual(response.headers["Cache-Control"], "no-store")
        self.assertEqual(response.headers["Referrer-Policy"], "no-referrer")
        self.assertIn("frame-ancestors", response.headers["Content-Security-Policy"])
        self.assertIn("https://661313.xyz", response.headers["Content-Security-Policy"])
        self.assertEqual(calls[0]["args"][0], "http://sub2api:8080")
        self.assertEqual(calls[0]["kwargs"]["token"], "jwt-token")
        self.assertEqual(calls[0]["kwargs"]["expected_user_id"], "7")
        self.assertEqual(calls[0]["kwargs"]["required_role"], "Admin")

        raw_cookie = response.headers["set-cookie"]
        self.assertIn(f"{main_module.SESSION_COOKIE}=v2.", raw_cookie)
        self.assertNotIn("jwt-token", raw_cookie)
        self.assertNotIn("admin", raw_cookie)
        cookie = SimpleCookie(raw_cookie)
        session_value = cookie[main_module.SESSION_COOKIE].value
        self.assertEqual(main_module.verify_session(session_value), "sub2api:7:admin")

    def test_disabled_sso_rejects_without_login_redirect_or_cookie(self) -> None:
        main_module.settings.sub2api_sso_enabled = False

        response = main_module.sub2api_sso_start(fake_request(), token="jwt-token", user_id="7")

        self.assertEqual(response.status_code, 403)
        self.assertEqual(response.body.decode("utf-8"), "Sub2API SSO required")
        self.assertEqual(response.headers["Cache-Control"], "no-store")
        self.assertEqual(response.headers["Referrer-Policy"], "no-referrer")
        self.assertNotIn("location", response.headers)
        self.assertNotIn("set-cookie", response.headers)

    def test_failed_sso_rejects_without_login_redirect_or_cookie(self) -> None:
        def fake_validator(*_args: object, **_kwargs: object) -> Sub2APISSOUser:
            raise Sub2APISSOError("upstream_unauthorized", "bad token")

        main_module.validate_sub2api_token = fake_validator

        response = main_module.sub2api_sso_start(fake_request(), token="jwt-token", user_id="7")

        self.assertEqual(response.status_code, 403)
        self.assertEqual(response.body.decode("utf-8"), "Sub2API SSO required")
        self.assertEqual(response.headers["Cache-Control"], "no-store")
        self.assertEqual(response.headers["Referrer-Policy"], "no-referrer")
        self.assertIn("frame-ancestors", response.headers["Content-Security-Policy"])
        self.assertNotIn("location", response.headers)
        self.assertNotIn("set-cookie", response.headers)


if __name__ == "__main__":
    unittest.main()
