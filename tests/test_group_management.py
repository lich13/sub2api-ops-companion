from __future__ import annotations

import asyncio
import copy
import json
import tempfile
import threading
import unittest
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient

from app.desktop_actions import GroupsRequest
from app.desktop_api import account_dto, install_desktop_api


class GroupDb:
    def __init__(self) -> None:
        now = datetime(2026, 10, 1, 12, 0, tzinfo=timezone.utc)
        self.row: dict | None = {
            "id": 1,
            "name": "OpenAI account",
            "platform": "openai",
            "type": "oauth",
            "status": "active",
            "schedulable": True,
            "updated_at": now,
            "priority": 1,
            "group_ids": [10, 99],
            "extra": {},
            "error_message": None,
            "credentials": {},
            "deleted_at": None,
        }
        self.groups = {
            10: {"id": 10, "platform": "openai"},
            11: {"id": 11, "platform": "openai"},
            99: {"id": 99, "platform": "openai"},
        }
        self.fetch_one_calls: list[tuple[str, dict]] = []
        self.fetch_all_calls: list[tuple[str, dict]] = []

    def fetch_one(self, sql: str, params: dict) -> dict | None:
        self.fetch_one_calls.append((sql, copy.deepcopy(params)))
        if self.row is None or params.get("id") != 1:
            return None
        return copy.deepcopy(self.row)

    def fetch_all(self, sql: str, params: dict | None = None) -> list[dict]:
        self.fetch_all_calls.append((sql, copy.deepcopy(params or {})))
        if "FROM groups" in sql:
            return [copy.deepcopy(self.groups[group_id]) for group_id in params["ids"] if group_id in self.groups]
        return []


