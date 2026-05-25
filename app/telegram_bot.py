from __future__ import annotations

import asyncio
import json
import secrets
import urllib.error
import urllib.request
from collections.abc import Awaitable, Callable
from datetime import datetime
from pathlib import Path
from typing import Any
from zoneinfo import ZoneInfo

from . import account_ops
from . import usage_query as usage_query_module
from .db import Database
from .guard_engine import is_oauth_account
from .settings import Settings
from .usage_query import (
    UsageOpener,
    UsageQueryConfig,
    UsageQueryStore,
    actual_available,
    apply_account_credentials,
    execute_usage_query,
)

BEIJING_TZ = ZoneInfo("Asia/Shanghai")
ACCOUNT_PAGE_SIZE = 8
PAIRING_CODE_HINT = "请到 Ops 面板的 Telegram 页面查看配对码，然后在私聊中发送 /pair <配对码>。"


GuardRunner = Callable[[str], Awaitable[list[dict[str, Any]]]]
GuardConfig = Callable[[], dict[str, Any]]
AsyncUsageOpener = Callable[[dict[str, Any], int], Awaitable[Any]]


def usage_query_configured(config: UsageQueryConfig) -> bool:
    return bool(config.updated_at or config.enabled or config.base_url or config.api_key or config.access_token)


