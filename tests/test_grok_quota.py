from __future__ import annotations

import asyncio
import json
import threading
import time
import unittest
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

from app.grok_quota import format_grok_account, grok_quota_reply, query_grok_billing
from app.telegram_bot import TelegramOpsBot, split_messages

NOW = datetime(2026, 9, 22, 1, 2, 3, tzinfo=timezone.utc)
ACCOUNT = {"id": 390, "name": "Grok OAuth", "platform": "grok", "type": "oauth"}


def response(*, weekly=40, monthly=20, partial=False, failed=None, fetched="2026-09-22T01:02:03Z", code=""):
    return {"subscription_tier": "PRO", "grok_billing": {
        "plan": "PRO", "fetched_at": fetched, "status_code": 200,
        "weekly_status_code": 200, "monthly_status_code": 200,
        "partial": partial, "failed_windows": failed or [],
    }, "seven_day": {"utilization": weekly, "resets_at": "2026-09-25T00:00:00Z"},
        "thirty_day": {"utilization": monthly, "resets_at": "2026-10-01T00:00:00Z"},
        "grok_request_quota": {"remaining": 12, "limit": 60},
        "grok_token_quota": {"remaining": 500, "limit": 2000},
        "grok_last_headers_seen_at": "2026-09-20T08:00:00Z", "error_code": code}


class QuotaDb:
    def __init__(self, rows):
        self.rows = rows

    def fetch_all(self, sql, _params=None):
        return [dict(row) for row in self.rows if row["platform"] == "grok" and row["type"] == "oauth"
                and row.get("deleted_at") is None]

    def fetch_one(self, _sql, params=None):
        return next((dict(row) for row in self.rows if row["id"] == params["account_id"]
                     and row.get("deleted_at") is None), None)


