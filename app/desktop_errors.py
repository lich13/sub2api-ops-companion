"""Safe JSON failures and request correlation for the desktop boundary."""
from __future__ import annotations

import logging
import re
import uuid

from fastapi import HTTPException
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from fastapi.routing import APIRoute

LOG = logging.getLogger("sub2ops.desktop")


class DesktopRoute(APIRoute):
    def get_route_handler(self):
        handler = super().get_route_handler()

        async def safe_handler(request):
            try:
                return await handler(request)
            except RequestValidationError:
                return JSONResponse({"detail": {"code": "invalid_request", "message": "请求参数无效，请刷新后重试"}}, status_code=422)
            except HTTPException as exc:
                return JSONResponse({"detail": exc.detail}, status_code=exc.status_code, headers=exc.headers)
        return safe_handler


class DesktopErrorMiddleware:
    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        path = scope.get("path", "")
        if scope["type"] != "http" or not path.startswith("/api/desktop/v1/"):
            return await self.app(scope, receive, send)
        request_id = uuid.uuid4().hex
        started = False

        async def correlated(message):
            nonlocal started
            if message["type"] == "http.response.start":
                started = True
                headers = [(k, v) for k, v in message.get("headers", []) if k.lower() not in {b"x-request-id", b"cache-control"}]
                message = {**message, "headers": headers + [(b"x-request-id", request_id.encode()), (b"cache-control", b"no-store")]}
            await send(message)

        try:
            await self.app(scope, receive, correlated)
        except Exception as exc:
            route = re.sub(r"/\d+(?=/|$)", "/:id", path)
            if not re.fullmatch(r"/api/desktop/v1/[a-zA-Z0-9_/:\-]+", route):
                route = "/api/desktop/v1/unknown"
            LOG.error("desktop_failure request_id=%s path=%s stage=%s type=%s", request_id, route,
                      "body" if started else "response", type(exc).__name__)
            if started:
                raise
            await JSONResponse({"detail": {"code": "internal_error", "message": "服务暂不可用", "request_id": request_id}},
                               status_code=500)(scope, receive, correlated)
