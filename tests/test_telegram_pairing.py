from __future__ import annotations

import asyncio
import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace

from app.telegram_bot import TelegramOpsBot, account_actions_keyboard, oauth_monitor_alert


NOW = datetime(2026, 3, 1, 8, 0, tzinfo=timezone.utc)


def oauth_account(account_id: int, name: str, plan: str) -> dict[str, object]:
    return {
        "id": account_id,
        "name": name,
        "platform": "openai",
        "type": "oauth",
        "credentials": {"plan_type": plan},
        "extra": {},
        "status": "active",
        "schedulable": True,
        "priority": 50,
        "concurrency": 1,
    }


def quota_result(account_id: int, plan: str, five: float | None, seven: float) -> dict[str, object]:
    windows: list[dict[str, object]] = []
    if five is not None:
        windows.append(
            {
                "key": "codex_5h",
                "label": "5h",
                "used_percent": 100 - five,
                "remaining_percent": five,
                "reset_at": (NOW + timedelta(hours=5)).isoformat(),
            }
        )
    windows.append(
        {
            "key": "codex_7d",
            "label": "7d",
            "used_percent": 100 - seven,
            "remaining_percent": seven,
            "reset_at": (NOW + timedelta(days=7)).isoformat(),
        }
    )
    return {
        "account_id": account_id,
        "template_type": "oauth",
        "success": True,
        "queried_at": NOW.isoformat(),
        "oauth_quota": {
            "plan_type": plan,
            "ui_windows": windows,
            "telegram_windows": [item for item in windows if float(item["remaining_percent"]) > 0],
        },
    }


class FakeDb:
    def __init__(self, rows: list[dict[str, object]]) -> None:
        self.rows = rows

    def fetch_all(self, _sql: str, _params: dict[str, object] | None = None) -> list[dict[str, object]]:
        return list(self.rows)

    def fetch_one(self, _sql: str, params: dict[str, object] | None = None) -> dict[str, object] | None:
        account_id = int((params or {}).get("account_id") or 0)
        return next((dict(row) for row in self.rows if int(row["id"]) == account_id), None)


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
        telegram_oauth_recovery_push_enabled=True,
    )