class GrokFormattingTests(unittest.TestCase):
    def test_complete_official_windows_plan_and_beijing_reset(self):
        text = format_grok_account(ACCOUNT, {"success": True, "queried_at": NOW.isoformat(), "data": response()})
        self.assertIn("#390 Grok OAuth · PRO", text)
        self.assertIn("7d 剩余 60%", text)
        self.assertIn("月度 剩余 80%", text)
        self.assertIn("2026-09-25 08:00:00", text)
        self.assertIn("请求限流历史快照：12/60 · 采集 2026-09-20", text)

    def test_partial_stale_and_exhausted_are_not_fresh(self):
        partial = response(weekly=100, monthly=5, partial=True, failed=["weekly"])
        text = format_grok_account(ACCOUNT, {"success": True, "queried_at": NOW.isoformat(), "data": partial})
        self.assertIn("7d 剩余未知（本次刷新失败）", text)
        self.assertNotIn("7d 剩余 0%", text)
        self.assertIn("月度 剩余 95%", text)
        exhausted = response(weekly=100, monthly=100)
        text = format_grok_account(ACCOUNT, {"success": True, "queried_at": NOW.isoformat(), "data": exhausted})
        self.assertIn("7d 剩余 0% · 耗尽", text)
        self.assertIn("月度 剩余 0% · 耗尽", text)
        stale = response(fetched="2026-09-21T01:02:03Z")
        text = format_grok_account(ACCOUNT, {"success": True, "queried_at": NOW.isoformat(), "data": stale})
        self.assertIn("7d 剩余未知", text)
        self.assertIn("月度 剩余未知", text)

    def test_free_unknown_auth_and_sensitive_error(self):
        free = {"subscription_tier": "FREE", "error_code": "quota_unknown"}
        text = format_grok_account(ACCOUNT, {"success": True, "queried_at": NOW.isoformat(), "data": free})
        self.assertIn("FREE", text)
        self.assertIn("7d 剩余未知", text)
        self.assertIn("月度 剩余未知", text)
        self.assertIn("请求限流历史快照：未知", text)
        auth = response(code="unauthenticated")
        auth["needs_reauth"] = True
        self.assertIn("认证异常 [unauthenticated]", format_grok_account(ACCOUNT, {"success": True,
                     "queried_at": NOW.isoformat(), "data": auth}))
        failed = format_grok_account(ACCOUNT, {"success": False,
            "error_code": "http_401", "error": "Authorization: Bearer secret-leak"})
        self.assertIn("http_401", failed)
        self.assertNotIn("secret-leak", failed)

    def test_real_loopback_capture_only_billing_usage_endpoint(self):
        calls = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                calls.append((self.path, self.headers.get("x-api-key")))
                payload = json.dumps({"code": 0, "data": response()}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *_args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            result = query_grok_billing(390, f"http://127.0.0.1:{server.server_port}", "test-key", now=NOW)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
        self.assertTrue(result["success"])
        self.assertEqual(calls, [("/api/v1/admin/accounts/390/usage?source=active&force=true", "test-key")])

    def test_business_code_timeout_and_secret_redaction(self):
        def business(_request, timeout):
            self.assertEqual(timeout, 30)
            return {"code": 401, "message": "Authorization: Bearer secret-leak"}

        result = query_grok_billing(390, "http://127.0.0.1:8080", "key", opener=business, now=NOW)
        self.assertFalse(result["success"])
        self.assertNotIn("secret-leak", json.dumps(result))

        def timed_out(_request, _timeout):
            raise TimeoutError("request timed out")

        result = query_grok_billing(390, "http://127.0.0.1:8080", "key", opener=timed_out, now=NOW)
        self.assertEqual(result["error_code"], "timeout")


class GrokReplyTests(unittest.IsolatedAsyncioTestCase):
    async def test_filters_apikey_and_deleted_revalidates_before_and_after(self):
        rows = [dict(ACCOUNT), {**ACCOUNT, "id": 391, "type": "apikey"},
                {**ACCOUNT, "id": 392, "deleted_at": NOW.isoformat()}]
        db = QuotaDb(rows)
        calls = []

        def runner(account_id, _base, _token):
            calls.append(account_id)
            rows[0]["deleted_at"] = NOW.isoformat()
            return {"success": True, "queried_at": NOW.isoformat(), "data": response()}

        text = await grok_quota_reply(db, "http://127.0.0.1", "key", 4, runner=runner)
        self.assertEqual(calls, [390])
        self.assertIn("结果已忽略", text)
        self.assertNotIn("391", text)
        self.assertNotIn("392", text)
        self.assertEqual(await grok_quota_reply(db, "url", "key", 4), "Grok OAuth\n没有 Grok OAuth 账号。")

    async def test_platform_failure_isolation_concurrency_and_long_message_split(self):
        rows = [{**ACCOUNT, "id": i, "name": "账号" + "X" * 1000} for i in range(390, 395)]
        db = QuotaDb(rows)
        running = 0
        maximum = 0
        lock = threading.Lock()

        def runner(account_id, _base, _token):
            nonlocal running, maximum
            with lock:
                running += 1
                maximum = max(maximum, running)
            time.sleep(0.01)
            with lock:
                running -= 1
            return {"success": True, "queried_at": NOW.isoformat(), "data": response()}

        text = await grok_quota_reply(db, "http://127.0.0.1", "key", 2, runner=runner)
        self.assertLessEqual(maximum, 2)
        self.assertEqual(text.count("限流历史快照"), 10)
        parts = split_messages("OpenAI OAuth\n失败\n\n" + text, limit=400)
        self.assertGreater(len(parts), 1)
        self.assertEqual("\n\n".join(parts), "OpenAI OAuth\n失败\n\n" + text)
        self.assertTrue(all(len(part.encode("utf-16-le")) // 2 <= 400 for part in parts))

    async def test_concurrent_bot_requests_share_both_platform_refreshes(self):
        class Monitor:
            base_url_provider = staticmethod(lambda: "http://127.0.0.1")

            def __init__(self):
                self.calls = 0
                self.store = type("Store", (), {"admin_token": lambda _: "key"})()

            def force_refresh(self, _timeout):
                self.calls += 1
                time.sleep(0.03)
                return {"success": True, "refresh_at": NOW.isoformat(), "success_count": 0}

        monitor = Monitor()
        settings = type("Settings", (), {"telegram_oauth_usage_refresh_concurrency": 2})()
        bot = TelegramOpsBot(settings, QuotaDb([ACCOUNT]), oauth_monitor=monitor)
        with patch("app.telegram_bot.grok_quota_reply", return_value="Grok OAuth\n可用") as grok:
            replies = await asyncio.gather(bot._quota_reply(), bot._quota_reply())
        self.assertEqual(monitor.calls, 1)
        self.assertEqual(grok.call_count, 1)
        self.assertTrue(all("Grok OAuth" in reply[0] for reply in replies))

    async def test_openai_failure_does_not_hide_grok(self):
        settings = type("Settings", (), {"telegram_oauth_usage_refresh_concurrency": 2})()
        bot = TelegramOpsBot(settings, QuotaDb([ACCOUNT]), oauth_monitor=None)
        with patch("app.telegram_bot.grok_quota_reply", return_value="Grok OAuth\n#390 剩余 60%"):
            text, _ = await bot._quota_reply()
        self.assertIn("OpenAI", text)
        self.assertIn("#390 剩余 60%", text)
