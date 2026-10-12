from __future__ import annotations

import asyncio
import copy
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient
from pydantic import ValidationError

from app.account_locks import account_lock
from app.account_operations import AccountOperations, ProxyBatchRequest
from app.account_settings import AccountSettings
from app.desktop_api import install_desktop_api
from app.operation_versions import versions


ADMIN_KEY = "fixture-admin-key-036"
CLIENT_ID = "fixture-client-id-036"
VERSION = "a" * 64


class _Response:
    def __init__(self, status_code: int, body: dict):
        self.status_code = status_code
        self._body = copy.deepcopy(body)
        self.content = json.dumps(self._body).encode()
        self.text = self.content.decode()

    def json(self):
        return copy.deepcopy(self._body)


class _Client:
    def __init__(self, fixture: "ProxyBatch036Tests"):
        self.fixture = fixture

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_args):
        return None

    async def request(self, method: str, path: str, **kwargs):
        """Exercise NativeBatches' actual client.request compatibility path."""
        fixture = self.fixture
        if method == "POST" and path == "accounts/bulk-update":
            # This fixture represents an older native deployment. The explicit
            # route response authorizes per-account compatibility, exactly once.
            return _Response(404, {"detail": "Not Found"})
        if method != "PUT" or not path.startswith("accounts/"):
            raise AssertionError(f"unexpected fixture request: {method} {path}")
        account_id = int(path.rsplit("/", 1)[-1])
        fixture.put_calls.append(account_id)
        if account_id in fixture.timeout_before_write:
            return _Response(504, {"code": "fixture_timeout"})
        if account_id in fixture.write_errors:
            return _Response(500, {"code": "fixture_write_failed"})
        body = copy.deepcopy(kwargs["json"])
        fixture.writes.append((account_id, body))
        if "proxy_id" in body:
            fixture.rows[account_id]["proxy_id"] = None if body["proxy_id"] == 0 else body["proxy_id"]
        if account_id in fixture.timeout_after_write:
            return _Response(504, {"code": "fixture_timeout"})
        return _Response(200, {"code": 0, "data": {}})


def _row(account_id: int, *, platform: str = "openai", proxy_id: int | None = 11,
         parent_account_id: int | None = None, deleted_at: str | None = None) -> dict:
    return {
        "id": account_id,
        "name": f"fixture-account-{account_id}",
        "platform": platform,
        "type": "apikey",
        "status": "active",
        "deleted_at": deleted_at,
        "schedulable": True,
        "priority": 1,
        "concurrency": 8,
        "load_factor": None,
        "proxy_id": proxy_id,
        "parent_account_id": parent_account_id,
        "group_ids": [],
        "updated_at": "fixture-updated-at",
        "extra": {},
        "credential_version": f"fixture-credential-{account_id}",
        "credential_settings_version": f"fixture-settings-{account_id}",
        "model_mapping_version": f"fixture-mapping-{account_id}",
        "model_mapping": {},
        "model_sync_project": False,
        "credentials": {"api_key": f"fixture-secret-{account_id}"},
    }


class _ProxyActions:
    def __init__(self, fixture: "ProxyBatch036Tests") -> None:
        self.fixture = fixture

    async def account(self, account_id: int) -> dict:
        row = self.fixture.rows.get(account_id)
        if row is None:
            raise HTTPException(404, "fixture account missing")
        return copy.deepcopy(row)

    async def accounts(self, account_ids: list[int]) -> dict[int, dict]:
        return {account_id: copy.deepcopy(self.fixture.rows[account_id])
                for account_id in account_ids if account_id in self.fixture.rows}

    def client(self, _key: str, _timeout: int = 10) -> _Client:
        return _Client(self.fixture)

    async def json_request(self, _client, method: str, path: str, **kwargs):
        self.fixture.calls.append((method, path, copy.deepcopy(kwargs)))
        if method == "GET" and path == "proxies/all":
            return copy.deepcopy(self.fixture.proxy_choices)
        if method != "PUT" or not path.startswith("accounts/"):
            raise AssertionError(f"unexpected fixture request: {method} {path}")

        account_id = int(path.rsplit("/", 1)[-1])
        self.fixture.put_calls.append(account_id)
        if account_id in self.fixture.write_errors:
            raise HTTPException(500, {"code": "fixture_write_failed"})
        if account_id in self.fixture.timeout_before_write:
            raise HTTPException(504, {"code": "fixture_timeout"})

        body = kwargs["json"]
        self.fixture.writes.append((account_id, copy.deepcopy(body)))
        if "proxy_id" in body:
            self.fixture.rows[account_id]["proxy_id"] = None if body["proxy_id"] == 0 else body["proxy_id"]
        if account_id in self.fixture.timeout_after_write:
            raise HTTPException(504, {"code": "fixture_timeout"})
        return {"code": 0}


