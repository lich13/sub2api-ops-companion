"""Desktop HTTP routes backed by the durable queue and an isolated native SSE server."""
from __future__ import annotations

import asyncio
import copy
import json
import re
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import httpx
from fastapi import FastAPI, HTTPException

from app.desktop_api import install_desktop_api
from app.operation_versions import digest, versions


PREFIX = "/api/desktop/v1/account-operations"
ADMIN_KEY = "fixture-http-admin-039"
CLIENT_ID = "fixture-http-client-039"
MODEL = "fixture-connection-model"
CONTENT = "fixture-native-content-must-not-be-retained-039"


class _Database:
    def __init__(self):
        self.rows = {}
        for aid, platform, kind, proxy in (
            (1, "openai", "oauth", None),
            (2, "grok", "apikey", 17),
            (3, "openai", "apikey", 29),
        ):
            self.rows[aid] = {
                "id": aid, "name": f"fixture-account-{aid}", "platform": platform,
                "type": kind, "status": "active", "schedulable": True,
                "deleted_at": None, "parent_account_id": None, "proxy_id": proxy,
                "priority": 1, "concurrency": None, "load_factor": None,
                "group_ids": [], "extra": {}, "updated_at": "fixture-version-1",
                "model_mapping": {}, "model_mapping_version": digest({}),
                "model_catalog_version": digest([aid, "fixture-catalog"]),
                "credential_version": digest([aid, "fixture-credential"]),
                "credential_settings_version": digest([aid, "fixture-settings"]),
                "credentials": {"access_token": f"fixture-access-{aid}"},
            }

    def fetch_one(self, _sql, params):
        return copy.deepcopy(self.rows.get(params["id"]))

    def fetch_all(self, _sql, params=None):
        return [copy.deepcopy(self.rows[aid]) for aid in (params or {}).get("ids", self.rows)
                if aid in self.rows]


