import unittest

import httpx
from fastapi import APIRouter, FastAPI, HTTPException
from pydantic import BaseModel

from app.desktop_errors import DesktopErrorMiddleware, DesktopRoute


class Payload(BaseModel):
    count: int


class DesktopErrorTests(unittest.IsolatedAsyncioTestCase):
    async def test_errors_are_json_correlated_and_do_not_expose_inputs(self):
        app = FastAPI()
        router = APIRouter(prefix="/api/desktop/v1", route_class=DesktopRoute)
        @router.get("/snapshot")
        def crash(): raise RuntimeError("secret-password request body")
        @router.post("/config")
        def validate(payload: Payload): return payload
        @router.get("/auth")
        def auth(): raise HTTPException(401, "管理员 API Key 无效")
        app.include_router(router); app.add_middleware(DesktopErrorMiddleware)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://test") as client:
            with self.assertLogs("sub2ops.desktop", level="ERROR") as logs:
                failed = await client.get("/api/desktop/v1/snapshot?key=secret-query")
            self.assertEqual(failed.status_code, 500)
            self.assertEqual(failed.json()["detail"]["request_id"], failed.headers["x-request-id"])
            self.assertEqual(failed.headers["cache-control"], "no-store")
            self.assertNotIn("secret", failed.text + str(logs.output))
            invalid = await client.post("/api/desktop/v1/config", json={"count": "secret-input"})
            self.assertEqual(invalid.status_code, 422)
            self.assertNotIn("secret-input", invalid.text)
            auth = await client.get("/api/desktop/v1/auth")
            self.assertEqual(auth.status_code, 401)
            self.assertEqual(auth.json()["detail"], "管理员 API Key 无效")
            self.assertNotEqual(auth.headers["x-request-id"], failed.headers["x-request-id"])

    async def test_retired_endpoints_are_404(self):
        from app.main import app
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://test") as client:
            for path in ("/telegram", "/telegram/config", "/telegram/push-test", "/telegram/oauth-settings", "/telegram/pairing-code/regenerate"):
                for method in ("GET", "POST"):
                    self.assertEqual((await client.request(method, path)).status_code, 404, path)
            for action in ("telegram-pairing", "telegram-test"):
                self.assertEqual((await client.post("/api/desktop/v1/actions/" + action)).status_code, 404)
            response = await client.put("/api/desktop/v1/config/telegram", json={"changes": {}, "expected_revision": "a" * 64})
            self.assertEqual(response.status_code, 404)