class ProxyBatch036Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="proxy-batch-036-")
        self.rows = {
            1: _row(1, platform="openai"),
            2: _row(2, platform="grok"),
            3: _row(3, platform="anthropic"),
        }
        self.proxy_choices = [
            {"id": 11, "name": "fixture current", "protocol": "https", "status": "active"},
            {"id": 21, "name": "fixture target", "protocol": "https", "status": "active"},
            {"id": 22, "name": "fixture inactive", "protocol": "https", "status": "inactive"},
        ]
        self.calls: list[tuple[str, str, dict]] = []
        self.put_calls: list[int] = []
        self.writes: list[tuple[int, dict]] = []
        self.write_errors: set[int] = set()
        self.timeout_before_write: set[int] = set()
        self.timeout_after_write: set[int] = set()
        self.db = SimpleNamespace()
        settings = SimpleNamespace(
            usage_query_state_path=str(Path(self.temp.name) / "oauth-state.json"),
            audit_path=str(Path(self.temp.name) / "audit.jsonl"),
        )
        self.actions = _ProxyActions(self)
        self.service = SimpleNamespace(
            r=SimpleNamespace(
                db=self.db,
                settings=settings,
                key_fallback_controller=None,
                oauth_state_store=lambda: SimpleNamespace(admin_token=lambda: ADMIN_KEY),
            ),
            actions=self.actions,
            authenticate=lambda *_args, **_kwargs: None,
            invalidate=lambda: None,
        )
        self.service.account_settings = AccountSettings(self.service)
        self.queue = AccountOperations(self.service)

    async def asyncTearDown(self) -> None:
        await self.queue.close()
        self.temp.cleanup()

    def request(self, account_ids: list[int], proxy_id: int | None, *, request_id: str,
                client_id: str = CLIENT_ID, expected: dict[int, str] | None = None) -> ProxyBatchRequest:
        expected = expected or {}
        accounts = []
        for account_id in account_ids:
            version = expected.get(account_id)
            if version is None:
                version = versions(self.rows[account_id])["proxy"] if account_id in self.rows else VERSION
            accounts.append({"account_id": account_id, "expected_version": version})
        return ProxyBatchRequest(
            action="proxy",
            payload={"proxy_id": proxy_id},
            accounts=accounts,
            request_id=request_id,
            client_id=client_id,
        )

    async def submit(self, account_ids: list[int], proxy_id: int | None, request_id: str,
                     *, expected: dict[int, str] | None = None):
        return await self.queue.submit_proxy_batch(
            self.request(account_ids, proxy_id, request_id=request_id, expected=expected), ADMIN_KEY
        )

    async def test_request_contract_rejects_bad_shapes_and_duplicate_accounts(self):
        with self.assertRaises(ValidationError):
            self.request([], 21, request_id="fixture-contract-empty")
        with self.assertRaises(ValidationError):
            self.request([1], 0, request_id="fixture-contract-zero")
        with self.assertRaises(ValidationError):
            ProxyBatchRequest(
                action="proxy", payload={"proxy_id": 21, "unexpected": True},
                accounts=[{"account_id": 1, "expected_version": VERSION}],
                request_id="fixture-contract-extra", client_id=CLIENT_ID,
            )
        with self.assertRaises(ValidationError):
            ProxyBatchRequest(
                action="proxy", payload={"proxy_id": 21},
                accounts=[{"account_id": i, "expected_version": VERSION} for i in range(1, 1002)],
                request_id="fixture-contract-too-many", client_id=CLIENT_ID,
            )
        duplicate = self.request([1, 1], 21, request_id="fixture-contract-duplicate")
        with self.assertRaises(HTTPException) as raised:
            await self.queue.submit_proxy_batch(duplicate, ADMIN_KEY)
        self.assertEqual(raised.exception.status_code, 422)
        self.assertEqual(self.queue.listing()["items"], [])

    async def test_multiple_platforms_unchanged_and_clear_are_listed_and_written(self):
        self.rows[3]["proxy_id"] = 21
        submitted = await self.submit([1, 2, 3], 21, "fixture-proxy-target-036")
        initial = {item["account_id"]: item for item in submitted["items"]}
        self.assertEqual(submitted["pending"], 2)
        self.assertEqual(initial[1]["status"], "queued")
        self.assertEqual(initial[2]["status"], "queued")
        self.assertEqual(initial[3]["status"], "completed")
        self.assertTrue(initial[3]["result"]["unchanged"])
        self.assertEqual(self.queue.listing(submitted["batch_id"]), submitted)

        for item in submitted["items"][:2]:
            await self.queue.run(item["id"])
        finished = self.queue.listing(submitted["batch_id"])
        self.assertEqual({item["status"] for item in finished["items"]}, {"completed"})
        self.assertEqual([account_id for account_id, _body in self.writes], [1, 2])

        clear = await self.submit(
            [1, 2, 3], None, "fixture-proxy-clear-036",
            expected={account_id: versions(self.rows[account_id])["proxy"] for account_id in (1, 2, 3)},
        )
        self.assertEqual(clear["pending"], 3)
        for item in clear["items"]:
            await self.queue.run(item["id"])
        clear_result = self.queue.listing(clear["batch_id"])
        self.assertEqual(clear_result["pending"], 0)
        self.assertEqual({item["status"] for item in clear_result["items"]}, {"completed"})
        self.assertEqual([self.rows[account_id]["proxy_id"] for account_id in (1, 2, 3)], [None, None, None])
        self.assertEqual([body for _account_id, body in self.writes[-3:]], [{"proxy_id": 0}] * 3)

    async def test_inheritance_deleted_missing_and_invalid_target_are_per_item(self):
        self.rows[4] = _row(4, platform="grok", parent_account_id=1)
        self.rows[5] = _row(5, platform="anthropic", deleted_at="fixture-deleted")
        self.rows[6] = _row(6, platform="openai")
        submitted = await self.submit([4, 5, 6, 99], 22, "fixture-proxy-invalid-036")
        items = {item["account_id"]: item for item in submitted["items"]}
        self.assertEqual(items[4]["status"], "failed")
        self.assertTrue(items[4]["reason"])
        self.assertEqual(items[5]["status"], "failed")
        self.assertEqual(items[6]["status"], "needs_confirmation")
        self.assertEqual(items[99]["status"], "failed")
        self.assertEqual(submitted["pending"], 1)
        self.assertEqual(self.put_calls, [])

    async def test_partial_proxy_writes_and_cancel_keep_each_child_state(self):
        self.write_errors.add(3)
        submitted = await self.submit([1, 2, 3], 21, "fixture-proxy-partial-036")
        by_account = {item["account_id"]: item for item in submitted["items"]}
        cancelled = await self.queue.cancel(by_account[2]["id"])
        self.assertEqual(cancelled["status"], "cancelled")
        await self.queue.run(by_account[1]["id"])
        await self.queue.run(by_account[2]["id"])
        await self.queue.run(by_account[3]["id"])
        result = self.queue.listing(submitted["batch_id"])
        statuses = {item["account_id"]: item["status"] for item in result["items"]}
        self.assertEqual(statuses, {1: "completed", 2: "cancelled", 3: "needs_confirmation"})
        # Native uncertain write remains visible as needs_confirmation until a
        # fresh preview, so it is still counted as pending.
        self.assertEqual(result["pending"], 1)
        self.assertEqual(self.put_calls, [1, 3])
        self.assertEqual([account_id for account_id, _body in self.writes], [1])
        self.assertEqual(self.rows[3]["proxy_id"], 11)

    async def test_field_conflict_is_confirmed_without_proxy_write(self):
        submitted = await self.submit([1], 21, "fixture-proxy-conflict-036")
        self.rows[1]["proxy_id"] = 22
        await self.queue.run(submitted["items"][0]["id"])
        result = self.queue.get(submitted["items"][0]["id"])
        self.assertEqual(result["status"], "needs_confirmation")
        self.assertIn("相关字段", result["reason"])
        self.assertEqual(self.put_calls, [])

    async def test_busy_queue_retries_once_after_account_becomes_free(self):
        submitted = await self.submit([2], 21, "fixture-proxy-busy-036")
        lock = account_lock(self.db, 2)
        self.assertTrue(lock.acquire())
        try:
            await self.queue.run(submitted["items"][0]["id"])
            self.assertEqual(self.queue.get(submitted["items"][0]["id"])["status"], "queued")
            self.assertEqual(self.put_calls, [])
        finally:
            lock.release()
        await self.queue.run(submitted["items"][0]["id"])
        self.assertEqual(self.queue.get(submitted["items"][0]["id"])["status"], "completed")
        self.assertEqual(self.put_calls, [2])

    async def test_idempotency_conflict_and_account_order_are_deterministic(self):
        first_request = self.request([2, 1], 21, request_id="fixture-proxy-idempotency-036")
        first = await self.queue.submit_proxy_batch(first_request, ADMIN_KEY)
        reordered = self.request([1, 2], 21, request_id="fixture-proxy-idempotency-036")
        second = await self.queue.submit_proxy_batch(reordered, "fixture-new-key")
        self.assertEqual(second["batch_id"], first["batch_id"])
        self.assertEqual([item["account_id"] for item in second["items"]], [2, 1])
        conflicting = self.request([1, 2], None, request_id="fixture-proxy-idempotency-036")
        with self.assertRaises(HTTPException) as raised:
            await self.queue.submit_proxy_batch(conflicting, ADMIN_KEY)
        self.assertEqual(raised.exception.status_code, 409)
        self.assertEqual(raised.exception.detail["code"], "idempotency_conflict")

    async def test_restart_of_checking_never_replays_proxy_write(self):
        self.timeout_before_write.add(1)
        submitted = await self.submit([1], 21, "fixture-proxy-restart-036")
        child_id = submitted["items"][0]["id"]
        await self.queue.run(child_id)
        self.assertEqual(self.queue.get(child_id)["status"], "needs_confirmation")
        self.assertEqual(self.put_calls, [1])

        await self.queue.close()
        self.queue = AccountOperations(self.service)
        await self.queue.run(child_id)
        self.assertEqual(self.queue.get(child_id)["status"], "needs_confirmation")
        self.assertEqual(self.put_calls, [1])

    async def test_timeout_after_proxy_write_reads_back_without_replay(self):
        self.timeout_after_write.add(1)
        submitted = await self.submit([1], 21, "fixture-proxy-timeout-readback-036")
        await self.queue.run(submitted["items"][0]["id"])
        result = self.queue.get(submitted["items"][0]["id"])
        self.assertEqual(result["status"], "completed")
        self.assertEqual(self.put_calls, [1])
        self.assertEqual(self.writes, [(1, {"proxy_id": 21})])

    async def test_save_failure_does_not_accept_batch_or_dispatch_proxy(self):
        request = self.request([1, 2], 21, request_id="fixture-proxy-save-failure-036")
        with patch("app.account_operations.write_json", side_effect=OSError("fixture-save-failed")) as save_json:
            with self.assertRaises(OSError):
                await self.queue.submit_proxy_batch(request, ADMIN_KEY)
        save_json.assert_called_once()
        self.assertEqual(self.queue.listing()["items"], [])
        if self.queue.store.path.exists():
            try:
                state = json.loads(self.queue.store.path.read_text())
            except json.JSONDecodeError:
                state = {}
            self.assertEqual(state.get("jobs", {}), {})
            self.assertEqual(state.get("batches", {}), {})
        self.assertEqual(self.put_calls, [])


