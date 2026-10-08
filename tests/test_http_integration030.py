from __future__ import annotations

import asyncio
import copy
import json
import tempfile
import threading
import unittest
import urllib.request
from collections import deque
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch
from urllib.parse import urlsplit

from app.bark import BarkNotifier, _NoRedirectHandler
from app.capacity_alerts import CapacityAlerts, MESSAGES
from app.model_detection import DEFAULT_MODEL, TARGET, ModelDetection
from app.model_test_stream import execute
from app.model_tests import ModelTestRequest, ModelTests


NOW = datetime(2026, 10, 8, 3, tzinfo=timezone.utc)
NUMBERS = " ".join(str(i * 13 % 355 + 1) for i in range(80))


class _Clock:
    def __init__(self):
        self.value = NOW

    def __call__(self):
        return self.value

    def advance(self, seconds):
        self.value += timedelta(seconds=seconds)


class _LoopbackCapture:
    """Real HTTP endpoints with only synthetic model input and fixture keys."""

    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.model_statuses = deque()
        self.bark_replies = deque()
        capture = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                payload = json.loads(raw)
                with capture.lock:
                    capture.requests.append({
                        "path": self.path,
                        "payload": payload,
                        "authorization": self.headers.get("Authorization"),
                        "content_type": self.headers.get("Content-Type"),
                    })
                    if self.path == "/v1/responses":
                        status = capture.model_statuses.popleft() if capture.model_statuses else 200
                        if status == 200:
                            events = [
                                {"type": "response.output_text.delta", "output_index": 0,
                                 "content_index": 0, "delta": NUMBERS},
                                {"type": "response.completed", "response": {
                                    "status": "completed", "model": DEFAULT_MODEL,
                                    "output": [{"type": "message", "content": [
                                        {"type": "output_text", "text": NUMBERS}]}],
                                }},
                            ]
                            body = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
                            content_type = "text/event-stream"
                        else:
                            body = json.dumps({"error": {"message": "fixture invalid request"}}).encode()
                            content_type = "application/json"
                    elif self.path == "/push":
                        status, reply = capture.bark_replies.popleft() if capture.bark_replies else (200, {"code": 200})
                        body, content_type = json.dumps(reply).encode(), "application/json"
                    else:
                        status, body, content_type = 404, b"{}", "application/json"
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, _format, *_args):
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": .01}, daemon=True)

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def received(self, path):
        with self.lock:
            return copy.deepcopy([request for request in self.requests if request["path"] == path])

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class _Db:
    """Account/state fixture matching test_detection030; transport remains real."""

    def __init__(self, url):
        self.accounts = {1: {
            "id": 1, "name": "fixture-account", "platform": "openai", "type": "apikey",
            "status": "active", "schedulable": True, "deleted_at": None,
            "parent_account_id": None, "group_ids": [], "model_catalog_version": "fixture-catalog",
            "extra": {"openai_responses_mode": "force_responses"}, "proxy_id": None,
            "credentials": {"api_key": "fixturekey", "base_url": url},
        }}
        self.errors, self.usage = [], []

    def fetch_one(self, sql, params=None):
        if "coalesce(max(id)" in sql:
            return {"id": max((row["id"] for row in self.errors), default=0)}
        return copy.deepcopy(self.accounts.get((params or {}).get("id")))

    def fetch_all(self, sql, params=None):
        params = params or {}
        if "FROM usage_logs" in sql:
            return copy.deepcopy(self.usage)
        if "FROM ops_error_logs" in sql:
            rows = [row for row in self.errors if row["id"] > params.get("cursor", params.get("after", 0))]
            if "since" in params:
                rows = [row for row in rows if datetime.fromisoformat(row["created_at"]) >= params["since"]]
            return copy.deepcopy(sorted(rows, key=lambda row: row["id"])[:200])
        return []


