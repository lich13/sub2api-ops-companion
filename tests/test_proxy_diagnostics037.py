from __future__ import annotations

import asyncio
import json
import tempfile
import unittest
from contextlib import asynccontextmanager
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

import httpx
from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient

from app.desktop_api import install_desktop_api
from app.proxy_diagnostics import ProxyDiagnostics


ADMIN_KEY = "fixture-admin-key-037"


class _ProxyTransport(httpx.AsyncBaseTransport):
    def __init__(self, fixture: "_ProxyHttpFixture") -> None:
        self.fixture = fixture

    async def handle_async_request(self, request: httpx.Request) -> httpx.Response:
        body = await request.aread()
        self.fixture.requests.append({
            "method": request.method,
            "path": request.url.path,
            "headers": dict(request.headers),
            "body": body,
        })
        self.fixture.entered.set()
        if self.fixture.blocked:
            await self.fixture.release.wait()
        if self.fixture.timeout:
            raise httpx.ReadTimeout("fixture upstream timeout", request=request)
        action = request.url.path.rsplit("/", 1)[-1]
        status = self.fixture.status.get(action, 200)
        return httpx.Response(
            status,
            json={"code": 0, "data": self.fixture.payloads[action]},
            request=request,
        )


class _ProxyHttpFixture:
    def __init__(self) -> None:
        self.requests: list[dict] = []
        self.client_calls: list[tuple[str, float]] = []
        self.status = {"test": 200, "quality-check": 200}
        self.blocked = False
        self.timeout = False
        self.entered = asyncio.Event()
        self.release = asyncio.Event()
        self.payloads = {
            "test": {
                "success": True,
                "message": "代理连接正常",
                "latency_ms": 31,
                "ip_address": "203.0.113.7",
                "city": "Taipei",
                "region": "Taiwan",
                "country": "TW",
                "country_code": "TW",
                "credentials": "fixture-secret",
                "password": "fixture-password",
            },
            "quality-check": {
                "score": 88,
                "grade": "yellow",
                "summary": "一个目标需要关注",
                "exit_ip": "203.0.113.8",
                "country": "TW",
                "country_code": "TW",
                "base_latency_ms": 20,
                "passed_count": 1,
                "warn_count": 1,
                "failed_count": 0,
                "challenge_count": 0,
                "checked_at": 1791774000,
                "items": [{
                    "target": "example.invalid",
                    "status": "pass",
                    "http_status": 200,
                    "latency_ms": 20,
                    "message": "目标可达",
                    "cf_ray": "fixture-ray",
                    "token": "fixture-token",
                }],
                "credentials": "fixture-secret",
            },
        }

    @asynccontextmanager
    async def client(self, key: str, timeout: float = 30):
        self.client_calls.append((key, timeout))
        transport = _ProxyTransport(self)
        async with httpx.AsyncClient(
            base_url="https://fixture.invalid/api/v1/admin/",
            headers={"x-api-key": key, "X-Admin-UI-Request": "1"},
            transport=transport,
            timeout=timeout,
        ) as client:
            yield client