class ProxyBatchRoute036Tests(unittest.TestCase):
    def test_post_batch_route_returns_202_and_forwards_key(self):
        app = FastAPI()
        temp = tempfile.TemporaryDirectory(prefix="proxy-route-036-")
        settings = SimpleNamespace(
            usage_query_state_path=str(Path(temp.name) / "oauth-state.json"),
            audit_path=str(Path(temp.name) / "audit.jsonl"),
        )
        runtime = SimpleNamespace(db=Mock(), settings=settings, APP_VERSION="fixture-0.1.36")
        service = install_desktop_api(app, runtime)
        result = {"batch_id": "b" * 32, "items": [], "pending": 0}
        operations = SimpleNamespace(
            submit_proxy_batch=AsyncMock(return_value=result),
            submit_batch=AsyncMock(return_value=result),
            close=AsyncMock(),
        )
        service._operations = operations
        service.authenticate = Mock(return_value=None)
        try:
            with TestClient(app) as client:
                response = client.post(
                    "/api/desktop/v1/account-operations/batch",
                    headers={"x-api-key": ADMIN_KEY},
                    json={
                        "action": "proxy",
                        "payload": {"proxy_id": None},
                        "accounts": [{"account_id": 1, "expected_version": VERSION}],
                        "request_id": "fixture-route-request-036",
                        "client_id": CLIENT_ID,
                    },
                )
            self.assertEqual(response.status_code, 202, response.text)
            self.assertEqual(response.json(), result)
            operations.submit_batch.assert_awaited_once()
            request, key = operations.submit_batch.await_args.args
            self.assertIsInstance(request, ProxyBatchRequest)
            self.assertEqual(request.payload.proxy_id, None)
            self.assertEqual(key, ADMIN_KEY)
            service.authenticate.assert_called_once_with(ADMIN_KEY, fresh=True)
        finally:
            asyncio.run(service.close())
            temp.cleanup()


if __name__ == "__main__":
    unittest.main()
