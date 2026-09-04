from __future__ import annotations

import asyncio
import json
import tempfile
import threading
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from app.oauth_monitor import OAuthStateStore
from app.telegram_bot import (
    TelegramOpsBot,
    account_actions_keyboard,
    format_oauth_window,
)

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


class FakeMonitor:
    def __init__(self, root: Path, *, report: dict[str, object] | None = None) -> None:
        self.store = OAuthStateStore(str(root / "usage-query-state.json"))
        self.calls = 0
        self.report = report or {
            "success": True,
            "refresh_at": NOW.isoformat(),
            "success_count": len(self.store.results()),
            "failure_count": 0,
            "depleted_count": 0,
            "night_deferred_count": 0,
            "recovered_count": 0,
        }

    def force_refresh(self, _timeout_seconds: float = 120) -> dict[str, object]:
        self.calls += 1
        self.store.reload()
        return dict(self.report)


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


class TelegramPairingTests(unittest.IsolatedAsyncioTestCase):
    async def test_sync_commands_registers_quota_and_account(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            bot = TelegramOpsBot(bot_settings(Path(directory)), FakeDb([]))
            calls: list[tuple[str, dict[str, object]]] = []

            async def api(method: str, payload: dict[str, object], timeout: int = 15) -> dict[str, object]:
                calls.append((method, payload))
                return {"ok": True, "result": []}

            bot._api = api  # type: ignore[method-assign]
            await bot.sync_commands()

            commands = calls[0][1]["commands"]
            self.assertEqual([item["command"] for item in commands], ["quota", "account"])

    async def test_account_command_returns_detail_and_action_buttons(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            row = oauth_account(9, "team-account", "team")
            bot = TelegramOpsBot(bot_settings(Path(directory)), FakeDb([row]))

            text, keyboard = await bot._text_reply("/account 9")

            self.assertIn("#9 team-account", text)
            serialized = json.dumps(keyboard, ensure_ascii=False)
            self.assertIn("pause:9", serialized)
            self.assertIn("cdmenu:9", serialized)
            self.assertIn("res:9", serialized)

    async def test_account_command_validates_id_and_missing_account(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            bot = TelegramOpsBot(bot_settings(Path(directory)), FakeDb([]))

            usage, usage_keyboard = await bot._text_reply("/account")
            invalid, invalid_keyboard = await bot._text_reply("/account nope")
            missing, missing_keyboard = await bot._text_reply("/account 99")

            self.assertIn("用法", usage)
            self.assertIsNone(usage_keyboard)
            self.assertIn("账号 ID 无效", invalid)
            self.assertIsNone(invalid_keyboard)
            self.assertIn("没有找到账号 #99", missing)
            self.assertIsNone(missing_keyboard)

    async def test_account_callbacks_run_pause_cooldown_and_resume_actions(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            row = oauth_account(9, "team-account", "team")
            bot = TelegramOpsBot(bot_settings(Path(directory)), FakeDb([row]))
            with (
                patch("app.telegram_bot.account_ops.pause_account", return_value=row) as pause,
                patch("app.telegram_bot.account_ops.cooldown_account", return_value=row) as cooldown,
                patch("app.telegram_bot.account_ops.resume_account", return_value=row) as resume,
            ):
                pause_text, _ = await bot._callback_reply(100, 200, "pause:9")
                cooldown_text, _ = await bot._callback_reply(100, 200, "cd:9:30")
                resume_text, _ = await bot._callback_reply(100, 200, "res:9")

            self.assertIn("已暂停", pause_text)
            self.assertIn("已冷却账号 30 分钟", cooldown_text)
            self.assertIn("已恢复", resume_text)
            pause.assert_called_once_with(
                bot.db,
                bot.settings.audit_path,
                9,
                "telegram:100:200",
                "telegram pause by telegram:100:200",
            )
            cooldown.assert_called_once_with(
                bot.db,
                bot.settings.audit_path,
                9,
                "telegram:100:200",
                30,
                "telegram cooldown 30m by telegram:100:200",
            )
            resume.assert_called_once_with(
                bot.db,
                bot.settings.audit_path,
                9,
                "telegram:100:200",
            )

    async def test_cooldown_menu_keeps_all_account_actions_reachable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            row = oauth_account(9, "team-account", "team")
            bot = TelegramOpsBot(bot_settings(Path(directory)), FakeDb([row]))

            text, keyboard = await bot._callback_reply(100, 200, "cdmenu:9")

            self.assertIn("选择冷却时间", text)
            serialized = json.dumps(keyboard, ensure_ascii=False)
            for value in ("cd:9:5", "cd:9:15", "cd:9:30", "acct:9"):
                self.assertIn(value, serialized)

    async def test_pairing_still_binds_private_chat(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            bot = TelegramOpsBot(bot_settings(Path(directory)), FakeDb([]))

            text, _keyboard = await bot._pair(100, 200, "private", "/pair ABCD-EFGH")

            self.assertIn("配对成功", text)
            self.assertEqual(await bot.allowed_chat_ids(), [100])

    async def test_quota_force_refreshes_and_sums_fresh_results_by_plan(self) -> None:
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
            monitor = FakeMonitor(root)
            bot = TelegramOpsBot(bot_settings(root), FakeDb(rows), oauth_monitor=monitor)

            text, keyboard = await bot._quota_reply()

            self.assertIsNone(keyboard)
            self.assertEqual(monitor.calls, 1)
            self.assertIn("刷新：完成", text)
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
            bot = TelegramOpsBot(
                bot_settings(root), FakeDb(rows), oauth_monitor=FakeMonitor(root)
            )

            text, _keyboard = await bot._quota_reply()

            free_line = next(line for line in text.splitlines() if "stale-free" in line)
            self.assertNotIn("5h", free_line)
            self.assertNotIn("seven-depleted", text)

    async def test_quota_without_fresh_results_reports_empty_current_round(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            monitor = FakeMonitor(root)
            bot = TelegramOpsBot(
                bot_settings(root),
                FakeDb([oauth_account(1, "empty", "plus")]),
                oauth_monitor=monitor,
            )

            text, _keyboard = await bot._quota_reply()

            self.assertEqual(monitor.calls, 1)
            self.assertIn("本轮没有可展示的 OAuth 可用额度", text)

    async def test_quota_never_uses_cache_when_force_refresh_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "usage-query-state.json").write_text(
                json.dumps({"oauth_results": {"1": quota_result(1, "plus", 80, 80)}}),
                encoding="utf-8",
            )
            monitor = FakeMonitor(
                root,
                report={
                    "success": False,
                    "timed_out": True,
                    "error": "OAuth 全量刷新超时",
                },
            )
            bot = TelegramOpsBot(
                bot_settings(root),
                FakeDb([oauth_account(1, "cached", "plus")]),
                oauth_monitor=monitor,
            )

            text, _keyboard = await bot._quota_reply()

            self.assertIn("刷新失败", text)
            self.assertNotIn("cached", text)

    async def test_quota_partial_refresh_shows_stats_and_filters_stale_accounts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stale = quota_result(1, "plus", 80, 80)
            stale["queried_at"] = (NOW - timedelta(minutes=1)).isoformat()
            fresh = quota_result(2, "plus", 20, 30)
            (root / "usage-query-state.json").write_text(
                json.dumps(
                    {
                        "oauth_results": {"1": stale, "2": fresh},
                        "scheduler": {},
                        "pending_events": {},
                    }
                ),
                encoding="utf-8",
            )
            monitor = FakeMonitor(
                root,
                report={
                    "success": False,
                    "refresh_at": NOW.isoformat(),
                    "success_count": 1,
                    "failure_count": 1,
                    "depleted_count": 1,
                    "night_deferred_count": 1,
                    "recovered_count": 0,
                },
            )
            bot = TelegramOpsBot(
                bot_settings(root),
                FakeDb(
                    [oauth_account(1, "stale", "plus"), oauth_account(2, "fresh", "plus")]
                ),
                oauth_monitor=monitor,
            )

            text, _keyboard = await bot._quota_reply()

            self.assertIn("刷新：部分失败", text)
            self.assertIn("成功 1 / 失败 1 / 耗尽 1 / 夜间延后 1 / 已恢复 0", text)
            self.assertIn("fresh", text)
            self.assertNotIn("stale", text)

    async def test_run_coalesces_overlapping_quota_updates_into_one_refresh(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            row = oauth_account(1, "plus-a", "plus")
            (root / "usage-query-state.json").write_text(
                json.dumps({"oauth_results": {"1": quota_result(1, "plus", 30, 40)}}),
                encoding="utf-8",
            )

            class SlowMonitor(FakeMonitor):
                def __init__(self, path: Path) -> None:
                    super().__init__(path)
                    self.release = threading.Event()

                def force_refresh(self, _timeout_seconds: float = 120) -> dict[str, object]:
                    self.calls += 1
                    self.release.wait(2)
                    self.store.reload()
                    return dict(self.report)

            class TrackingBot(TelegramOpsBot):
                def __init__(self, *args: object, **kwargs: object) -> None:
                    super().__init__(*args, **kwargs)
                    self.quota_entries = 0
                    self.both_entered = asyncio.Event()

                async def _quota_reply(self) -> tuple[str, dict[str, object] | None]:
                    self.quota_entries += 1
                    if self.quota_entries == 2:
                        self.both_entered.set()
                    return await super()._quota_reply()

            monitor = SlowMonitor(root)
            bot = TrackingBot(bot_settings(root), FakeDb([row]), oauth_monitor=monitor)
            sent = 0
            both_sent = asyncio.Event()
            poll_count = 0

            async def allowed(_chat_id: int, _user_id: int) -> bool:
                return True

            async def send(_chat_id: int, _text: str, _keyboard: object = None) -> bool:
                nonlocal sent
                sent += 1
                if sent == 2:
                    both_sent.set()
                return True

            async def api(
                method: str, _payload: dict[str, object], timeout: int = 15
            ) -> dict[str, object]:
                nonlocal poll_count
                if method == "setMyCommands":
                    return {"ok": True, "result": True}
                self.assertEqual(method, "getUpdates")
                poll_count += 1
                if poll_count == 1:
                    return {
                        "ok": True,
                        "result": [
                            {
                                "update_id": 1,
                                "message": {
                                    "text": "/quota",
                                    "chat": {"id": 10, "type": "private"},
                                    "from": {"id": 10},
                                },
                            },
                            {
                                "update_id": 2,
                                "message": {
                                    "text": "/quota",
                                    "chat": {"id": 20, "type": "private"},
                                    "from": {"id": 20},
                                },
                            },
                        ],
                    }
                await asyncio.wait_for(bot.both_entered.wait(), timeout=1)
                monitor.release.set()
                await asyncio.wait_for(both_sent.wait(), timeout=1)
                raise asyncio.CancelledError

            bot._allowed = allowed  # type: ignore[method-assign]
            bot._send_message = send  # type: ignore[method-assign]
            bot._api = api  # type: ignore[method-assign]

            with self.assertRaises(asyncio.CancelledError):
                await asyncio.wait_for(bot.run(), timeout=2)

            self.assertEqual(bot.quota_entries, 2)
            self.assertEqual(monitor.calls, 1)
            self.assertEqual(sent, 2)

class TelegramFormattingTests(unittest.TestCase):
    def test_reset_label_distinguishes_exact_from_estimated(self) -> None:
        base = {
            "label": "5h",
            "remaining_percent": 0,
            "reset_at": NOW.isoformat(),
        }
        self.assertIn("恢复时间", format_oauth_window({**base, "reset_source": "server_exact"}))
        self.assertIn(
            "预计恢复时间",
            format_oauth_window({**base, "reset_source": "estimated_from_remaining"}),
        )

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

if __name__ == "__main__":
    unittest.main()
