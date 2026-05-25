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
from app.telegram_bot import (
    TelegramOpsBot,
    account_actions_keyboard,
    error_chain_alert,
    format_guard_actions,
    normalize_pairing_code,
    recovery_alert,
)
from app.usage_query import UsageQueryConfig, UsageQueryStore


async def guard_runner(_: str) -> list[dict[str, Any]]:
    return []


def guard_config() -> dict[str, Any]:
    return {}


def make_settings(state_path: str, pairing_code: str = "ABCD-EFGH") -> Settings:
    return Settings(
        database_url="postgresql://unused",
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

    async def test_quota_command_queries_enabled_configs_and_shows_available_and_total(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            usage_path = Path(tmpdir) / "usage-query-state.json"
            settings = make_settings(str(state_path))
            settings.usage_query_state_path = str(usage_path)
            store = UsageQueryStore(str(usage_path))
            store.save_config(
                UsageQueryConfig(
                    account_id=7,
                    enabled=False,
                    template_type="custom",
                    code="""({
  request: {url: "https://quota.example.com/7", method: "GET", headers: {}},
  extractor: function(response) {
    return {planName: "wallet", remaining: response.remaining, total: response.total, unit: "USD"};
  }
})""",
                    upstream_multiplier=0.5,
                )
            )

            class FakeDB:
                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return {
                        "id": 7,
                        "name": "quota-account",
                        "platform": "openai",
                        "type": "apikey",
                        "schedulable": True,
                        "credentials": {},
                    }

            async def quota_opener(_request: dict[str, Any], _timeout: int) -> dict[str, Any]:
                return {"remaining": 12.5, "total": 20.0}

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config, usage_query_opener=quota_opener)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertIn("额度查询", reply)
            self.assertIn("总可用：25 USD", reply)
            self.assertIn("#7 quota-account", reply)
            self.assertIn("可用 25 USD", reply)
            self.assertNotIn("总额", reply)
            self.assertNotIn("wallet", reply)
            self.assertEqual(UsageQueryStore(str(usage_path)).result(7)["actual_available"], 25.0)

    async def test_quota_command_excludes_non_positive_available_from_total(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            usage_path = Path(tmpdir) / "usage-query-state.json"
            settings = make_settings(str(state_path))
            settings.usage_query_state_path = str(usage_path)
            store = UsageQueryStore(str(usage_path))
            store.save_config(
                UsageQueryConfig(
                    account_id=7,
                    enabled=True,
                    template_type="custom",
                    code="""({
  request: {url: "https://quota.example.com/7", method: "GET", headers: {}},
  extractor: function(response) {
    return {remaining: response.remaining, unit: "USD"};
  }
})""",
                    upstream_multiplier=1,
                )
            )

            class FakeDB:
                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return {
                        "id": 7,
                        "name": "depleted-account",
                        "platform": "openai",
                        "type": "apikey",
                        "schedulable": True,
                        "credentials": {},
                    }

            async def quota_opener(_request: dict[str, Any], _timeout: int) -> dict[str, Any]:
                return {"remaining": -0.0586}

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config, usage_query_opener=quota_opener)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertIn("总可用：-", reply)
            self.assertIn("#7 depleted-account：可用 -0.0586 USD", reply)
            self.assertNotIn("总可用：-0.0586 USD", reply)

    async def test_quota_command_excludes_non_positive_snapshot_from_total(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            usage_path = Path(tmpdir) / "usage-query-state.json"
            settings = make_settings(str(state_path))
            settings.usage_query_state_path = str(usage_path)
            store = UsageQueryStore(str(usage_path))
            store.save_config(
                UsageQueryConfig(
                    account_id=7,
                    enabled=True,
                    template_type="sub2api",
                    use_account_credentials=True,
                    code="""({
  request: {url: "{{baseUrl}}/v1/usage", method: "GET", headers: {}},
  extractor: function(response) {
    return {remaining: response.remaining, unit: "USD"};
  }
})""",
                    upstream_multiplier=1,
                )
            )
            store.save_result(
                7,
                {
                    "account_id": 7,
                    "template_type": "sub2api",
                    "success": True,
                    "remaining": -0.0586,
                    "unit": "USD",
                    "upstream_multiplier": 1.0,
                    "actual_available": -0.0586,
                    "queried_at": "2026-05-22T00:00:00+00:00",
                },
            )

            class FakeDB:
                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return {
                        "id": 7,
                        "name": "snapshot-depleted",
                        "platform": "openai",
                        "type": "apikey",
                        "schedulable": True,
                        "credentials": {"base_url": "https://quota.example.com", "api_key": "sk-test"},
                    }

            async def failing_opener(_request: dict[str, Any], _timeout: int) -> dict[str, Any]:
                raise TimeoutError("boom")

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config, usage_query_opener=failing_opener)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertIn("总可用：-", reply)
            self.assertIn("#7 snapshot-depleted：可用 -0.0586 USD", reply)
            self.assertNotIn("总可用：-0.0586 USD", reply)

    async def test_quota_command_reports_no_configured_accounts(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            settings = make_settings(str(Path(tmpdir) / "state.json"))
            settings.usage_query_state_path = str(Path(tmpdir) / "usage-query-state.json")
            bot = TelegramOpsBot(settings, object(), guard_runner, guard_config)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "额度")

            self.assertIsNone(keyboard)
            self.assertIn("没有配置额度查询", reply)

    async def test_quota_command_includes_oauth_account_without_usage_query_config(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            usage_path = Path(tmpdir) / "usage-query-state.json"
            settings = make_settings(str(state_path))
            settings.usage_query_state_path = str(usage_path)

            class FakeDB:
                def fetch_all(self, _sql: str, _params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
                    return [
                        {
                            "id": 8,
                            "name": "oauth-account",
                            "platform": "openai",
                            "type": "oauth",
                            "schedulable": True,
                        }
                    ]

                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return {
                        "id": 8,
                        "name": "oauth-account",
                        "platform": "openai",
                        "type": "oauth",
                        "schedulable": True,
                        "credentials": {"plan_type": "free"},
                        "extra": {
                            "codex_5h_used_percent": 25,
                            "codex_5h_reset_at": "2026-05-25T10:30:00Z",
                            "codex_7d_used_percent": 25,
                            "codex_7d_reset_at": "2026-05-28T22:30:00Z",
                        },
                    }

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertIn("#8 oauth-account", reply)
            self.assertIn("free", reply)
            self.assertNotIn("5h", reply)
            self.assertIn("7d 剩余 75%（恢复 05-29 06:30）", reply)

    async def test_quota_command_skips_deleted_accounts_without_query_or_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            usage_path = Path(tmpdir) / "usage-query-state.json"
            settings = make_settings(str(state_path))
            settings.usage_query_state_path = str(usage_path)
            store = UsageQueryStore(str(usage_path))
            store.save_config(
                UsageQueryConfig(
                    account_id=7,
                    enabled=True,
                    template_type="custom",
                    code="""({
  request: {url: "https://quota.example.com/deleted", method: "GET", headers: {}},
  extractor: function(response) {
    return {remaining: response.remaining, unit: "USD"};
  }
})""",
                    upstream_multiplier=0.5,
                )
            )
            store.save_result(
                7,
                {
                    "account_id": 7,
                    "template_type": "custom",
                    "success": True,
                    "data": [],
                    "error": "",
                    "queried_at": "2026-05-22T00:00:00+00:00",
                    "remaining": 12.5,
                    "unit": "USD",
                    "upstream_multiplier": 0.5,
                    "actual_available": 25.0,
                },
            )

            class FakeDB:
                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return None

            query_calls = 0

            async def quota_opener(_request: dict[str, Any], _timeout: int) -> dict[str, Any]:
                nonlocal query_calls
                query_calls += 1
                return {"remaining": 99.0}

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config, usage_query_opener=quota_opener)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertEqual(query_calls, 0)
            self.assertNotIn("#7", reply)
            self.assertNotIn("25 USD", reply)
            self.assertNotIn("跳过已删除账号", reply)
            self.assertEqual(UsageQueryStore(str(usage_path)).result(7)["actual_available"], 25.0)

    async def test_quota_command_includes_oauth_codex_windows_without_http_query_or_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            usage_path = Path(tmpdir) / "usage-query-state.json"
            settings = make_settings(str(state_path))
            settings.usage_query_state_path = str(usage_path)
            store = UsageQueryStore(str(usage_path))
            store.save_config(
                UsageQueryConfig(
                    account_id=8,
                    enabled=True,
                    template_type="custom",
                    code="""({
  request: {url: "https://quota.example.com/oauth", method: "GET", headers: {}},
  extractor: function(response) {
    return {remaining: response.remaining, unit: "USD"};
  }
})""",
                )
            )

            class FakeDB:
                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return {
                        "id": 8,
                        "name": "oauth-account",
                        "platform": "openai",
                        "type": "oauth",
                        "schedulable": True,
                        "credentials": {"plan_type": "plus"},
                        "extra": {
                            "codex_5h_used_percent": 80,
                            "codex_5h_reset_at": "2026-05-25T10:30:00Z",
                            "codex_7d_used_percent": 100,
                            "codex_7d_reset_at": "2026-05-28T22:30:00Z",
                        },
                    }

            query_calls = 0

            async def quota_opener(_request: dict[str, Any], _timeout: int) -> dict[str, Any]:
                nonlocal query_calls
                query_calls += 1
                return {"remaining": 99.0}

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config, usage_query_opener=quota_opener)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertEqual(query_calls, 0)
            self.assertIn("#8 oauth-account", reply)
            self.assertIn("· plus：", reply)
            self.assertNotIn("Codex plus", reply)
            self.assertIn("5h 剩余 20%（恢复 05-25 18:30）", reply)
            self.assertNotIn("7d", reply)
            self.assertNotIn("Codex oauth", reply)
            self.assertNotIn("跳过 OAuth", reply)
            self.assertEqual(UsageQueryStore(str(usage_path)).result(8), {})

    async def test_quota_command_prefers_saved_oauth_active_usage_result(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            usage_path = Path(tmpdir) / "usage-query-state.json"
            settings = make_settings(str(state_path))
            settings.usage_query_state_path = str(usage_path)
            store = UsageQueryStore(str(usage_path))
            store.save_config(UsageQueryConfig(account_id=8, enabled=True, template_type="sub2api"))
            store.save_result(
                8,
                {
                    "account_id": 8,
                    "template_type": "oauth",
                    "success": True,
                    "oauth_quota": {
                        "plan_type": "pro",
                        "telegram_windows": [
                            {
                                "key": "codex_7d",
                                "label": "7d",
                                "used_percent": 30,
                                "remaining_percent": 70,
                                "reset_at": "2026-05-30T00:30:00Z",
                            }
                        ],
                        "ui_windows": [],
                    },
                },
            )

            class FakeDB:
                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return {
                        "id": 8,
                        "name": "oauth-account",
                        "platform": "openai",
                        "type": "oauth",
                        "schedulable": True,
                        "credentials": {"plan_type": "free"},
                        "extra": {
                            "codex_7d_used_percent": 90,
                            "codex_7d_reset_at": "2026-05-28T22:30:00Z",
                        },
                    }

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertIn("#8 oauth-account · pro：7d 剩余 70%（恢复 05-30 08:30）", reply)
            self.assertNotIn("free：7d 剩余 10%", reply)

    async def test_quota_command_omits_oauth_account_when_no_remaining_codex_windows(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            usage_path = Path(tmpdir) / "usage-query-state.json"
            settings = make_settings(str(state_path))
            settings.usage_query_state_path = str(usage_path)
            store = UsageQueryStore(str(usage_path))
            store.save_config(
                UsageQueryConfig(
                    account_id=8,
                    enabled=True,
                    template_type="custom",
                    code="""({
  request: {url: "https://quota.example.com/oauth", method: "GET", headers: {}},
  extractor: function(response) {
    return {remaining: response.remaining, unit: "USD"};
  }
})""",
                )
            )

            class FakeDB:
                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return {
                        "id": 8,
                        "name": "oauth-account",
                        "platform": "openai",
                        "type": "oauth",
                        "schedulable": True,
                        "credentials": {"plan_type": "plus"},
                        "extra": {"codex_5h_used_percent": 100},
                    }

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertNotIn("#8 oauth-account", reply)
            self.assertNotIn("plus", reply)
            self.assertNotIn("跳过 OAuth", reply)
            self.assertEqual(UsageQueryStore(str(usage_path)).result(8), {})

    async def test_quota_command_uses_last_success_snapshot_when_live_query_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            usage_path = Path(tmpdir) / "usage-query-state.json"
            settings = make_settings(str(state_path))
            settings.usage_query_state_path = str(usage_path)
            store = UsageQueryStore(str(usage_path))
            store.save_config(
                UsageQueryConfig(
                    account_id=7,
                    enabled=True,
                    template_type="sub2api",
                    use_account_credentials=True,
                    code="""({
  request: {
    url: "{{baseUrl}}/v1/usage",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}"
    }
  },
  extractor: function(response) {
    const asNumber = function(value) {
      const parsed = typeof value === "number" ? value : Number(value);
      return Number.isFinite(parsed) ? parsed : undefined;
    };
    const remaining = asNumber(response?.remaining ?? response?.quota?.remaining ?? response?.balance);
    const used = asNumber(response?.used ?? response?.quota?.used ?? response?.usage?.total?.actual_cost ?? response?.usage?.total?.cost);
    const explicitTotal = asNumber(response?.total ?? response?.quota?.total);
    const total = explicitTotal ?? (remaining !== undefined && used !== undefined ? remaining + used : undefined);
    const unit = response?.unit ?? response?.quota?.unit ?? "USD";
    return {
      isValid: response?.is_active ?? response?.isValid ?? true,
      planName: response?.planName ?? response?.plan_name ?? response?.quota?.planName ?? "",
      remaining,
      total,
      used,
      unit
    };
  }
})""",
                    upstream_multiplier=0.5,
                )
            )
            store.save_result(
                7,
                {
                    "account_id": 7,
                    "template_type": "sub2api",
                    "success": True,
                    "data": [],
                    "error": "",
                    "queried_at": "2026-05-22T00:00:00+00:00",
                    "plan_name": "wallet",
                    "extra": "",
                    "remaining": 12.5,
                    "used": 27.5,
                    "total": 20.0,
                    "unit": "USD",
                    "invalid_message": "",
                    "upstream_multiplier": 0.5,
                    "actual_available": 25.0,
                },
            )

            class FakeDB:
                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return {
                        "id": 7,
                        "name": "snapshot-account",
                        "platform": "openai",
                        "type": "apikey",
                        "schedulable": True,
                        "credentials": {
                            "base_url": "https://quota.example.com",
                            "api_key": "sk-test",
                        },
                    }

            async def failing_opener(_request: dict[str, Any], _timeout: int) -> dict[str, Any]:
                raise TimeoutError("boom")

            bot = TelegramOpsBot(
                settings,
                FakeDB(),  # type: ignore[arg-type]
                guard_runner,
                guard_config,
                usage_query_opener=failing_opener,
            )

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertIn("总可用：25 USD", reply)
            self.assertIn("#7 snapshot-account：可用 25 USD", reply)
            self.assertNotIn("总额", reply)
            self.assertNotIn("wallet", reply)
            self.assertNotIn("快照", reply)
            self.assertEqual(UsageQueryStore(str(usage_path)).result(7)["success"], True)

    async def test_sync_commands_registers_quota_menu_command(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            calls: list[tuple[str, dict[str, Any]]] = []

            class FakeBot(TelegramOpsBot):
                async def _api(
                    self,
                    method: str,
                    payload: dict[str, Any],
                    timeout: int = 15,
                ) -> dict[str, Any]:
                    calls.append((method, payload))
                    return {"ok": True, "result": True}

            bot = FakeBot(make_settings(str(Path(tmpdir) / "state.json")), object(), guard_runner, guard_config)  # type: ignore[arg-type]

            await bot.sync_commands()

            self.assertEqual(calls[0][0], "setMyCommands")
            self.assertIn({"command": "quota", "description": "查询账号额度"}, calls[0][1]["commands"])

    def test_account_action_keyboard_has_only_direct_account_actions(self) -> None:
        keyboard = account_actions_keyboard({"id": 7, "schedulable": True})
        callback_data = [button["callback_data"] for row in keyboard["inline_keyboard"] for button in row]

        self.assertEqual(callback_data, ["pause:7", "cd:7:5", "cd:7:15", "cd:7:30", "acct:7"])
        self.assertNotIn("menu", callback_data)
        self.assertNotIn("acctlist:all:0", callback_data)

    def test_cooldown_keyboard_uses_short_fixed_presets(self) -> None:
        from app.telegram_bot import cooldown_keyboard

        keyboard = cooldown_keyboard(7)
        callback_data = [button["callback_data"] for row in keyboard["inline_keyboard"] for button in row]

        self.assertEqual(callback_data, ["cd:7:5", "cd:7:15", "cd:7:30", "acct:7"])
        self.assertNotIn("cd:7:120", callback_data)
        self.assertNotIn("cd:7:1440", callback_data)

    def test_pairing_code_normalization_ignores_case_spaces_and_hyphen(self) -> None:
        self.assertEqual(normalize_pairing_code("ab cd-ef gh"), "ABCDEFGH")

    async def test_error_alert_cursor_persists_in_state(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            bot = TelegramOpsBot(make_settings(str(Path(tmpdir) / "state.json")), object(), guard_runner, guard_config)  # type: ignore[arg-type]

            self.assertEqual(await bot.error_alert_cursor_id(), 0)
            await bot.set_error_alert_cursor_id(123)

            self.assertEqual(await bot.error_alert_cursor_id(), 123)

    async def test_recovery_alert_cursor_persists_separately(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            bot = TelegramOpsBot(make_settings(str(Path(tmpdir) / "state.json")), object(), guard_runner, guard_config)  # type: ignore[arg-type]

            self.assertEqual(await bot.recovery_alert_cursor_id(), 0)
            await bot.set_error_alert_cursor_id(123)
            await bot.set_recovery_alert_cursor_id(456)

            self.assertEqual(await bot.error_alert_cursor_id(), 123)
            self.assertEqual(await bot.recovery_alert_cursor_id(), 456)

    def test_error_chain_alert_includes_request_account_and_message(self) -> None:
        text = error_chain_alert(
            {
                "request_id": "req-1",
                "client_request_id": "client-1",
                "platform": "openai",
                "model": "gpt-test",
                "attempt_no": 2,
                "account_id": 7,
                "account_name": "acct",
                "status_code": 429,
                "category": "provider_rate_limit",
                "message": "rate limit exceeded",
            },
            None,
        )

        self.assertIn("错误链路异常", text)
        self.assertIn("req-1", text)
        self.assertIn("#7 acct", text)
        self.assertIn("provider_rate_limit", text)
        self.assertIn("rate limit exceeded", text)

    def test_recovery_alert_includes_account_plan_and_latency(self) -> None:
        text = recovery_alert(
            {
                "account_id": 7,
                "account_name": "acct",
                "platform": "openai",
                "type": "oauth",
                "model_id": "gpt-test",
                "cron_expression": "*/5 * * * *",
                "latency_ms": 2345,
            }
        )

        self.assertIn("账号已自动恢复", text)
        self.assertIn("#7 acct", text)
        self.assertIn("openai / oauth", text)
        self.assertIn("gpt-test", text)
        self.assertIn("2.35s", text)

    def test_guard_action_message_includes_action_reason_and_load_factor(self) -> None:
        text = format_guard_actions(
            "自动 Guard 已处理",
            [
                {
                    "account_id": 9,
                    "name": "wong",
                    "action": "cooldown",
                    "minutes": 15,
                    "load_factor": 1,
                    "reason": "auto guard: provider rate limit",
                }
            ],
        )

        self.assertIn("自动 Guard 已处理", text)
        self.assertIn("#9 wong", text)
        self.assertIn("cooldown 15m", text)
        self.assertIn("load_factor=1", text)
        self.assertIn("provider rate limit", text)


if __name__ == "__main__":
    unittest.main()
