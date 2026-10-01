from __future__ import annotations

import unittest
import asyncio
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace
from unittest.mock import AsyncMock
from datetime import datetime, timedelta, timezone

from pydantic import ValidationError

from app.desktop_actions import DesktopActions, PriorityRequest, TestRequest, billing_is_fresh, exact_model_options, filter_model_options


class DesktopActionContractTests(unittest.TestCase):
    def test_model_options_use_union_of_current_group_allowlists(self) -> None:
        items = [
            {"id": "gpt-6-luna", "display_name": "Luna", "type": "text"},
            {"id": "gpt-6-astra", "display_name": "Astra", "type": "text"},
            {"id": "gpt-5.6-terra", "display_name": "Terra", "type": "text"},
            {"id": "gpt-6-luna", "display_name": "duplicate", "type": "text"},
        ]
        allowlists = [
            {"enabled": True, "models": ["gpt-6-luna"]},
            {"enabled": True, "models": ["gpt-5.*"]},
        ]
        self.assertEqual([item["id"] for item in filter_model_options(items, allowlists)],
                         ["gpt-6-luna", "gpt-5.6-terra"])
        self.assertEqual(len(filter_model_options(items, [{"enabled": False, "models": []}])), 3)
        self.assertEqual(len(filter_model_options(items, [])), 3)

    def test_exact_model_test_options_ignore_wildcards_media_and_blanks(self) -> None:
        allowlists = [
            {"enabled": True, "models": ["gpt-6-luna", "gpt-6-*", "", "gpt-image-1"]},
            {"enabled": True, "models": ["gpt-6-luna", "gpt-5.6-terra", "gpt-6-audio"]},
        ]
        self.assertEqual([item["id"] for item in exact_model_options(allowlists)],
                         ["gpt-6-luna", "gpt-5.6-terra"])
        self.assertEqual(exact_model_options([{"enabled": True, "models": ["*"]}]), [])

    def test_priority_is_strict_and_bounded(self) -> None:
        base = {"expected_version": "a" * 64}
        self.assertEqual(PriorityRequest(priority=0, **base).priority, 0)
        with self.assertRaises(ValidationError):
            PriorityRequest(priority=True, **base)
        with self.assertRaises(ValidationError):
            PriorityRequest(priority=-1, **base)

    def test_test_request_accepts_missing_or_legacy_confirmation(self) -> None:
        request = TestRequest(expected_version="a" * 64, mode="image", model_id="gpt-image-1")
        self.assertFalse(request.confirmed)
        self.assertTrue(TestRequest(expected_version="a" * 64, confirmed=True).confirmed)
        with self.assertRaises(ValidationError):
            TestRequest(expected_version="a" * 64, mode="unknown")

    def test_grok_batch_accepts_only_fresh_complete_billing(self) -> None:
        queried = datetime(2026, 9, 25, 8, 0, tzinfo=timezone.utc)
        payload = {"grok_billing": {"fetched_at": queried.isoformat(), "partial": False}}
        self.assertTrue(billing_is_fresh(payload, queried.isoformat()))
        self.assertFalse(billing_is_fresh({"grok_billing": {"fetched_at": (queried - timedelta(seconds=1)).isoformat()}}, queried.isoformat()))
        self.assertFalse(billing_is_fresh({"grok_billing": {"fetched_at": queried.isoformat(), "partial": True}}, queried.isoformat()))


