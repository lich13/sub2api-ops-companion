"""Explicit desktop operations; polling never enters this module's write paths."""
from __future__ import annotations

import asyncio
import base64
import json
import time
import uuid
from urllib.parse import urlsplit
from datetime import datetime, timezone
from typing import Any, Literal

import httpx
from fastapi import HTTPException
from pydantic import BaseModel, ConfigDict, Field, StrictBool, StrictInt

from .audit import write_audit
from .bark import sanitize_error_text
from .usage_query import execute_oauth_usage_query, parse_iso_datetime


def billing_is_fresh(data: dict[str, Any], queried_at: str) -> bool:
    billing = data.get("grok_billing")
    if not isinstance(billing, dict) or billing.get("partial") or billing.get("failed_windows"):
        return False
    fetched = parse_iso_datetime(billing.get("fetched_at"))
    requested = parse_iso_datetime(queried_at)
    return bool(fetched and requested and fetched >= requested.replace(microsecond=0))


def validate_media_data_url(value: str, kind: str) -> None:
    if not value:
        return
    header, separator, encoded = value.partition(",")
    allowed = {"image/png", "image/jpeg", "image/webp", "image/gif"} if kind == "image" else None
    if not separator or not header.startswith(f"data:{kind}/") or ";base64" not in header:
        raise HTTPException(422, "素材必须是有效的图片或音频文件")
    media_type = header[5:].split(";", 1)[0].lower()
    if allowed is not None and media_type not in allowed:
        raise HTTPException(422, "图片格式仅支持 PNG、JPEG、WebP 或 GIF")
    try:
        size = len(base64.b64decode(encoded, validate=True))
    except (ValueError, base64.binascii.Error):
        raise HTTPException(422, "素材编码无效") from None
    if size > (4 if kind == "image" else 8) * 1024 * 1024:
        raise HTTPException(422, "素材超过大小限制")


def stamp() -> str:
    return datetime.now(timezone.utc).isoformat()


class PriorityRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    priority: StrictInt = Field(ge=0, le=2147483647)
    expected_version: str = Field(min_length=64, max_length=64)


class TestRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_version: str = Field(min_length=64, max_length=64)
    confirmed: StrictBool = False
    model_id: str = Field(default="", max_length=200)
    mode: Literal["default", "compact", "text", "image", "video", "search", "tts", "stt", "realtime"] = "default"
    prompt: str = Field(default="", max_length=16000)
    image_data_url: str = Field(default="", max_length=16_777_216)
    audio_data_url: str = Field(default="", max_length=16_777_216)


