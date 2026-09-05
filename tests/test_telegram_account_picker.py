from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

from app.account_ops import openai_picker_accounts
from app.telegram_bot import TelegramOpsBot, picker_button_label


def bot_settings(root: Path) -> SimpleNamespace:
    return SimpleNamespace(
        telegram_enabled=True,
        telegram_bot_token="token",
        telegram_poll_timeout_seconds=5,
        telegram_pairing_enabled=True,
        telegram_pairing_code="ABCD-EFGH",
        telegram_allowed_chat_ids=(),
        telegram_allowed_user_ids=(),
        telegram_state_path=str(root / "telegram-state.json"),
        usage_query_state_path=str(root / "usage-query-state.json"),
        audit_path=str(root / "audit.jsonl"),
    )


def account(
    account_id: int,
    *,
    name: str = "",
    platform: str = "openai",
    type_name: str = "oauth",
    status: str = "active",
    schedulable: bool = True,
    credentials: dict[str, object] | None = None,
) -> dict[str, object]:
    return {
        "id": account_id,
        "name": name or f"account-{account_id}",
        "platform": platform,
        "type": type_name,
        "status": status,
        "schedulable": schedulable,
        "temp_unschedulable_until": None,
        "credentials": credentials or {"api_key": "secret-should-not-be-selected"},
        "extra": {"token": "also-secret"},
    }


class RecordingDb:
    def __init__(self, rows: list[dict[str, object]]) -> None:
        self.rows = rows
        self.queries: list[tuple[str, dict[str, object] | None]] = []

    def fetch_all(self, sql: str, params: dict[str, object] | None = None) -> list[dict[str, object]]:
        self.queries.append((sql, params))
        return list(self.rows)

    def fetch_one(self, sql: str, params: dict[str, object] | None = None) -> dict[str, object] | None:
        self.queries.append((sql, params))
        account_id = int((params or {}).get("account_id") or 0)
        return next((dict(row) for row in self.rows if int(row["id"]) == account_id), None)


