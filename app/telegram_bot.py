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
from .oauth_monitor import OAuthStateStore
from .settings import Settings
from .usage_query import (
    format_percent_value,
    oauth_has_available_seven_day,
    oauth_quota_summary_from_result,
    oauth_windows_by_key,
    percent_or_none,
)


BEIJING_TZ = ZoneInfo("Asia/Shanghai")
PAIRING_CODE_HINT = "请到 Ops 面板的 Telegram 页面查看配对码，然后在私聊中发送 /pair <配对码>。"


class TelegramOpsBot:
    def __init__(self, settings: Settings, db: Any) -> None:
        self.settings = settings
        self.db = db
        self._state_lock = asyncio.Lock()

    @property
    def enabled(self) -> bool:
        return bool(self.settings.telegram_enabled and self.settings.telegram_bot_token.strip())

    async def run(self) -> None:
        if not self.enabled:
            return
        await self.sync_commands()
        offset = 0
        while True:
            try:
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
                    await self._handle_update(update)
            except asyncio.CancelledError:
                raise
            except Exception:
                await asyncio.sleep(3)

    async def sync_commands(self) -> None:
        if self.enabled:
            await self._api(
                "setMyCommands",
                {"commands": [{"command": "quota", "description": "查询 OAuth 账号额度"}]},
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

    async def notify_oauth_monitor_events(self, events: list[dict[str, Any]]) -> list[dict[str, Any]]:
        if not self.enabled or not getattr(self.settings, "telegram_oauth_recovery_push_enabled", True):
            return []
        chat_ids = await self.allowed_chat_ids()
        if not chat_ids:
            return []
        delivered: list[dict[str, Any]] = []
        for event in events:
            keyboard = account_actions_keyboard(event) if int(event.get("account_id") or 0) > 0 else None
            sent = [
                await self._send_message(chat_id, oauth_monitor_alert(event), keyboard)
                for chat_id in chat_ids
            ]
            if sent and all(sent):
                delivered.append(event)
        return delivered

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
                text, keyboard = f"执行失败：{exc}", None
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
            reply, keyboard = f"执行失败：{exc}", None
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
        command = normalize_command(text.split()[0] if text.split() else "")
        if command in {"/quota", "/usage", "quota", "usage", "额度", "查额度"}:
            return await self._quota_reply()
        if command in {"/start", "/help", "/menu", "menu", "菜单"}:
            return "可用命令：/quota。\n\nOAuth 额度恢复、测活失败和认证异常会按面板开关推送。", None
        return "可用命令：/quota。", None

    async def _callback_reply(
        self,
        chat_id: int,
        user_id: int,
        data: str,
    ) -> tuple[str, dict[str, Any] | None]:
        actor_name = actor(chat_id, user_id)
        if data.startswith("acct:"):
            return await self._account_detail_reply(parse_account_id(data.split(":", 1)[1]))
        if data.startswith(("pauseask:", "pause:")):
            return await self._pause_reply(parse_account_id(data.split(":", 1)[1]), actor_name)
        if data.startswith(("resask:", "res:")):
            return await self._resume_reply(parse_account_id(data.split(":", 1)[1]), actor_name)
        if data.startswith("cdmenu:"):
            return await self._cooldown_menu_reply(parse_account_id(data.split(":", 1)[1]))
        if data.startswith("cd:"):
            _, account_raw, minutes_raw = (data.split(":") + ["", "15"])[:3]
            return await self._cooldown_reply(
                parse_account_id(account_raw), parse_minutes(minutes_raw, 15), actor_name
            )
        return "无法识别这个按钮，可能来自旧消息。", None

    async def _account_detail_reply(self, account_id: int) -> tuple[str, dict[str, Any] | None]:
        row = await asyncio.to_thread(self._account_detail, account_id)
        if not row:
            return f"没有找到账号 #{account_id}", None
        return account_detail(row), account_actions_keyboard(row)

    async def _pause_reply(self, account_id: int, actor_name: str) -> tuple[str, dict[str, Any] | None]:
        row = await asyncio.to_thread(
            account_ops.pause_account,
            self.db,
            self.settings.audit_path,
            account_id,
            actor_name,
            f"telegram pause by {actor_name}",
        )
        if not row:
            return f"暂停失败：没有找到账号 #{account_id}", None
        detail = await asyncio.to_thread(self._account_detail, account_id)
        return f"已暂停账号。\n\n{account_detail(detail or row)}", account_actions_keyboard(detail or row)

    async def _resume_reply(self, account_id: int, actor_name: str) -> tuple[str, dict[str, Any] | None]:
        row = await asyncio.to_thread(
            account_ops.resume_account,
            self.db,
            self.settings.audit_path,
            account_id,
            actor_name,
        )
        if not row:
            return f"恢复失败：没有找到账号 #{account_id}", None
        detail = await asyncio.to_thread(self._account_detail, account_id)
        return f"已恢复账号。\n\n{account_detail(detail or row)}", account_actions_keyboard(detail or row)

    async def _cooldown_menu_reply(self, account_id: int) -> tuple[str, dict[str, Any] | None]:
        row = await asyncio.to_thread(self._account_detail, account_id)
        if not row:
            return f"没有找到账号 #{account_id}", None
        return f"选择冷却时间\n\n{account_detail(row)}", cooldown_keyboard(account_id)

    async def _cooldown_reply(
        self, account_id: int, minutes: int, actor_name: str
    ) -> tuple[str, dict[str, Any] | None]:
        row = await asyncio.to_thread(
            account_ops.cooldown_account,
            self.db,
            self.settings.audit_path,
            account_id,
            actor_name,
            minutes,
            f"telegram cooldown {minutes}m by {actor_name}",
        )
        if not row:
            return f"冷却失败：没有找到账号 #{account_id}", None
        detail = await asyncio.to_thread(self._account_detail, account_id)
        return f"已冷却账号 {minutes} 分钟。\n\n{account_detail(detail or row)}", account_actions_keyboard(detail or row)

    def _account_detail(self, account_id: int) -> dict[str, Any] | None:
        return account_ops.fallback_account(self.db, account_id)

    async def _quota_reply(self) -> tuple[str, dict[str, Any] | None]:
        try:
            rows = await asyncio.to_thread(account_ops.current_oauth_accounts, self.db)
        except Exception:
            rows = []
        results = OAuthStateStore(self.settings.usage_query_state_path).results()
        lines: list[str] = []
        totals: dict[str, dict[str, float]] = {}
        counts: dict[str, int] = {}
        for row in rows:
            account_id = int(row.get("id") or 0)
            cached = results.get(account_id)
            if not cached or not cached.get("success"):
                continue
            summary = oauth_quota_summary_from_result(row, cached)
            if not oauth_has_available_seven_day(summary):
                continue
            line = format_oauth_quota_line(row, summary)
            if not line:
                continue
            lines.append(line)
            add_oauth_quota_totals(totals, counts, summary)
        if not lines:
            return "暂无可用的 OAuth 额度快照。", None
        output = ["OAuth 额度", *format_oauth_quota_totals(totals, counts), *lines]
        return "\n".join(output), None

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
        body: dict[str, Any] = {
            "chat_id": chat_id,
            "text": truncate(text, 3800),
            "disable_web_page_preview": True,
        }
        if keyboard:
            body["reply_markup"] = keyboard
        return bool((await self._api("sendMessage", body)).get("ok"))

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


def account_actions_keyboard(row: dict[str, Any]) -> dict[str, Any]:
    account_id = int(row.get("id") or row.get("account_id") or 0)
    return {
        "inline_keyboard": [
            [
                {"text": "暂停", "callback_data": f"pause:{account_id}"},
                {"text": "冷却", "callback_data": f"cdmenu:{account_id}"},
                {"text": "恢复", "callback_data": f"res:{account_id}"},
            ],
            [{"text": "查看账号", "callback_data": f"acct:{account_id}"}],
        ]
    }


def cooldown_keyboard(account_id: int) -> dict[str, Any]:
    return {
        "inline_keyboard": [
            [
                {"text": "5 分钟", "callback_data": f"cd:{account_id}:5"},
                {"text": "15 分钟", "callback_data": f"cd:{account_id}:15"},
                {"text": "30 分钟", "callback_data": f"cd:{account_id}:30"},
            ],
            [{"text": "返回", "callback_data": f"acct:{account_id}"}],
        ]
    }


def account_detail(row: dict[str, Any]) -> str:
    account_id = int(row.get("id") or row.get("account_id") or 0)
    lines = [
        f"#{account_id} {row.get('name') or '-'}",
        f"平台 / 类型：{row.get('platform') or '-'} / {row.get('type') or '-'}",
        f"调度：{account_ops.account_state(row)}",
    ]
    if row.get("temp_unschedulable_until"):
        lines.append(f"冷却到：{bj_time(row.get('temp_unschedulable_until'))}")
    if row.get("temp_unschedulable_reason"):
        lines.append(f"原因：{row.get('temp_unschedulable_reason')}")
    return "\n".join(lines)


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
        text += f"（恢复 {bj_time(window.get('reset_at'), '%m-%d %H:%M')}）"
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


def oauth_monitor_alert(event: dict[str, Any]) -> str:
    account_text = f"#{int(event.get('account_id') or 0)} {event.get('account_name') or '-'} · {event.get('plan_type') or 'oauth'}"
    status = str(event.get("status") or "")
    if status == "recovered":
        windows = "/".join(str(item) for item in event.get("window_labels") or []) or "必要窗口"
        return (
            "OAuth 账号额度已恢复可用\n"
            f"{account_text}\n"
            f"确认窗口：{windows}\n"
            f"测试通过：{event.get('model_id') or '-'}"
        )
    if status == "test_failed":
        return (
            "OAuth 账号额度恢复后测试失败\n"
            f"{account_text}\n"
            f"错误码：{event.get('error_code') or 'unknown_test_error'}\n"
            f"错误：{event.get('error') or '-'}\n"
            f"模型：{event.get('model_id') or '-'}"
        )
    stage = "active usage" if event.get("stage") == "active_usage" else str(event.get("stage") or "unknown")
    return (
        "OAuth 账号认证异常\n"
        f"{account_text}\n"
        f"阶段：{stage}\n"
        f"错误码：{event.get('error_code') or '-'}\n"
        f"错误：{event.get('error') or '-'}\n"
        f"时间：{bj_time(event.get('checked_at'))}"
    )


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


def parse_account_id(value: Any) -> int:
    text = str(value or "").strip().lstrip("#")
    account_id = int(text)
    if account_id <= 0:
        raise ValueError("账号 ID 必须大于 0")
    return account_id


def parse_minutes(value: Any, default: int) -> int:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        parsed = default
    return max(1, min(1440, parsed))


def actor(chat_id: int, user_id: int) -> str:
    return f"telegram:{chat_id}:{user_id}"


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


def truncate(text: str, limit: int) -> str:
    return text if len(text) <= limit else text[: max(0, limit - 3)] + "..."