class ProxyDiagnostics037Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.http = _ProxyHttpFixture()
        self.diagnostics = ProxyDiagnostics(self.http)

    async def test_safe_proxy_test_forwards_native_action_with_bounded_timeout(self):
        result = await self.diagnostics.run(ADMIN_KEY, 7, "test")

        self.assertEqual(result, {
            "success": True,
            "message": "代理连接正常",
            "latency_ms": 31,
            "ip_address": "203.0.113.7",
            "city": "Taipei",
            "region": "Taiwan",
            "country": "TW",
            "country_code": "TW",
        })
        self.assertEqual(self.http.client_calls, [(ADMIN_KEY, 90)])
        self.assertEqual(len(self.http.requests), 1)
        request = self.http.requests[0]
        self.assertEqual((request["method"], request["path"]), ("POST", "/api/v1/admin/proxies/7/test"))
        self.assertEqual(json.loads(request["body"]), {})
        self.assertNotIn("fixture-secret", json.dumps(result))
        self.assertNotIn("fixture-password", json.dumps(result))

    async def test_quality_result_keeps_safe_items_and_checked_at_without_credentials(self):
        result = await self.diagnostics.run(ADMIN_KEY, 7, "quality-check")

        self.assertEqual(result["score"], 88)
        self.assertEqual(result["grade"], "yellow")
        self.assertEqual(result["summary"], "一个目标需要关注")
        self.assertEqual(result["exit_ip"], "203.0.113.8")
        self.assertEqual(result["base_latency_ms"], 20)
        self.assertEqual(result["passed_count"], 1)
        self.assertEqual(result["warn_count"], 1)
        self.assertEqual(result["failed_count"], 0)
        self.assertEqual(result["challenge_count"], 0)
        self.assertEqual(result["checked_at"], 1791774000)
        self.assertEqual(result["items"], [{
            "target": "example.invalid",
            "status": "pass",
            "http_status": 200,
            "latency_ms": 20,
            "message": "目标可达",
        }])
        self.assertNotIn("cf_ray", result["items"][0])
        encoded = json.dumps(result, ensure_ascii=False)
        self.assertNotIn("fixture-secret", encoded)
        self.assertNotIn("fixture-token", encoded)

    async def test_same_proxy_and_action_coalesces_while_different_action_is_independent(self):
        self.http.blocked = True
        first = asyncio.create_task(self.diagnostics.run(ADMIN_KEY, 7, "test"))
        await asyncio.wait_for(self.http.entered.wait(), 1)
        second = asyncio.create_task(self.diagnostics.run(ADMIN_KEY, 7, "test"))
        await asyncio.sleep(0)
        self.assertEqual(len(self.http.requests), 1)
        self.http.release.set()
        self.assertEqual(await first, await second)
        self.assertEqual(len(self.http.requests), 1)

        self.http.blocked = False
        await self.diagnostics.run(ADMIN_KEY, 7, "quality-check")
        self.assertEqual(len(self.http.requests), 2)
        self.assertEqual(self.http.requests[-1]["path"], "/api/v1/admin/proxies/7/quality-check")

    async def test_timeout_returns_without_replaying_the_native_request(self):
        self.http.timeout = True
        with self.assertRaises(HTTPException) as raised:
            await self.diagnostics.run(ADMIN_KEY, 7, "test")

        self.assertEqual(raised.exception.status_code, 504)
        self.assertIn("未重放", str(raised.exception.detail))
        self.assertEqual(len(self.http.requests), 1)
        self.assertEqual(self.http.client_calls, [(ADMIN_KEY, 90)])

    async def test_native_404_is_reported_as_an_unsupported_diagnostic(self):
        self.http.status["test"] = 404
        with self.assertRaises(HTTPException) as raised:
            await self.diagnostics.run(ADMIN_KEY, 7, "test")

        self.assertEqual(raised.exception.status_code, 502)
        self.assertIn("不支持", str(raised.exception.detail))
        self.assertEqual(len(self.http.requests), 1)


class ProxyDiagnosticsRoute037Tests(unittest.TestCase):
    def test_desktop_routes_authenticate_and_forward_quality_check(self):
        temp = tempfile.TemporaryDirectory(prefix="proxy-diagnostics-route-037-")
        settings = SimpleNamespace(
            usage_query_state_path=str(Path(temp.name) / "oauth-state.json"),
            audit_path=str(Path(temp.name) / "audit.jsonl"),
        )
        runtime = SimpleNamespace(db=Mock(), settings=settings, APP_VERSION="fixture-0.1.37")
        app = FastAPI()
        service = install_desktop_api(app, runtime)
        service.authenticate = Mock(return_value=None)
        http = _ProxyHttpFixture()
        service.proxy_diagnostics = ProxyDiagnostics(http)
        client = TestClient(app)
        try:
            response = client.post(
                "/api/desktop/v1/proxies/12/quality-check",
                headers={"x-api-key": ADMIN_KEY},
            )
            self.assertEqual(response.status_code, 200, response.text)
            self.assertEqual(response.json()["score"], 88)
            service.authenticate.assert_called_once_with(ADMIN_KEY, fresh=True)
            self.assertEqual(http.requests[0]["path"], "/api/v1/admin/proxies/12/quality-check")
        finally:
            client.close()
            asyncio.run(service.close())
            temp.cleanup()


if __name__ == "__main__":
    unittest.main()
