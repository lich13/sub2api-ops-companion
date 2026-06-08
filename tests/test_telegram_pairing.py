from __future__ import annotations

import json
import tempfile
import sys
import types
import unittest
from pathlib import Path
from typing import Any

import os

os.environ.setdefault("OPS_SESSION_SECRET", "test-session-secret")
os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")

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
    account_alert,
    account_actions_keyboard,
    error_chain_alert,
    format_oauth_quota_line,
    oauth_quota_recovery_alert,
    format_guard_actions,
    normalize_pairing_code,
    recovery_alert,
)
from app.guard_store import GuardStore
from app.usage_query import UsageQueryConfig, UsageQueryStore
from app import main as main_module


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


class WhitelistFakeDB:
    def __init__(self, rows: list[dict[str, Any]]) -> None:
        self.rows = rows

    def fetch_all(self, *_args: Any, **_kwargs: Any) -> list[dict[str, Any]]:
        return self.rows

    def fetch_one(self, _sql: str, params: dict[str, Any] | None = None) -> dict[str, Any] | None:
        account_id = int((params or {}).get("account_id") or 0)
        for row in self.rows:
            if int(row.get("id") or 0) == account_id:
                return row
        return None


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

    async def test_quota_command_excludes_non_positive_available_from_reply(self) -> None:
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
            self.assertIn("没有配置额度查询", reply)
            self.assertNotIn("#7 depleted-account", reply)
            self.assertNotIn("-0.0586 USD", reply)

    async def test_quota_command_uses_latest_account_credentials_not_stale_state_values(self) -> None:
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
                    base_url="https://stale-state.example.com",
                    api_key="stale-state-secret",
                    use_account_credentials=True,
                )
            )

            class FakeDB:
                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return {
                        "id": 7,
                        "name": "synced-account",
                        "platform": "openai",
                        "type": "apikey",
                        "schedulable": True,
                        "credentials": {
                            "base_url": "https://current-account.example.com",
                            "api_key": "current-account-secret",
                        },
                    }

            requests: list[dict[str, Any]] = []

            async def quota_opener(request: dict[str, Any], _timeout: int) -> dict[str, Any]:
                requests.append(request)
                return {"remaining": 4.0, "unit": "USD"}

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config, usage_query_opener=quota_opener)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertIn("#7 synced-account：可用 4 USD", reply)
            self.assertEqual(requests[0]["url"], "https://current-account.example.com/v1/usage")
            self.assertEqual(requests[0]["headers"]["Authorization"], "Bearer current-account-secret")
            self.assertEqual(UsageQueryStore(str(usage_path)).config(7).base_url, "https://stale-state.example.com")

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
            self.assertIn("没有配置额度查询", reply)
            self.assertNotIn("#7 snapshot-depleted", reply)
            self.assertNotIn("-0.0586 USD", reply)

    async def test_quota_command_omits_non_positive_live_result(self) -> None:
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

            class FakeDB:
                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    return {
                        "id": 7,
                        "name": "live-depleted",
                        "platform": "openai",
                        "type": "apikey",
                        "schedulable": True,
                        "credentials": {"base_url": "https://quota.example.com", "api_key": "sk-test"},
                    }

            async def depleted_opener(_request: dict[str, Any], _timeout: int) -> dict[str, Any]:
                return {"remaining": 0, "unit": "USD"}

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config, usage_query_opener=depleted_opener)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertIn("没有配置额度查询", reply)
            self.assertNotIn("#7 live-depleted", reply)
            self.assertEqual(UsageQueryStore(str(usage_path)).result(7)["actual_available"], 0.0)

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

    async def test_quota_command_includes_current_oauth_account_outside_quality_rows(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            usage_path = Path(tmpdir) / "usage-query-state.json"
            settings = make_settings(str(state_path))
            settings.usage_query_state_path = str(usage_path)

            class FakeDB:
                def fetch_all(self, sql: str, _params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
                    lowered = sql.lower()
                    if "lower(coalesce(platform" in lowered and "lower(coalesce(type" in lowered:
                        return [
                            {
                                "id": 9,
                                "name": "current-oauth",
                                "platform": "openai",
                                "type": "oauth",
                                "schedulable": True,
                            },
                            {
                                "id": 10,
                                "name": "deleted-oauth",
                                "platform": "openai",
                                "type": "oauth",
                                "schedulable": True,
                            }
                        ]
                    return []

                def fetch_one(self, _sql: str, _params: dict[str, Any] | None = None) -> dict[str, Any] | None:
                    if (_params or {}).get("account_id") == 10:
                        return None
                    return {
                        "id": 9,
                        "name": "current-oauth",
                        "platform": "openai",
                        "type": "oauth",
                        "schedulable": True,
                        "credentials": {"plan_type": "plus"},
                        "extra": {
                            "codex_5h_used_percent": 100,
                            "codex_5h_reset_at": "2026-05-25T10:30:00Z",
                            "codex_7d_used_percent": 40,
                            "codex_7d_reset_at": "2026-05-28T22:30:00Z",
                        },
                    }

            bot = TelegramOpsBot(settings, FakeDB(), guard_runner, guard_config)  # type: ignore[arg-type]

            reply, keyboard = await bot._text_reply(100, 200, "/quota")

            self.assertIsNone(keyboard)
            self.assertIn("额度查询", reply)
            self.assertIn("总可用：-", reply)
            self.assertIn("#9 current-oauth · plus：7d 剩余 60%（恢复 05-29 06:30）", reply)
            self.assertNotIn("没有配置额度查询", reply)
            self.assertNotIn("deleted-oauth", reply)
            self.assertNotIn("5h", reply)

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

    async def test_quota_command_rebuilds_saved_oauth_result_with_current_account_plan(self) -> None:
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
                    "data": {
                        "five_hour": {
                            "utilization": 0,
                            "resets_at": "2026-05-25T10:30:00Z",
                        },
                        "seven_day": {
                            "utilization": 30,
                            "resets_at": "2026-05-30T00:30:00Z",
                        },
                    },
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
            self.assertIn("#8 oauth-account · free：7d 剩余 70%（恢复 05-30 08:30）", reply)
            self.assertNotIn("pro：", reply)
            self.assertNotIn("5h", reply)

    async def test_oauth_quota_line_sanitizes_cached_plus_snapshot_for_current_free_account(self) -> None:
        line = format_oauth_quota_line(
            {
                "id": 8,
                "name": "oauth-account",
                "type": "oauth",
                "credentials": {"plan_type": "free"},
                "extra": {},
            },
            result={
                "success": True,
                "oauth_quota": {
                    "plan_type": "plus",
                    "ui_windows": [
                        {
                            "key": "codex_5h",
                            "label": "5h",
                            "used_percent": 0,
                            "remaining_percent": 100,
                            "reset_at": "2026-05-25T10:30:00Z",
                        },
                        {
                            "key": "codex_7d",
                            "label": "7d",
                            "used_percent": 30,
                            "remaining_percent": 70,
                            "reset_at": "2026-05-30T00:30:00Z",
                        },
                    ],
                    "telegram_windows": [
                        {
                            "key": "codex_5h",
                            "label": "5h",
                            "used_percent": 0,
                            "remaining_percent": 100,
                            "reset_at": "2026-05-25T10:30:00Z",
                        },
                        {
                            "key": "codex_7d",
                            "label": "7d",
                            "used_percent": 30,
                            "remaining_percent": 70,
                            "reset_at": "2026-05-30T00:30:00Z",
                        },
                    ],
                },
            },
        )

        self.assertEqual(line, "#8 oauth-account · free：7d 剩余 70%（恢复 05-30 08:30）")
        self.assertNotIn("plus", line)
        self.assertNotIn("5h", line)

    async def test_oauth_quota_line_shows_remaining_percent_for_available_windows(self) -> None:
        line = format_oauth_quota_line(
            {
                "id": 8,
                "name": "oauth-account",
                "type": "oauth",
                "credentials": {"plan_type": "plus"},
                "extra": {
                    "codex_5h_used_percent": 100,
                    "codex_5h_reset_at": "2026-05-25T10:30:00Z",
                    "codex_7d_used_percent": 40,
                    "codex_7d_reset_at": "2026-05-28T22:30:00Z",
                },
            }
        )

        self.assertEqual(line, "#8 oauth-account · plus：7d 剩余 60%（恢复 05-29 06:30）")
        self.assertNotIn("5h", line)

    async def test_oauth_quota_line_hides_depleted_seven_day_and_shows_available_five_hour(self) -> None:
        line = format_oauth_quota_line(
            {
                "id": 8,
                "name": "oauth-account",
                "type": "oauth",
                "credentials": {"plan_type": "pro"},
                "extra": {
                    "codex_5h_used_percent": 80,
                    "codex_5h_reset_at": "2026-05-25T10:30:00Z",
                    "codex_7d_used_percent": 100,
                    "codex_7d_reset_at": "2026-05-28T22:30:00Z",
                },
            }
        )

        self.assertEqual(line, "#8 oauth-account · pro：5h 剩余 20%（恢复 05-25 18:30）")
        self.assertNotIn("7d", line)

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
            self.assertIn({"command": "whitelist", "description": "查询 Guard 白名单"}, calls[0][1]["commands"])
            self.assertIn({"command": "endless", "description": "查询 Guard 无尽模式"}, calls[0][1]["commands"])

    async def test_whitelist_callback_adds_account_idempotently_and_removes_endless_mode(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            guard_path = Path(tmpdir) / "guard-state.json"
            audit_path = Path(tmpdir) / "audit.jsonl"
            settings = make_settings(str(state_path))
            settings.guard_state_path = str(guard_path)
            settings.audit_path = str(audit_path)
            GuardStore(str(guard_path)).save_policy(
                {"failure_threshold": 8, "whitelist_account_ids": [9], "endless_account_ids": [7, 12]}
            )
            bot = TelegramOpsBot(
                settings,
                WhitelistFakeDB([{"id": 7, "name": "target", "schedulable": True}]),  # type: ignore[arg-type]
                guard_runner,
                guard_config,
            )

            reply, keyboard = await bot._callback_reply(100, 200, "wladd:7")
            duplicate_reply, _duplicate_keyboard = await bot._callback_reply(100, 200, "wladd:#7")

            policy = GuardStore(str(guard_path)).policy_config()
            self.assertEqual(policy["failure_threshold"], 8)
            self.assertEqual(policy["whitelist_account_ids"], [7, 9])
            self.assertEqual(policy["endless_account_ids"], [12])
            self.assertIn("已将账号 #7 target 加入 Guard 白名单", reply)
            self.assertIn("已在 Guard 白名单", duplicate_reply)
            self.assertIn("wlrm:7", json.dumps(keyboard, ensure_ascii=False))
            audit_text = audit_path.read_text(encoding="utf-8")
            self.assertIn("telegram_whitelist_add", audit_text)
            self.assertIn('"changed": true', audit_text)
            self.assertIn('"changed": false', audit_text)

    async def test_whitelist_command_lists_and_removes_accounts(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            guard_path = Path(tmpdir) / "guard-state.json"
            settings = make_settings(str(state_path))
            settings.guard_state_path = str(guard_path)
            GuardStore(str(guard_path)).save_policy(
                {
                    "failure_threshold": 8,
                    "success_threshold": 3,
                    "whitelist_account_ids": [7, 9],
                }
            )
            bot = TelegramOpsBot(
                settings,
                WhitelistFakeDB([{"id": 7, "name": "target", "schedulable": True}]),  # type: ignore[arg-type]
                guard_runner,
                guard_config,
            )

            reply, keyboard = await bot._text_reply(100, 200, "/whitelist")
            self.assertIn("#7 target", reply)
            self.assertIn("#9 当前账号不存在或已删除", reply)
            self.assertIn("wlrm:7", json.dumps(keyboard, ensure_ascii=False))
            self.assertIn("wlrm:9", json.dumps(keyboard, ensure_ascii=False))

            reply, keyboard = await bot._text_reply(100, 200, "/wl rm #7")
            self.assertIn("已将账号 #7 移出 Guard 白名单", reply)
            self.assertEqual(GuardStore(str(guard_path)).policy_config()["whitelist_account_ids"], [9])
            self.assertIn("wlrm:9", json.dumps(keyboard, ensure_ascii=False))

            reply, _keyboard = await bot._text_reply(100, 200, "/whitelist remove 7")
            self.assertIn("账号 #7 不在 Guard 白名单", reply)
            self.assertEqual(GuardStore(str(guard_path)).policy_config()["whitelist_account_ids"], [9])

    async def test_whitelist_remove_callback_removes_account(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            guard_path = Path(tmpdir) / "guard-state.json"
            settings = make_settings(str(state_path))
            settings.guard_state_path = str(guard_path)
            GuardStore(str(guard_path)).save_policy({"failure_threshold": 8, "whitelist_account_ids": [7, 9]})
            bot = TelegramOpsBot(
                settings,
                WhitelistFakeDB([{"id": 9, "name": "remain", "schedulable": True}]),  # type: ignore[arg-type]
                guard_runner,
                guard_config,
            )

            reply, keyboard = await bot._callback_reply(100, 200, "wlrm:7")

            self.assertIn("已将账号 #7 移出 Guard 白名单", reply)
            self.assertEqual(GuardStore(str(guard_path)).policy_config()["whitelist_account_ids"], [9])
            self.assertNotIn("wlrm:7", json.dumps(keyboard, ensure_ascii=False))
            self.assertIn("wlrm:9", json.dumps(keyboard, ensure_ascii=False))

    async def test_endless_callback_adds_account_idempotently_and_removes_whitelist_mode(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            guard_path = Path(tmpdir) / "guard-state.json"
            audit_path = Path(tmpdir) / "audit.jsonl"
            settings = make_settings(str(state_path))
            settings.guard_state_path = str(guard_path)
            settings.audit_path = str(audit_path)
            GuardStore(str(guard_path)).save_policy(
                {"failure_threshold": 8, "whitelist_account_ids": [7, 9], "endless_account_ids": [12]}
            )
            bot = TelegramOpsBot(
                settings,
                WhitelistFakeDB([{"id": 7, "name": "target", "schedulable": True}]),  # type: ignore[arg-type]
                guard_runner,
                guard_config,
            )

            reply, keyboard = await bot._callback_reply(100, 200, "endadd:7")
            duplicate_reply, _duplicate_keyboard = await bot._callback_reply(100, 200, "endadd:#7")

            policy = GuardStore(str(guard_path)).policy_config()
            self.assertEqual(policy["failure_threshold"], 8)
            self.assertEqual(policy["whitelist_account_ids"], [9])
            self.assertEqual(policy["endless_account_ids"], [7, 12])
            self.assertIn("已将账号 #7 target 加入 Guard 无尽模式", reply)
            self.assertIn("已在 Guard 无尽模式", duplicate_reply)
            self.assertIn("endrm:7", json.dumps(keyboard, ensure_ascii=False))
            self.assertIn("endrm:12", json.dumps(keyboard, ensure_ascii=False))
            audit_text = audit_path.read_text(encoding="utf-8")
            self.assertIn("telegram_endless_add", audit_text)
            self.assertIn('"changed": true', audit_text)
            self.assertIn('"changed": false', audit_text)

    async def test_endless_callback_rejects_oauth_accounts(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            guard_path = Path(tmpdir) / "guard-state.json"
            audit_path = Path(tmpdir) / "audit.jsonl"
            settings = make_settings(str(state_path))
            settings.guard_state_path = str(guard_path)
            settings.audit_path = str(audit_path)
            GuardStore(str(guard_path)).save_policy({"failure_threshold": 8, "endless_account_ids": [12]})
            bot = TelegramOpsBot(
                settings,
                WhitelistFakeDB([{"id": 7, "name": "oauth-target", "type": "oauth", "schedulable": True}]),  # type: ignore[arg-type]
                guard_runner,
                guard_config,
            )

            reply, keyboard = await bot._callback_reply(100, 200, "endadd:7")

            policy = GuardStore(str(guard_path)).policy_config()
            self.assertEqual(policy["endless_account_ids"], [12])
            self.assertIn("OAuth 账号不支持 Guard 无尽模式", reply)
            self.assertIn("endrm:12", json.dumps(keyboard, ensure_ascii=False))
            audit_text = audit_path.read_text(encoding="utf-8")
            self.assertIn("telegram_endless_add_rejected", audit_text)
            self.assertIn('"reason": "oauth_account"', audit_text)

    async def test_endless_command_lists_and_removes_accounts(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            guard_path = Path(tmpdir) / "guard-state.json"
            settings = make_settings(str(state_path))
            settings.guard_state_path = str(guard_path)
            GuardStore(str(guard_path)).save_policy(
                {
                    "failure_threshold": 8,
                    "success_threshold": 3,
                    "endless_account_ids": [7, 9],
                }
            )
            bot = TelegramOpsBot(
                settings,
                WhitelistFakeDB([{"id": 7, "name": "target", "schedulable": True}]),  # type: ignore[arg-type]
                guard_runner,
                guard_config,
            )

            reply, keyboard = await bot._text_reply(100, 200, "/endless")
            alias_reply, alias_keyboard = await bot._text_reply(100, 200, "查无尽")
            remove_reply, remove_keyboard = await bot._text_reply(100, 200, "/endless rm #7")
            duplicate_reply, _duplicate_keyboard = await bot._text_reply(100, 200, "/endless remove 7")

            self.assertIn("Guard 无尽模式", reply)
            self.assertIn("#7 target", reply)
            self.assertIn("#9 当前账号不存在或已删除", reply)
            self.assertIn("endrm:7", json.dumps(keyboard, ensure_ascii=False))
            self.assertIn("endrm:9", json.dumps(keyboard, ensure_ascii=False))
            self.assertIn("Guard 无尽模式", alias_reply)
            self.assertIn("endrm:7", json.dumps(alias_keyboard, ensure_ascii=False))
            self.assertIn("已将账号 #7 移出 Guard 无尽模式", remove_reply)
            self.assertEqual(GuardStore(str(guard_path)).policy_config()["endless_account_ids"], [9])
            self.assertIn("endrm:9", json.dumps(remove_keyboard, ensure_ascii=False))
            self.assertIn("账号 #7 不在 Guard 无尽模式", duplicate_reply)

    async def test_endless_remove_callback_removes_account(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            state_path = Path(tmpdir) / "state.json"
            guard_path = Path(tmpdir) / "guard-state.json"
            audit_path = Path(tmpdir) / "audit.jsonl"
            settings = make_settings(str(state_path))
            settings.guard_state_path = str(guard_path)
            settings.audit_path = str(audit_path)
            GuardStore(str(guard_path)).save_policy({"failure_threshold": 8, "endless_account_ids": [7, 9]})
            bot = TelegramOpsBot(
                settings,
                WhitelistFakeDB([{"id": 9, "name": "remain", "schedulable": True}]),  # type: ignore[arg-type]
                guard_runner,
                guard_config,
            )

            reply, keyboard = await bot._callback_reply(100, 200, "endrm:7")

            self.assertIn("已将账号 #7 移出 Guard 无尽模式", reply)
            self.assertEqual(GuardStore(str(guard_path)).policy_config()["endless_account_ids"], [9])
            self.assertNotIn("endrm:7", json.dumps(keyboard, ensure_ascii=False))
            self.assertIn("endrm:9", json.dumps(keyboard, ensure_ascii=False))
            audit_text = audit_path.read_text(encoding="utf-8")
            self.assertIn("telegram_endless_remove", audit_text)
            self.assertIn('"changed": true', audit_text)

    def test_account_action_keyboard_has_only_direct_account_actions(self) -> None:
        keyboard = account_actions_keyboard({"id": 7, "schedulable": True})
        callback_data = [button["callback_data"] for row in keyboard["inline_keyboard"] for button in row]

        self.assertEqual(
            callback_data,
            ["pause:7", "cd:7:5", "cd:7:15", "cd:7:30", "acct:7", "wladd:7", "endadd:7"],
        )
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

    async def test_oauth_recovery_state_persists_dedupe_without_nesting(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            bot = TelegramOpsBot(make_settings(str(Path(tmpdir) / "state.json")), object(), guard_runner, guard_config)  # type: ignore[arg-type]

            state = await bot.oauth_recovery_state()
            state["oauth_account_recovery_alerts"] = {"9:reset": {"account_id": 9}}
            await bot.save_oauth_recovery_state(state)

            reloaded = await bot.oauth_recovery_state()
            self.assertEqual(reloaded["oauth_account_recovery_alerts"], {"9:reset": {"account_id": 9}})
            self.assertNotIn("oauth_account_recovery_alerts", reloaded["oauth_account_recovery_alerts"]["9:reset"])

    async def test_oauth_recovery_notify_reports_delivery_success(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            settings = make_settings(str(Path(tmpdir) / "state.json"))
            settings.telegram_enabled = True
            settings.telegram_bot_token = "token"
            settings.telegram_allowed_chat_ids = [123]
            bot = TelegramOpsBot(settings, object(), guard_runner, guard_config)  # type: ignore[arg-type]
            calls: list[tuple[int, str]] = []

            async def fake_send(chat_id: int, text: str, _keyboard: dict[str, Any] | None = None) -> bool:
                calls.append((chat_id, text))
                return True

            bot._send_message = fake_send  # type: ignore[method-assign]
            delivered = await bot.notify_oauth_quota_recovery_alerts(
                [{"account_id": 9, "account_name": "lt", "plan_type": "plus", "window_labels": ["5h", "7d"]}]
            )

            self.assertEqual([row["account_id"] for row in delivered], [9])
            self.assertEqual(calls[0][0], 123)
            self.assertIn("OAuth 账号额度已恢复可用", calls[0][1])

    async def test_oauth_recovery_notify_reports_delivery_failure(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            settings = make_settings(str(Path(tmpdir) / "state.json"))
            settings.telegram_enabled = True
            settings.telegram_bot_token = "token"
            settings.telegram_allowed_chat_ids = [123]
            bot = TelegramOpsBot(settings, object(), guard_runner, guard_config)  # type: ignore[arg-type]

            async def fake_send(_chat_id: int, _text: str, _keyboard: dict[str, Any] | None = None) -> bool:
                return False

            bot._send_message = fake_send  # type: ignore[method-assign]
            delivered = await bot.notify_oauth_quota_recovery_alerts(
                [{"account_id": 9, "account_name": "lt", "plan_type": "plus", "window_labels": ["5h", "7d"]}]
            )

            self.assertEqual(delivered, [])

    async def test_oauth_recovery_notify_returns_only_rows_delivered_to_every_chat(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            settings = make_settings(str(Path(tmpdir) / "state.json"))
            settings.telegram_enabled = True
            settings.telegram_bot_token = "token"
            settings.telegram_allowed_chat_ids = [123, 456]
            bot = TelegramOpsBot(settings, object(), guard_runner, guard_config)  # type: ignore[arg-type]
            calls: list[tuple[int, str]] = []

            async def fake_send(chat_id: int, text: str, _keyboard: dict[str, Any] | None = None) -> bool:
                calls.append((chat_id, text))
                return "despond" in text or chat_id != 456

            bot._send_message = fake_send  # type: ignore[method-assign]
            delivered = await bot.notify_oauth_quota_recovery_alerts(
                [
                    {"account_id": 26, "account_name": "growing.generic.7p+g5@icloud.com", "plan_type": "plus"},
                    {"account_id": 27, "account_name": "despond.dipper-0e+g5@icloud.com", "plan_type": "plus"},
                ]
            )

            self.assertEqual([row["account_id"] for row in delivered], [27])
            self.assertEqual(len(calls), 4)

    async def test_oauth_recovery_notify_skips_when_push_disabled(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            settings = make_settings(str(Path(tmpdir) / "state.json"))
            settings.telegram_enabled = True
            settings.telegram_bot_token = "token"
            settings.telegram_allowed_chat_ids = [123]
            settings.telegram_oauth_recovery_push_enabled = False
            bot = TelegramOpsBot(settings, object(), guard_runner, guard_config)  # type: ignore[arg-type]

            async def unexpected_send(_chat_id: int, _text: str, _keyboard: dict[str, Any] | None = None) -> bool:
                raise AssertionError("OAuth recovery push is disabled")

            bot._send_message = unexpected_send  # type: ignore[method-assign]
            delivered = await bot.notify_oauth_quota_recovery_alerts(
                [{"account_id": 9, "account_name": "lt", "plan_type": "plus", "window_labels": ["5h", "7d"]}]
            )

            self.assertEqual(delivered, [])

    def test_telegram_template_contains_oauth_monitoring_switches(self) -> None:
        template = (Path(__file__).resolve().parents[1] / "app" / "templates" / "telegram.html").read_text(
            encoding="utf-8"
        )

        self.assertIn("OAuth 账号监控", template)
        self.assertIn('action="{{ base_path }}/telegram/oauth-settings"', template)
        self.assertIn('name="oauth_usage_refresh_enabled"', template)
        self.assertIn('name="oauth_recovery_monitor_enabled"', template)
        self.assertIn('name="oauth_recovery_push_enabled"', template)

    async def test_telegram_oauth_settings_save_preserves_token_and_updates_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            config_path = Path(tmpdir) / "telegram-config.json"
            audit_path = Path(tmpdir) / "audit.jsonl"
            original_settings = main_module.settings
            original_restart = main_module.restart_telegram_bot
            main_module.settings = make_settings(str(Path(tmpdir) / "state.json"))
            main_module.settings.telegram_config_path = str(config_path)
            main_module.settings.audit_path = str(audit_path)
            main_module.settings.telegram_bot_token = "123:test"
            main_module.save_telegram_runtime_config(
                {
                    "enabled": True,
                    "bot_token": "123:test",
                    "pairing_enabled": True,
                    "pairing_code": "ABCD-EFGH",
                    "oauth_usage_refresh_enabled": True,
                    "oauth_recovery_monitor_enabled": True,
                    "oauth_recovery_push_enabled": True,
                }
            )
            restart_calls = 0

            async def fake_restart() -> None:
                nonlocal restart_calls
                restart_calls += 1

            class FakeForm:
                def getlist(self, key: str) -> list[str]:
                    values = {
                        "oauth_recovery_monitor_enabled": ["1"],
                    }
                    return values.get(key, [])

            class FakeRequest:
                async def form(self) -> FakeForm:
                    return FakeForm()

            try:
                main_module.restart_telegram_bot = fake_restart  # type: ignore[assignment]
                response = await main_module.telegram_oauth_settings_save(FakeRequest(), "tester")  # type: ignore[arg-type]
            finally:
                main_module.settings = original_settings
                main_module.restart_telegram_bot = original_restart  # type: ignore[assignment]

            saved = json.loads(config_path.read_text(encoding="utf-8"))
            self.assertEqual(response.status_code, 303)
            self.assertEqual(saved["bot_token"], "123:test")
            self.assertFalse(saved["oauth_usage_refresh_enabled"])
            self.assertTrue(saved["oauth_recovery_monitor_enabled"])
            self.assertFalse(saved["oauth_recovery_push_enabled"])
            self.assertEqual(restart_calls, 1)
            self.assertIn('"oauth_usage_refresh_enabled": false', audit_path.read_text(encoding="utf-8"))

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

    def test_oauth_quota_recovery_alert_includes_plan_windows_and_test_status(self) -> None:
        text = oauth_quota_recovery_alert(
            {
                "account_id": 9,
                "account_name": "lt",
                "plan_type": "plus",
                "window_labels": ["5h", "7d"],
                "trigger_window_labels": ["5h"],
                "reset_at": "2026-05-25T00:00:00+00:00",
                "remaining_summary": "5h 100% / 7d 100%",
                "test_latency_ms": 2345,
                "test_model_id": "gpt-test",
            }
        )

        self.assertIn("OAuth 账号额度已恢复可用", text)
        self.assertIn("#9 lt · plus：5h/7d 已恢复，测试通过", text)
        self.assertIn("触发窗口：5h", text)
        self.assertIn("当前剩余：5h 100% / 7d 100%", text)
        self.assertIn("测试模型：gpt-test", text)
        self.assertIn("测试耗时：2.35s", text)

    def test_oauth_quota_recovery_alert_includes_test_failure_code_and_message(self) -> None:
        text = oauth_quota_recovery_alert(
            {
                "account_id": 9,
                "account_name": "lt",
                "plan_type": "plus",
                "window_labels": ["5h", "7d"],
                "trigger_window_labels": ["7d"],
                "status": "test_failed",
                "error_code": "rate_limited",
                "error": "rate limit",
                "test_latency_ms": 1000,
            }
        )

        self.assertIn("OAuth 账号额度恢复后测试失败", text)
        self.assertIn("#9 lt · plus：5h/7d 已有额度，测试失败", text)
        self.assertIn("错误代码：rate_limited", text)
        self.assertIn("错误信息：rate limit", text)
        self.assertIn("触发窗口：7d", text)

    def test_oauth_quota_recovery_alert_omits_free_five_hour_window(self) -> None:
        text = oauth_quota_recovery_alert(
            {
                "account_id": 9,
                "account_name": "free-account",
                "plan_type": "free",
                "window_labels": ["7d"],
                "reset_at": "2026-05-25T00:00:00+00:00",
                "remaining_summary": "7d 100%",
                "test_latency_ms": 1000,
            }
        )

        self.assertIn("#9 free-account · free：7d 已恢复，测试通过", text)
        self.assertNotIn("5h", text)

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

    def test_guard_action_message_includes_endless_recovery_plan_result(self) -> None:
        success_text = format_guard_actions(
            "自动 Guard 已处理",
            [
                {
                    "account_id": 9,
                    "name": "endless",
                    "action": "pause",
                    "reason": "auto guard endless mode",
                    "endless_recovery_plan": {"scheduled": True, "cron_expression": "* * * * *"},
                }
            ],
        )
        failed_text = format_guard_actions(
            "自动 Guard 已处理",
            [
                {
                    "account_id": 10,
                    "name": "endless-failed",
                    "action": "pause",
                    "reason": "auto guard endless mode",
                    "endless_recovery_plan": {"scheduled": False, "error": "missing scheduled_test_plans"},
                }
            ],
        )
        alert_text = account_alert(
            "账号异常",
            {
                "account_id": 10,
                "name": "endless-failed",
                "action": "pause",
                "endless_recovery_plan": {"scheduled": False, "error": "missing scheduled_test_plans"},
            },
            None,
        )

        self.assertIn("恢复计划：1m 已创建", success_text)
        self.assertIn("恢复计划：1m 创建失败 missing scheduled_test_plans", failed_text)
        self.assertIn("无尽恢复计划：1m 创建失败 missing scheduled_test_plans", alert_text)


if __name__ == "__main__":
    unittest.main()
