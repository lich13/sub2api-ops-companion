from __future__ import annotations

import asyncio
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

from app.telegram_bot import TelegramOpsBot


class ReadOnlyDb:
    def __init__(self) -> None:
        self.writes = 0

    def fetch_all(self, *_args, **_kwargs):
        return []

    def fetch_one(self, *_args, **_kwargs):
        self.writes += 1
        raise AssertionError("retired Telegram callbacks must not query or write account state")


def make_bot(root: Path) -> TelegramOpsBot:
    settings = SimpleNamespace(
        telegram_enabled=True, telegram_bot_token="token", telegram_poll_timeout_seconds=5,
        telegram_pairing_enabled=True, telegram_pairing_code="ABCD-EFGH",
        telegram_allowed_chat_ids=(100,), telegram_allowed_user_ids=(200,),
        telegram_state_path=str(root / "telegram-state.json"),
        usage_query_state_path=str(root / "usage-query-state.json"), audit_path=str(root / "audit.jsonl"),
        telegram_oauth_usage_refresh_concurrency=2,
    )
    return TelegramOpsBot(settings, ReadOnlyDb())


class RetiredTelegramCallbackTests(unittest.IsolatedAsyncioTestCase):
    async def test_all_historical_account_callbacks_are_read_only_and_no_keyboard(self):
        with tempfile.TemporaryDirectory() as directory:
            bot = make_bot(Path(directory))
            for callback in ("acctp:1", "acct:390", "pause:390", "pauseask:390",
                             "cdmenu:390", "cd:390:15", "resask:390", "res:390", "unknown"):
                text, keyboard = await bot._callback_reply(100, 200, callback)
                self.assertEqual(text, "账号操作已移除，请使用 /quota")
                self.assertIsNone(keyboard)

    async def test_callback_update_clears_old_message_keyboard(self):
        with tempfile.TemporaryDirectory() as directory:
            bot = make_bot(Path(directory))
            calls = []

            async def api(method, payload, timeout=15):
                calls.append((method, payload))
                return {"ok": True, "result": []}

            bot._api = api
            await bot._handle_update({"callback_query": {
                "id": "callback", "from": {"id": 200},
                "message": {"message_id": 7, "chat": {"id": 100, "type": "private"}},
                "data": "pause:390",
            }})
            self.assertIn(("editMessageReplyMarkup", {
                "chat_id": 100, "message_id": 7, "reply_markup": {"inline_keyboard": []}
            }), calls)
            self.assertNotIn("sendMessage", [method for method, _ in calls[:1]])

    async def test_callback_still_replies_when_old_keyboard_cannot_be_edited(self):
        with tempfile.TemporaryDirectory() as directory:
            bot = make_bot(Path(directory))
            calls = []

            async def api(method, payload, timeout=15):
                calls.append((method, payload))
                if method == "editMessageReplyMarkup":
                    raise ValueError("message can't be edited")
                return {"ok": True, "result": []}

            bot._api = api
            await bot._handle_update({"callback_query": {
                "id": "callback", "from": {"id": 200},
                "message": {"message_id": 7, "chat": {"id": 100, "type": "private"}},
                "data": "res:390",
            }})
            replies = [payload for method, payload in calls if method == "sendMessage"]
            self.assertEqual(len(replies), 1)
            self.assertEqual(replies[0]["text"], "账号操作已移除，请使用 /quota")