class UpstreamCapture:
    def __init__(self, fixture: "GroupManagementTests") -> None:
        self.fixture = fixture
        self.calls: list[tuple[str, str, dict | None]] = []
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), self._handler())
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def _handler(self):
        fixture = self.fixture
        capture = self

        class Handler(BaseHTTPRequestHandler):
            def respond(self, status: int, body: dict) -> None:
                encoded = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def do_GET(self) -> None:
                capture.calls.append(("GET", self.path, None))
                if self.headers.get("x-api-key") != "qa-admin":
                    self.respond(401, {"code": 401, "data": []})
                    return
                self.respond(200, {"code": 0, "data": []})

            def do_PUT(self) -> None:
                length = int(self.headers.get("Content-Length", 0))
                payload = json.loads(self.rfile.read(length))
                capture.calls.append(("PUT", self.path, payload))
                if fixture.upstream_behavior == "reject":
                    self.respond(500, {"code": 500})
                    return
                fixture.db.row["group_ids"] = list(payload["group_ids"])
                self.respond(200, {"code": 0, "data": {}})

            def log_message(self, *_: object) -> None:
                return

        return Handler

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class GroupManagementTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.db = GroupDb()
        self.upstream_behavior = "ok"
        self.capture = UpstreamCapture(self)
        self.auto_reset = Mock()
        self.runtime = SimpleNamespace(
            db=self.db,
            settings=SimpleNamespace(audit_path=str(Path(self.tmp.name) / "audit.jsonl")),
            oauth_base_url=lambda: self.capture.url,
            oauth_monitor=SimpleNamespace(_run_lock=threading.Lock(), auto_reset=self.auto_reset),
            key_fallback_controller=None,
            bark_notifier=Mock(),
        )
        self.app = FastAPI()
        self.service = install_desktop_api(self.app, self.runtime)
        self.client = TestClient(self.app)

    def tearDown(self) -> None:
        self.client.close()
        self.capture.close()
        asyncio.run(self.service.close())
        self.tmp.cleanup()

    def version(self) -> str:
        assert self.db.row is not None
        return account_dto(self.db.row, datetime.now(timezone.utc), set())["version"]

    def payload(self, **changes: object) -> dict:
        value = {
            "expected_version": self.version() if self.db.row is not None else "a" * 64,
            "scope_group_ids": [10, 11],
            "group_ids": [11],
        }
        value.update(changes)
        return value

    def put(self, **changes: object):
        return self.client.put(
            "/api/desktop/v1/accounts/1/groups",
            headers={"x-api-key": "qa-admin"},
            json=self.payload(**changes),
        )

    def test_put_uses_scope_target_version_and_preserves_out_of_scope_members(self) -> None:
        response = self.put()
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json()["group_ids"], [11, 99])
        self.assertEqual(self.db.row["group_ids"], [11, 99])

        puts = [call for call in self.capture.calls if call[0] == "PUT"]
        self.assertEqual(len(puts), 1)
        self.assertEqual(puts[0][1], "/api/v1/admin/accounts/1")
        self.assertEqual(puts[0][2], {"group_ids": [11, 99]})
        self.auto_reset.cancel.assert_not_called()
        paths = [path for _method, path, _body in self.capture.calls]
        self.assertFalse(any(path.endswith("/models") or "/usage" in path for path in paths))
        self.assertEqual(
            [params.get("ids") for sql, params in self.db.fetch_all_calls if "FROM groups" in sql],
            [[10, 11]],
        )

    def test_put_requires_fresh_admin_auth_and_response_excludes_credentials(self) -> None:
        body = self.payload()
        missing = self.client.put("/api/desktop/v1/accounts/1/groups", json=body)
        self.assertEqual(missing.status_code, 401)
        invalid = self.client.put(
            "/api/desktop/v1/accounts/1/groups",
            headers={"x-api-key": "wrong-key"},
            json=body,
        )
        self.assertEqual(invalid.status_code, 401)

        response = self.put()
        self.assertEqual(response.status_code, 200, response.text)
        self.assertNotIn("credentials", response.text)
        self.assertNotIn("api_key", response.text)
        self.assertNotIn("extra", response.text)

    def test_cross_platform_deleted_scope_and_invalid_target_are_rejected_without_put(self) -> None:
        self.db.groups[11]["platform"] = "grok"
        self.assertEqual(self.put().status_code, 409)
        self.assertEqual([call for call in self.capture.calls if call[0] == "PUT"], [])

        self.db.groups[11]["platform"] = "openai"
        del self.db.groups[11]
        self.assertEqual(self.put().status_code, 409)
        self.assertEqual([call for call in self.capture.calls if call[0] == "PUT"], [])

        self.assertEqual(self.put(group_ids=[12]).status_code, 422)
        self.assertEqual([call for call in self.capture.calls if call[0] == "PUT"], [])

    def test_account_platform_deleted_account_and_version_conflicts_are_rejected(self) -> None:
        self.db.row["platform"] = "anthropic"
        self.assertEqual(self.put().status_code, 422)
        self.assertEqual([call for call in self.capture.calls if call[0] == "PUT"], [])

        self.db.row["platform"] = "openai"
        self.db.row = None
        self.assertEqual(self.put(expected_version="a" * 64).status_code, 404)
        self.assertEqual([call for call in self.capture.calls if call[0] == "PUT"], [])

        self.db.row = GroupDb().row
        self.assertEqual(self.put(expected_version="a" * 64).status_code, 409)
        self.assertEqual([call for call in self.capture.calls if call[0] == "PUT"], [])

    def test_timeout_reads_back_applied_membership_without_replaying_put(self) -> None:
        calls: list[tuple[str, str, dict]] = []

        async def timeout_after_apply(_client, method: str, path: str, **kwargs):
            calls.append((method, path, kwargs["json"]))
            self.db.row["group_ids"] = [11, 99]
            raise HTTPException(504, "请求超时，未重放操作")

        request = GroupsRequest(**self.payload())
        with patch.object(self.service.actions, "json_request", side_effect=timeout_after_apply):
            result = asyncio.run(self.service.actions.set_groups(1, request, "qa-admin"))
        self.assertTrue(result["verified"])
        self.assertEqual(result["group_ids"], [11, 99])
        self.assertEqual(len(calls), 1)
        self.auto_reset.cancel.assert_not_called()

    def test_upstream_success_without_readback_is_an_unconfirmed_error_without_replay(self) -> None:
        calls: list[tuple[str, str, dict]] = []

        async def success_without_apply(_client, method: str, path: str, **kwargs):
            calls.append((method, path, kwargs["json"]))
            return {}

        request = GroupsRequest(**self.payload())
        with patch.object(self.service.actions, "json_request", side_effect=success_without_apply):
            with self.assertRaises(HTTPException) as raised:
                asyncio.run(self.service.actions.set_groups(1, request, "qa-admin"))
        self.assertEqual(raised.exception.status_code, 502)
        self.assertIn("未确认", str(raised.exception.detail))
        self.assertEqual(len(calls), 1)
        self.assertEqual(self.db.row["group_ids"], [10, 99])
        self.auto_reset.cancel.assert_not_called()

    def test_request_schema_forbids_extra_fields_and_empty_scope(self) -> None:
        response = self.client.put(
            "/api/desktop/v1/accounts/1/groups",
            headers={"x-api-key": "qa-admin"},
            json={**self.payload(), "unexpected": True},
        )
        self.assertEqual(response.status_code, 422)
        response = self.put(scope_group_ids=[])
        self.assertEqual(response.status_code, 422)
        self.assertEqual([call for call in self.capture.calls if call[0] == "PUT"], [])


if __name__ == "__main__":
    unittest.main()