class DesktopActions:
    def __init__(self, service: Any) -> None:
        self.s = service
        self.batch: dict[str, Any] | None = None
        self.batch_task: asyncio.Task | None = None
        self.batch_lock = asyncio.Lock()

    def client(self, key: str, timeout: float = 30) -> httpx.AsyncClient:
        return httpx.AsyncClient(base_url=self.s.r.oauth_base_url().rstrip("/") + "/api/v1/admin/",
            headers={"x-api-key": key, "X-Admin-UI-Request": "1", "Accept": "application/json"},
            timeout=httpx.Timeout(timeout, connect=5), follow_redirects=False, trust_env=False)

    async def json_request(self, client: httpx.AsyncClient, method: str, path: str, **kwargs: Any) -> Any:
        try:
            response = await client.request(method, path, **kwargs)
            if not 200 <= response.status_code < 300:
                raise HTTPException(502, f"Sub2API 请求失败 [http_{response.status_code}]")
            if len(response.content) > 8_000_000:
                raise HTTPException(502, "Sub2API 响应过大")
            body = response.json()
            if not isinstance(body, dict) or body.get("code") != 0:
                raise HTTPException(502, "Sub2API 未确认操作成功")
            return body.get("data")
        except httpx.TimeoutException:
            raise HTTPException(504, "请求超时，未重放操作") from None
        except (httpx.HTTPError, ValueError):
            raise HTTPException(502, "无法读取 Sub2API 响应") from None

    async def account(self, account_id: int, version: str | None = None) -> dict[str, Any]:
        from .desktop_api import ACCOUNT_SQL, account_dto
        row = await asyncio.to_thread(self.s.r.db.fetch_one, ACCOUNT_SQL.format(filter="AND a.id=%(id)s"), {"id": account_id})
        if not row:
            raise HTTPException(404, "账号不存在")
        if version and version != account_dto(row, datetime.now(timezone.utc), set())["version"]:
            raise HTTPException(409, "账号已变化，请刷新后重试")
        return row

    async def set_priority(self, account_id: int, payload: PriorityRequest, key: str) -> dict[str, Any]:
        lock = self.s.account_lock(account_id)
        if not lock.acquire(blocking=False):
            raise HTTPException(409, "此账号正在执行操作")
        try:
            row = await self.account(account_id, payload.expected_version)
            async with self.client(key, 10) as client:
                await self.json_request(client, "PUT", f"accounts/{account_id}", json={"priority": payload.priority})
            live = await self.account(account_id)
            if live.get("priority") != payload.priority or (live["platform"], live["type"]) != (row["platform"], row["type"]):
                raise HTTPException(502, "优先级写入未确认，请刷新查看实际值")
            write_audit(self.s.r.settings.audit_path, "desktop_priority", {"account_id": account_id, "priority": payload.priority})
            return {"verified": True, "priority": payload.priority}
        finally:
            lock.release()
            await asyncio.to_thread(self.s.invalidate)

    async def models(self, account_id: int, key: str) -> list[dict[str, str]]:
        await self.account(account_id)
        async with self.client(key) as client:
            data = await self.json_request(client, "GET", f"accounts/{account_id}/models")
        if not isinstance(data, list):
            raise HTTPException(502, "模型列表格式无效")
        return [{field: sanitize_error_text(str(item.get(field) or ""), limit=200)
                 for field in ("id", "display_name", "type")}
                for item in data[:1000] if isinstance(item, dict) and isinstance(item.get("id"), str)]

    async def prepare_test(self, account_id: int, payload: TestRequest) -> tuple[Any, Any]:
        if not payload.confirmed:
            raise HTTPException(409, "请确认本次测试会发送真实模型请求")
        lock = self.s.account_lock(account_id)
        if not lock.acquire(blocking=False):
            raise HTTPException(409, "此账号正在执行操作")
        monitor_lock = None
        try:
            row = await self.account(account_id, payload.expected_version)
            platform = row["platform"]
            if platform not in {"openai", "grok"} or row["type"] not in {"oauth", "apikey"}:
                raise HTTPException(422, "此账号不支持连接测试")
            if platform == "openai" and payload.mode not in {"default", "compact"}:
                raise HTTPException(422, "OpenAI 测试模式无效")
            if platform == "grok" and payload.mode not in {"text", "image", "video", "search", "tts", "stt", "realtime"}:
                raise HTTPException(422, "Grok 测试模式无效")
            for value, prefix in ((payload.image_data_url, "data:image/"), (payload.audio_data_url, "data:audio/")):
                if value and (not value.startswith(prefix) or ";base64," not in value):
                    raise HTTPException(422, "素材必须是有效的图片或音频文件")
            validate_media_data_url(payload.image_data_url, "image")
            validate_media_data_url(payload.audio_data_url, "audio")
            openai_image = platform == "openai" and payload.mode == "default" and payload.model_id.startswith("gpt-image-")
            if payload.image_data_url and not (openai_image or (platform == "grok" and payload.mode in {"image", "video"})):
                raise HTTPException(422, "当前模式不接受图片素材")
            if payload.audio_data_url and (platform != "grok" or payload.mode != "stt"):
                raise HTTPException(422, "当前模式不接受音频素材")
            monitor = getattr(self.s.r, "oauth_monitor", None)
            if platform == "openai" and monitor:
                monitor_lock = monitor._run_lock
                if not monitor_lock.acquire(blocking=False):
                    monitor_lock = None
                    raise HTTPException(409, "OAuth 查询或恢复正在进行，请稍后测试")
            return lock, monitor_lock
        except BaseException:
            lock.release()
            raise

    async def test_stream(self, account_id: int, payload: TestRequest, key: str, locks: tuple[Any, Any]):
        from .desktop_api import safe_error_text
        started = time.monotonic()
        completed = False
        def encode(event: dict[str, Any]) -> str:
            return "data: " + json.dumps(event, ensure_ascii=False) + "\n\n"
        try:
            data = payload.model_dump(exclude={"expected_version", "confirmed"}, exclude_defaults=False)
            if payload.mode in {"search", "tts", "stt", "realtime"}:
                data["model_id"] = ""
            async with asyncio.timeout(300), self.client(key, 90) as client:
                async with client.stream("POST", f"accounts/{account_id}/test", json=data,
                                         headers={"Accept": "text/event-stream"}) as response:
                    if response.status_code != 200:
                        yield encode({"type": "error", "error": f"测试请求失败 [http_{response.status_code}]"})
                        return
                    total = 0
                    async for line in response.aiter_lines():
                        total += len(line)
                        if total > 134_217_728:
                            yield encode({"type": "error", "error": "测试输出超过大小限制"})
                            return
                        if not line.startswith("data:"):
                            continue
                        try:
                            event = json.loads(line[5:].strip())
                        except ValueError:
                            continue
                        if not isinstance(event, dict):
                            continue
                        kind = event.get("type")
                        if kind not in {"test_start", "status", "content", "image", "audio", "video", "test_complete", "error"}:
                            continue
                        value: dict[str, Any] = {"type": kind}
                        for field in ("text", "model", "error"):
                            if field in event:
                                value[field] = safe_error_text(event[field], 16000)
                        if kind in {"image", "audio", "video"}:
                            media = str(event.get(f"{kind}_url") or "")
                            remote = urlsplit(media) if not media.startswith("data:") else None
                            valid_remote = remote and remote.scheme == "https" and remote.hostname and not remote.username and not remote.password
                            if (not media.startswith(f"data:{kind}/") and not valid_remote) or len(media) > 90_000_000:
                                yield encode({"type": "error", "error": "上游返回了不支持的媒体格式"})
                                return
                            value[f"{kind}_url"] = media
                            value["mime_type"] = sanitize_error_text(str(event.get("mime_type") or ""), limit=80)
                        if kind == "test_complete":
                            value["success"] = event.get("success") is True
                            completed = True
                        if kind in {"test_complete", "error"}:
                            completed = True
                            value.update(completed_at=stamp(), duration_ms=round((time.monotonic()-started)*1000))
                        yield encode(value)
                    if not completed:
                        yield encode({"type": "error", "error": "连接结束，未收到测试完成结果"})
        except (TimeoutError, httpx.TimeoutException):
            yield encode({"type": "error", "error": "测试超时，未重放请求"})
        except httpx.HTTPError:
            yield encode({"type": "error", "error": "测试连接中断，未重放请求"})
        finally:
            for lock in reversed(locks):
                if lock:
                    lock.release()
            await asyncio.to_thread(self.s.invalidate)

    def batch_view(self) -> dict[str, Any]:
        return json.loads(json.dumps(self.batch)) if self.batch else {"status": "idle", "items": [], "total": 0, "completed": 0}

    async def start_batch(self, key: str) -> dict[str, Any]:
        async with self.batch_lock:
            if self.batch_task and not self.batch_task.done():
                return self.batch_view()
            from .desktop_api import ACCOUNT_SQL
            rows = await asyncio.to_thread(self.s.r.db.fetch_all, ACCOUNT_SQL.format(
                filter="AND a.platform IN ('openai','grok') AND a.type='oauth'"))
            self.batch = {"id": uuid.uuid4().hex, "status": "running", "started_at": stamp(), "completed_at": None,
                          "total": len(rows), "completed": 0, "items": [
                              {"account_id": row["id"], "account_name": row["name"], "platform": row["platform"], "status": "pending"}
                              for row in rows]}
            self.batch_task = asyncio.create_task(self._batch(rows, key))
            return self.batch_view()

    async def _batch(self, rows: list[dict[str, Any]], key: str) -> None:
        batch = self.batch
        if batch is None:
            return
        limit = max(1, min(16, self.s.r.settings.telegram_oauth_usage_refresh_concurrency))
        monitor = getattr(self.s.r, "oauth_monitor", None)
        async def platform_run(platform: str):
            platform_rows = [r for r in rows if r["platform"] == platform]
            if not platform_rows:
                return
            monitor_lock = monitor._run_lock if platform == "openai" and monitor else None
            acquired = False
            try:
                if monitor_lock:
                    while not monitor_lock.acquire(blocking=False):
                        await asyncio.sleep(0.05)
                    acquired = True
                semaphore = asyncio.Semaphore(limit)
                async with self.client(key) as client:
                    async def query(row: dict[str, Any]):
                        item = next(i for i in batch["items"] if i["account_id"] == row["id"])
                        async with semaphore:
                            lock = self.s.account_lock(row["id"])
                            if not lock.acquire(blocking=False):
                                item.update(status="failed", error="此账号正在执行其他操作", error_code="account_busy")
                                batch["completed"] += 1
                                return
                            queried_at = stamp()
                            try:
                                live = await self.account(row["id"])
                                if (live["platform"], live["type"]) != (platform, "oauth"):
                                    raise HTTPException(409, "账号类型已变化")
                                item["status"] = "running"
                                async with asyncio.timeout(30):
                                    data = await self.json_request(client, "GET", f"accounts/{row['id']}/usage?source=active&force=true")
                                live = await self.account(row["id"])
                                if (live["platform"], live["type"]) != (platform, "oauth"):
                                    raise HTTPException(409, "账号已删除或类型变化，结果未采用")
                                if not isinstance(data, dict):
                                    raise HTTPException(502, "额度响应格式无效")
                                if platform == "openai" and monitor:
                                    result = execute_oauth_usage_query(row["id"], self.s.r.oauth_base_url(), key,
                                        account_row={**live, "credentials": {"plan_type": live.get("quota_plan_type")}},
                                        opener=lambda *_: {"code": 0, "data": data}, now=parse_iso_datetime(queried_at))
                                    if not result.get("success"):
                                        raise HTTPException(502, "OpenAI 额度响应不完整")
                                    await asyncio.to_thread(monitor.store.commit, results={row["id"]: result}, scheduler_updates={row["id"]: {
                                        "last_success_at": queried_at, "last_error_code": ""}})
                                if platform == "grok" and not billing_is_fresh(data, queried_at):
                                    item.update(status="partial", error="没有完整的新鲜账单窗口", error_code="partial_billing")
                                else:
                                    item["status"] = "success"
                                item["queried_at"] = queried_at
                            except (HTTPException, TimeoutError, httpx.HTTPError) as exc:
                                reason = str(exc.detail) if isinstance(exc, HTTPException) else "查询超时"
                                item.update(status="failed", error=sanitize_error_text(reason, limit=200), error_code="quota_query_failed")
                                if platform == "openai" and monitor:
                                    await asyncio.to_thread(monitor.store.commit, scheduler_updates={row["id"]: {
                                        "last_error_at": stamp(), "last_error_code": "quota_query_failed"}})
                            except asyncio.CancelledError:
                                item.update(status="failed", error="整轮查询超时，未重复请求", error_code="batch_timeout")
                                raise
                            finally:
                                lock.release()
                                batch["completed"] += 1
                                await asyncio.to_thread(self.s.invalidate)
                    await asyncio.gather(*(query(row) for row in platform_rows))
            finally:
                if acquired:
                    monitor_lock.release()
        try:
            async with asyncio.timeout(120):
                await asyncio.gather(platform_run("openai"), platform_run("grok"))
        except (TimeoutError, asyncio.CancelledError):
            pass
        except Exception:
            # Never include request URLs or headers in background task diagnostics.
            write_audit(self.s.r.settings.audit_path, "desktop_quota_batch_error", {"code": "batch_failed"})
        finally:
            for item in batch["items"]:
                if item["status"] in {"pending", "running"}:
                    item.update(status="failed", error="本轮未取得结果，未重复请求", error_code="batch_incomplete")
            batch.update(status="completed", completed=len(batch["items"]), completed_at=stamp())
            await asyncio.to_thread(self.s.invalidate)

    async def close(self):
        if self.batch_task and not self.batch_task.done():
            self.batch_task.cancel()
            await asyncio.gather(self.batch_task, return_exceptions=True)
