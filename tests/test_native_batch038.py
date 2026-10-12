"""Black-box native account batch coverage for the 0.1.38 coordinator.

The fixture uses a real loopback HTTP server and the production async client so
the tests observe request bodies, request counts, readback, and timeout
boundaries without contacting a Sub2API deployment.
"""

from __future__ import annotations

import asyncio
import copy
import json
import tempfile
import threading
import time
import unittest
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from fastapi import HTTPException
from pydantic import ValidationError

from app.account_locks import account_lock
from app.account_operations import AccountBatchRequest, AccountOperations, ProxyBatchRequest
from app.account_templates import TemplateBatchRequest, account_version
from app.desktop_actions import DesktopActions
from app.operation_versions import digest, versions


ADMIN_KEY = "fixture-admin-key"
CLIENT_ID = "fixture-native-client-038"
VERSION = "a" * 64


def _row(account_id: int, *, proxy_id=None, groups=None, parent=None, platform="openai") -> dict:
    mapping = {"fixture-model": "fixture-model"}
    row = {
        "id": account_id,
        "name": f"fixture-{account_id}",
        "platform": platform,
        "type": "apikey",
        "deleted_at": None,
        "parent_account_id": parent,
        "model_mapping": copy.deepcopy(mapping),
        "model_mapping_version": digest(mapping),
        "credential_version": digest({"credential": account_id}),
        "credential_settings_version": digest({"settings": account_id}),
        "model_catalog_version": digest({"catalog": account_id}),
        "extra": {},
        "status": "active",
        "schedulable": True,
        "priority": 1,
        "concurrency": None,
        "load_factor": None,
        "proxy_id": proxy_id,
        "group_ids": list(groups or []),
    }
    if parent:
        row["extra"] = {"parent_account_id": parent}
    return row


class _FixtureDB:
    def __init__(self, rows: dict[int, dict]):
        self.rows = rows
        self.groups = {gid: {"id": gid, "platform": "openai"} for gid in range(1, 401)}

    def fetch_one(self, _sql, params):
        row = self.rows.get(int(params["id"]))
        if not row or row.get("deleted_at"):
            return None
        return copy.deepcopy(row)

    def fetch_all(self, sql, params=None):
        params = params or {}
        ids = {int(value) for value in params.get("ids", [])}
        if "FROM groups" in sql:
            return [copy.deepcopy(self.groups[gid]) for gid in sorted(ids) if gid in self.groups]
        if "parent_account_id=ANY" in sql or "nullif(to_jsonb(a)" in sql:
            return [
                {"id": row["id"], "parent_account_id": row.get("parent_account_id")}
                for row in self.rows.values()
                if not row.get("deleted_at") and row.get("parent_account_id") in ids
            ]
        selected = ids or set(self.rows)
        return [copy.deepcopy(self.rows[aid]) for aid in sorted(selected)
                if aid in self.rows and not self.rows[aid].get("deleted_at")]


class _TemplateReader:
    def __init__(self, fixture):
        self.fixture = fixture
        self.target = {"fixture-model": "fixture-model"}
        self.profile = {"whitelist": ["fixture-model"], "mappings": []}

    def account(self, aid):
        row = self.fixture.rows.get(int(aid))
        return copy.deepcopy(row) if row and not row.get("deleted_at") else None

    def accounts(self, ids):
        return {
            int(aid): copy.deepcopy(self.fixture.rows[int(aid)])
            for aid in ids
            if int(aid) in self.fixture.rows and not self.fixture.rows[int(aid)].get("deleted_at")
        }

    def desired(self, _payload):
        version = getattr(_payload, "template_version", None)
        if version is None and isinstance(_payload, dict):
            version = _payload.get("template_version")
        if version not in {digest(self.profile), digest(self.view())}:
            raise HTTPException(409, "模板内容已变化，请重新选择")
        return copy.deepcopy(self.target)

    def view(self):
        return {
            "configured": True,
            "version": digest(self.profile),
            "templates": {"full": copy.deepcopy(self.profile)},
            "template_versions": {"full": digest(self.profile)},
        }

    def fingerprint(self, aid):
        return account_version(self.account(aid) or {})


