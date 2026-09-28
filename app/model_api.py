from __future__ import annotations

import asyncio
from typing import Any

from fastapi import APIRouter, HTTPException, Request, Response
from pydantic import BaseModel, ConfigDict, Field

from .desktop_errors import DesktopRoute
from .model_catalog import ModelCatalogService


class Draft(BaseModel):
    model_config = ConfigDict(extra="forbid")
    allowlist: dict[str, Any]
    overrides: dict[str, dict[str, Any]]


class SaveAllowlist(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_version: str = Field(min_length=64, max_length=64)
    allowlist: dict[str, Any]


class SaveOverrides(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_version: str = Field(min_length=64, max_length=64)
    expected_revision: str = Field(min_length=64, max_length=64)
    overrides: dict[str, dict[str, Any]]


class ImportModel(BaseModel):
    model_config = ConfigDict(extra="forbid")
    model: str = Field(min_length=1, max_length=256)


def install_model_api(app, runtime, desktop) -> ModelCatalogService:
    service = ModelCatalogService(runtime)
    router = APIRouter(prefix="/api/desktop/v1", route_class=DesktopRoute)

    async def auth(request: Request) -> str:
        key = request.headers.get("x-api-key", "")
        await asyncio.to_thread(desktop.authenticate, key, fresh=request.method != "GET")
        return key

    async def call(function, *args):
        try:
            return await asyncio.to_thread(function, *args)
        except ValueError as exc:
            # Validation errors are controlled strings, never upstream bodies.
            raise HTTPException(422, str(exc)) from None

    @router.get("/model-groups")
    async def groups(request: Request):
        await auth(request)
        return await call(service.list_groups)

    @router.get("/model-groups/{group_id}")
    async def group(group_id: int, request: Request):
        key = await auth(request)
        return await call(service.read_group, group_id, key)

    @router.post("/model-groups/{group_id}/preview")
    async def preview(group_id: int, payload: Draft, request: Request):
        await auth(request)
        return await call(service.preview, group_id, payload.model_dump())

    @router.put("/model-groups/{group_id}/allowlist")
    async def save_allowlist(group_id: int, payload: SaveAllowlist, request: Request):
        key = await auth(request)
        return await call(service.save_allowlist, group_id, key, payload.model_dump())

    @router.put("/model-groups/{group_id}/overrides")
    async def save_overrides(group_id: int, payload: SaveOverrides, request: Request):
        await auth(request)
        return await call(service.save_overrides, group_id, payload.model_dump())

    @router.post("/model-groups/{group_id}/upstream-import")
    async def upstream_import(group_id: int, payload: ImportModel, request: Request):
        await auth(request)
        return await call(service.upstream_import, group_id, payload.model)

    @router.get("/model-catalog")
    async def catalog(request: Request):
        await auth(request)
        return await call(service.catalog)

    app.include_router(router)

    @app.get("/v1/models")
    @app.get("/models")
    @app.get("/backend-api/codex/models")
    async def proxy(request: Request):
        # nginx routes only Codex requests here. Keep the same constraint for
        # direct requests to this port; ordinary lists belong to Sub2API.
        if request.url.path != "/backend-api/codex/models" and not request.query_params.get("client_version"):
            raise HTTPException(404)
        status, headers, body = await asyncio.to_thread(service.proxy, request.url.path, request.url.query, dict(request.headers))
        return Response(body, status_code=status, headers=headers)

    return service
