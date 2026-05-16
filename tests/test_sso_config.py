from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from app.sso_config import build_sso_panel_config, load_sso_runtime_config, save_sso_config


class SSOConfigTests(unittest.TestCase):
    def test_missing_file_uses_env_defaults(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            config = load_sso_runtime_config(
                str(Path(tmpdir) / "missing.json"),
                env_enabled=True,
                env_base_url="https://661313.xyz/",
                env_verify_base_url="http://sub2api:8080/",
                env_required_role="Admin",
                env_session_ttl_seconds=900,
                env_verify_timeout_seconds=4,
            )

            self.assertTrue(config.enabled)
            self.assertEqual(config.base_url, "https://661313.xyz")
            self.assertEqual(config.verify_base_url, "http://sub2api:8080")
            self.assertEqual(config.required_role, "Admin")
            self.assertEqual(config.session_ttl_seconds, 900)
            self.assertEqual(config.verify_timeout_seconds, 4)

    def test_save_and_load_panel_config(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            path = str(Path(tmpdir) / "sso.json")
            save_sso_config(
                path,
                enabled=True,
                base_url="https://661313.xyz/",
                verify_base_url="http://sub2api:8080/",
                required_role="admin",
                session_ttl_seconds=86400,
                verify_timeout_seconds=5,
                updated_by="sub2api:7:admin",
            )

            config = load_sso_runtime_config(path)
            panel = build_sso_panel_config(config, base_path="/sub2ops")

            self.assertTrue(config.enabled)
            self.assertEqual(config.base_url, "https://661313.xyz")
            self.assertEqual(config.verify_base_url, "http://sub2api:8080")
            self.assertEqual(panel["menu_url"], "https://661313.xyz/sub2ops/sso/start")
            self.assertEqual(panel["verify_url"], "http://sub2api:8080/api/v1/auth/me")
            self.assertEqual(panel["updated_by"], "sub2api:7:admin")

    def test_ttl_and_timeout_are_clamped(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            path = str(Path(tmpdir) / "sso.json")
            save_sso_config(
                path,
                enabled=True,
                base_url="https://661313.xyz",
                verify_base_url="",
                required_role="admin",
                session_ttl_seconds=10,
                verify_timeout_seconds=100,
                updated_by="admin",
            )

            config = load_sso_runtime_config(path)

            self.assertEqual(config.session_ttl_seconds, 300)
            self.assertEqual(config.verify_timeout_seconds, 20)


if __name__ == "__main__":
    unittest.main()
