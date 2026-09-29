from __future__ import annotations

import asyncio
from typing import Literal

from fastapi import APIRouter, HTTPException, Request, Response
from pydantic import BaseModel, ConfigDict, Field

from .desktop_errors import DesktopRoute
from .model_catalog import ModelCatalogService

Effort = Literal["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"]


class ResolveModel(BaseModel):
    model_config = ConfigDict(extra="forbid")
    model: str = Field(min_length=1, max_length=256)
    efforts: list[Effort] | None = None
    default_effort: str = ""


class ModelVersion(BaseModel):
    model_config = ConfigDict(extra="forbid")
    model: str = Field(min_length=1, max_length=256)
    expected_version: str = Field(min_length=64, max_length=64)
    expected_revision: str = Field(min_length=64, max_length=64)


class SaveModel(ModelVersion):
    expected_binding: str = Field(min_length=64, max_length=64)
    efforts: list[Effort] = Field(min_length=1, max_length=8)
    default_effort: Effort
    confirm_allowlist: bool = False


def install_model_api(app, runtime, desktop) -> ModelCatalogService:
    service = ModelCatalogService(runtime)
    router = APIRouter(prefix="/api/desktop/v1", route_class=DesktopRoute)

    async def auth(request: Request) -> str:
        key = request.headers.get("x-api-key", "")
        await asyncio.to_thread(desktop.authenticate, key, fresh=request.method != "GET")
        return key

    async def call(function, *args):
        try:
            result = await asyncio.to_thread(function, *args)
            # Internal descriptor material never crosses the desktop boundary.
            return {k: v for k, v in result.items() if not k.startswith("_")}
        except ValueError as exc:
            raise HTTPException(422, str(exc)) from None

    @router.get("/model-groups")
    async def groups(request: Request):
        await auth(request)
        return await call(service.list_groups)

    @router.get("/model-groups/{group_id}/reasoning")
    async def group(group_id: int, request: Request):
        key = await auth(request)
        return await call(service.read_reasoning, group_id, key)

    @router.post("/model-groups/{group_id}/reasoning/resolve")
    async def resolve(group_id: int, payload: ResolveModel, request: Request):
        key = await auth(request)
        return await call(service.resolve_reasoning, group_id, key, payload.model_dump())

    @router.put("/model-groups/{group_id}/reasoning")
    async def save(group_id: int, payload: SaveModel, request: Request):
        key = await auth(request)
        return await call(service.save_reasoning, group_id, key, payload.model_dump())

    @router.delete("/model-groups/{group_id}/reasoning")
    async def remove(group_id: int, payload: ModelVersion, request: Request):
        key = await auth(request)
        return await call(service.remove_reasoning, group_id, key, payload.model_dump())

    app.include_router(router)

    @app.get("/v1/models")
    @app.get("/models")
    @app.get("/backend-api/codex/models")
    async def proxy(request: Request):
        if request.url.path != "/backend-api/codex/models" and not request.query_params.get("client_version"):
            raise HTTPException(404)
        status, headers, body = await asyncio.to_thread(service.proxy, request.url.path, request.url.query, dict(request.headers))
        return Response(body, status_code=status, headers=headers)

    return service