class TelegramOpsBot:
    def __init__(
        self,
        settings: Settings,
        db: Database,
        guard_runner: GuardRunner,
        guard_config: GuardConfig,
        usage_query_opener: AsyncUsageOpener | UsageOpener | None = None,
    ) -> None:
        self.settings = settings
        self.db = db
        self.guard_runner = guard_runner
        self.guard_config = guard_config
        self.usage_query_opener = usage_query_opener
        self._state_lock = asyncio.Lock()

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

    @property
    def enabled(self) -> bool:
        return self.settings.telegram_enabled and bool(self.settings.telegram_bot_token.strip())

    async def notify(self, text: str, keyboard: dict[str, Any] | None = None) -> None:
        if not self.enabled:
            return
        for chat_id in await self.allowed_chat_ids():
            await self._send_message(chat_id, text, keyboard)

    async def sync_commands(self) -> None:
        if not self.enabled:
            return
        await self._api(
            "setMyCommands",
            {
                "commands": [
                    {"command": "quota", "description": "查询账号额度"},
                ]
            },
        )

    async def notify_account_alerts(self, title: str, actions: list[dict[str, Any]]) -> None:
        if not self.enabled:
            return
        chat_ids = await self.allowed_chat_ids()
        if not chat_ids:
            return
        for action in actions[:10]:
            try:
                account_id = parse_account_id(action.get("account_id") or action.get("id") or "")
            except ValueError:
                account_id = 0
            row = await asyncio.to_thread(self._account_detail, account_id) if account_id else None
            alert_text = account_alert(title, action, row)
            keyboard = account_actions_keyboard(row or action) if account_id else None
            for chat_id in chat_ids:
                await self._send_message(chat_id, alert_text, keyboard)
        if len(actions) > 10:
            for chat_id in chat_ids:
                await self._send_message(chat_id, f"{title}\n另有 {len(actions) - 10} 个账号异常未展开。")

    async def notify_error_chain_alerts(self, rows: list[dict[str, Any]]) -> None:
        if not self.enabled:
            return
        chat_ids = await self.allowed_chat_ids()
        if not chat_ids:
            return
        for row in rows:
            try:
                account_id = parse_account_id(row.get("account_id") or "")
            except ValueError:
                continue
            detail = await asyncio.to_thread(self._account_detail, account_id)
            alert_text = error_chain_alert(row, detail)
            keyboard = account_actions_keyboard(detail or row)
            for chat_id in chat_ids:
                await self._send_message(chat_id, alert_text, keyboard)

    async def notify_recovery_alerts(self, rows: list[dict[str, Any]]) -> None:
        if not self.enabled:
            return
        chat_ids = await self.allowed_chat_ids()
        if not chat_ids:
            return
        for row in rows:
            try:
                account_id = parse_account_id(row.get("account_id") or row.get("id") or "")
            except ValueError:
                continue
            detail = await asyncio.to_thread(self._account_detail, account_id)
            alert_text = recovery_alert(row, detail)
            keyboard = account_actions_keyboard(detail or row)
            for chat_id in chat_ids:
                await self._send_message(chat_id, alert_text, keyboard)

    async def allowed_chat_ids(self) -> list[int]:
        state = await self._load_state()
        return unique_ints(list(self.settings.telegram_allowed_chat_ids) + list(state.get("paired_chat_ids") or []))

    async def error_alert_cursor_id(self) -> int:
        state = await self._load_state()
        try:
            return max(0, int(state.get("error_alert_cursor_id") or 0))
        except (TypeError, ValueError):
            return 0

    async def set_error_alert_cursor_id(self, cursor_id: int) -> None:
        async with self._state_lock:
            state = await self._load_state_unlocked()
            state["error_alert_cursor_id"] = max(0, int(cursor_id or 0))
            state["error_alert_cursor_updated_at"] = datetime.now(BEIJING_TZ).isoformat()
            await asyncio.to_thread(self._save_state_sync, state)

    async def recovery_alert_cursor_id(self) -> int:
        state = await self._load_state()
        try:
            return max(0, int(state.get("recovery_alert_cursor_id") or 0))
        except (TypeError, ValueError):
            return 0

    async def set_recovery_alert_cursor_id(self, cursor_id: int) -> None:
        async with self._state_lock:
            state = await self._load_state_unlocked()
            state["recovery_alert_cursor_id"] = max(0, int(cursor_id or 0))
            state["recovery_alert_cursor_updated_at"] = datetime.now(BEIJING_TZ).isoformat()
            await asyncio.to_thread(self._save_state_sync, state)

    async def _handle_update(self, update: dict[str, Any]) -> None:
        callback = update.get("callback_query")
        if callback:
            callback_id = str(callback.get("id") or "")
            if callback_id:
                await self._api("answerCallbackQuery", {"callback_query_id": callback_id})
            message = callback.get("message") or {}
            chat_id = int((message.get("chat") or {}).get("id") or 0)
            user_id = int((callback.get("from") or {}).get("id") or 0)
            chat_type = str((message.get("chat") or {}).get("type") or "private")
            if not chat_id or not await self._allowed(chat_id, user_id):
                if chat_id and chat_type == "private":
                    await self._send_message(chat_id, f"未授权。{PAIRING_CODE_HINT}")
                return
            try:
                text, keyboard = await self._callback_reply(
                    chat_id,
                    user_id,
                    str(callback.get("data") or "menu"),
                )
            except Exception as exc:
                text, keyboard = f"执行失败：{exc}", None
            await self._send_message(chat_id, text, keyboard)
            return

        message = update.get("message") or {}
        text = str(message.get("text") or "").strip()
        if not text:
            return
        chat_id = int((message.get("chat") or {}).get("id") or 0)
        user_id = int((message.get("from") or {}).get("id") or 0)
        chat_type = str((message.get("chat") or {}).get("type") or "private")
        if not chat_id:
            return

        if is_pair_command(text):
            reply, keyboard = await self._pair(chat_id, user_id, chat_type, text)
            await self._send_message(chat_id, reply, keyboard)
            return

        if not await self._allowed(chat_id, user_id):
            if chat_type != "private":
                await self._send_message(chat_id, "未授权。请在私聊中和 Bot 交互。")
            else:
                await self._send_message(chat_id, f"未授权。{PAIRING_CODE_HINT}")
            return

        try:
            reply, keyboard = await self._text_reply(chat_id, user_id, text)
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
            return (
                "请在 Telegram 私聊中配对，群聊不会绑定。",
                None,
            )
        if await self._allowed(chat_id, user_id):
            return (
                f"当前会话已配对。\nchat_id：{chat_id}\nuser_id：{user_id}\n\n后续会推送账号异常，直接使用消息下方按钮处理账号。",
                None,
            )
        if not self.settings.telegram_pairing_enabled:
            return ("Telegram 配对已关闭，请在 Ops 面板中重新启用。", None)
        expected = normalize_pairing_code(self.settings.telegram_pairing_code)
        if not expected:
            return (f"当前没有可用配对码。{PAIRING_CODE_HINT}", None)
        provided = normalize_pairing_code(pairing_code_from_text(text))
        if not provided:
            return (f"发送 /pair <配对码> 完成绑定。{PAIRING_CODE_HINT}", None)
        if not secrets.compare_digest(provided, expected):
            return ("配对码不正确或已被重新生成，请回到 Ops 面板查看最新配对码。", None)

        async with self._state_lock:
            state = await self._load_state_unlocked()
            state["paired_chat_ids"] = unique_ints(list(state.get("paired_chat_ids") or []) + [chat_id])
            state["paired_user_ids"] = unique_ints(list(state.get("paired_user_ids") or []) + [user_id])
            state["updated_at"] = datetime.now(BEIJING_TZ).isoformat()
            await asyncio.to_thread(self._save_state_sync, state)
        return (
            f"配对成功。\nchat_id：{chat_id}\nuser_id：{user_id}\n\n后续会推送账号异常，直接使用消息下方按钮处理账号。",
            None,
        )

    async def _text_reply(
        self,
        chat_id: int,
        user_id: int,
        text: str,
    ) -> tuple[str, dict[str, Any] | None]:
        parts = text.split()
        command = normalize_command(parts[0] if parts else "")

        if command in {"/quota", "/usage", "quota", "usage", "额度", "查额度"}:
            return await self._quota_reply()

        if command in {"/start", "/help", "/menu", "menu", "菜单"}:
            return (
                "Telegram 命令菜单已关闭。\n\n后续只推送账号异常信息；每条异常消息下方会附带暂停、冷却、恢复和查看详情按钮。",
                None,
            )

        return (
            "不再通过文本命令做账号运维。请等待异常推送，并直接点击异常消息下方的账号操作按钮。",
            None,
        )

    async def _callback_reply(
        self,
        chat_id: int,
        user_id: int,
        data: str,
    ) -> tuple[str, dict[str, Any] | None]:
        if data in {"menu", "status", "push", "guard", "guardrun", "acctmenu", "acctsearch"}:
            return (
                "这个 Telegram 菜单已下线。请直接使用异常推送下方的账号操作按钮。",
                None,
            )
        if data.startswith("acctlist:"):
            return (
                "账号列表命令已下线。异常账号会在推送消息里直接给出操作按钮。",
                None,
            )
        if data.startswith("acct:"):
            account_id = parse_account_id(data.split(":", 1)[1])
            return await self._account_detail_reply(account_id)
        if data.startswith("pauseask:"):
            account_id = parse_account_id(data.split(":", 1)[1])
            return await self._pause_apply_reply(account_id, actor(chat_id, user_id))
        if data.startswith("pause:"):
            account_id = parse_account_id(data.split(":", 1)[1])
            return await self._pause_apply_reply(account_id, actor(chat_id, user_id))
        if data.startswith("resask:"):
            account_id = parse_account_id(data.split(":", 1)[1])
            return await self._resume_apply_reply(account_id, actor(chat_id, user_id))
        if data.startswith("res:"):
            account_id = parse_account_id(data.split(":", 1)[1])
            return await self._resume_apply_reply(account_id, actor(chat_id, user_id))
        if data.startswith("cdmenu:"):
            account_id = parse_account_id(data.split(":", 1)[1])
            return await self._cooldown_menu_reply(account_id)
        if data.startswith("cd:"):
            _, account_raw, minutes_raw = (data.split(":") + ["", "15"])[:3]
            return await self._cooldown_apply_reply(
                parse_account_id(account_raw),
                parse_minutes(minutes_raw, 15),
                actor(chat_id, user_id),
            )
        return ("无法识别这个按钮，可能来自旧消息。", None)

    async def _menu_reply(self) -> tuple[str, dict[str, Any]]:
        chats = await self.allowed_chat_ids()
        text = (
            "Sub2API Ops 远程控制\n"
            f"平台/分组：{self.settings.telegram_default_platform} / {self.settings.telegram_default_group}\n"
            f"已配对推送目标：{len(chats)} 个\n\n"
            "可远程查看账号质量、执行 Guard、暂停/恢复/冷却具体账号。"
        )
        return text, main_keyboard()

    async def _status_reply(self) -> tuple[str, dict[str, Any]]:
        def load() -> tuple[dict[str, int], dict[str, Any], str]:
            rows = self._quality_rows()
            summary = account_ops.account_summary(rows)
            self.db.fetch_one("SELECT 1 AS ok")
            return summary, self.guard_config(), "ok"

        summary, guard, db_status = await asyncio.to_thread(load)
        text = (
            "Sub2API Ops 状态\n"
            f"DB：{db_status}\n"
            f"Guard：{'开启' if guard.get('enabled') else '关闭'} / {guard.get('interval_seconds')}s\n"
            f"Guard 运行中：{'是' if (guard.get('state') or {}).get('running') else '否'}\n"
            f"上次 Guard：{bj_time((guard.get('state') or {}).get('last_run_at'))}\n\n"
            f"账号：总 {summary['total']}，可调度 {summary['active']}，冷却 {summary['cooling']}，已停 {summary['paused']}\n"
            f"错误：额度 {summary['balance']}，403 {summary['blocked']}，限流 {summary['rate']}，5xx/流式 {summary['unstable']}"
        )
        return text, main_keyboard()

    async def _guard_reply(self) -> tuple[str, dict[str, Any]]:
        guard = await asyncio.to_thread(self.guard_config)
        state = guard.get("state") or {}
        last_actions = state.get("last_actions") or []
        text = (
            "自动 Guard\n"
            f"状态：{'开启' if guard.get('enabled') else '关闭'}\n"
            f"扫描间隔：{guard.get('interval_seconds')}s\n"
            "扫描范围：全部账号\n"
            f"余额/额度阈值：{guard.get('threshold')}\n"
            f"上次运行：{bj_time(state.get('last_run_at'))}\n"
            f"上次错误：{state.get('last_error') or '-'}\n"
            f"上次动作：{len(last_actions)} 个"
        )
        return text, guard_keyboard()

    async def _guard_run_reply(self, chat_id: int, user_id: int) -> tuple[str, dict[str, Any]]:
        actions = await self.guard_runner(actor(chat_id, user_id, "guard"))
        if not actions:
            return "Guard 已执行：没有需要处理的账号。", guard_keyboard()
        return format_guard_actions("Guard 已执行", actions), guard_keyboard()

    async def _accounts_menu_reply(self) -> tuple[str, dict[str, Any]]:
        rows = await asyncio.to_thread(self._quality_rows)
        summary = account_ops.account_summary(rows)
        text = (
            "账号运维\n"
            f"平台/分组：{self.settings.telegram_default_platform} / {self.settings.telegram_default_group}\n"
            f"统计窗口：{self.settings.telegram_quality_hours}h\n\n"
            f"总数：{summary['total']}，可调度：{summary['active']}，冷却中：{summary['cooling']}，已停：{summary['paused']}\n"
            f"有错误：{summary['bad']}，额度/余额：{summary['balance']}，403：{summary['blocked']}，限流：{summary['rate']}，5xx/流式：{summary['unstable']}"
        )
        return text, accounts_keyboard(summary)

    async def _account_list_reply(self, filter_name: str, page: int) -> tuple[str, dict[str, Any]]:
        rows = await asyncio.to_thread(self._quality_rows)
        key = account_ops.normalize_filter(filter_name)
        filtered = account_ops.filter_rows(rows, key)
        page_count = max(1, (len(filtered) + ACCOUNT_PAGE_SIZE - 1) // ACCOUNT_PAGE_SIZE)
        page = max(0, min(page, page_count - 1))
        start = page * ACCOUNT_PAGE_SIZE
        selected = filtered[start : start + ACCOUNT_PAGE_SIZE]
        if not selected:
            return f"{account_ops.filter_title(key)} 没有账号。", accounts_keyboard()

        lines = [account_row(row) for row in selected]
        text = (
            f"账号列表：{account_ops.filter_title(key)}\n"
            f"第 {page + 1}/{page_count} 页，共 {len(filtered)} 个\n\n"
            + "\n".join(lines)
        )
        return text, account_list_keyboard(selected, key, page, page_count)

    async def _account_search_reply(self, query: str) -> tuple[str, dict[str, Any]]:
        rows = await asyncio.to_thread(self._quality_rows)
        matches = account_ops.find_accounts(rows, query, 12)
        if not matches:
            return f"没有找到账号：{query}", accounts_keyboard()
        if len(matches) == 1:
            return account_detail(matches[0]), account_actions_keyboard(matches[0])
        text = f"搜索结果：{query}\n共 {len(matches)} 个\n\n" + "\n".join(account_row(row) for row in matches)
        return text, account_list_keyboard(matches, "all", 0, 1, show_pager=False)

    async def _account_detail_reply(self, account_id: int) -> tuple[str, dict[str, Any]]:
        row = await asyncio.to_thread(self._account_detail, account_id)
        if not row:
            return f"没有找到账号 #{account_id}", accounts_keyboard()
        return account_detail(row), account_actions_keyboard(row)

    async def _pause_confirm_reply(self, account_id: int) -> tuple[str, dict[str, Any]]:
        row = await asyncio.to_thread(self._account_detail, account_id)
        if not row:
            return f"没有找到账号 #{account_id}", accounts_keyboard()
        text = f"确认暂停账号？\n\n{account_row(row)}\n\n暂停后不会再调度，直到手动恢复。"
        return text, confirm_keyboard("确认暂停", f"pause:{account_id}", f"acct:{account_id}")

    async def _pause_apply_reply(self, account_id: int, actor_name: str) -> tuple[str, dict[str, Any]]:
        reason = f"telegram remote pause by {actor_name}"
        row = await asyncio.to_thread(
            account_ops.pause_account,
            self.db,
            self.settings.audit_path,
            account_id,
            actor_name,
            reason,
        )
        if not row:
            return f"暂停失败：没有找到账号 #{account_id}", accounts_keyboard()
        detail = await asyncio.to_thread(self._account_detail, account_id)
        return f"已暂停账号。\n\n{account_detail(detail or row)}", account_actions_keyboard(detail or row)

    async def _resume_confirm_reply(self, account_id: int) -> tuple[str, dict[str, Any]]:
        row = await asyncio.to_thread(self._account_detail, account_id)
        if not row:
            return f"没有找到账号 #{account_id}", accounts_keyboard()
        text = f"确认恢复账号调度？\n\n{account_row(row)}\n\n恢复会清除手动暂停、临时冷却和限流/错误状态。"
        return text, confirm_keyboard("确认恢复", f"res:{account_id}", f"acct:{account_id}")

    async def _resume_apply_reply(self, account_id: int, actor_name: str) -> tuple[str, dict[str, Any]]:
        row = await asyncio.to_thread(
            account_ops.resume_account,
            self.db,
            self.settings.audit_path,
            account_id,
            actor_name,
        )
        if not row:
            return f"恢复失败：没有找到账号 #{account_id}", accounts_keyboard()
        detail = await asyncio.to_thread(self._account_detail, account_id)
        return f"已恢复账号。\n\n{account_detail(detail or row)}", account_actions_keyboard(detail or row)

    async def _cooldown_menu_reply(self, account_id: int) -> tuple[str, dict[str, Any]]:
        row = await asyncio.to_thread(self._account_detail, account_id)
        if not row:
            return f"没有找到账号 #{account_id}", accounts_keyboard()
        return f"选择冷却时间\n\n{account_row(row)}", cooldown_keyboard(account_id)

    async def _cooldown_apply_reply(
        self,
        account_id: int,
        minutes: int,
        actor_name: str,
    ) -> tuple[str, dict[str, Any]]:
        reason = f"telegram remote cooldown {minutes}m by {actor_name}"
        row = await asyncio.to_thread(
            account_ops.cooldown_account,
            self.db,
            self.settings.audit_path,
            account_id,
            actor_name,
            minutes,
            reason,
        )
        if not row:
            return f"冷却失败：没有找到账号 #{account_id}", accounts_keyboard()
        detail = await asyncio.to_thread(self._account_detail, account_id)
        return f"已冷却账号 {minutes} 分钟。\n\n{account_detail(detail or row)}", account_actions_keyboard(detail or row)

    def _quality_rows(self) -> list[dict[str, Any]]:
        return account_ops.quality_rows(
            self.db,
            self.settings.telegram_default_group,
            self.settings.telegram_default_platform,
            self.settings.telegram_quality_hours,
        )

    def _account_detail(self, account_id: int) -> dict[str, Any] | None:
        rows = self._quality_rows()
        return account_ops.account_by_id(rows, account_id) or account_ops.fallback_account(self.db, account_id)

    def _usage_query_account_row(self, account_id: int) -> dict[str, Any] | None:
        try:
            return account_ops.fallback_account(self.db, account_id, True)
        except Exception:
            return account_ops.fallback_account(self.db, account_id, False)

    async def _quota_reply(self) -> tuple[str, dict[str, Any] | None]:
        store = UsageQueryStore(self.settings.usage_query_state_path)
        if not store.usage_query_enabled():
            return "全局额度查询已关闭。请先在速度页开启额度查询。", None
        configs = [config for config in store.configs() if usage_query_configured(config)]

        account_lines: list[str] = []
        totals: dict[str, float] = {}
        seen_account_ids: set[int] = set()
        for config in configs[:30]:
            seen_account_ids.add(config.account_id)
            row = await asyncio.to_thread(self._usage_query_account_row, config.account_id)
            if not row:
                continue
            if row and is_oauth_account(row):
                line = await asyncio.to_thread(format_oauth_quota_line, row, config.account_id)
                if line:
                    account_lines.append(line)
                continue
            hydrated = apply_account_credentials(config, row)
            previous = store.result(config.account_id)
            result = await self._execute_quota_query(hydrated)
            if result.get("success"):
                store.save_result(config.account_id, result)
                account_lines.append(format_quota_line(hydrated, row, result))
                add_quota_total(totals, quota_available(result, hydrated), str(result.get("unit") or ""))
                continue
            if previous.get("success"):
                account_lines.append(format_quota_line(hydrated, row, previous))
                add_quota_total(totals, quota_available(previous, hydrated), str(previous.get("unit") or ""))
                continue
            store.save_result(config.account_id, result)
            account_lines.append(format_quota_line(hydrated, row, result))
        oauth_lines = await self._current_oauth_quota_lines(seen_account_ids)
        account_lines.extend(oauth_lines)
        if not configs and not account_lines:
            return "没有配置额度查询的账号。请先在速度页配置额度查询。", None
        lines = [
            "额度查询",
            f"总可用：{format_quota_totals(totals)}",
            *account_lines,
        ]
        if len(configs) > 30:
            lines.append(f"... 另有 {len(configs) - 30} 个已配置账号未展开")
        return "\n".join(lines), None

    async def _current_oauth_quota_lines(self, seen_account_ids: set[int]) -> list[str]:
        try:
            rows = await asyncio.to_thread(self._quality_rows)
        except Exception:
            return []
        lines: list[str] = []
        for row in rows:
            account_id = int(row.get("id") or row.get("account_id") or 0)
            if account_id <= 0 or account_id in seen_account_ids or not is_oauth_account(row):
                continue
            full_row = await asyncio.to_thread(self._usage_query_account_row, account_id)
            if not full_row:
                continue
            line = await asyncio.to_thread(format_oauth_quota_line, full_row, account_id)
            if line:
                lines.append(line)
            seen_account_ids.add(account_id)
        return lines

    async def _execute_quota_query(self, config: UsageQueryConfig) -> dict[str, Any]:
        opener = self.usage_query_opener
        if opener is None:
            return await asyncio.to_thread(execute_usage_query, config)

        async def async_opener(request: dict[str, Any], timeout: int) -> Any:
            value = opener(request, timeout)
            if hasattr(value, "__await__"):
                return await value  # type: ignore[misc]
            return value

        return await asyncio.to_thread(
            execute_usage_query,
            config,
            opener=lambda request, timeout: asyncio.run(async_opener(request, timeout)),
        )

    async def _allowed(self, chat_id: int, user_id: int) -> bool:
        state = await self._load_state()
        chat_ids = unique_ints(list(self.settings.telegram_allowed_chat_ids) + list(state.get("paired_chat_ids") or []))
        user_ids = unique_ints(list(self.settings.telegram_allowed_user_ids) + list(state.get("paired_user_ids") or []))
        if not chat_ids and not user_ids:
            return False
        if chat_ids and chat_id not in chat_ids:
            return False
        if user_ids and user_id not in user_ids:
            return False
        return True

    async def _load_state(self) -> dict[str, Any]:
        async with self._state_lock:
            return await self._load_state_unlocked()

    async def _load_state_unlocked(self) -> dict[str, Any]:
        return await asyncio.to_thread(self._load_state_sync)

    def _load_state_sync(self) -> dict[str, Any]:
        path = Path(self.settings.telegram_state_path)
        try:
            state = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            state = {}
        state.setdefault("paired_chat_ids", [])
        state.setdefault("paired_user_ids", [])
        return state

    def _save_state_sync(self, state: dict[str, Any]) -> None:
        path = Path(self.settings.telegram_state_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(state, ensure_ascii=False, indent=2), encoding="utf-8")

    async def _send_message(
        self,
        chat_id: int,
        text: str,
        keyboard: dict[str, Any] | None = None,
    ) -> None:
        body: dict[str, Any] = {
            "chat_id": chat_id,
            "text": truncate(text, 3800),
            "disable_web_page_preview": True,
        }
        if keyboard:
            body["reply_markup"] = keyboard
        await self._api("sendMessage", body)

    async def _api(self, method: str, payload: dict[str, Any], timeout: int = 15) -> dict[str, Any]:
        return await asyncio.to_thread(self._api_sync, method, payload, timeout)

    def _api_sync(self, method: str, payload: dict[str, Any], timeout: int) -> dict[str, Any]:
        token = self.settings.telegram_bot_token.strip()
        if not token:
            return {"ok": False, "result": []}
        data = json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            f"https://api.telegram.org/bot{token}/{method}",
            data=data,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return json.loads(response.read().decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError):
            return {"ok": False, "result": []}


def main_keyboard() -> dict[str, Any]:
    return {
        "inline_keyboard": [
            [{"text": "状态", "callback_data": "status"}, {"text": "账号运维", "callback_data": "acctmenu"}],
            [{"text": "自动 Guard", "callback_data": "guard"}, {"text": "执行 Guard", "callback_data": "guardrun"}],
            [{"text": "推送测试", "callback_data": "push"}],
        ]
    }


def accounts_keyboard(summary: dict[str, int] | None = None) -> dict[str, Any]:
    suffix = summary or {}
    return {
        "inline_keyboard": [
            [
                {"text": label("全部", suffix.get("total")), "callback_data": "acctlist:all:0"},
                {"text": label("可调度", suffix.get("active")), "callback_data": "acctlist:active:0"},
            ],
            [
                {"text": label("已停", suffix.get("paused")), "callback_data": "acctlist:paused:0"},
                {"text": label("冷却中", suffix.get("cooling")), "callback_data": "acctlist:cooling:0"},
            ],
            [
                {"text": label("额度/余额", suffix.get("balance")), "callback_data": "acctlist:balance:0"},
                {"text": label("403", suffix.get("blocked")), "callback_data": "acctlist:blocked:0"},
            ],
            [
                {"text": label("限流", suffix.get("rate")), "callback_data": "acctlist:rate:0"},
                {"text": label("5xx/流式", suffix.get("unstable")), "callback_data": "acctlist:unstable:0"},
            ],
            [{"text": "搜索账号", "callback_data": "acctsearch"}, {"text": "主菜单", "callback_data": "menu"}],
        ]
    }


def account_list_keyboard(
    rows: list[dict[str, Any]],
    filter_name: str,
    page: int,
    page_count: int,
    show_pager: bool = True,
) -> dict[str, Any]:
    buttons = [
        [{"text": f"#{row.get('id')} {truncate(str(row.get('name') or '-'), 28)} · {account_ops.account_state(row)}", "callback_data": f"acct:{row.get('id')}"}]
        for row in rows
    ]
    if show_pager and page_count > 1:
        prev_page = max(0, page - 1)
        next_page = min(page_count - 1, page + 1)
        buttons.append(
            [
                {"text": "上一页", "callback_data": f"acctlist:{filter_name}:{prev_page}"},
                {"text": f"{page + 1}/{page_count}", "callback_data": f"acctlist:{filter_name}:{page}"},
                {"text": "下一页", "callback_data": f"acctlist:{filter_name}:{next_page}"},
            ]
        )
    buttons.append([{"text": "筛选菜单", "callback_data": "acctmenu"}, {"text": "搜索账号", "callback_data": "acctsearch"}])
    buttons.append([{"text": "主菜单", "callback_data": "menu"}])
    return {"inline_keyboard": buttons}


def account_actions_keyboard(row: dict[str, Any]) -> dict[str, Any]:
    account_id = int(row.get("id") or row.get("account_id") or 0)
    if row.get("schedulable", True) and not account_ops.is_cooling(row):
        first = [
            {"text": "暂停", "callback_data": f"pause:{account_id}"},
            {"text": "冷却 5m", "callback_data": f"cd:{account_id}:5"},
            {"text": "冷却 15m", "callback_data": f"cd:{account_id}:15"},
            {"text": "冷却 30m", "callback_data": f"cd:{account_id}:30"},
        ]
    elif row.get("schedulable", True):
        first = [
            {"text": "恢复", "callback_data": f"res:{account_id}"},
            {"text": "冷却 5m", "callback_data": f"cd:{account_id}:5"},
            {"text": "冷却 15m", "callback_data": f"cd:{account_id}:15"},
            {"text": "冷却 30m", "callback_data": f"cd:{account_id}:30"},
        ]
    else:
        first = [
            {"text": "恢复", "callback_data": f"res:{account_id}"},
            {"text": "冷却 5m", "callback_data": f"cd:{account_id}:5"},
            {"text": "冷却 15m", "callback_data": f"cd:{account_id}:15"},
            {"text": "冷却 30m", "callback_data": f"cd:{account_id}:30"},
        ]
    return {
        "inline_keyboard": [
            first,
            [{"text": "查看详情", "callback_data": f"acct:{account_id}"}],
        ]
    }


def cooldown_keyboard(account_id: int) -> dict[str, Any]:
    return {
        "inline_keyboard": [
            [
                {"text": "5m", "callback_data": f"cd:{account_id}:5"},
                {"text": "15m", "callback_data": f"cd:{account_id}:15"},
                {"text": "30m", "callback_data": f"cd:{account_id}:30"},
            ],
            [{"text": "查看详情", "callback_data": f"acct:{account_id}"}],
        ]
    }


def confirm_keyboard(confirm_text: str, confirm_data: str, back_data: str) -> dict[str, Any]:
    return {
        "inline_keyboard": [
            [{"text": confirm_text, "callback_data": confirm_data}],
            [{"text": "返回账号", "callback_data": back_data}],
        ]
    }


def guard_keyboard() -> dict[str, Any]:
    return {
        "inline_keyboard": [
            [{"text": "立即执行 Guard", "callback_data": "guardrun"}],
            [{"text": "账号运维", "callback_data": "acctmenu"}, {"text": "主菜单", "callback_data": "menu"}],
        ]
    }


def account_row(row: dict[str, Any]) -> str:
    return (
        f"#{row.get('id')} {row.get('name')} · {row.get('type') or '-'} · "
        f"G{row.get('group_priority') or '-'} / P{row.get('account_priority') or row.get('priority') or '-'} · "
        f"load {row.get('load_factor') or row.get('effective_load_factor') or '-'} · "
        f"{account_ops.account_state(row)} · 成功/错 {int(row.get('success_window') or 0)}/{int(row.get('account_quality_errors_window') or 0)} · "
        f"余额 {int(row.get('balance_or_quota_window') or 0)} 403 {int(row.get('blocked_403_window') or 0)} "
        f"限流 {int(row.get('rate_limit_window') or 0)} 5xx {int(row.get('unstable_5xx_stream_window') or 0)}"
    )


def account_detail(row: dict[str, Any]) -> str:
    lines = [
        f"#{row.get('id')} {row.get('name')}",
        f"平台/类型：{row.get('platform') or '-'} / {row.get('type') or '-'}",
        f"状态：{account_ops.account_state(row)}，G{row.get('group_priority') or '-'} / P{row.get('account_priority') or row.get('priority') or '-'}，并发 {row.get('concurrency') or '-'}，负载因子 {row.get('load_factor') or row.get('effective_load_factor') or '-'}",
        f"窗口成功/错误：{int(row.get('success_window') or 0)} / {int(row.get('account_quality_errors_window') or 0)}",
        f"错误拆分：余额 {int(row.get('balance_or_quota_window') or 0)}，403 {int(row.get('blocked_403_window') or 0)}，限流 {int(row.get('rate_limit_window') or 0)}，5xx/流式 {int(row.get('unstable_5xx_stream_window') or 0)}",
    ]
    if row.get("temp_unschedulable_until"):
        lines.append(f"冷却到：{bj_time(row.get('temp_unschedulable_until'))}")
    if row.get("temp_unschedulable_reason"):
        lines.append(f"停调度原因：{truncate(str(row.get('temp_unschedulable_reason')), 800)}")
    if row.get("last_error_at"):
        lines.append(
            f"最近错误：{bj_time(row.get('last_error_at'))} · {row.get('last_error_status') or '-'} · {row.get('last_error_category') or '-'}"
        )
        if row.get("last_error_message"):
            lines.append(f"错误内容：{truncate(str(row.get('last_error_message')), 1000)}")
    return "\n".join(lines)


def account_alert(title: str, action: dict[str, Any], row: dict[str, Any] | None) -> str:
    lines = [title]
    if row:
        lines.extend(["", account_detail(row)])
    else:
        lines.append(f"账号：#{action.get('account_id') or action.get('id')} {action.get('name') or '-'}")

    if action.get("balance_error_count"):
        lines.append(f"余额/额度错误：{action.get('balance_error_count')}")
    if action.get("action"):
        detail = str(action.get("action"))
        if action.get("minutes"):
            detail += f" {action.get('minutes')}m"
        lines.append(f"Guard 动作：{detail}")
    if action.get("load_factor"):
        lines.append(f"软降载：load_factor={action.get('load_factor')}")
    if action.get("last_error_at"):
        lines.append(f"异常时间：{bj_time(action.get('last_error_at'))}")
    if action.get("last_message"):
        lines.append(f"异常内容：{truncate(str(action.get('last_message')), 1000)}")
    if action.get("reason"):
        lines.append(f"处理原因：{truncate(str(action.get('reason')), 800)}")
    return "\n".join(lines)


def error_chain_alert(row: dict[str, Any], account: dict[str, Any] | None) -> str:
    account_id = row.get("account_id") or (account or {}).get("id") or "-"
    account_name = row.get("account_name") or (account or {}).get("name") or "-"
    lines = [
        "错误链路异常",
        f"请求：{row.get('request_id') or '-'}",
        f"客户端请求：{row.get('client_request_id') or '-'}",
        f"平台/模型：{row.get('platform') or '-'} / {row.get('upstream_model') or row.get('requested_model') or row.get('model') or '-'}",
        f"账号：#{account_id} {account_name}",
        f"尝试：{row.get('attempt_no') or '-'}，状态：{row.get('status_code') or row.get('final_status_code') or '-'}，分类：{row.get('category') or '-'}",
    ]
    if row.get("created_at"):
        lines.append(f"时间：{bj_time(row.get('created_at'))}")
    if row.get("kind") or row.get("error_phase"):
        lines.append(f"类型/阶段：{row.get('kind') or '-'} / {row.get('error_phase') or '-'}")
    if row.get("message"):
        lines.append(f"错误内容：{truncate(str(row.get('message')), 1200)}")
    if account:
        lines.append(f"当前状态：{account_ops.account_state(account)}")
    return "\n".join(lines)


def recovery_alert(row: dict[str, Any], account: dict[str, Any] | None = None) -> str:
    account_id = row.get("account_id") or row.get("id") or (account or {}).get("id") or "-"
    account_name = row.get("account_name") or row.get("name") or (account or {}).get("name") or "-"
    latency_ms = row.get("latency_ms") or row.get("result_latency_ms")
    lines = [
        "账号已自动恢复",
        f"账号：#{account_id} {account_name}",
        f"平台/类型：{row.get('platform') or (account or {}).get('platform') or '-'} / {row.get('type') or (account or {}).get('type') or '-'}",
        f"测试模型：{row.get('model_id') or row.get('plan_model_id') or 'Sub2API 默认'}",
        f"计划：{row.get('cron_expression') or row.get('plan_cron_expression') or '-'}",
    ]
    if latency_ms not in (None, ""):
        lines.append(f"测试耗时：{format_seconds(latency_ms)}")
    if row.get("finished_at") or row.get("created_at"):
        lines.append(f"恢复时间：{bj_time(row.get('finished_at') or row.get('created_at'))}")
    if account:
        lines.append(f"当前状态：{account_ops.account_state(account)}")
    return "\n".join(lines)


def format_guard_actions(title: str, actions: list[dict[str, Any]]) -> str:
    lines = [title]
    for item in actions[:10]:
        action_label = str(item.get("action") or "-")
        if item.get("minutes"):
            action_label += f" {item.get('minutes')}m"
        if item.get("load_factor"):
            action_label += f" / load_factor={item.get('load_factor')}"
        lines.append(
            f"#{item.get('account_id')} {item.get('name') or '-'} · {action_label} · "
            f"{truncate(str(item.get('reason') or ''), 160)}"
        )
    if len(actions) > 10:
        lines.append(f"... 另有 {len(actions) - 10} 个动作")
    return "\n".join(lines)


def format_quota_line(
    config: UsageQueryConfig,
    row: dict[str, Any] | None,
    result: dict[str, Any],
) -> str:
    account_id = int((row or {}).get("id") or config.account_id)
    account_name = str((row or {}).get("name") or "-")
    prefix = f"#{account_id} {account_name}"
    if not result.get("success"):
        return f"{prefix}：可用 -"

    unit = str(result.get("unit") or "")
    return f"{prefix}：可用 {format_quota_amount(quota_available(result, config), unit)}"


def format_oauth_quota_line(row: dict[str, Any], fallback_account_id: int = 0) -> str:
    helper = getattr(usage_query_module, "oauth_quota_windows", None)
    if not callable(helper):
        return ""
    summary = helper(row)
    if not isinstance(summary, dict):
        return ""
    account_id = int(row.get("id") or row.get("account_id") or fallback_account_id)
    account_name = str(row.get("name") or "-")
    plan_type = str(summary.get("plan_type") or "oauth")
    windows = remaining_oauth_quota_windows(summary.get("telegram_windows") or summary.get("ui_windows"))
    rendered = " / ".join(format_oauth_quota_window(window) for window in windows if isinstance(window, dict))
    if not rendered:
        return ""
    return f"#{account_id} {account_name} · Codex {plan_type}：{rendered}"


def remaining_oauth_quota_windows(raw_windows: Any) -> list[dict[str, Any]]:
    if not isinstance(raw_windows, list):
        return []
    windows: list[dict[str, Any]] = []
    for window in raw_windows:
        if not isinstance(window, dict):
            continue
        used_percent = numeric_value(window.get("used_percent"))
        if used_percent is not None:
            if used_percent >= 100:
                continue
            windows.append(window)
            continue
        remaining = numeric_value(window.get("remaining"))
        if remaining is not None and remaining > 0:
            windows.append(window)
    return windows


def format_oauth_quota_window(window: dict[str, Any]) -> str:
    label = str(window.get("label") or "-")
    remaining = numeric_value(window.get("remaining"))
    total = numeric_value(window.get("total"))
    unit = str(window.get("unit") or "").strip()
    if remaining is not None and total is not None:
        return f"{label} 剩余 {format_quota_amount(remaining, unit)} / {format_quota_amount(total, unit)}"
    if remaining is not None:
        return f"{label} 剩余 {format_quota_amount(remaining, unit)}"
    remaining_percent = window.get("remaining_percent")
    if remaining_percent in (None, ""):
        used_percent = numeric_value(window.get("used_percent"))
        remaining_percent = None if used_percent is None else max(0.0, 100.0 - used_percent)
    return f"{label} 剩余 {format_quota_percent(remaining_percent)}"


def numeric_value(value: Any) -> float | None:
    if value in (None, ""):
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def quota_available(result: dict[str, Any], config: UsageQueryConfig) -> float | None:
    recalculated = actual_available(result.get("remaining"), config.upstream_multiplier)
    if recalculated is not None:
        return recalculated
    value = result.get("actual_available")
    if value in (None, ""):
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def add_quota_total(totals: dict[str, float], value: float | None, unit: str) -> None:
    if value is None:
        return
    totals[unit] = totals.get(unit, 0.0) + value


def format_quota_totals(totals: dict[str, float]) -> str:
    if not totals:
        return "-"
    return " / ".join(format_quota_amount(value, unit) for unit, value in sorted(totals.items()))


def format_quota_amount(value: Any, unit: str) -> str:
    if value in (None, ""):
        return "-"
    try:
        numeric = float(value)
    except (TypeError, ValueError):
        return "-"
    rendered = f"{numeric:.6f}".rstrip("0").rstrip(".")
    return f"{rendered} {unit}".strip()


def format_quota_percent(value: Any) -> str:
    if value in (None, ""):
        return "-"
    try:
        numeric = float(value)
    except (TypeError, ValueError):
        return "-"
    rendered = f"{numeric:.2f}".rstrip("0").rstrip(".")
    return f"{rendered}%"


def bj_time(value: Any) -> str:
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
        parsed = parsed.replace(tzinfo=BEIJING_TZ)
    return parsed.astimezone(BEIJING_TZ).strftime("%Y-%m-%d %H:%M")


def is_pair_command(text: str) -> bool:
    parts = text.strip().split()
    return bool(parts) and normalize_command(parts[0]) == "/pair"


def pairing_code_from_text(text: str) -> str:
    parts = text.strip().split(maxsplit=1)
    return parts[1].strip() if len(parts) > 1 else ""


def normalize_pairing_code(value: str) -> str:
    return "".join(ch for ch in str(value or "").upper() if ch.isalnum())


def normalize_command(command: str) -> str:
    command = command.strip()
    if command.startswith("/"):
        return command.split("@", 1)[0].lower()
    return command.lower()


def parse_account_id(value: str) -> int:
    raw = str(value or "").strip().lstrip("#")
    if not raw.isdigit():
        raise ValueError("账号 ID 必须是数字")
    return int(raw)


def parse_minutes(value: str, default: int) -> int:
    try:
        return max(1, min(1440, int(value)))
    except (TypeError, ValueError):
        return default


def parse_page(value: str) -> int:
    try:
        return max(0, int(value))
    except (TypeError, ValueError):
        return 0


def actor(chat_id: int, user_id: int, label_name: str = "control") -> str:
    return f"telegram:{label_name}:chat={chat_id}:user={user_id}"


def label(text: str, count: int | None) -> str:
    return f"{text} {count}" if count is not None else text


def format_seconds(value: Any) -> str:
    try:
        seconds = float(value) / 1000
    except (TypeError, ValueError):
        return "-"
    return f"{seconds:.2f}s"


def unique_ints(values: list[Any]) -> list[int]:
    result: list[int] = []
    for value in values:
        try:
            parsed = int(value)
        except (TypeError, ValueError):
            continue
        if parsed and parsed not in result:
            result.append(parsed)
    return sorted(result)


def truncate(text: str, limit: int) -> str:
    if len(text) <= limit:
        return text
    return text[: max(0, limit - 1)] + "…"
