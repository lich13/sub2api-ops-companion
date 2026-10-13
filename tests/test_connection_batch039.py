"""Durable connection batches exercised against an isolated native SSE server."""
from __future__ import annotations

import asyncio
import copy
import json
import re
import tempfile
import threading
import unittest
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from fastapi import HTTPException
from pydantic import ValidationError

from app.account_locks import account_lock
from app.account_operations import AccountOperations, OperationRequest, TestBatchRequest
from app.desktop_actions import DesktopActions
from app.operation_versions import digest, versions


ADMIN_KEY = "fixture-admin-connection-039"
CLIENT_ID = "fixture-client-connection-039"
DEFAULT_MODEL = "gpt-5.6-luna"
BODY_MARKER = "fixture-model-response-must-not-be-retained-039"
BEARER_MARKER = "fixture-bearer-value-039"
PROVIDER_MARKER = "fixture-provider-value-039"


def _row(account_id: int, *, platform="openai", account_type="oauth", parent=None):
    return {
        "id": account_id,
        "name": f"fixture-account-{account_id}",
        "platform": platform,
        "type": account_type,
        "status": "active",
        "schedulable": True,
        "deleted_at": None,
        "parent_account_id": parent,
        "priority": 1,
        "concurrency": None,
        "load_factor": None,
        "proxy_id": None,
        "group_ids": [13],
        "updated_at": "fixture-updated-at",
        "extra": {"parent_account_id": parent} if parent else {},
        "model_mapping": {},
        "model_mapping_version": digest({}),
        "model_catalog_version": digest([account_id, "fixture-catalog"]),
        "credential_version": digest([account_id, "fixture-credential"]),
        "credential_settings_version": digest([account_id, "fixture-settings"]),
        "credentials": {"access_token": f"fixture-access-value-{account_id}"},
    }


class _FixtureHTTPServer(ThreadingHTTPServer):
    request_queue_size = 32


class _FixtureDB:
    def __init__(self, rows):
        self.rows = rows
        self.groups = {
            13: {"id": 13, "platform": "openai", "model_allowlist": {
                "enabled": True, "models": ["fixture-unrelated-allowed-model"],
            }},
        }

    def fetch_one(self, _sql, params):
        row = self.rows.get(int(params["id"]))
        return copy.deepcopy(row) if row and not row.get("deleted_at") else None

    def fetch_all(self, sql, params=None):
        ids = (params or {}).get("ids", self.rows)
        if "FROM groups" in sql:
            return [copy.deepcopy(self.groups[gid]) for gid in ids if gid in self.groups]
        return [copy.deepcopy(self.rows[aid]) for aid in ids
                if aid in self.rows and not self.rows[aid].get("deleted_at")]