class _NativeFixture:
    def __init__(self):
        self.calls = []
        self.gates = {}
        self.no_terminal = set()
        self.lock = threading.Lock()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                match = re.fullmatch(r"/api/v1/admin/accounts/(\d+)/test", self.path)
                if match is None:
                    self.send_response(404)
                    self.end_headers()
                    return
                aid = int(match[1])
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
                authorized = self.headers.get("x-api-key") == ADMIN_KEY
                with fixture.lock:
                    fixture.calls.append({"account_id": aid, "body": body, "authorized": authorized})
                self.send_response(200 if authorized else 401)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                if not authorized:
                    return
                try:
                    for event in (
                        {"type": "test_start", "model": body.get("model_id") or "fixture-grok-model"},
                        {"type": "content", "text": CONTENT},
                    ):
                        self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
                    self.wfile.flush()
                    gate = fixture.gates.get(aid)
                    if gate is not None:
                        gate.wait(5)
                    if aid in fixture.no_terminal:
                        return
                    self.wfile.write(b'data: {"type":"test_complete","success":true}\n\n')
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever,
                                       kwargs={"poll_interval": .01}, name="fixture-batch-http039")
        self.thread.start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def received(self):
        with self.lock:
            return copy.deepcopy(self.calls)

    def release(self):
        for gate in self.gates.values():
            gate.set()

    def close(self):
        self.release()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class ConnectionBatchHttp039Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = self.enterContext(tempfile.TemporaryDirectory(prefix="connection-batch-http039-"))
        self.native = _NativeFixture()
        self.addAsyncCleanup(self.close_native)
        self.db = _Database()
        settings = SimpleNamespace(
            usage_query_state_path=str(Path(self.temp) / "oauth-state.json"),
            audit_path=str(Path(self.temp) / "audit.jsonl"),
            oauth_recovery_test_model_id=MODEL, oauth_recovery_test_concurrency=2,
        )
        runtime = SimpleNamespace(
            db=self.db, settings=settings, APP_VERSION="fixture-0.1.39",
            key_fallback_controller=None, oauth_monitor=None,
            oauth_base_url=lambda: self.native.url,
            oauth_state_store=lambda: SimpleNamespace(admin_token=lambda: ADMIN_KEY),
        )
        app = FastAPI()
        self.service = install_desktop_api(app, runtime)
        self.addAsyncCleanup(self.service.close)

        def authenticate(key, *, fresh=False):
            if key != ADMIN_KEY:
                raise HTTPException(401, "fixture administrator authorization required")

        self.service.authenticate = Mock(side_effect=authenticate)
        self.queue = self.service.operations
        self.client = httpx.AsyncClient(transport=httpx.ASGITransport(app=app),
                                       base_url="http://127.0.0.1", trust_env=False)
        self.addAsyncCleanup(self.client.aclose)
        self.headers = {"x-api-key": ADMIN_KEY}
        self.sequence = 0

    async def close_native(self):
        await asyncio.to_thread(self.native.close)
        self.assertFalse(self.native.thread.is_alive(), "fixture HTTP listener did not stop")
        self.assertEqual(self.native.server.fileno(), -1)

    async def asyncTearDown(self):
        self.native.release()

    def body(self, account_ids=(1, 2, 3), *, action="test", request_id=None):
        self.sequence += 1
        return {
            "action": action,
            "payload": {"confirmed": True} if action == "test" else {"proxy_id": None},
            "accounts": [{"account_id": aid, "expected_version": versions(self.db.rows[aid])[action]}
                         for aid in account_ids],
            "request_id": request_id or f"fixture-http-request-{self.sequence:04}",
            "client_id": CLIENT_ID,
        }

    def decoded(self, response, status):
        self.assertEqual(response.status_code, status, response.text)
        self.assertEqual(response.headers.get("cache-control"), "no-store")
        self.assertRegex(response.headers.get("x-request-id", ""), r"^[a-f0-9]{32}$")
        return response.json()

    async def submit(self, body):
        return self.decoded(await self.client.post(PREFIX + "/batch", headers=self.headers, json=body), 202)

    async def listing(self, batch_id):
        return self.decoded(await self.client.get(PREFIX, headers=self.headers,
                                                  params={"batch_id": batch_id}), 200)

    async def cancel(self, batch_id):
        return self.decoded(await self.client.post(f"{PREFIX}/batches/{batch_id}/cancel",
                                                   headers=self.headers), 200)

    async def cancel_job(self, job_id):
        return self.decoded(await self.client.post(f"{PREFIX}/{job_id}/cancel",
                                                   headers=self.headers), 200)

    async def restart_queue(self):
        await self.queue.close()
        self.service._operations = None
        self.queue = self.service.operations

    async def wait_for_native(self):
        async def wait():
            while not self.native.received():
                await asyncio.sleep(.01)
        await asyncio.wait_for(wait(), timeout=3)

    async def test_test_batch_is_202_idempotent_and_readable_after_queue_restart(self):
        body = self.body()
        reversed_body = copy.deepcopy(body)
        reversed_body["accounts"].reverse()
        responses = await asyncio.gather(
            self.client.post(PREFIX + "/batch", headers=self.headers, json=body),
            self.client.post(PREFIX + "/batch", headers=self.headers, json=reversed_body),
        )
        batch, duplicate = [self.decoded(response, 202) for response in responses]
        self.assertEqual(batch, duplicate)
        self.assertEqual(batch["pending"], 3)
        self.assertEqual({item["account_id"] for item in batch["items"]}, {1, 2, 3})
        self.assertTrue(all(item["action"] == "test" and item["status"] == "queued"
                            for item in batch["items"]))
        items = {item["account_id"]: item for item in batch["items"]}
        self.assertEqual(items[1]["requested"], {"confirmed": True, "mode": "default", "model_id": MODEL})
        self.assertEqual(items[2]["requested"], {"confirmed": True, "mode": "text", "model_id": ""})
        self.assertEqual(items[3]["requested"], items[1]["requested"])
        state = self.queue.store.read()
        self.assertEqual(len(state["jobs"]), 3)
        self.assertEqual(len(state["batches"]), 1)
        self.assertEqual(state["batch_requests"], {body["request_id"]: batch["batch_id"]})
        self.assertEqual(await self.listing(batch["batch_id"]), batch)

        await self.restart_queue()
        self.assertEqual(await self.listing(batch["batch_id"]), batch)
        self.assertEqual(await self.submit(body), batch)
        self.assertEqual(len(self.queue.store.read()["jobs"]), 3)
        self.assertEqual(self.native.received(), [])
        for marker in (ADMIN_KEY, "fixture-access-"):
            self.assertNotIn(marker, self.queue.store.path.read_text())
            self.assertNotIn(marker, json.dumps(batch))

    async def test_request_id_conflicts_do_not_accept_changed_or_other_action_batches(self):
        body = self.body((1,))
        batch = await self.submit(body)
        before = self.queue.store.path.read_bytes()
        changed = self.body((2,), request_id=body["request_id"])
        proxy = self.body((1,), action="proxy", request_id=body["request_id"])
        for candidate in (changed, proxy):
            with self.subTest(action=candidate["action"]):
                response = await self.client.post(PREFIX + "/batch", headers=self.headers, json=candidate)
                self.assertEqual(self.decoded(response, 409)["detail"]["code"], "idempotency_conflict")
                self.assertEqual(self.queue.store.path.read_bytes(), before)
        self.assertEqual(await self.listing(batch["batch_id"]), batch)
        self.assertEqual(self.native.received(), [])

    async def test_submit_list_and_cancel_require_admin_authorization(self):
        batch = await self.submit(self.body((1,)))
        before = self.queue.store.path.read_bytes()
        requests = (
            ("POST", PREFIX + "/batch", {"json": self.body((2,))}),
            ("GET", PREFIX, {"params": {"batch_id": batch["batch_id"]}}),
            ("POST", f"{PREFIX}/batches/{batch['batch_id']}/cancel", {}),
            ("POST", f"{PREFIX}/{batch['items'][0]['id']}/cancel", {}),
        )
        for headers in ({}, {"x-api-key": "fixture-invalid-admin"}):
            for method, path, kwargs in requests:
                with self.subTest(method=method, path=path, missing=not headers):
                    response = await self.client.request(method, path, headers=headers, **kwargs)
                    self.decoded(response, 401)
                    self.service.authenticate.assert_called_with(headers.get("x-api-key", ""), fresh=method != "GET")
                    self.assertEqual(self.queue.store.path.read_bytes(), before)
        self.assertEqual(self.native.received(), [])

    async def test_forbidden_and_mixed_action_payloads_are_rejected_without_persistence(self):
        valid = self.body((1,))
        cases = []
        for field, value in (("model_id", "fixture-model"), ("mode", "image"),
                             ("prompt", CONTENT), ("proxy_id", 17), ("confirmed", "true")):
            body = copy.deepcopy(valid)
            body["payload"][field] = value
            cases.append((f"test payload {field}", body))
        body = copy.deepcopy(valid)
        body["upstream_url"] = "https://example.invalid"
        cases.append(("unknown top-level field", body))
        body = copy.deepcopy(valid)
        body["accounts"][0]["payload"] = {"model_id": "fixture-model"}
        cases.append(("per-account payload", body))
        body = self.body((1,), action="proxy")
        body["payload"]["confirmed"] = True
        cases.append(("proxy with test field", body))
        before = self.queue.store.path.read_bytes()
        for label, body in cases:
            with self.subTest(case=label):
                response = await self.client.post(PREFIX + "/batch", headers=self.headers, json=body)
                self.assertEqual(self.decoded(response, 422)["detail"]["code"], "invalid_request")
                self.assertNotIn(CONTENT, response.text)
                self.assertEqual(self.queue.store.path.read_bytes(), before)
        self.assertEqual(self.native.received(), [])

    async def test_proxy_batch_still_uses_existing_union_branch_and_cannot_be_test_cancelled(self):
        batch = await self.submit(self.body(action="proxy"))
        items = {item["account_id"]: item for item in batch["items"]}
        self.assertTrue(all(item["action"] == "proxy" and item["requested"] == {"proxy_id": None}
                            for item in items.values()))
        self.assertEqual(items[1]["status"], "completed")
        self.assertEqual([items[aid]["status"] for aid in (2, 3)], ["queued", "queued"])
        self.assertEqual(batch["pending"], 2)
        self.assertEqual(await self.listing(batch["batch_id"]), batch)
        before = self.queue.store.path.read_bytes()
        response = await self.client.post(f"{PREFIX}/batches/{batch['batch_id']}/cancel", headers=self.headers)
        self.decoded(response, 404)
        self.assertEqual(self.queue.store.path.read_bytes(), before)
        self.assertEqual(self.native.received(), [])

    async def test_cancel_stops_undispatched_items_and_allows_running_native_test_to_finish(self):
        self.native.gates[1] = threading.Event()
        body = self.body()
        body["accounts"][2]["expected_version"] = "a" * 64
        batch = await self.submit(body)
        items = {item["account_id"]: item for item in batch["items"]}
        self.assertEqual(items[3]["status"], "needs_confirmation")
        self.queue.launch(self.queue.store.read()["jobs"][items[1]["id"]])
        task = self.queue.tasks[items[1]["id"]]
        await self.wait_for_native()
        cancelled = await self.cancel(batch["batch_id"])
        states = {item["account_id"]: item for item in cancelled["items"]}
        self.assertEqual(states[1]["status"], "running")
        self.assertTrue(states[1]["started_at"])
        self.assertEqual([states[aid]["status"] for aid in (2, 3)], ["cancelled", "cancelled"])
        self.assertEqual(cancelled["pending"], 1)
        self.assertEqual(await self.listing(batch["batch_id"]), cancelled)
        self.native.gates[1].set()
        await asyncio.wait_for(asyncio.shield(task), timeout=3)
        finished = await self.listing(batch["batch_id"])
        self.assertEqual(finished["pending"], 0)
        self.assertEqual(finished["items"][0]["status"], "completed")
        result = finished["items"][0]["result"]
        self.assertIs(result["success"], True)
        self.assertEqual((result["requested_model"], result["actual_model"]), (MODEL, MODEL))
        self.assertGreaterEqual(result["duration_ms"], 0)
        for item in finished["items"]:
            await self.queue.run(item["id"])
        calls = self.native.received()
        self.assertEqual([call["account_id"] for call in calls], [1])
        self.assertTrue(calls[0]["authorized"])
        self.assertEqual(calls[0]["body"]["model_id"], MODEL)
        self.assertEqual(await self.cancel(batch["batch_id"]), finished)
        detail = self.decoded(await self.client.get(f"{PREFIX}/{items[1]['id']}", headers=self.headers), 200)
        self.assertEqual(detail["events"], [])
        self.assertNotIn(CONTENT, json.dumps(finished))
        self.assertNotIn(CONTENT, self.queue.store.path.read_text())
        await self.restart_queue()
        self.assertEqual(await self.listing(batch["batch_id"]), finished)

    async def test_generic_cancel_cannot_interrupt_dispatched_batch_item(self):
        self.native.gates[1] = threading.Event()
        batch = await self.submit(self.body((1,)))
        jid = batch["items"][0]["id"]
        self.queue.launch(self.queue.store.read()["jobs"][jid])
        task = self.queue.tasks[jid]
        await self.wait_for_native()
        running = self.decoded(await self.client.get(f"{PREFIX}/{jid}", headers=self.headers), 200)
        self.assertEqual(running["status"], "running")
        self.assertTrue(running["started_at"])
        before = self.queue.store.path.read_bytes()

        self.assertEqual(await self.cancel_job(jid), running)
        self.assertEqual(self.queue.store.path.read_bytes(), before)
        self.assertFalse(task.done())
        self.assertEqual((await self.listing(batch["batch_id"]))["pending"], 1)

        self.native.gates[1].set()
        await asyncio.wait_for(asyncio.shield(task), timeout=3)
        finished = await self.listing(batch["batch_id"])
        self.assertEqual(finished["pending"], 0)
        self.assertEqual(finished["items"][0]["status"], "completed")
        self.assertIs(finished["items"][0]["result"]["success"], True)
        self.assertEqual([call["account_id"] for call in self.native.received()], [1])

    async def test_generic_cancel_preserves_dispatched_unknown_result_after_restart(self):
        self.native.no_terminal.add(1)
        batch = await self.submit(self.body((1,)))
        jid = batch["items"][0]["id"]
        await self.queue.run(jid)
        uncertain = self.decoded(await self.client.get(f"{PREFIX}/{jid}", headers=self.headers), 200)
        self.assertEqual(uncertain["status"], "needs_confirmation")
        self.assertTrue(uncertain["started_at"])
        self.assertIs(uncertain["result"]["uncertain"], True)
        self.assertIs(uncertain["result"]["success"], False)
        before = self.queue.store.path.read_bytes()

        self.assertEqual(await self.cancel_job(jid), uncertain)
        self.assertEqual(self.queue.store.path.read_bytes(), before)
        await self.restart_queue()
        self.assertEqual(await self.cancel_job(jid), uncertain)
        await self.queue.run(jid)
        retained = await self.listing(batch["batch_id"])
        self.assertEqual(retained["pending"], 1)
        self.assertEqual(retained["items"][0]["status"], "needs_confirmation")
        self.assertEqual(retained["items"][0]["result"], uncertain["result"])
        self.assertEqual([call["account_id"] for call in self.native.received()], [1])

    async def test_cancelled_batch_item_rejects_late_needs_confirmation_update(self):
        batch = await self.submit(self.body((1,)))
        jid = batch["items"][0]["id"]
        reading, release = asyncio.Event(), asyncio.Event()
        read_account = self.queue.read_account

        async def delayed_account(aid):
            reading.set()
            await release.wait()
            return await read_account(aid)

        with patch.object(self.queue, "read_account", side_effect=delayed_account):
            self.queue.launch(self.queue.store.read()["jobs"][jid])
            task = self.queue.tasks[jid]
            try:
                await asyncio.wait_for(reading.wait(), timeout=3)
                cancelled = await self.cancel(batch["batch_id"])
                self.assertEqual(cancelled["items"][0]["status"], "cancelled")
                self.assertEqual(cancelled["pending"], 0)
                self.db.rows[1]["credential_version"] = digest([1, "fixture-changed-credential"])
            finally:
                release.set()
            await asyncio.wait_for(asyncio.shield(task), timeout=3)

        self.assertEqual(await self.listing(batch["batch_id"]), cancelled)
        self.assertEqual(self.native.received(), [])
        await self.restart_queue()
        self.assertEqual(await self.listing(batch["batch_id"]), cancelled)

    async def test_account_busy_requeue_clears_dispatch_time_and_remains_cancellable(self):
        batch = await self.submit(self.body((1,)))
        jid = batch["items"][0]["id"]
        lock = self.service.account_lock(1)
        self.assertTrue(lock.acquire(blocking=False))
        try:
            await self.queue.run(jid)
        finally:
            lock.release()
        waiting = (await self.listing(batch["batch_id"]))["items"][0]
        self.assertEqual(waiting["status"], "queued")
        self.assertIn("started_at", waiting)
        self.assertIsNone(waiting["started_at"])
        self.assertIsNone(self.queue.store.read()["jobs"][jid]["started_at"])
        cancelled = await self.cancel(batch["batch_id"])
        self.assertEqual(cancelled["pending"], 0)
        self.assertEqual(cancelled["items"][0]["status"], "cancelled")
        await self.queue.run(jid)
        await self.restart_queue()
        self.assertEqual(await self.listing(batch["batch_id"]), cancelled)
        self.assertEqual(self.native.received(), [])

    async def test_unknown_and_malformed_batch_ids_return_404_without_changes(self):
        before = self.queue.store.path.read_bytes()
        for bid in ("f" * 32, "fixture-invalid-batch"):
            with self.subTest(batch_id=bid):
                self.decoded(await self.client.get(PREFIX, headers=self.headers, params={"batch_id": bid}), 404)
                self.decoded(await self.client.post(f"{PREFIX}/batches/{bid}/cancel", headers=self.headers), 404)
                self.assertEqual(self.queue.store.path.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