class DesktopStreamTests(unittest.IsolatedAsyncioTestCase):
    async def test_models_filters_using_live_group_allowlist_union(self):
        class Db:
            def fetch_all(self, sql, params):
                self.params = params
                return [
                    {"model_allowlist": {"enabled": True, "models": ["gpt-6-luna"]}},
                    {"model_allowlist": {"enabled": True, "models": ["gpt-5.*"]}},
                ]

        class Client:
            async def __aenter__(self):
                return self

            async def __aexit__(self, *args):
                return None

        service = SimpleNamespace(r=SimpleNamespace(db=Db()))
        actions = DesktopActions(service)
        actions.account = AsyncMock(return_value={"group_ids": [7, 8]})
        actions.client = lambda key: Client()
        actions.json_request = AsyncMock(return_value=[
            {"id": "gpt-6-luna", "display_name": "Luna", "type": "text"},
            {"id": "gpt-5.6-terra", "display_name": "Terra", "type": "text"},
            {"id": "gpt-6-astra", "display_name": "Astra", "type": "text"},
        ])
        result = await actions.models(7, "admin-key")
        self.assertEqual([item["id"] for item in result], ["gpt-6-luna", "gpt-5.6-terra"])
        self.assertEqual(service.r.db.params, {"ids": [7, 8]})

    async def test_model_test_candidates_are_full_exact_group_union_without_catalog_intersection(self):
        exact = ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-6.1-sol",
                 "gpt-6-sol", "gpt-6-luna", "gpt-6-astra", "codex-auto-review"]
        class Db:
            def fetch_all(self, sql, params):
                self.sql, self.params = sql, params
                return [{"id": 13, "model_allowlist": {"enabled": True, "models": exact}}]

        class UnusedClient:
            async def __aenter__(self): raise AssertionError("strict whitelist should not query provider catalog")
            async def __aexit__(self, *args): return None

        db = Db()
        service = SimpleNamespace(r=SimpleNamespace(db=db))
        actions = DesktopActions(service)
        actions.account = AsyncMock(return_value={"id": 387, "platform": "openai", "group_ids": [13]})
        actions.client = lambda key: UnusedClient()
        result = await actions.models(387, "admin-key", "model_test")
        self.assertEqual([item["id"] for item in result], exact)
        self.assertEqual([item["display_name"] for item in result], exact)
        self.assertEqual(db.params, {"ids": [13], "platform": "openai"})
        self.assertIn("ORDER BY sort_order,id", db.sql)

    async def test_model_test_candidates_fall_back_when_no_group_or_allowlist_disabled(self):
        class Db:
            def fetch_all(self, sql, params):
                return [{"id": 13, "model_allowlist": {"enabled": False, "models": ["gpt-6-sol"]}}]
        class Client:
            async def __aenter__(self): return self
            async def __aexit__(self, *args): return None
        provider = [{"id": "gpt-6-sol", "display_name": "Codex", "type": "text"},
                    {"id": "gpt-image-1", "display_name": "Image", "type": "image"}]
        service = SimpleNamespace(r=SimpleNamespace(db=Db()))
        actions = DesktopActions(service)
        actions.account = AsyncMock(return_value={"platform": "openai", "group_ids": [13]})
        actions.client = lambda key: Client()
        actions.json_request = AsyncMock(return_value=provider)
        self.assertEqual([item["id"] for item in await actions.models(7, "admin-key", "model_test")], ["gpt-6-sol"])
        actions.account = AsyncMock(return_value={"platform": "openai", "group_ids": []})
        actions.json_request = AsyncMock(return_value=provider)
        self.assertEqual([item["id"] for item in await actions.models(7, "admin-key", "model_test")], ["gpt-6-sol"])

    async def test_model_test_submission_rechecks_exact_allowlist(self):
        class Db:
            def fetch_all(self, sql, params):
                return [{"id": 13, "model_allowlist": {"enabled": True, "models": ["gpt-6-sol", "gpt-6-*"]}}]
        actions = DesktopActions(SimpleNamespace(r=SimpleNamespace(db=Db())))
        row = {"platform": "openai", "group_ids": [13]}
        self.assertTrue(await actions.model_allowed(row, "gpt-6-sol"))
        self.assertFalse(await actions.model_allowed(row, "gpt-6-luna"))
        self.assertFalse(await actions.model_allowed(row, "gpt-6-*"))
        self.assertFalse(await actions.model_allowed(row, "gpt-image-1"))

    async def test_real_sse_keeps_whitespace_and_releases_lock_without_confirmation(self):
        chunks = ["Hello", " ", "world", "!\n", "\t", "  code = 1\n\n", "中文 👋", " "]
        requests = []
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_POST(self):
                requests.append((self.path, json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                for text in chunks:
                    self.wfile.write(("data: " + json.dumps({"type": "content", "text": text}, ensure_ascii=False) + "\n\n").encode())
                self.wfile.write(b'data: {"type":"test_complete","success":true}\n\n')
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        lock = threading.Lock()
        service = SimpleNamespace(r=SimpleNamespace(oauth_base_url=lambda: f"http://127.0.0.1:{server.server_port}"),
                                  account_lock=lambda _: lock, invalidate=lambda: None)
        actions = DesktopActions(service)
        actions.account = AsyncMock(return_value={"platform": "openai", "type": "oauth"})
        payload = TestRequest(expected_version="a" * 64)
        try:
            locks = await actions.prepare_test(7, payload)
            with self.assertRaises(Exception): await actions.prepare_test(7, payload)
            events = [json.loads(line[6:]) async for line in actions.test_stream(7, payload, "test-key", locks)]
            self.assertEqual([e["text"] for e in events if e["type"] == "content"], chunks)
            self.assertTrue(events[-1]["success"])
            self.assertEqual(len(requests), 1)
            self.assertEqual(requests[0][0], "/api/v1/admin/accounts/7/test")
            self.assertNotIn("confirmed", requests[0][1])
            self.assertFalse(lock.locked())
        finally:
            await asyncio.to_thread(server.shutdown); server.server_close(); thread.join()


if __name__ == "__main__":
    unittest.main()
