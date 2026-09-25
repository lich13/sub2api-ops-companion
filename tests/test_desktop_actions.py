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

from app.desktop_actions import DesktopActions, PriorityRequest, TestRequest, billing_is_fresh


class DesktopActionContractTests(unittest.TestCase):
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