class _ProxySettings:
    def __init__(self, fixture):
        self.fixture = fixture

    async def proxies(self, _key):
        return copy.deepcopy(self.fixture.proxies)


class _Controller:
    def __init__(self, managed=()):
        self._lock = threading.RLock()
        self.config = SimpleNamespace(
            valid=True,
            managed_account_ids=set(managed),
            openai_enabled=True,
            grok_enabled=True,
            config_version=1,
        )
        self.writes = []

    def load_config(self):
        return copy.deepcopy(self.config)

    def _write_config_unlocked(self, **changes):
        self.writes.append(copy.deepcopy(changes))
        for key, value in changes.items():
            if key == "managed_account_ids":
                value = set(value)
            setattr(self.config, key, value)


class _Actions(DesktopActions):
    def __init__(self, service, fixture):
        super().__init__(service)
        self.fixture = fixture

    async def accounts(self, ids):
        return {
            int(aid): copy.deepcopy(self.fixture.rows[int(aid)])
            for aid in ids
            if int(aid) in self.fixture.rows and not self.fixture.rows[int(aid)].get("deleted_at")
        }

    def client(self, key, timeout=30):
        # Native sends use production httpx.AsyncClient against the loopback
        # receiver, with a short fixture timeout for the timeout boundary tests.
        return super().client(key, min(timeout, self.fixture.http_timeout))


