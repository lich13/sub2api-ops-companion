from __future__ import annotations

import unittest
from pathlib import Path
from typing import Any

from app.turnstile import (
    TURNSTILE_ENABLED_KEY,
    TURNSTILE_SECRET_KEY,
    TURNSTILE_SETTING_KEYS,
    TURNSTILE_SETTING_UPSERT_SQL,
    TURNSTILE_SITE_KEY,
    build_turnstile_config,
    save_turnstile_config,
    turnstile_setting_values,
)

REPO_ROOT = Path(__file__).resolve().parents[1]


class FakeDatabase:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any]]] = []

    def fetch_one(self, sql: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        payload = params or {}
        self.calls.append((sql, payload))
        return {"key": payload["key"], "value": payload["value"], "updated_at": "now"}


class TurnstileConfigTests(unittest.TestCase):
    def test_config_hides_secret_value(self) -> None:
        config = build_turnstile_config(
            [
                {"key": TURNSTILE_ENABLED_KEY, "value": "true", "updated_at": "2026-05-15T00:00:00Z"},
                {"key": TURNSTILE_SITE_KEY, "value": "0x4AAAAAADPfCPB_O-N3j6ON"},
                {"key": TURNSTILE_SECRET_KEY, "value": "secret-do-not-render"},
            ]
        )

        self.assertTrue(config["enabled"])
        self.assertEqual(config["site_key"], "0x4AAAAAADPfCPB_O-N3j6ON")
        self.assertTrue(config["secret_key_set"])
        self.assertNotIn("secret_key", config)
        self.assertNotIn("secret-do-not-render", str(config))
        self.assertFalse(build_turnstile_config([])["secret_key_set"])

    def test_blank_secret_keeps_existing_value(self) -> None:
        self.assertEqual(
            turnstile_setting_values(True, " site-key ", None),
            [
                (TURNSTILE_ENABLED_KEY, "true"),
                (TURNSTILE_SITE_KEY, "site-key"),
            ],
        )

    def test_save_updates_secret_only_when_present(self) -> None:
        db = FakeDatabase()

        rows = save_turnstile_config(db, enabled=True, site_key="site-key", secret_key="secret-key")

        self.assertEqual([params["key"] for _, params in db.calls], list(TURNSTILE_SETTING_KEYS))
        self.assertEqual(rows[-1]["key"], TURNSTILE_SECRET_KEY)
        for sql, _ in db.calls:
            self.assertEqual(sql, TURNSTILE_SETTING_UPSERT_SQL)
            self.assertIn("ON CONFLICT (key)", sql)

    def test_panel_and_nav_exist_without_secret_value_field(self) -> None:
        base = (REPO_ROOT / "app" / "templates" / "base.html").read_text(encoding="utf-8")
        template = (REPO_ROOT / "app" / "templates" / "turnstile.html").read_text(encoding="utf-8")

        self.assertIn('href="{{ base_path }}/turnstile"', base)
        self.assertIn("登录防护", base)
        self.assertIn("Cloudflare Turnstile", template)
        self.assertIn('name="site_key"', template)
        self.assertIn('name="secret_key"', template)
        self.assertIn("留空不变", template)
        self.assertNotIn('value="{{ turnstile.secret_key', template)

        main_py = (REPO_ROOT / "app" / "main.py").read_text(encoding="utf-8")
        self.assertIn("启用 Turnstile 前需要填写 Secret Key", main_py)
        self.assertIn("existing.get(\"secret_key_set\")", main_py)


if __name__ == "__main__":
    unittest.main()