class ConnectionBatch039Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="connection-batch-039-")
        self.rows = {aid: _row(aid) for aid in range(1, 17)}
        self.db = _FixtureDB(self.rows)
        self.scenarios = {}
        self.gates = {}
        self.calls = []
        self.active = set()
        self.active_snapshots = []
        self.peak = 0
        self.trace_lock = threading.Lock()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                with fixture.trace_lock:
                    fixture.calls.append({"method": "GET", "path": self.path})
                self.send_response(404)
                self.end_headers()

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
                match = re.fullmatch(r"/api/v1/admin/accounts/(\d+)/test", self.path)
                account_id = int(match[1]) if match else None
                with fixture.trace_lock:
                    fixture.calls.append({"method": "POST", "path": self.path,
                        "account_id": account_id, "body": body,
                        "admin_header_ok": self.headers.get("x-api-key") == ADMIN_KEY})
                    if account_id is not None:
                        fixture.active.add(account_id)
                        fixture.active_snapshots.append(set(fixture.active))
                        fixture.peak = max(fixture.peak, len(fixture.active))
                try:
                    if account_id is None:
                        self.send_response(404)
                        self.end_headers()
                        return
                    scenario = fixture.scenarios.get(account_id, "success")
                    if scenario == "disconnect":
                        self.close_connection = True
                        return
                    if scenario == "http_error":
                        self.send_response(503)
                        self.end_headers()
                        self.wfile.write(BODY_MARKER.encode())
                        return
                    row = fixture.rows[account_id]
                    requested = body.get("model_id", "")
                    actual = row["model_mapping"].get(requested, requested or "fixture-grok-native-model")
                    events = [
                        {"type": "test_start", "model": actual},
                        {"type": "content", "text": BODY_MARKER},
                        {"type": "status", "text": BODY_MARKER},
                        {"type": "image", "image_url": "data:image/png;base64," + BODY_MARKER},
                    ]
                    if scenario == "no_model":
                        events = [event for event in events if event["type"] != "test_start"]
                    prefix = b"".join(("data: " + json.dumps(event) + "\n\n").encode() for event in events)
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    if scenario == "truncated":
                        self.send_header("Content-Length", str(len(prefix) + 200))
                    self.end_headers()
                    self.wfile.write(prefix)
                    self.wfile.flush()
                    gate = fixture.gates.get(account_id)
                    if gate is not None:
                        gate.wait(10)
                    if scenario in {"no_terminal", "truncated"}:
                        self.close_connection = True
                        return
                    if scenario == "native_error":
                        terminal = {"type": "error", "error":
                            f"provider rejected: Bearer {BEARER_MARKER}; api_key={PROVIDER_MARKER}"}
                    else:
                        terminal = {"type": "test_complete", "success": scenario != "failure"}
                    self.wfile.write(("data: " + json.dumps(terminal) + "\n\n").encode())
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
                finally:
                    with fixture.trace_lock:
                        fixture.active.discard(account_id)

        self.server = _FixtureHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.server_thread = threading.Thread(target=self.server.serve_forever,
                                               kwargs={"poll_interval": 0.01}, daemon=True)
        self.server_thread.start()
        self.settings = SimpleNamespace(
            usage_query_state_path=str(Path(self.temp.name) / "oauth-state.json"),
            audit_path=str(Path(self.temp.name) / "audit.jsonl"),
            oauth_recovery_test_model_id=DEFAULT_MODEL,
            oauth_recovery_test_concurrency=2,
        )
        self.service = SimpleNamespace(
            r=SimpleNamespace(db=self.db, settings=self.settings, key_fallback_controller=None,
                oauth_monitor=None, oauth_state_store=lambda: SimpleNamespace(admin_token=lambda: ADMIN_KEY),
                oauth_base_url=lambda: f"http://127.0.0.1:{self.server.server_port}"),
            account_lock=lambda aid: account_lock(self.db, aid),
            authenticate=lambda *_args, **_kwargs: None,
            invalidate=lambda: None,
        )
        self.service.actions = DesktopActions(self.service)
        self.queue = AccountOperations(self.service)
        self.loop = None
        self.sequence = 0

    async def asyncTearDown(self):
        for gate in self.gates.values():
            gate.set()
        await self.stop_loop()
        await self.queue.close()
        await asyncio.to_thread(self.server.shutdown)
        self.server.server_close()
        await asyncio.to_thread(self.server_thread.join, 2)
        self.temp.cleanup()

    def request(self, account_ids, *, request_id=None, client_id=CLIENT_ID, expected=None, payload=None):
        self.sequence += 1
        expected = expected or {}
        return TestBatchRequest(action="test", payload={"confirmed": True} if payload is None else payload,
            request_id=request_id or f"fixture-batch-request-{self.sequence:04}", client_id=client_id,
            accounts=[{"account_id": aid, "expected_version": expected.get(aid,
                       versions(self.rows[aid])["test"] if aid in self.rows else "a" * 64)}
                      for aid in account_ids])

    async def submit(self, account_ids, **kwargs):
        return await self.queue.submit_batch(self.request(account_ids, **kwargs), ADMIN_KEY)

    async def start_loop(self):
        self.loop = asyncio.create_task(self.queue.loop())

    async def stop_loop(self):
        if self.loop is not None:
            self.loop.cancel()
            await asyncio.gather(self.loop, return_exceptions=True)
            self.loop = None

    async def wait_for(self, predicate, message, timeout=5):
        deadline = asyncio.get_running_loop().time() + timeout
        while asyncio.get_running_loop().time() < deadline:
            if predicate():
                return
            await asyncio.sleep(0.01)
        self.fail(message)

    async def settle(self, batch_id):
        await self.wait_for(lambda: all(item["status"] not in {"queued", "running", "checking"}
            for item in self.queue.listing(batch_id=batch_id)["items"]), "connection batch did not settle")
        return self.queue.listing(batch_id=batch_id)

    async def run_batch(self, batch):
        for item in batch["items"]:
            await self.queue.run(item["id"])
        return self.queue.listing(batch_id=batch["batch_id"])

    def assert_summary(self, item, *, requested=DEFAULT_MODEL, actual=DEFAULT_MODEL, success=True):
        result = item["result"]
        self.assertEqual(item["action"], "test")
        self.assertEqual(result["requested_model"], requested)
        self.assertEqual(result["actual_model"], actual)
        self.assertIs(result["success"], success)
        self.assertIsInstance(result["duration_ms"], int)
        self.assertGreaterEqual(result["duration_ms"], 0)
        self.assertIsNotNone(datetime.fromisoformat(result["completed_at"]).tzinfo)
        self.assertLessEqual(set(result), {"requested_model", "actual_model", "success", "duration_ms",
                                          "completed_at", "error", "uncertain"})

    def assert_native_only(self):
        for call in self.calls:
            self.assertEqual(call["method"], "POST")
            self.assertRegex(call["path"], r"^/api/v1/admin/accounts/\d+/test$")
            self.assertTrue(call["admin_header_ok"])
            self.assertNotIn("confirmed", call["body"])
            self.assertNotIn("expected_version", call["body"])

    async def test_acceptance_is_atomic_idempotent_and_order_independent(self):
        request = self.request([1, 2])
        first, duplicate = await asyncio.gather(self.queue.submit_batch(request, ADMIN_KEY),
                                               self.queue.submit_batch(request, ADMIN_KEY))
        reordered = await self.queue.submit_batch(request.model_copy(update={
            "accounts": list(reversed(request.accounts))}), ADMIN_KEY)
        self.assertEqual(first["batch_id"], duplicate["batch_id"])
        self.assertEqual(first["batch_id"], reordered["batch_id"])
        self.assertEqual(first["pending"], 2)
        state = self.queue.store.read()
        self.assertEqual(len(state["batches"]), 1)
        self.assertEqual(len(state["jobs"]), 2)
        self.assertEqual(len(state["batch_requests"]), 1)
        self.assertEqual({job["batch_id"] for job in state["jobs"].values()}, {first["batch_id"]})
        self.assertEqual(self.queue.store.path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.calls, [])
        for item in first["items"]:
            self.assertEqual(item["requested"], {"mode": "default", "model_id": DEFAULT_MODEL, "confirmed": True})

        before = self.queue.store.path.read_bytes()
        with patch("app.account_operations.write_json", side_effect=OSError("fixture acceptance save failure")):
            with self.assertRaisesRegex(OSError, "fixture acceptance save failure"):
                await self.submit([3, 4])
        self.assertEqual(self.queue.store.path.read_bytes(), before)
        self.assertEqual(self.calls, [])
        self.assertEqual(set(self.queue.keys), {item["id"] for item in first["items"]})

    async def test_request_id_conflicts_and_duplicate_accounts_do_not_add_jobs(self):
        request = self.request([1, 2])
        await self.queue.submit_batch(request, ADMIN_KEY)
        for changed in (self.request([1], request_id=request.request_id),
                        self.request([1, 2], request_id=request.request_id, client_id="fixture-other-client-039"),
                        self.request([1, 2], request_id=request.request_id, expected={1: "f" * 64})):
            with self.subTest(request=changed.model_dump(exclude={"client_id", "request_id"})):
                with self.assertRaises(HTTPException) as caught:
                    await self.queue.submit_batch(changed, ADMIN_KEY)
                self.assertEqual(caught.exception.status_code, 409)
        with self.assertRaises(HTTPException) as duplicate:
            await self.submit([3, 3])
        self.assertEqual(duplicate.exception.status_code, 422)
        self.assertEqual(len(self.queue.store.read()["jobs"]), 2)
        self.assertEqual(self.calls, [])

    async def test_single_and_batch_request_ids_share_conflict_boundary(self):
        single = OperationRequest(action="test", request_id="fixture-single-request-039", client_id=CLIENT_ID,
            expected_version=versions(self.rows[1])["test"], payload={"confirmed": True})
        await self.queue.submit(1, single, ADMIN_KEY)
        with self.assertRaises(HTTPException) as first:
            await self.submit([2], request_id=single.request_id)
        self.assertEqual(first.exception.status_code, 409)
        batch = self.request([2])
        await self.queue.submit_batch(batch, ADMIN_KEY)
        with self.assertRaises(HTTPException) as second:
            await self.queue.submit(3, single.model_copy(update={"request_id": batch.request_id,
                "expected_version": versions(self.rows[3])["test"]}), ADMIN_KEY)
        self.assertEqual(second.exception.status_code, 409)
        self.assertEqual(self.calls, [])

    async def test_confirmed_payload_is_required_and_strict(self):
        for payload in ({}, {"confirmed": "true"}, {"confirmed": 1},
                        {"confirmed": True, "model_id": "fixture-untrusted-model"}):
            with self.subTest(payload=payload), self.assertRaises(ValidationError):
                self.request([1], payload=payload)
        with self.assertRaises(HTTPException) as caught:
            await self.submit([1], payload={"confirmed": False})
        self.assertEqual(caught.exception.status_code, 422)
        self.assertEqual(self.queue.store.read()["batches"], {})

    async def test_one_thousand_boundary_and_complete_listing(self):
        self.rows.update({aid: _row(aid) for aid in range(17, 1002)})
        for ids in ([], list(range(1, 1002))):
            with self.subTest(size=len(ids)), self.assertRaises(ValidationError):
                self.request(ids)
        with self.assertRaises(ValidationError):
            self.request(["1"])
        batch = await self.submit(list(range(1, 1001)))
        self.assertEqual(len(batch["items"]), 1000)
        self.assertEqual(batch["pending"], 1000)
        cancelled = await self.queue.cancel_batch(batch["batch_id"])
        self.assertEqual(cancelled["pending"], 0)
        self.assertEqual(len(self.queue.listing(batch_id=batch["batch_id"])["items"]), 1000)
        self.assertEqual({item["status"] for item in cancelled["items"]}, {"cancelled"})
        self.assertEqual(self.calls, [])

    async def test_absent_allowlisted_model_still_dispatches_and_native_mapping_is_reported(self):
        self.rows[1]["model_mapping"] = {DEFAULT_MODEL: "fixture-native-mapped-model"}
        before_rows, before_groups = copy.deepcopy(self.rows), copy.deepcopy(self.db.groups)
        batch = await self.submit([1])
        final = await self.run_batch(batch)
        self.assertEqual(final["pending"], 0)
        self.assertEqual(final["items"][0]["status"], "completed")
        self.assert_summary(final["items"][0], actual="fixture-native-mapped-model")
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(self.calls[0]["body"]["model_id"], DEFAULT_MODEL)
        self.assertEqual(self.calls[0]["body"]["mode"], "default")
        self.assert_native_only()
        self.assertEqual(self.rows, before_rows)
        self.assertEqual(self.db.groups, before_groups)

    async def test_accepted_model_is_frozen_and_idempotent_after_configuration_change(self):
        self.settings.oauth_recovery_test_model_id = "fixture-accepted-model"
        request = self.request([1])
        batch = await self.queue.submit_batch(request, ADMIN_KEY)
        self.settings.oauth_recovery_test_model_id = "fixture-later-model"
        replay = await self.queue.submit_batch(request, ADMIN_KEY)
        self.assertEqual(batch, replay)
        first = (await self.run_batch(batch))["items"][0]
        second = (await self.run_batch(await self.submit([2])))["items"][0]
        self.assert_summary(first, requested="fixture-accepted-model", actual="fixture-accepted-model")
        self.assert_summary(second, requested="fixture-later-model", actual="fixture-later-model")
        self.assertEqual([call["body"]["model_id"] for call in self.calls],
                         ["fixture-accepted-model", "fixture-later-model"])

    async def test_missing_model_setting_uses_default(self):
        del self.settings.oauth_recovery_test_model_id
        final = await self.run_batch(await self.submit([1]))
        self.assert_summary(final["items"][0])
        self.assertEqual(self.calls[0]["body"]["model_id"], DEFAULT_MODEL)

    async def test_mixed_platform_types_missing_deleted_and_stale_items_are_isolated(self):
        self.rows[2] = _row(2, platform="grok", account_type="apikey")
        self.rows[3] = _row(3, platform="anthropic")
        self.rows[4]["deleted_at"] = "fixture-deleted"
        self.rows[5]["type"] = "fixture-unsupported"
        self.rows[7]["type"] = "apikey"
        final = await self.run_batch(await self.submit([1, 2, 3, 4, 5, 6, 7, 99], expected={6: "f" * 64}))
        items = {item["account_id"]: item for item in final["items"]}
        for aid in (1, 2, 7):
            self.assertEqual(items[aid]["status"], "completed")
        for aid in (3, 4, 5, 99):
            self.assertEqual(items[aid]["status"], "failed")
        self.assertEqual(items[6]["status"], "needs_confirmation")
        self.assertEqual(final["pending"], 1)
        self.assertEqual(items[2]["requested"], {"confirmed": True, "mode": "text", "model_id": ""})
        self.assert_summary(items[2], requested="", actual="fixture-grok-native-model")
        self.assertEqual([call["account_id"] for call in self.calls], [1, 2, 7])
        self.assertEqual(self.calls[1]["body"]["model_id"], "")
        self.assertEqual(self.calls[1]["body"]["mode"], "text")
        self.assert_native_only()

    async def test_version_change_before_dispatch_requires_confirmation(self):
        batch = await self.submit([1, 2])
        self.rows[1]["credential_version"] = digest("fixture-replaced-credential")
        self.rows[2]["extra"] = {"passive_usage": 42}
        self.rows[2]["updated_at"] = "fixture-new-observation"
        final = await self.run_batch(batch)
        items = {item["account_id"]: item for item in final["items"]}
        self.assertEqual(items[1]["status"], "needs_confirmation")
        self.assertEqual(items[1]["requested"]["model_id"], DEFAULT_MODEL)
        self.assertEqual(items[2]["status"], "completed")
        self.assertEqual([call["account_id"] for call in self.calls], [2])

    async def assert_concurrency_limit(self, configured, expected):
        if configured is None:
            del self.settings.oauth_recovery_test_concurrency
        else:
            self.settings.oauth_recovery_test_concurrency = configured
        ids = list(range(1, expected + 3))
        self.gates = {aid: threading.Event() for aid in ids}
        batch = await self.submit(ids)
        await self.start_loop()
        await self.wait_for(lambda: len(self.calls) == expected, "configured connection concurrency not reached")
        await asyncio.sleep(0.3)
        self.assertEqual(len(self.calls), expected)
        self.assertEqual(self.peak, expected)
        for gate in self.gates.values():
            gate.set()
        final = await self.settle(batch["batch_id"])
        non_completed = [
            {key: item.get(key) for key in ("account_id", "status", "reason", "result")}
            for item in final["items"] if item["status"] != "completed"
        ]
        self.assertEqual({item["status"] for item in final["items"]}, {"completed"}, non_completed)
        self.assertEqual(len(self.calls), len(ids))
        self.assertLessEqual(self.peak, expected)

    async def test_default_concurrency_is_two(self):
        await self.assert_concurrency_limit(None, 2)

    async def test_concurrency_is_clamped_to_one(self):
        await self.assert_concurrency_limit(0, 1)

    async def test_concurrency_is_clamped_to_eight(self):
        await self.assert_concurrency_limit(99, 8)

    async def test_global_eight_slots_include_existing_single_account_operations(self):
        self.settings.oauth_recovery_test_concurrency = 8
        self.gates = {aid: threading.Event() for aid in range(1, 10)}
        singles = []
        for aid in range(1, 8):
            singles.append(await self.queue.submit(aid, OperationRequest(action="test",
                request_id=f"fixture-single-global-{aid:04}", client_id=CLIENT_ID,
                expected_version=versions(self.rows[aid])["test"],
                payload={"confirmed": True, "model_id": "fixture-single-model"}), ADMIN_KEY))
        await self.start_loop()
        await self.wait_for(lambda: len(self.calls) == 7, "single operations did not occupy seven slots")
        batch = await self.submit([8, 9])
        await self.wait_for(lambda: len(self.calls) == 8, "batch did not occupy the remaining slot")
        await asyncio.sleep(0.3)
        self.assertEqual(len(self.calls), 8)
        self.assertEqual(self.peak, 8)
        for gate in self.gates.values():
            gate.set()
        await self.settle(batch["batch_id"])
        await self.wait_for(lambda: all(self.queue.get(item["id"])["status"] == "completed"
                                      for item in singles), "single operations did not complete")
        self.assertLessEqual(self.peak, 8)
        self.assertEqual(len(self.calls), 9)

    async def test_parent_siblings_and_same_account_never_overlap(self):
        self.settings.oauth_recovery_test_concurrency = 8
        self.rows[2] = _row(2, parent=1)
        self.rows[3] = _row(3, parent=1)
        self.gates = {1: threading.Event(), 4: threading.Event()}
        first = await self.submit([1, 2, 3, 4])
        second = await self.submit([4])
        await self.start_loop()
        await self.wait_for(lambda: len(self.calls) == 2, "independent account did not run alongside parent")
        await asyncio.sleep(0.3)
        self.assertEqual({call["account_id"] for call in self.calls}, {1, 4})
        self.assertEqual(len(self.calls), 2)
        for gate in self.gates.values():
            gate.set()
        await self.settle(first["batch_id"])
        await self.settle(second["batch_id"])
        self.assertEqual([call["account_id"] for call in self.calls].count(4), 2)
        for snapshot in self.active_snapshots:
            shared = []
            for aid in snapshot:
                shared.append({aid, self.rows[aid].get("parent_account_id")} - {None})
            for index, resources in enumerate(shared):
                for other in shared[index + 1:]:
                    self.assertTrue(resources.isdisjoint(other), "shared credential owners ran concurrently")

    async def test_dispatch_state_save_failure_sends_no_http_request(self):
        batch = await self.submit([1])
        item = batch["items"][0]
        with patch("app.account_operations.write_json", side_effect=OSError("fixture dispatch save failure")):
            await self.queue.run(item["id"])
        self.assertEqual(self.calls, [])
        self.assertEqual(self.queue.get(item["id"])["status"], "queued")

    async def test_cancel_batch_leaves_running_item_and_cancels_only_unstarted_items(self):
        self.settings.oauth_recovery_test_concurrency = 1
        self.gates[1] = threading.Event()
        batch = await self.submit([1, 2, 3], expected={3: "f" * 64})
        await self.start_loop()
        await self.wait_for(lambda: len(self.calls) == 1, "running fixture did not start")
        cancelled = await self.queue.cancel_batch(batch["batch_id"])
        items = {item["account_id"]: item for item in cancelled["items"]}
        self.assertEqual(items[1]["status"], "running")
        self.assertEqual(items[2]["status"], "cancelled")
        self.assertEqual(items[3]["status"], "cancelled")
        self.assertEqual(cancelled["pending"], 1)
        self.gates[1].set()
        final = await self.settle(batch["batch_id"])
        self.assertEqual(final["pending"], 0)
        self.assertEqual(final["items"][0]["status"], "completed")
        self.assertEqual(len(self.calls), 1)

    async def test_restart_resumes_queued_once_and_never_replays_dispatched(self):
        request = self.request([1, 2])
        batch = await self.queue.submit_batch(request, ADMIN_KEY)
        self.gates[2] = threading.Event()
        self.queue.launch(self.queue.store.read()["jobs"][batch["items"][1]["id"]])
        await self.wait_for(lambda: len(self.calls) == 1, "dispatched request did not reach native server")
        self.assertEqual(self.calls[0]["account_id"], 2)
        await self.queue.close()
        self.gates[2].set()
        await self.wait_for(lambda: 2 not in self.active, "native request did not release after shutdown")
        self.queue = AccountOperations(self.service)
        replay = await self.queue.submit_batch(request, ADMIN_KEY)
        self.assertEqual(batch["batch_id"], replay["batch_id"])
        self.assertEqual({item["id"] for item in batch["items"]}, {item["id"] for item in replay["items"]})
        final = await self.run_batch(replay)
        items = {item["account_id"]: item for item in final["items"]}
        self.assertEqual(items[1]["status"], "completed")
        self.assertEqual(items[2]["status"], "needs_confirmation")
        self.assertEqual([call["account_id"] for call in self.calls], [2, 1])
        await self.queue.cancel_batch(batch["batch_id"])
        self.assertEqual(self.queue.get(items[2]["id"])["status"], "needs_confirmation")
        await self.queue.run(items[2]["id"])
        self.assertEqual(len(self.calls), 2)

    async def test_clean_eof_truncated_stream_and_disconnect_are_uncertain_without_retry(self):
        self.scenarios.update({1: "no_terminal", 2: "truncated", 3: "disconnect"})
        batch = await self.submit([1, 2, 3])
        final = await self.run_batch(batch)
        for item in final["items"]:
            self.assertEqual(item["status"], "needs_confirmation")
            self.assertFalse(item["result"]["success"])
            self.assertTrue(item["result"]["uncertain"])
            self.assertTrue(item["result"]["error"])
            await self.queue.run(item["id"])
        self.assertEqual(len(self.calls), 3)
        await self.queue.close()
        self.queue = AccountOperations(self.service)
        await self.run_batch(batch)
        self.assertEqual(len(self.calls), 3)
        self.assertEqual(self.queue.listing(batch_id=batch["batch_id"])["pending"], 3)

    async def test_only_native_success_terminal_passes_and_actual_model_is_never_invented(self):
        self.scenarios.update({1: "failure", 2: "native_error", 3: "no_model", 4: "http_error"})
        final = await self.run_batch(await self.submit([1, 2, 3, 4]))
        items = {item["account_id"]: item for item in final["items"]}
        for aid in (1, 2, 4):
            self.assertFalse(items[aid]["result"]["success"])
            self.assertTrue(items[aid]["result"]["error"])
        self.assert_summary(items[3], actual=None)
        self.assertEqual(len(self.calls), 4)

    async def test_batch_retains_only_sanitized_summary_without_response_content(self):
        self.scenarios[2] = "native_error"
        final = await self.run_batch(await self.submit([1, 2]))
        self.assert_summary(final["items"][0])
        self.assert_summary(final["items"][1], success=False)
        public = json.dumps(final)
        persisted = self.queue.store.path.read_text()
        audit = Path(self.settings.audit_path).read_text()
        for marker in (ADMIN_KEY, BODY_MARKER, BEARER_MARKER, PROVIDER_MARKER, "fixture-access-value-"):
            for name, surface in (("public", public), ("state", persisted), ("audit", audit)):
                self.assertNotIn(marker, surface, name)
        for item in final["items"]:
            self.assertEqual(self.queue.get(item["id"])["events"], [])
            self.assertNotIn(item["id"], self.queue.events)
        self.assertIn("[REDACTED]", final["items"][1]["result"]["error"])

    async def test_single_account_test_keeps_existing_content_stream(self):
        job = await self.queue.submit(1, OperationRequest(action="test",
            request_id="fixture-single-compatibility-039", client_id=CLIENT_ID,
            expected_version=versions(self.rows[1])["test"],
            payload={"confirmed": True, "model_id": "fixture-single-model"}), ADMIN_KEY)
        await self.queue.run(job["id"])
        result = self.queue.get(job["id"])
        self.assertEqual(result["status"], "completed")
        self.assertTrue(result["result"]["success"])
        self.assertIn(BODY_MARKER, [event.get("text") for event in result["events"] if event["type"] == "content"])
        self.assertEqual(self.calls[0]["body"]["model_id"], "fixture-single-model")
        self.assertNotIn(BODY_MARKER, self.queue.store.path.read_text())


if __name__ == "__main__":
    unittest.main()