class _Receiver:
    def __init__(self, fixture):
        self.fixture = fixture
        self.calls = []
        self.mode = None
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), self._handler())
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def _handler(self):
        fixture = self.fixture
        receiver = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                receiver.handle(self, "POST")

            def do_PUT(self):
                receiver.handle(self, "PUT")

            def do_DELETE(self):
                receiver.handle(self, "DELETE")

        return Handler

    def close(self):
        self.server.shutdown()
        self.server.server_close()

    def _read(self, request):
        length = int(request.headers.get("Content-Length", "0"))
        raw = request.rfile.read(length) if length else b"{}"
        try:
            return json.loads(raw)
        except ValueError:
            return None

    def _response(self, request, status, body):
        data = json.dumps(body).encode()
        request.send_response(status)
        request.send_header("Content-Type", "application/json")
        request.send_header("Content-Length", str(len(data)))
        request.end_headers()
        try:
            request.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def _ids(self, body, path):
        if isinstance(body, dict) and isinstance(body.get("account_ids"), list):
            return [int(value) for value in body["account_ids"] if type(value) is int]
        if path.startswith("/api/v1/admin/accounts/"):
            try:
                return [int(path.rsplit("/", 1)[-1])]
            except ValueError:
                return []
        return []

    def _apply(self, path, method, body, ids):
        fixture = self.fixture
        if path.endswith("batch-delete") or method == "DELETE":
            for aid in ids:
                if aid in fixture.rows:
                    fixture.rows[aid]["deleted_at"] = "fixture-deleted"
            return
        target = body or {}
        if "credentials" in target:
            mapping = copy.deepcopy(target["credentials"].get("model_mapping"))
            for aid in ids:
                fixture.rows[aid]["model_mapping"] = copy.deepcopy(mapping)
                fixture.rows[aid]["model_mapping_version"] = digest(mapping)
            return
        if "proxy_id" in target:
            value = target["proxy_id"] or None
            for aid in ids:
                fixture.rows[aid]["proxy_id"] = value
            return
        if "group_ids" in target:
            for aid in ids:
                fixture.rows[aid]["group_ids"] = list(target["group_ids"])

    def handle(self, request, method):
        path = request.path.split("?", 1)[0]
        body = self._read(request)
        ids = self._ids(body, path)
        self.calls.append({"method": method, "path": path, "body": copy.deepcopy(body), "ids": ids})

        if self.mode == "generic-404":
            self._response(request, 404, {"detail": "account does not exist"})
            return
        if self.mode == "route-404" and path.endswith("accounts/bulk-update"):
            self._response(request, 404, {"detail": "Not Found"})
            return

        if method == "POST" and path.endswith("bulk-update") or method == "POST" and path.endswith("batch-delete"):
            failed = set(self.fixture.partial_failed) if self.mode == "partial" else set()
            if self.mode == "malformed-result":
                self._response(request, 200, {"code": 0, "data": {
                    "success": len(ids), "failed": 0, "success_ids": ids, "failed_ids": [],
                    "total": len(ids),
                    "results": [{"account_id": str(aid), "success": True} for aid in ids],
                    "errors": [],
                }})
                return
            if self.mode == "malformed-total":
                self._response(request, 200, {"code": 0, "data": {
                    "success": len(ids), "failed": 0, "success_ids": ids, "failed_ids": [],
                    "total": len(ids) + 1,
                    "results": [{"account_id": aid, "success": True} for aid in ids],
                    "errors": [],
                }})
                return
            if self.mode == "timeout-before":
                time.sleep(self.fixture.http_timeout * 4)
                return
            self._apply(path, method, body, [aid for aid in ids if aid not in failed])
            if self.mode == "timeout-after":
                time.sleep(self.fixture.http_timeout * 4)
                return
            success = [aid for aid in ids if aid not in failed]
            if path.endswith("batch-delete"):
                entries = [{"account_id": aid, "success": False} for aid in failed]
            else:
                entries = [{"account_id": aid, "success": aid in success} for aid in ids]
            response_data = {
                "success": len(success), "failed": len(failed), "success_ids": success,
                "failed_ids": sorted(failed), "results": entries, "errors": entries if path.endswith("batch-delete") else [],
                "total": len(ids),
            }
            self._response(request, 200, {"code": 0, "data": response_data})
            return

        # Explicit compatibility requests after an unsupported native route.
        if self.mode == "route-404" and method in {"PUT", "DELETE"}:
            self._apply(path, method, body, ids)
            self._response(request, 200, {"code": 0, "data": {}})
            return
        self._response(request, 404, {"detail": "Not Found"})


class NativeBatchFixture:
    def __init__(self, *, count=205, managed=()):
        self.tmp = tempfile.TemporaryDirectory(prefix="native-batch-038-")
        self.rows = {aid: _row(aid, proxy_id=21 if aid <= count else None, groups=[1, 8])
                     for aid in range(1, count + 1)}
        self.proxies = [{"id": 21, "status": "active"}, {"id": 22, "status": "active"},
                        {"id": 23, "status": "inactive"}]
        self.partial_failed = set()
        self.http_timeout = 0.15
        self.receiver = _Receiver(self)
        self.db = _FixtureDB(self.rows)
        self.controller = _Controller(managed)
        self.service = SimpleNamespace()
        self.service.r = SimpleNamespace(
            db=self.db,
            settings=SimpleNamespace(
                usage_query_state_path=str(Path(self.tmp.name) / "usage.json"),
                audit_path=str(Path(self.tmp.name) / "audit.jsonl"),
            ),
            key_fallback_controller=self.controller,
            oauth_state_store=lambda: SimpleNamespace(admin_token=lambda: ADMIN_KEY),
            oauth_base_url=lambda: f"http://127.0.0.1:{self.receiver.server.server_port}",
            oauth_monitor=None,
        )
        self.service.config = SimpleNamespace(thread_lock=threading.RLock())
        self.service.actions = _Actions(self.service, self)
        self.service.account_templates = _TemplateReader(self)
        self.service.account_settings = _ProxySettings(self)
        self.service.authenticate = lambda *_args, **_kwargs: None
        self.service.invalidate = lambda: None

        @contextmanager
        def recovery_guard(_row):
            yield

        self.service.recovery_guard = recovery_guard
        self.queue = AccountOperations(self.service)

    def close(self):
        self.receiver.close()
        self.tmp.cleanup()


