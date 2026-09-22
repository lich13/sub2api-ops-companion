from __future__ import annotations

import asyncio
import json
import secrets
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from zoneinfo import ZoneInfo

from . import account_ops
from .bark import sanitize_error_text
from .grok_quota import grok_quota_reply
from .settings import Settings
from .usage_query import (
    format_percent_value,
    oauth_has_available_seven_day,
    oauth_quota_summary_from_result,
    oauth_windows_by_key,
    parse_iso_datetime,
    percent_or_none,
)

BEIJING_TZ = ZoneInfo("Asia/Shanghai")
PAIRING_CODE_HINT = "请到 Ops 面板的 Telegram 页面查看配对码，然后在私聊中发送 /pair <配对码>。"
MAX_INFLIGHT_UPDATES = 8


class TelegramOpsBot:
    def __init__(
        self,
        settings: Settings,
        db: Any,
        *,
        oauth_monitor: Any | None = None,
    ) -> None:
        self.settings = settings
        self.db = db
        self.oauth_monitor = oauth_monitor
        self._state_lock = asyncio.Lock()
        self._quota_refresh_lock = asyncio.Lock()
        self._quota_refresh_task: asyncio.Task[str] | None = None

    @property
    def enabled(self) -> bool:
        return bool(self.settings.telegram_enabled and self.settings.telegram_bot_token.strip())

    async def run(self) -> None:
        if not self.enabled:
            return
        await self.sync_commands()
        offset = 0
        update_tasks: set[asyncio.Task[None]] = set()
        try:
            while True:
                try:
                    while len(update_tasks) >= MAX_INFLIGHT_UPDATES:
                        done, _pending = await asyncio.wait(
                            update_tasks, return_when=asyncio.FIRST_COMPLETED
                        )
                        update_tasks.difference_update(done)
                    response = await self._api(
                        "getUpdates",
                        {
                            "timeout": self.settings.telegram_poll_timeout_seconds,
                            "offset": offset,
                            "allowed_updates": ["message", "callback_query"],
                        },
                        timeout=self.settings.telegram_poll_timeout_seconds + 10,
                    )
                    for update in response.get("result", []):
                        offset = int(update.get("update_id", offset)) + 1
                        while len(update_tasks) >= MAX_INFLIGHT_UPDATES:
                            done, _pending = await asyncio.wait(
                                update_tasks, return_when=asyncio.FIRST_COMPLETED
                            )
                            update_tasks.difference_update(done)
                        task = asyncio.create_task(self._run_update(update))
                        update_tasks.add(task)
                        task.add_done_callback(update_tasks.discard)
                except asyncio.CancelledError:
                    raise
                except Exception:
                    await asyncio.sleep(3)
        finally:
            for task in update_tasks:
                task.cancel()
            if update_tasks:
                await asyncio.gather(*update_tasks, return_exceptions=True)

    async def _run_update(self, update: dict[str, Any]) -> None:
        try:
            await self._handle_update(update)
        except asyncio.CancelledError:
            raise
        except Exception:
            return

    async def sync_commands(self) -> None:
        if self.enabled:
            await self._api(
                "setMyCommands",
                {
                    "commands": [
                        {"command": "quota", "description": "查询 OAuth 账号额度"},
                    ]
                },
            )

    async def allowed_chat_ids(self) -> list[int]:
        state = await self._load_state()
        return unique_ints(
            list(self.settings.telegram_allowed_chat_ids) + list(state.get("paired_chat_ids") or [])
        )

    async def notify(self, text: str, keyboard: dict[str, Any] | None = None) -> None:
        if not self.enabled:
            return
        for chat_id in await self.allowed_chat_ids():
            await self._send_message(chat_id, text, keyboard)

    async def _handle_update(self, update: dict[str, Any]) -> None:
        callback = update.get("callback_query")
        if callback:
            callback_id = str(callback.get("id") or "")
            if callback_id:
                await self._api("answerCallbackQuery", {"callback_query_id": callback_id})
            message = callback.get("message") or {}
            chat = message.get("chat") or {}
            chat_id = int(chat.get("id") or 0)
            user_id = int((callback.get("from") or {}).get("id") or 0)
            if not chat_id or not await self._allowed(chat_id, user_id):
                if chat_id and str(chat.get("type") or "private") == "private":
                    await self._send_message(chat_id, f"未授权。{PAIRING_CODE_HINT}")
                return
            try:
                text, keyboard = await self._callback_reply(
                    chat_id, user_id, str(callback.get("data") or "")
                )
            except Exception as exc:
                text, keyboard = f"执行失败：{sanitize_error_text(exc)}", None
            if message.get("message_id"):
                try:
                    await self._api("editMessageReplyMarkup", {
                        "chat_id": chat_id, "message_id": message["message_id"],
                        "reply_markup": {"inline_keyboard": []},
                    })
                except Exception:
                    pass
            await self._send_message(chat_id, text, keyboard)
            return

        message = update.get("message") or {}
        text = str(message.get("text") or "").strip()
        chat = message.get("chat") or {}
        chat_id = int(chat.get("id") or 0)
        user_id = int((message.get("from") or {}).get("id") or 0)
        chat_type = str(chat.get("type") or "private")
        if not text or not chat_id:
            return
        if is_pair_command(text):
            reply, keyboard = await self._pair(chat_id, user_id, chat_type, text)
            await self._send_message(chat_id, reply, keyboard)
            return
        if not await self._allowed(chat_id, user_id):
            message_text = "未授权。请在私聊中和 Bot 交互。" if chat_type != "private" else f"未授权。{PAIRING_CODE_HINT}"
            await self._send_message(chat_id, message_text)
            return
        try:
            reply, keyboard = await self._text_reply(text)
        except Exception as exc:
            reply, keyboard = f"执行失败：{sanitize_error_text(exc)}", None
        await self._send_message(chat_id, reply, keyboard)

    async def _pair(
        self,
        chat_id: int,
        user_id: int,
        chat_type: str,
        text: str,
    ) -> tuple[str, dict[str, Any] | None]:
        if chat_type != "private":
            return "请在 Telegram 私聊中配对，群聊不会绑定。", None
        if await self._allowed(chat_id, user_id):
            return f"当前会话已配对。\nchat_id：{chat_id}\nuser_id：{user_id}", None
        if not self.settings.telegram_pairing_enabled:
            return "Telegram 配对已关闭。", None
        expected = normalize_pairing_code(self.settings.telegram_pairing_code)
        provided = normalize_pairing_code(pairing_code_from_text(text))
        if not expected:
            return f"当前没有可用配对码。{PAIRING_CODE_HINT}", None
        if not provided:
            return f"发送 /pair <配对码> 完成绑定。{PAIRING_CODE_HINT}", None
        if not secrets.compare_digest(provided, expected):
            return "配对码不正确或已失效。", None
        async with self._state_lock:
            state = await self._load_state_unlocked()
            state["paired_chat_ids"] = unique_ints(list(state.get("paired_chat_ids") or []) + [chat_id])
            state["paired_user_ids"] = unique_ints(list(state.get("paired_user_ids") or []) + [user_id])
            state["updated_at"] = datetime.now(timezone.utc).isoformat()
            await asyncio.to_thread(self._save_state_sync, state)
        return f"配对成功。\nchat_id：{chat_id}\nuser_id：{user_id}", None

    async def _text_reply(self, text: str) -> tuple[str, dict[str, Any] | None]:
        parts = text.split()
        command = normalize_command(parts[0] if parts else "")
        if command in {"/quota", "/usage", "quota", "usage", "额度", "查额度"}:
            return await self._quota_reply()
        if command in {"/start", "/help", "/menu", "menu", "菜单"}:
            return (
                "可用命令：\n"
                "/quota 查询 OAuth 账号额度\n"
                "OAuth 恢复成功、测活失败、自动恢复失败和认证异常由 Bark 推送。"
            ), None
        return "可用命令：/quota。", None

    async def _callback_reply(
        self,
        chat_id: int,
        user_id: int,
        data: str,
    ) -> tuple[str, dict[str, Any] | None]:
        return "账号操作已移除，请使用 /quota", None

    async def _openai_quota_reply(self) -> tuple[str, dict[str, Any] | None]:
        if self.oauth_monitor is None:
            return "OpenAI OAuth\n额度刷新失败：监控器未就绪。", None
        try:
            report = await asyncio.to_thread(self.oauth_monitor.force_refresh, 120)
        except TimeoutError:
            return "OAuth 额度刷新失败：等待 120 秒后超时。", None
        except Exception as exc:
            return f"OAuth 额度刷新失败：{sanitize_error_text(exc)}", None
        if report.get("timed_out"):
            return f"OAuth 额度刷新失败：{sanitize_error_text(report.get('error') or '等待 120 秒后超时')}。", None
        if report.get("error_code") == "missing_sub2api_admin_credentials":
            return "OAuth 额度刷新失败：缺少 Sub2API 地址或 Admin API Key。", None
        try:
            rows = await asyncio.to_thread(account_ops.current_oauth_accounts, self.db)
        except Exception as exc:
            return f"OAuth 额度刷新失败：读取账号清单失败（{sanitize_error_text(exc)}）。", None
        results = self.oauth_monitor.store.results()
        refresh_at = parse_iso_datetime(report.get("refresh_at"))
        lines: list[str] = []
        totals: dict[str, dict[str, float]] = {}
        counts: dict[str, int] = {}
        for row in rows:
            account_id = int(row.get("id") or 0)
            cached = results.get(account_id)
            if not cached or not cached.get("success"):
                continue
            queried_at = parse_iso_datetime(cached.get("queried_at"))
            if refresh_at is not None and (queried_at is None or queried_at < refresh_at):
                continue
            summary = oauth_quota_summary_from_result(row, cached)
            if not oauth_has_available_seven_day(summary):
                continue
            line = format_oauth_quota_line(row, summary)
            if not line:
                continue
            lines.append(line)
            add_oauth_quota_totals(totals, counts, summary)
        status = "完成" if report.get("success") else "部分失败"
        output = [
            "OpenAI OAuth",
            f"刷新：{status} · {bj_time(report.get('refresh_at'))}",
            (
                f"成功 {int(report.get('success_count') or 0)} / "
                f"失败 {int(report.get('failure_count') or 0)} / "
                f"耗尽 {int(report.get('depleted_count') or 0)} / "
                f"已恢复 {int(report.get('recovered_count') or 0)}"
            ),
        ]
        if not lines:
            output.append("本轮没有可展示的 OAuth 可用额度。")
            return "\n".join(output), None
        output.extend(format_oauth_quota_totals(totals, counts))
        return "\n".join(output) + "\n\n" + "\n\n".join(lines), None

    async def _quota_reply(self) -> tuple[str, dict[str, Any] | None]:
        async with self._quota_refresh_lock:
            task = self._quota_refresh_task
            if task is None or task.done():
                task = asyncio.create_task(self._refresh_platforms())
                self._quota_refresh_task = task
        return await asyncio.shield(task), None

    async def _refresh_platforms(self) -> str:
        async def openai() -> str:
            try:
                return (await self._openai_quota_reply())[0]
            except Exception as exc:
                return f"OpenAI OAuth\n查询失败：{sanitize_error_text(str(exc))}"

        async def grok() -> str:
            try:
                monitor = self.oauth_monitor
                base_url = monitor.base_url_provider() if monitor else ""
                token = await asyncio.to_thread(monitor.store.admin_token) if monitor else ""
                return await grok_quota_reply(self.db, base_url, token,
                    self.settings.telegram_oauth_usage_refresh_concurrency)
            except Exception as exc:
                return f"Grok OAuth\n查询失败：{sanitize_error_text(str(exc))}"

        tasks = {"OpenAI": asyncio.create_task(openai()), "Grok": asyncio.create_task(grok())}
        done, pending = await asyncio.wait(tasks.values(), timeout=120)
        for task in pending:
            task.cancel()
        if pending:
            await asyncio.gather(*pending, return_exceptions=True)
        return "\n\n".join(
            task.result() if task in done else f"{platform} OAuth\n查询超时（120 秒），未重复请求。"
            for platform, task in tasks.items()
        )

    async def _allowed(self, chat_id: int, user_id: int) -> bool:
        state = await self._load_state()
        chat_ids = unique_ints(
            list(self.settings.telegram_allowed_chat_ids) + list(state.get("paired_chat_ids") or [])
        )
        user_ids = unique_ints(
            list(self.settings.telegram_allowed_user_ids) + list(state.get("paired_user_ids") or [])
        )
        if not chat_ids and not user_ids:
            return False
        if chat_ids and chat_id not in chat_ids:
            return False
        return not user_ids or user_id in user_ids

    async def _load_state(self) -> dict[str, Any]:
        async with self._state_lock:
            return await self._load_state_unlocked()

    async def _load_state_unlocked(self) -> dict[str, Any]:
        return await asyncio.to_thread(self._load_state_sync)

    def _load_state_sync(self) -> dict[str, Any]:
        path = Path(self.settings.telegram_state_path)
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            raw = {}
        return {
            "paired_chat_ids": unique_ints(list(raw.get("paired_chat_ids") or [])),
            "paired_user_ids": unique_ints(list(raw.get("paired_user_ids") or [])),
            "updated_at": raw.get("updated_at"),
        }

    def _save_state_sync(self, state: dict[str, Any]) -> None:
        path = Path(self.settings.telegram_state_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "paired_chat_ids": unique_ints(list(state.get("paired_chat_ids") or [])),
            "paired_user_ids": unique_ints(list(state.get("paired_user_ids") or [])),
            "updated_at": state.get("updated_at") or datetime.now(timezone.utc).isoformat(),
        }
        path.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
        path.chmod(0o600)

    async def _send_message(
        self,
        chat_id: int,
        text: str,
        keyboard: dict[str, Any] | None = None,
    ) -> bool:
        success = True
        for part in split_messages(text):
            body: dict[str, Any] = {
                "chat_id": chat_id, "text": part, "disable_web_page_preview": True,
            }
            if keyboard:
                body["reply_markup"] = keyboard
            success = bool((await self._api("sendMessage", body)).get("ok")) and success
        return success

    async def _api(self, method: str, payload: dict[str, Any], timeout: int = 15) -> dict[str, Any]:
        return await asyncio.to_thread(self._api_sync, method, payload, timeout)

    def _api_sync(self, method: str, payload: dict[str, Any], timeout: int) -> dict[str, Any]:
        token = self.settings.telegram_bot_token.strip()
        if not token:
            return {"ok": False, "result": []}
        request = urllib.request.Request(
            f"https://api.telegram.org/bot{token}/{method}",
            data=json.dumps(payload).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return json.loads(response.read().decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError):
            return {"ok": False, "result": []}


def format_oauth_quota_line(row: dict[str, Any], summary: dict[str, Any]) -> str:
    windows = [
        item
        for item in summary.get("telegram_windows") or []
        if isinstance(item, dict) and (percent_or_none(item.get("remaining_percent")) or 0) > 0
    ]
    if not windows:
        return ""
    rendered = " / ".join(format_oauth_window(item) for item in windows)
    return (
        f"#{int(row.get('id') or 0)} {row.get('name') or '-'} · "
        f"{summary.get('plan_type') or 'oauth'}：{rendered}"
    )


def format_oauth_window(window: dict[str, Any]) -> str:
    text = f"{window.get('label') or '-'} 剩余 {format_percent_value(window.get('remaining_percent'))}"
    if window.get("reset_at"):
        label = "预计恢复时间" if window.get("reset_source") == "estimated_from_remaining" else "恢复时间"
        text += f"（{label} {bj_time(window.get('reset_at'), '%m-%d %H:%M')}）"
    return text


def add_oauth_quota_totals(
    totals: dict[str, dict[str, float]],
    counts: dict[str, int],
    summary: dict[str, Any],
) -> None:
    if not oauth_has_available_seven_day(summary):
        return
    plan = str(summary.get("plan_type") or "oauth")
    bucket = totals.setdefault(plan, {"codex_5h": 0.0, "codex_7d": 0.0})
    counts[plan] = counts.get(plan, 0) + 1
    for key, item in oauth_windows_by_key(summary.get("ui_windows")).items():
        remaining = percent_or_none(item.get("remaining_percent"))
        if remaining is not None and remaining > 0 and key in bucket:
            bucket[key] += abs(remaining)


def format_oauth_quota_totals(
    totals: dict[str, dict[str, float]], counts: dict[str, int]
) -> list[str]:
    lines = ["OAuth 账号总余量"]
    order = {"free": 0, "plus": 1, "pro": 2, "team": 3, "enterprise": 4, "oauth": 5}
    for plan in sorted(totals, key=lambda item: (order.get(item, 99), item)):
        bucket = totals[plan]
        parts: list[str] = []
        if plan != "free" and bucket.get("codex_5h", 0) > 0:
            parts.append(f"5h {format_percent_value(bucket['codex_5h'])}")
        if bucket.get("codex_7d", 0) > 0:
            parts.append(f"7d {format_percent_value(bucket['codex_7d'])}")
        if parts:
            lines.append(f"{plan}：{' · '.join(parts)}（{counts.get(plan, 0)} 个）")
    return lines


def bj_time(value: Any, pattern: str = "%Y-%m-%d %H:%M") -> str:
    if value in (None, ""):
        return "-"
    if isinstance(value, datetime):
        parsed = value
    else:
        try:
            parsed = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
        except ValueError:
            return str(value)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(BEIJING_TZ).strftime(pattern)


def is_pair_command(text: str) -> bool:
    command = normalize_command(text.split()[0] if text.split() else "")
    return command in {"/pair", "pair", "配对"}


def pairing_code_from_text(text: str) -> str:
    parts = text.split(maxsplit=1)
    return parts[1].strip() if len(parts) > 1 else ""


def normalize_pairing_code(value: str) -> str:
    return "".join(char for char in str(value or "").upper() if char.isalnum())


def normalize_command(command: str) -> str:
    text = str(command or "").strip().lower()
    if text.startswith("/") and "@" in text:
        text = text.split("@", 1)[0]
    return text


def unique_ints(values: list[Any]) -> list[int]:
    result: list[int] = []
    for value in values:
        try:
            parsed = int(value)
        except (TypeError, ValueError):
            continue
        if parsed not in result:
            result.append(parsed)
    return result


def split_messages(text: str, limit: int = 3800) -> list[str]:
    # Telegram counts UTF-16 units. Keep complete account blocks together.
    def size(value: str) -> int:
        return len(value.encode("utf-16-le")) // 2

    parts: list[str] = []
    current = ""
    for block in text.split("\n\n"):
        if size(block) > limit:
            if current:
                parts.append(current)
                current = ""
            fragment = ""
            units = 0
            for char in block:
                count = size(char)
                if units + count > limit:
                    parts.append(fragment)
                    fragment, units = "", 0
                fragment += char
                units += count
            current = fragment
        elif current and size(current) + 2 + size(block) > limit:
            parts.append(current)
            current = block
        else:
            current += ("\n\n" if current else "") + block
    if current:
        parts.append(current)
    return parts or ["-"]