class TelegramAccountPickerTests(unittest.IsolatedAsyncioTestCase):
    async def test_account_without_id_lists_openai_oauth_and_apikey_ascending(self) -> None:
        rows = [
            account(30, name="later-oauth", type_name="oauth"),
            account(4, name="key-four", type_name="apikey"),
            account(9, name="oauth-nine", type_name="oauth"),
            account(12, name="grok-key", platform="grok", type_name="apikey"),
            account(15, name="claude", platform="anthropic", type_name="oauth"),
            account(18, name="other-type", type_name="session"),
        ]
        with tempfile.TemporaryDirectory() as directory:
            db = RecordingDb(rows)
            bot = TelegramOpsBot(bot_settings(Path(directory)), db)

            text, keyboard = await bot._text_reply("/account")
            serialized = json.dumps(keyboard, ensure_ascii=False)

        self.assertIn("第 1/1 页", text)
        self.assertIn("#4 key-four · apikey · 可调度", serialized)
        self.assertIn("#9 oauth-nine · oauth · 可调度", serialized)
        self.assertIn("#30 later-oauth · oauth · 可调度", serialized)
        self.assertLess(serialized.index("acct:4"), serialized.index("acct:9"))
        self.assertLess(serialized.index("acct:9"), serialized.index("acct:30"))
        self.assertNotIn("grok-key", serialized)
        self.assertNotIn("claude", serialized)
        self.assertNotIn("other-type", serialized)
        self.assertNotIn("secret-should-not-be-selected", serialized)
        self.assertIn("IN ('oauth', 'apikey')", " ".join(db.queries[0][0].split()))
        self.assertNotIn("credentials", db.queries[0][0])
        self.assertNotIn("extra", db.queries[0][0])

    async def test_picker_paginates_eight_per_page_and_returns_from_details(self) -> None:
        rows = [account(index, name=f"acc-{index}", type_name="apikey" if index % 2 else "oauth") for index in range(1, 10)]
        with tempfile.TemporaryDirectory() as directory:
            bot = TelegramOpsBot(bot_settings(Path(directory)), RecordingDb(rows))

            page1_text, page1 = await bot._text_reply("/account")
            page2_text, page2 = await bot._callback_reply(1, 2, "acctp:2")
            overflow_text, overflow = await bot._callback_reply(1, 2, "acctp:99")
            malformed_text, malformed = await bot._callback_reply(1, 2, "acctp:nope")
            detail_text, detail = await bot._callback_reply(1, 2, "acct:9")
            back_text, back = await bot._callback_reply(1, 2, "acctp:2")

        page1_data = json.dumps(page1, ensure_ascii=False)
        page2_data = json.dumps(page2, ensure_ascii=False)
        detail_data = json.dumps(detail, ensure_ascii=False)
        self.assertIn("第 1/2 页", page1_text)
        self.assertIn("acct:8", page1_data)
        self.assertNotIn("acct:9", page1_data)
        self.assertIn("下一页", page1_data)
        self.assertIn("acctp:2", page1_data)
        self.assertIn("第 2/2 页", page2_text)
        self.assertIn("acct:9", page2_data)
        self.assertNotIn("acct:8", page2_data)
        self.assertIn("上一页", page2_data)
        self.assertEqual(overflow_text, page2_text)
        self.assertIn("第 1/2 页", malformed_text)
        self.assertEqual(json.dumps(malformed), json.dumps(page1))
        self.assertIn("#9 acc-9", detail_text)
        self.assertIn("pause:9", detail_data)
        self.assertIn("返回列表", detail_data)
        self.assertIn("acctp:2", detail_data)
        self.assertIn("第 2/2 页", back_text)
        self.assertIn("acct:9", json.dumps(back))

    async def test_direct_id_still_opens_non_openai_accounts_and_deleted_is_explicit(self) -> None:
        grok = account(44, name="grok-one", platform="grok", type_name="oauth")
        with tempfile.TemporaryDirectory() as directory:
            bot = TelegramOpsBot(bot_settings(Path(directory)), RecordingDb([grok]))

            picker_text, picker_keyboard = await bot._text_reply("/account")
            detail_text, detail_keyboard = await bot._text_reply("/account 44")
            missing_text, missing_keyboard = await bot._text_reply("/account 99")
            bad_callback, bad_keyboard = await bot._callback_reply(1, 2, "acct:0")
            malformed_callback, malformed_keyboard = await bot._callback_reply(1, 2, "acct:abc")

        self.assertIn("当前没有可选择的 OpenAI 账号", picker_text)
        self.assertIsNone(picker_keyboard)
        self.assertIn("#44 grok-one", detail_text)
        self.assertIn("grok / oauth", detail_text)
        self.assertIsNotNone(detail_keyboard)
        self.assertIn("没有找到账号 #99", missing_text)
        self.assertIsNone(missing_keyboard)
        self.assertIn("账号 ID 无效", bad_callback)
        self.assertIsNone(bad_keyboard)
        self.assertIn("账号 ID 无效", malformed_callback)
        self.assertIsNone(malformed_keyboard)

    async def test_picker_auth_gate_unchanged_for_unpaired_private_chat(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            bot = TelegramOpsBot(
                bot_settings(Path(directory)),
                RecordingDb([account(3, type_name="apikey")]),
            )
            sent: list[str] = []

            async def api(method: str, payload: dict[str, object], timeout: int = 15) -> dict[str, object]:
                if method == "sendMessage":
                    sent.append(str(payload.get("text") or ""))
                return {"ok": True, "result": []}

            bot._api = api  # type: ignore[method-assign]
            await bot._handle_update(
                {
                    "message": {
                        "text": "/account",
                        "chat": {"id": 11, "type": "private"},
                        "from": {"id": 22},
                    }
                }
            )
            await bot._handle_update(
                {
                    "callback_query": {
                        "id": "cb-1",
                        "data": "acctp:1",
                        "from": {"id": 22},
                        "message": {"chat": {"id": 11, "type": "private"}},
                    }
                }
            )

        self.assertTrue(sent)
        self.assertTrue(all("未授权" in text for text in sent))
        self.assertFalse(any("选择 OpenAI 账号" in text for text in sent))

    def test_picker_projection_omits_credentials_and_keeps_id_order(self) -> None:
        db = RecordingDb(
            [
                account(8, type_name="apikey"),
                account(2, type_name="oauth"),
                account(5, platform="grok", type_name="apikey"),
            ]
        )

        rows = openai_picker_accounts(db)

        self.assertEqual([int(row["id"]) for row in rows], [2, 8])
        sql = db.queries[0][0]
        self.assertNotIn("credentials", sql)
        self.assertNotIn("extra", sql)
        self.assertIn("ORDER BY id", sql)

    def test_picker_button_label_is_concise(self) -> None:
        label = picker_button_label(account(7, name="short", type_name="apikey", schedulable=False))
        self.assertEqual(label, "#7 short · apikey · 已停")
        self.assertLessEqual(len(picker_button_label(account(7, name="汉" * 80))), 64)


if __name__ == "__main__":
    unittest.main()