class NativeBatch038Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.fixture = NativeBatchFixture()
        self.queue = self.fixture.queue
        self.addAsyncCleanup(self._cleanup)

    async def _cleanup(self):
        await self.queue.close()
        self.fixture.close()

    def _proxy_request(self, ids, proxy_id, request_id="fixture-proxy-native-038"):
        return ProxyBatchRequest(
            action="proxy", payload={"proxy_id": proxy_id},
            accounts=[{"account_id": aid, "expected_version": versions(self.fixture.rows[aid])["proxy"]}
                      for aid in ids],
            request_id=request_id, client_id=CLIENT_ID,
        )

    def _template_request(self, ids, request_id="fixture-template-native-038"):
        return TemplateBatchRequest(
            template_id="full", template_version=digest(self.fixture.service.account_templates.profile),
            accounts=[{"account_id": aid, "expected_version": account_version(self.fixture.rows[aid])}
                      for aid in ids],
            request_id=digest(request_id)[:32], client_id="c" * 32,
        )

    def _groups_request(self, items, request_id="fixture-groups-native-038"):
        return AccountBatchRequest(
            action="groups", payload={}, accounts=[{
                "account_id": aid,
                "expected_version": versions(self.fixture.rows[aid])["groups"],
                "payload": payload,
            } for aid, payload in items], request_id=request_id, client_id=CLIENT_ID,
        )

    def _delete_request(self, items, confirmed=True, request_id="fixture-delete-native-038"):
        return AccountBatchRequest(
            action="delete", payload={"confirmed": confirmed}, accounts=[{
                "account_id": aid,
                "expected_version": versions(self.fixture.rows[aid], aid in self.fixture.controller.config.managed_account_ids)["delete"],
                "payload": payload,
            } for aid, payload in items], request_id=request_id, client_id=CLIENT_ID,
        )

    async def test_groups_merge_final_payload_and_preserve_scope_outside(self):
        self.fixture.rows[1]["group_ids"] = [1, 8]
        self.fixture.rows[2]["group_ids"] = [3, 8]
        submitted = await self.queue.submit_batch(self._groups_request([
            (1, {"scope_group_ids": [1, 2], "group_ids": [2]}),
            (2, {"scope_group_ids": [2, 3], "group_ids": [2]}),
        ]), ADMIN_KEY)

        await self.queue.run(submitted["items"][0]["id"])

        calls = self.fixture.receiver.calls
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["path"], "/api/v1/admin/accounts/bulk-update")
        self.assertEqual(calls[0]["body"], {"account_ids": [1, 2], "group_ids": [2, 8]})
        self.assertEqual(self.fixture.rows[1]["group_ids"], [2, 8])
        self.assertEqual(self.fixture.rows[2]["group_ids"], [2, 8])
        self.assertEqual({item["status"] for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]}, {"completed"})

    async def test_proxy_clear_is_one_native_post_with_zero_only(self):
        submitted = await self.queue.submit_proxy_batch(
            self._proxy_request([1, 2], None), ADMIN_KEY,
        )
        await self.queue.run(submitted["items"][0]["id"])

        self.assertEqual(len(self.fixture.receiver.calls), 1)
        self.assertEqual(self.fixture.receiver.calls[0]["body"], {"account_ids": [1, 2], "proxy_id": 0})
        self.assertEqual(self.fixture.rows[1]["proxy_id"], None)
        self.assertEqual(self.fixture.rows[2]["proxy_id"], None)
        self.assertEqual({item["status"] for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]}, {"completed"})

    async def test_template_batch_writes_only_credentials_model_mapping(self):
        for aid in (1, 2):
            self.fixture.rows[aid]["model_mapping"] = {"fixture-old-model": "fixture-old-model"}
            self.fixture.rows[aid]["model_mapping_version"] = digest(self.fixture.rows[aid]["model_mapping"])
        before = {aid: copy.deepcopy(self.fixture.rows[aid]) for aid in (1, 2)}
        submitted = await self.queue.submit_template_batch(self._template_request([1, 2]), ADMIN_KEY)
        await self.queue.run(submitted["items"][0]["id"])

        self.assertEqual(len(self.fixture.receiver.calls), 1)
        self.assertEqual(self.fixture.receiver.calls[0]["body"], {
            "account_ids": [1, 2], "credentials": {"model_mapping": {"fixture-model": "fixture-model"}},
        })
        for aid in (1, 2):
            self.assertEqual(self.fixture.rows[aid]["model_mapping"], {"fixture-model": "fixture-model"})
            for field in before[aid].keys() - {"model_mapping", "model_mapping_version"}:
                self.assertEqual(self.fixture.rows[aid][field], before[aid][field], field)
            self.assertEqual(self.queue.get(next(item["id"] for item in submitted["items"] if item["account_id"] == aid))["status"], "completed")

    async def test_more_than_100_accounts_are_chunked_without_replay(self):
        request = self._proxy_request(list(range(1, 206)), 22, request_id="fixture-proxy-chunks-native-038")
        submitted = await self.queue.submit_proxy_batch(request, ADMIN_KEY)
        await self.queue.run(submitted["items"][0]["id"])

        self.assertEqual([len(call["ids"]) for call in self.fixture.receiver.calls], [100, 100, 5])
        self.assertEqual([call["body"]["proxy_id"] for call in self.fixture.receiver.calls], [22, 22, 22])
        self.assertEqual({item["status"] for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]}, {"completed"})

    async def test_busy_account_stays_queued_while_other_account_runs(self):
        lock = account_lock(self.fixture.db, 1)
        self.assertTrue(lock.acquire(blocking=False))
        try:
            submitted = await self.queue.submit_proxy_batch(
                self._proxy_request([1, 2], 22, request_id="fixture-proxy-busy-native-038"), ADMIN_KEY,
            )
            await self.queue.run(submitted["items"][0]["id"])
            by_id = {item["account_id"]: item for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]}
            self.assertEqual(by_id[1]["status"], "queued")
            self.assertEqual(by_id[2]["status"], "completed")
            self.assertEqual(self.fixture.receiver.calls[0]["ids"], [2])
        finally:
            lock.release()
        await self.queue.run(submitted["items"][0]["id"])
        self.assertEqual([call["ids"] for call in self.fixture.receiver.calls], [[2], [1]])
        self.assertEqual({item["status"] for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]}, {"completed"})

    async def test_unchanged_children_are_verified_without_native_write(self):
        self.fixture.rows[1]["proxy_id"] = 22
        request = self._proxy_request([1, 2], 22, request_id="fixture-proxy-unchanged-native-038")
        submitted = await self.queue.submit_proxy_batch(request, ADMIN_KEY)
        await self.queue.run(next(item["id"] for item in submitted["items"] if item["account_id"] == 2))

        self.assertEqual(len(self.fixture.receiver.calls), 1)
        self.assertEqual(self.fixture.receiver.calls[0]["ids"], [2])
        results = {item["account_id"]: item for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]}
        self.assertTrue(results[1]["result"]["unchanged"])
        self.assertEqual(results[1]["status"], "completed")
        self.assertEqual(results[2]["status"], "completed")

    async def test_partial_result_marks_only_failed_account(self):
        self.fixture.receiver.mode = "partial"
        self.fixture.partial_failed = {2}
        submitted = await self.queue.submit_proxy_batch(
            self._proxy_request([1, 2], 22, request_id="fixture-proxy-partial-native-038"), ADMIN_KEY,
        )
        await self.queue.run(submitted["items"][0]["id"])
        results = {item["account_id"]: item for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]}

        self.assertEqual(len(self.fixture.receiver.calls), 1)
        self.assertEqual(results[1]["status"], "completed")
        self.assertEqual(results[2]["status"], "failed")
        self.assertEqual(results[2]["result"], None)

    async def test_malformed_native_result_never_claims_success_or_replays(self):
        for index, mode in enumerate(("malformed-result", "malformed-total")):
            self.fixture.receiver.mode = mode
            ids = [1, 2] if index == 0 else [3, 4]
            submitted = await self.queue.submit_proxy_batch(
                self._proxy_request(ids, 22, request_id=f"fixture-proxy-{mode}-native-038"), ADMIN_KEY,
            )
            await self.queue.run(submitted["items"][0]["id"])
            results = self.queue.listing(batch_id=submitted["batch_id"])["items"]

            self.assertEqual({item["status"] for item in results}, {"needs_confirmation"})
            await self.queue.run(submitted["items"][0]["id"])
            self.assertEqual(len(self.fixture.receiver.calls), index + 1)

    async def test_dispatch_persistence_failure_never_writes_native(self):
        submitted = await self.queue.submit_proxy_batch(
            self._proxy_request([1, 2], 22, request_id="fixture-proxy-persist-failure-native-038"), ADMIN_KEY,
        )
        writes = []
        import app.account_operations as account_operations

        real_write_json = account_operations.write_json

        def fail_once(path, data):
            writes.append(path)
            if len(writes) == 1:
                raise OSError("fixture dispatch persistence failure")
            return real_write_json(path, data)

        with patch.object(account_operations, "write_json", side_effect=fail_once):
            await self.queue.run(submitted["items"][0]["id"])

        self.assertEqual(len(writes), 2)
        self.assertEqual(self.fixture.receiver.calls, [])
        self.assertEqual(
            {item["status"] for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]},
            {"queued"},
        )

    async def test_timeout_before_write_and_after_write_are_readback_only(self):
        self.fixture.receiver.mode = "timeout-before"
        before = await self.queue.submit_proxy_batch(
            self._proxy_request([1], 22, request_id="fixture-proxy-timeout-before-native-038"), ADMIN_KEY,
        )
        await self.queue.run(before["items"][0]["id"])
        self.assertEqual(len(self.fixture.receiver.calls), 1)
        self.assertEqual(self.fixture.rows[1]["proxy_id"], 21)
        self.assertEqual(self.queue.get(before["items"][0]["id"])["status"], "needs_confirmation")

        self.fixture.receiver.mode = "timeout-after"
        after = await self.queue.submit_proxy_batch(
            self._proxy_request([2], 22, request_id="fixture-proxy-timeout-after-native-038"), ADMIN_KEY,
        )
        await self.queue.run(after["items"][0]["id"])
        self.assertEqual(len(self.fixture.receiver.calls), 2)
        self.assertEqual(self.fixture.rows[2]["proxy_id"], 22)
        self.assertEqual(self.queue.get(after["items"][0]["id"])["status"], "completed")
        await self.queue.run(after["items"][0]["id"])
        self.assertEqual(len(self.fixture.receiver.calls), 2)

    async def test_cancelled_child_is_not_sent_in_native_batch(self):
        submitted = await self.queue.submit_proxy_batch(
            self._proxy_request([1, 2], 22, request_id="fixture-proxy-cancel-native-038"), ADMIN_KEY,
        )
        child = next(item for item in submitted["items"] if item["account_id"] == 1)
        await self.queue.cancel(child["id"])
        await self.queue.run(submitted["items"][1]["id"])

        self.assertEqual(self.queue.get(child["id"])["status"], "cancelled")
        self.assertEqual(self.fixture.receiver.calls[0]["ids"], [2])

    async def test_restart_running_child_becomes_read_only_checking(self):
        submitted = await self.queue.submit_proxy_batch(
            self._proxy_request([1], 22, request_id="fixture-proxy-restart-native-038"), ADMIN_KEY,
        )
        child = submitted["items"][0]
        self.queue.update(child["id"], status="running")
        await self.queue.close()
        self.queue = AccountOperations(self.fixture.service)
        await self.queue.run(child["id"])

        self.assertEqual(self.queue.get(child["id"])["status"], "needs_confirmation")
        self.assertEqual(self.fixture.receiver.calls, [])

    async def test_delete_requires_confirmation_detaches_managed_and_sends_only_ids(self):
        self.fixture.controller.config.managed_account_ids = {1}
        with self.assertRaises(HTTPException) as raised:
            await self.queue.submit_batch(self._delete_request([(1, {"detach_managed": True})], confirmed=False), ADMIN_KEY)
        self.assertEqual(raised.exception.status_code, 422)

        submitted = await self.queue.submit_batch(self._delete_request(
            [(1, {"detach_managed": True}), (2, {"detach_managed": False})],
        ), ADMIN_KEY)
        await self.queue.run(submitted["items"][0]["id"])

        self.assertEqual(self.fixture.receiver.calls[0]["path"], "/api/v1/admin/accounts/batch-delete")
        self.assertEqual(self.fixture.receiver.calls[0]["body"], {"account_ids": [1, 2]})
        self.assertEqual(self.fixture.rows[1]["deleted_at"], "fixture-deleted")
        self.assertEqual(self.fixture.rows[2]["deleted_at"], "fixture-deleted")
        self.assertEqual(self.fixture.controller.config.managed_account_ids, set())
        self.assertEqual({item["status"] for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]}, {"completed"})

    async def test_delete_child_set_change_requires_confirmation_without_native_write(self):
        submitted = await self.queue.submit_batch(
            self._delete_request([(1, {"detach_managed": True})], request_id="fixture-delete-child-change-native-038"),
            ADMIN_KEY,
        )
        self.fixture.rows[2]["parent_account_id"] = 1
        self.fixture.rows[2]["extra"] = {"parent_account_id": 1}

        await self.queue.run(submitted["items"][0]["id"])

        item = self.queue.get(submitted["items"][0]["id"])
        self.assertEqual(item["status"], "needs_confirmation")
        self.assertIn("关联账号", item["reason"])
        self.assertEqual(self.fixture.receiver.calls, [])

    async def test_delete_managed_child_set_change_requires_confirmation_without_native_write(self):
        self.fixture.rows[2]["parent_account_id"] = 1
        self.fixture.rows[2]["extra"] = {"parent_account_id": 1}
        submitted = await self.queue.submit_batch(
            self._delete_request([(1, {"detach_managed": True})], request_id="fixture-delete-managed-child-change-native-038"),
            ADMIN_KEY,
        )
        self.assertEqual(self.queue.get(submitted["items"][0]["id"])["managed_child_ids"], [])
        self.fixture.controller.config.managed_account_ids = {2}

        await self.queue.run(submitted["items"][0]["id"])

        item = self.queue.get(submitted["items"][0]["id"])
        self.assertEqual(item["status"], "needs_confirmation")
        self.assertIn("关联账号", item["reason"])
        self.assertEqual(self.fixture.receiver.calls, [])
        self.assertEqual(self.fixture.controller.config.managed_account_ids, {2})

    async def test_inherited_proxy_and_template_children_are_rejected_without_write(self):
        self.fixture.rows[2]["parent_account_id"] = 1
        self.fixture.rows[2]["extra"] = {"parent_account_id": 1}
        self.fixture.rows[2]["model_mapping"] = {"fixture-old-model": "fixture-old-model"}
        self.fixture.rows[2]["model_mapping_version"] = digest(self.fixture.rows[2]["model_mapping"])
        proxy = await self.queue.submit_proxy_batch(
            self._proxy_request([2], 22, request_id="fixture-proxy-inherited-native-038"), ADMIN_KEY,
        )
        template = await self.queue.submit_template_batch(
            self._template_request([2], request_id="fixture-template-inherited-native-038"), ADMIN_KEY,
        )
        await self.queue.run(proxy["items"][0]["id"])
        await self.queue.run(template["items"][0]["id"])

        self.assertEqual(self.fixture.receiver.calls, [])
        self.assertEqual(self.queue.get(proxy["items"][0]["id"])["status"], "failed")
        self.assertEqual(self.queue.get(template["items"][0]["id"])["status"], "failed")

    async def test_template_version_change_requires_confirmation(self):
        request = self._template_request([1])
        request.template_version = "b" * 64
        with self.assertRaises(HTTPException) as raised:
            await self.queue.submit_template_batch(request, ADMIN_KEY)
        self.assertEqual(raised.exception.status_code, 409)
        self.assertEqual(self.fixture.receiver.calls, [])

    async def test_unavailable_proxy_is_rejected_without_native_write(self):
        self.fixture.proxies = [{"id": 22, "status": "inactive"}]
        submitted = await self.queue.submit_proxy_batch(
            self._proxy_request([1], 22, request_id="fixture-proxy-unavailable-native-038"), ADMIN_KEY,
        )
        self.assertEqual(submitted["items"][0]["status"], "needs_confirmation")
        await self.queue.run(submitted["items"][0]["id"])
        self.assertEqual(self.fixture.receiver.calls, [])

    async def test_loop_auto_launches_one_native_batch(self):
        loop = asyncio.create_task(self.queue.loop())
        self.addAsyncCleanup(loop.cancel)
        submitted = await self.queue.submit_proxy_batch(
            self._proxy_request([1, 2], 22, request_id="fixture-proxy-loop-native-038"), ADMIN_KEY,
        )
        for _ in range(100):
            states = {item["status"] for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]}
            if states == {"completed"}:
                break
            await asyncio.sleep(0.02)
        self.assertEqual(states, {"completed"})
        self.assertEqual(len(self.fixture.receiver.calls), 1)

    async def test_explicit_route_404_falls_back_once_but_generic_404_does_not_replay(self):
        self.fixture.receiver.mode = "route-404"
        route = await self.queue.submit_proxy_batch(
            self._proxy_request([1, 2], 22, request_id="fixture-proxy-route-404-native-038"), ADMIN_KEY,
        )
        await self.queue.run(route["items"][0]["id"])
        self.assertEqual([call["method"] for call in self.fixture.receiver.calls], ["POST", "PUT", "PUT"])
        self.assertEqual({item["status"] for item in self.queue.listing(batch_id=route["batch_id"])["items"]}, {"completed"})

        self.fixture.receiver.calls.clear()
        self.fixture.receiver.mode = "generic-404"
        self.fixture.rows[1]["proxy_id"] = 21
        generic = await self.queue.submit_proxy_batch(
            self._proxy_request([1], 22, request_id="fixture-proxy-generic-404-native-038"), ADMIN_KEY,
        )
        await self.queue.run(generic["items"][0]["id"])
        self.assertEqual([call["method"] for call in self.fixture.receiver.calls], ["POST"])
        self.assertEqual(self.queue.get(generic["items"][0]["id"])["status"], "needs_confirmation")

    async def test_malformed_account_batch_id_is_rejected_before_persist(self):
        with self.assertRaises(ValidationError):
            AccountBatchRequest(
                action="groups", payload={}, accounts=[{
                    "account_id": "1", "expected_version": VERSION,
                    "payload": {"scope_group_ids": [1], "group_ids": [1]},
                }], request_id="fixture-malformed-id-native-038", client_id=CLIENT_ID,
            )
        self.assertEqual(self.queue.listing()["items"], [])


if __name__ == "__main__":
    unittest.main()