class DetectionHttpIntegration030Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.tmp = self.enterContext(tempfile.TemporaryDirectory())
        self.http = self.enterContext(_LoopbackCapture())
        self.clock, self.db = _Clock(), _Db(self.http.url)
        self.settings = SimpleNamespace(
            usage_query_state_path=str(Path(self.tmp) / "usage.json"),
            audit_path=str(Path(self.tmp) / "audit.jsonl"),
            bark_enabled=True, bark_device_key="fixturekey", bark_server_url=self.http.url,
        )
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirectHandler())

        def loopback_open(request, *, timeout):
            self.assertEqual(urlsplit(request.full_url).hostname, "127.0.0.1")
            return opener.open(request, timeout=timeout)

        self.notifier = BarkNotifier(self.settings, urlopen=loopback_open)
        self.capacity = CapacityAlerts(self.settings, self.db, self.notifier, clock=self.clock)

        async def account(aid, *_args):
            return copy.deepcopy(self.db.accounts[aid])

        async def model_allowed(*_args, **_kwargs):
            return True

        self.service = SimpleNamespace(
            invalidate=Mock(), actions=SimpleNamespace(account=account, model_allowed=model_allowed),
            r=SimpleNamespace(
                db=self.db, settings=self.settings, capacity_alerts=self.capacity,
                bark_notifier=self.notifier,
                oauth_state_store=lambda: SimpleNamespace(admin_token=lambda: "fixturekey"),
                oauth_base_url=lambda: self.http.url,
                fingerprint_bank=SimpleNamespace(capture=lambda: ({"models": [{"id": TARGET}]}, "fixture-bank")),
            ),
        )
        self._create_services()
        self.serial = 0
        self.prediction, self.probability = DEFAULT_MODEL, .999

        def score(outputs, **_kwargs):
            self.assertTrue(outputs)
            self.assertTrue(all(output == NUMBERS for output in outputs))
            return {"prediction": self.prediction, "probability": self.probability, "used_outputs": len(outputs)}

        self.enterContext(patch("app.model_tests.analyze", side_effect=score))
        self.enterContext(patch("app.model_tests.challenges", return_value=[(80, NUMBERS)] * 3))
        self.stop_schedule = self.enterContext(patch(
            "app.key_fallback.execute_sub2api_set_schedulable", side_effect=self._stop_schedule))
        self.addAsyncCleanup(self._close_services)

    def _create_services(self):
        self.detection = ModelDetection(self.service, clock=self.clock)
        self.service.model_detection = self.detection
        self.tests = ModelTests(self.service)
        self.service.model_tests = self.tests
        self.assertIs(self.tests.execute, execute)

    async def _close_services(self):
        await self.tests.close()

    async def _restart(self):
        await self.tests.close()
        self._create_services()

    def _stop_schedule(self, aid, schedulable, **_kwargs):
        self.assertTrue(self.detection.mark(aid)["marked"])
        self.assertFalse(schedulable)
        self.assertEqual(self.http.received("/push"), [])
        self.db.accounts[aid]["schedulable"] = schedulable

    async def _start(self, *, automatic=True, causes=None, concurrency=1):
        self.serial += 1
        payload = ModelTestRequest(model_id=DEFAULT_MODEL, expected_version="a" * 64,
                                   request_id=f"fixture-http-{self.serial:08d}", concurrency=concurrency)
        metadata = None if not automatic else {
            "detection_generation": self.detection.control(1)["generation"],
            "detection_mark_version": self.detection.mark(1)["version"],
            "triggers": causes or ["scheduled"],
        }
        return await self.tests.start(1, payload, automatic=metadata)

    async def _finish(self, job):
        task = self.tests.tasks.get(job["id"])
        if task is not None:
            await asyncio.wait_for(task, timeout=8)
        return self.tests.get(job["id"])

    async def _queued(self, job):
        async def wait():
            while True:
                state = self.tests.get(job["id"])
                if state.get("waiting_until"):
                    return state
                self.assertEqual(state["status"], "queued")
                await asyncio.sleep(.01)

        return await asyncio.wait_for(wait(), timeout=3)

    def _outbox(self):
        return self.detection.data().get("disposition_notifications", {})

    def _collect_clues(self):
        self.capacity.poll()
        self.clock.advance(1)
        at = self.clock()
        self.db.errors.append({
            "id": 1, "account_id": 1, "created_at": at.isoformat(),
            "error_owner": "provider", "error_phase": "upstream", "requested_model": DEFAULT_MODEL,
            "model": DEFAULT_MODEL, "upstream_model": DEFAULT_MODEL, "upstream_error_message": MESSAGES[0],
            "upstream_status_code": 503, "account_name": "fixture-account", "account_platform": "openai",
            "account_type": "apikey", "account_deleted_at": None,
        })
        self.db.usage = [{
            "id": 20 - i, "account_id": 1, "created_at": (at - timedelta(seconds=i)).isoformat(),
            "model": DEFAULT_MODEL, "upstream_model": DEFAULT_MODEL, "stream": True,
            "first_token_ms": 11001 if i < 8 else 9000, "duration_ms": 15000, "output_tokens": 64,
            "inbound_endpoint": "/v1/responses", "image_count": 0, "image_output_tokens": 0,
            "video_count": 0, "video_duration_seconds": 0,
            "account_name": "fixture-account", "account_platform": "openai", "account_type": "apikey",
        } for i in range(10)]
        self.capacity.poll()

    def _put_clue(self, key):
        with self.capacity.store.transaction() as data:
            data["detection_events"][key] = {"account_id": 1, "created_at": self.clock().isoformat()}

    def _assert_model_requests(self, count):
        requests = self.http.received("/v1/responses")
        self.assertEqual(len(requests), count)
        for request in requests:
            self.assertEqual(request["authorization"], "Bearer fixturekey")
            self.assertEqual(request["payload"]["model"], DEFAULT_MODEL)
            self.assertIs(request["payload"]["stream"], True)
            self.assertIs(request["payload"]["store"], False)
            self.assertEqual(request["payload"]["input"], [
                {"role": "user", "content": [{"type": "input_text", "text": NUMBERS}]}])

    async def test_capacity_and_slow_clues_never_send_bark_directly(self):
        self._collect_clues()
        clues = self.capacity.store.snapshot()["detection_events"]
        self.assertEqual(set(clues), {"error:1", "slow:1:20"})
        self.capacity.deliver_due()
        self.detection.deliver_notifications()
        self.capacity.poll()
        self.assertEqual(self.capacity.store.snapshot()["detection_events"], clues)
        self._assert_model_requests(0)
        self.assertEqual(self.http.received("/push"), [])

    async def test_low_confidence_first_sample_stops_then_notifies_once_across_restart(self):
        self.prediction, self.probability = TARGET, .05
        result = await self._finish(await self._start(concurrency=3, causes=["error:1", "slow:1:20"]))
        self.assertEqual(result["status"], "completed")
        self.assertEqual(result["completion_reason"], "automatic_degradation")
        self.assertEqual(result["completed_groups"], 1)
        self.assertEqual(result["attempts"], 1)
        self.assertEqual(result["report"]["probability"], .05)
        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertFalse(self.db.accounts[1]["schedulable"])
        self.assertTrue(result["automatic_disposition"]["schedule_verified"])
        self.stop_schedule.assert_called_once()
        self._assert_model_requests(1)
        diagnostics = result["groups"][0]["diagnostics"]
        self.assertEqual(diagnostics["http_status"], 200)
        self.assertEqual(diagnostics["response_protocol"], "responses")
        self.assertEqual(diagnostics["events"], 2)
        self.assertEqual(self.http.received("/push"), [])

        self.detection.deliver_notifications()
        pushes = self.http.received("/push")
        self.assertEqual(len(pushes), 1)
        self.assertEqual(pushes[0]["content_type"], "application/json")
        payload = pushes[0]["payload"]
        self.assertEqual((payload["device_key"], payload["level"], payload["sound"]),
                         ("fixturekey", "critical", "alarm"))
        self.assertIn("5.00%", payload["body"])
        self.assertEqual(self._outbox()[result["id"]]["status"], "delivered")
        await self._restart()
        for key in ("error:1", "slow:1:20"):
            self._put_clue(key)
        await self.detection.tick()
        self.detection.deliver_notifications()
        self.detection.deliver_notifications()
        self._assert_model_requests(1)
        self.assertEqual(len(self.http.received("/push")), 1)

    async def test_normal_three_sample_result_neither_marks_nor_sends_bark(self):
        self.probability = .60
        result = await self._finish(await self._start())
        self.assertEqual(result["status"], "completed")
        self.assertEqual(result["completed_groups"], 3)
        self.assertFalse(self.detection.mark(1)["marked"])
        self.assertTrue(self.db.accounts[1]["schedulable"])
        self.stop_schedule.assert_not_called()
        self.detection.deliver_notifications()
        self._assert_model_requests(3)
        self.assertEqual(self._outbox(), {})
        self.assertEqual(self.http.received("/push"), [])

    async def test_bark_waits_for_schedule_readback(self):
        self.prediction, self.probability = TARGET, .05
        self.stop_schedule.side_effect = None
        result = await self._finish(await self._start())
        self.assertTrue(self.detection.mark(1)["marked"])
        self.assertTrue(self.db.accounts[1]["schedulable"])
        self.assertFalse(result["automatic_disposition"]["schedule_verified"])
        self.assertEqual(self._outbox(), {})
        self.detection.deliver_notifications()
        self.assertEqual(self.http.received("/push"), [])
        self.db.accounts[1]["schedulable"] = False
        disposition = self.detection.finish_disposition(1)
        self.assertTrue(disposition["schedule_verified"])
        self.stop_schedule.assert_called_once()
        self.detection.deliver_notifications()
        self._assert_model_requests(1)
        self.assertEqual(len(self.http.received("/push")), 1)

    async def test_cooldown_counts_real_http_and_releases_at_exact_300_seconds(self):
        first = await self._finish(await self._start())
        self.assertEqual(first["status"], "completed")
        self.clock.advance(299)
        for cause in ("scheduled", "error:1", "slow:1:20", "recovery", "credit"):
            self.assertIsNone(await self.detection.trigger(1, [cause]))
        job = await self._start(causes=["automatic-api"])
        queued = await self._queued(job)
        self.assertEqual(datetime.fromisoformat(queued["waiting_until"]), NOW + timedelta(seconds=300))
        self.assertEqual(self.tests.job_slots._value, 2)
        self.assertFalse(self.tests.waiting_leases[job["id"]].held)
        self._assert_model_requests(1)
        self.clock.advance(1)
        result = await self._finish(job)
        self.assertEqual(result["status"], "completed")
        self._assert_model_requests(2)
        self.assertEqual(self.http.received("/push"), [])

    async def test_manual_request_does_not_consume_or_extend_automatic_budget(self):
        await self._finish(await self._start())
        start = copy.deepcopy(self.detection.data()["automatic_starts"]["1"])
        self.clock.advance(1)
        result = await self._finish(await self._start(automatic=False))
        self.assertEqual(result["status"], "completed")
        self.assertEqual(self.detection.data()["automatic_starts"]["1"], start)
        self.assertIsNone(await self.detection.trigger(1, ["scheduled"]))
        self._assert_model_requests(2)

    async def test_failed_http_dispatch_still_cools_after_restart_and_result_pruning(self):
        self.http.model_statuses.append(400)
        result = await self._finish(await self._start())
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["error_code"], "invalid_model_or_request")
        self._assert_model_requests(1)
        with self.tests.store.transaction() as data:
            data.update(jobs={}, accounts={}, requests={})
        await self._restart()
        self.clock.advance(299)
        job = await self._start()
        await self._queued(job)
        self._assert_model_requests(1)
        self.clock.advance(1)
        result = await self._finish(job)
        self.assertEqual(result["status"], "completed")
        self._assert_model_requests(2)
        self.assertEqual(self.http.received("/push"), [])

    async def test_cooling_and_duplicate_clues_do_not_replay_after_restart(self):
        self._collect_clues()
        await self.detection.tick()
        job_id = self.detection.control(1)["job_id"]
        await self._finish({"id": job_id})
        await self.detection.tick()
        self._assert_model_requests(1)
        self.clock.advance(1)
        self._put_clue("error:2")
        await self.detection.tick()
        self.assertEqual(set(self.detection.data()["consumed"]), {"error:1", "slow:1:20", "error:2"})
        self.assertEqual(self.capacity.store.snapshot()["detection_events"], {})
        await self._restart()
        self.clock.advance(300)
        for key in ("error:1", "slow:1:20", "error:2"):
            self._put_clue(key)
        await self.detection.tick()
        self._assert_model_requests(1)
        self._put_clue("error:3")
        await self.detection.tick()
        await self._finish({"id": self.detection.control(1)["job_id"]})
        self._assert_model_requests(2)
        self.assertEqual(self.http.received("/push"), [])

    async def test_bark_response_failure_retries_once_when_due_after_restart(self):
        self.prediction, self.probability = TARGET, .05
        result = await self._finish(await self._start())
        self.http.bark_replies.extend([(200, {"code": 500}), (200, {"code": 200})])
        self.detection.deliver_notifications()
        pending = self._outbox()[result["id"]]
        self.assertEqual(pending["status"], "retry")
        self.assertEqual(pending["error_code"], "bark_response_code")
        self.assertEqual(pending["attempts"], 1)
        self.assertEqual(len(self.http.received("/push")), 1)
        await self._restart()
        self.clock.advance(4)
        self.detection.deliver_notifications()
        self.assertEqual(len(self.http.received("/push")), 1)
        self.clock.advance(1)
        self.detection.deliver_notifications()
        self.assertEqual(self._outbox()[result["id"]]["status"], "delivered")
        self.assertEqual(self._outbox()[result["id"]]["attempts"], 2)
        self.detection.deliver_notifications()
        self.assertEqual(len(self.http.received("/push")), 2)
        self._assert_model_requests(1)


if __name__ == "__main__":
    unittest.main()