class TelegramPairingTests(unittest.IsolatedAsyncioTestCase):
    async def test_sync_commands_registers_quota_only(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            bot = TelegramOpsBot(bot_settings(Path(directory)), FakeDb([]))
            calls: list[tuple[str, dict[str, object]]] = []

            async def api(method: str, payload: dict[str, object], timeout: int = 15) -> dict[str, object]:
                calls.append((method, payload))
                return {"ok": True, "result": []}

            bot._api = api  # type: ignore[method-assign]
            await bot.sync_commands()

            commands = calls[0][1]["commands"]
            self.assertEqual([item["command"] for item in commands], ["quota"])

    async def test_pairing_still_binds_private_chat(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            bot = TelegramOpsBot(bot_settings(Path(directory)), FakeDb([]))

            text, _keyboard = await bot._pair(100, 200, "private", "/pair ABCD-EFGH")

            self.assertIn("配对成功", text)
            self.assertEqual(await bot.allowed_chat_ids(), [100])

    async def test_quota_reads_cached_oauth_only_and_sums_by_plan(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rows = [
                oauth_account(1, "plus-a", "plus"),
                oauth_account(2, "plus-b", "plus"),
                oauth_account(3, "free-a", "free"),
            ]
            (root / "usage-query-state.json").write_text(
                json.dumps(
                    {
                        "version": 2,
                        "settings": {},
                        "oauth_results": {
                            "1": quota_result(1, "plus", 30, 40),
                            "2": quota_result(2, "plus", 20, 10),
                            "3": quota_result(3, "free", None, 60),
                        },
                        "scheduler": {},
                        "pending_events": {},
                    }
                ),
                encoding="utf-8",
            )
            bot = TelegramOpsBot(bot_settings(root), FakeDb(rows))

            text, keyboard = await bot._quota_reply()

            self.assertIsNone(keyboard)
            self.assertIn("plus：5h 50% · 7d 50%", text)
            self.assertIn("free：7d 60%", text)
            self.assertIn("#1 plus-a · plus", text)
            self.assertIn("5h 剩余 30%", text)
            self.assertIn("7d 剩余 40%", text)

    async def test_free_hides_five_hour_and_depleted_seven_day_account_is_absent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rows = [
                oauth_account(1, "stale-free", "free"),
                oauth_account(2, "seven-depleted", "plus"),
            ]
            stale_free = quota_result(1, "plus", 80, 40)
            depleted = quota_result(2, "plus", 90, 0)
            (root / "usage-query-state.json").write_text(
                json.dumps(
                    {
                        "version": 2,
                        "settings": {},
                        "oauth_results": {"1": stale_free, "2": depleted},
                        "scheduler": {},
                        "pending_events": {},
                    }
                ),
                encoding="utf-8",
            )
            bot = TelegramOpsBot(bot_settings(root), FakeDb(rows))

            text, _keyboard = await bot._quota_reply()

            free_line = next(line for line in text.splitlines() if "stale-free" in line)
            self.assertNotIn("5h", free_line)
            self.assertNotIn("seven-depleted", text)

    async def test_quota_without_cache_does_not_make_network_query(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bot = TelegramOpsBot(bot_settings(root), FakeDb([oauth_account(1, "empty", "plus")]))

            text, _keyboard = await bot._quota_reply()

            self.assertIn("暂无可用的 OAuth 额度快照", text)

    async def test_monitor_event_is_only_acknowledged_after_all_chats_receive_it(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state = {"paired_chat_ids": [10, 20], "paired_user_ids": [], "updated_at": NOW.isoformat()}
            (root / "telegram-state.json").write_text(json.dumps(state), encoding="utf-8")
            bot = TelegramOpsBot(bot_settings(root), FakeDb([]))
            sent: list[int] = []

            async def send(chat_id: int, _text: str, _keyboard: object = None) -> bool:
                sent.append(chat_id)
                return chat_id == 10

            bot._send_message = send  # type: ignore[method-assign]
            event = {
                "account_id": 9,
                "account_name": "name",
                "plan_type": "plus",
                "status": "recovered",
                "window_labels": ["5h", "7d"],
                "model_id": "gpt-5.6-luna",
            }

            delivered = await bot.notify_oauth_monitor_events([event])

            self.assertEqual(sent, [10, 20])
            self.assertEqual(delivered, [])


class TelegramFormattingTests(unittest.TestCase):
    def test_account_actions_have_no_guard_whitelist_or_endless_buttons(self) -> None:
        keyboard = account_actions_keyboard(oauth_account(9, "name", "plus"))
        serialized = json.dumps(keyboard, ensure_ascii=False)

        self.assertIn("暂停", serialized)
        self.assertIn("冷却", serialized)
        self.assertIn("恢复", serialized)
        self.assertNotIn("白名单", serialized)
        self.assertNotIn("无尽", serialized)
        self.assertNotIn("wladd:", serialized)
        self.assertNotIn("endadd:", serialized)

    def test_auth_and_test_failure_alerts_include_error_code(self) -> None:
        auth_text = oauth_monitor_alert(
            {
                "account_id": 9,
                "account_name": "name",
                "plan_type": "plus",
                "status": "auth_failed",
                "stage": "active_usage",
                "error_code": "http_401",
                "error": "invalid token",
                "checked_at": NOW.isoformat(),
            }
        )
        failure_text = oauth_monitor_alert(
            {
                "account_id": 9,
                "account_name": "name",
                "plan_type": "plus",
                "status": "test_failed",
                "error_code": "http_502",
                "error": "upstream failed",
                "model_id": "gpt-5.6-luna",
            }
        )

        self.assertIn("http_401", auth_text)
        self.assertIn("active usage", auth_text)
        self.assertIn("http_502", failure_text)
        self.assertIn("gpt-5.6-luna", failure_text)


if __name__ == "__main__":
    unittest.main()
