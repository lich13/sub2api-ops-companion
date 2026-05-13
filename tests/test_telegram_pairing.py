from __future__ import annotations

import tempfile
import sys
import types
import unittest
from pathlib import Path
from typing import Any

psycopg_rows = types.ModuleType("psycopg.rows")
psycopg_rows.dict_row = object()
psycopg_pool = types.ModuleType("psycopg_pool")


class ConnectionPool:  # pragma: no cover - only satisfies imports in this unit test.
    def __init__(self, *_: Any, **__: Any) -> None:
        pass


psycopg_pool.ConnectionPool = ConnectionPool
sys.modules.setdefault("psycopg", types.ModuleType("psycopg"))
sys.modules.setdefault("psycopg.rows", psycopg_rows)
sys.modules.setdefault("psycopg_pool", psycopg_pool)

from app.settings import Settings
from app.telegram_bot import TelegramOpsBot, account_actions_keyboard, normalize_pairing_code


async def guard_runner(_: str) -> list[dict[str, Any]]:
    return []


def guard_config() -> dict[str, Any]:
    return {}


def make_settings(state_path: str, pairing_code: str = "ABCD-EFGH") -> Settings:
    return Settings(
        database_url="postgresql://unused",
        basic_user="admin",
        basic_password="password",
        session_secret="secret",
        session_ttl_seconds=3600,
        base_path="/sub2ops",
        audit_path="/tmp/sub2ops-audit.jsonl",
        telegram_enabled=True,
        telegram_bot_token="123:test",
        telegram_pairing_enabled=True,
        telegram_pairing_code=pairing_code,
        telegram_state_path=state_path,
    )


class TelegramPairingTests(unittest.IsolatedAsyncioTestCase):
    async def test_pairing_requires_current_code(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            bot = TelegramOpsBot(make_settings(str(Path(tmpdir) / "state.json")), object(), guard_runner, guard_config)  # type: ignore[arg-type]

            self.assertFalse(await bot._allowed(100, 200))

            reply, keyboard = await bot._pair(100, 200, "private", "/pair WRONG")
            self.assertIn("配对码不正确", reply)
            self.assertIsNone(keyboard)
            self.assertFalse(await bot._allowed(100, 200))

            reply, keyboard = await bot._pair(100, 200, "private", "/pair abcd efgh")
            self.assertIn("配对成功", reply)
            self.assertIsNone(keyboard)
            self.assertTrue(await bot._allowed(100, 200))

    async def test_text_commands_are_disabled_after_pairing(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            bot = TelegramOpsBot(make_settings(str(Path(tmpdir) / "state.json")), object(), guard_runner, guard_config)  # type: ignore[arg-type]
            await bot._pair(100, 200, "private", "/pair ABCD-EFGH")

            reply, keyboard = await bot._text_reply(100, 200, "/accounts")

            self.assertIn("不再通过文本命令做账号运维", reply)
            self.assertIsNone(keyboard)

    def test_account_action_keyboard_has_only_direct_account_actions(self) -> None:
        keyboard = account_actions_keyboard({"id": 7, "schedulable": True})
        callback_data = [button["callback_data"] for row in keyboard["inline_keyboard"] for button in row]

        self.assertEqual(callback_data, ["pause:7", "cd:7:30", "cd:7:120", "acct:7"])
        self.assertNotIn("menu", callback_data)
        self.assertNotIn("acctlist:all:0", callback_data)

    def test_pairing_code_normalization_ignores_case_spaces_and_hyphen(self) -> None:
        self.assertEqual(normalize_pairing_code("ab cd-ef gh"), "ABCDEFGH")


if __name__ == "__main__":
    unittest.main()
