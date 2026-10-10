"""Desktop configuration writes, including optimistic concurrency."""
from __future__ import annotations

import asyncio
import fcntl
import hashlib
import json
import threading
from contextlib import contextmanager, nullcontext
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .audit import write_audit
from .bark import DEFAULT_BARK_SERVER_URL, normalize_bark_server_url
from .settings import Settings, daily_test_time


class ConfigConflict(ValueError):
    pass


OAUTH_FIELDS = {
    "oauth_recovery_monitor_enabled", "oauth_daily_test_enabled", "oauth_auto_reset_credit_enabled",
    "oauth_daily_test_time", "oauth_usage_refresh_concurrency", "oauth_recovery_test_concurrency",
    "oauth_early_probe_batch_size",
    "oauth_recovery_test_model_id",
    "oauth_recovery_connection_account_ids", "oauth_recovery_model_account_ids",
}


def revision(value: Any) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, default=str).encode()).hexdigest()


class ConfigService:
    def __init__(self, runtime: Any) -> None:
        self.r = runtime
        self.lock = asyncio.Lock()
        self.thread_lock = threading.RLock()
        self._oauth_config_expected: bool | None = None

    @contextmanager
    def file_lock(self):
        config_path = Path(self.r.settings.oauth_config_path)
        path = config_path.with_suffix('.lock')
        if self._oauth_config_expected is None:
            self._oauth_config_expected = config_path.exists() or path.exists()
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open('a+') as handle:
            path.chmod(0o600)
            fcntl.flock(handle, fcntl.LOCK_EX)
            try:
                if self._oauth_config_expected and not config_path.is_file():
                    raise ValueError("OAuth 配置文件缺失，已暂停配置写入")
                yield
                self._oauth_config_expected = config_path.is_file()
            finally:
                fcntl.flock(handle, fcntl.LOCK_UN)

    def reconcile_recovery_accounts(self, rows=None):
        from .recovery_policy import migrate_recovery_selection
        with self.thread_lock, self.file_lock():
            return migrate_recovery_selection(self.r, rows)

    def snapshot(self, section: str | None = None) -> dict[str, Any]:
        with self.thread_lock:
            return self._snapshot(section)

    def _snapshot(self, section: str | None = None) -> dict[str, Any]:
        r, s = self.r, self.r.settings
        config = r.oauth_config_file() if section in (None, "oauth") else {}
        oauth = {key: config.get(key, getattr(s, key, getattr(Settings, key))) for key in sorted(OAUTH_FIELDS)} if section in (None, "oauth") else {}
        bark = r.build_bark_config() if section in (None, "bark") else {}
        fallback = r.key_fallback_controller.panel_snapshot() if r.key_fallback_controller and section in (None, "key_fallback") else {}
        result = {"oauth": oauth, "bark": bark,
                  "key_fallback": fallback}
        # Include secret/config updates in revisions, never in response fields.
        versions = {"oauth": [oauth, config],
                    "bark": [bark, r.bark_config_file() if section in (None, "bark") else {}],
                    "key_fallback": fallback}
        return {key: {**values, "revision": revision(versions[key])} for key, values in result.items()}

    async def save(self, section: str, changes: dict[str, Any], user: str,
                   expected_revision: str | None = None) -> dict[str, Any]:
        async with self.lock:
            result = await asyncio.to_thread(self._save, section, changes, user, expected_revision)
            return result

    def _save(self, section: str, changes: dict[str, Any], user: str,
              expected_revision: str | None) -> dict[str, Any]:
        with self.thread_lock, self.file_lock() if section == "oauth" else nullcontext():
            current = self.snapshot(section)
            if section not in current:
                raise ValueError("未知设置分区")
            if expected_revision is not None and expected_revision != current[section]["revision"]:
                raise ConfigConflict("设置已被其他窗口修改，请刷新后重试")
            r, s = self.r, self.r.settings
            stamp = {"updated_at": datetime.now(timezone.utc).isoformat(), "updated_by": user}
            if section == "oauth":
                changes = {key: value for key, value in changes.items() if key != "oauth_7d_probe_interval_seconds"}
                if set(changes) - OAUTH_FIELDS:
                    raise ValueError("未知 OAuth 设置")
                values = {key: current[section][key] for key in OAUTH_FIELDS}
                values.update(changes)
                for key in ("oauth_recovery_monitor_enabled", "oauth_daily_test_enabled", "oauth_auto_reset_credit_enabled"):
                    if not isinstance(values[key], bool):
                        raise ValueError("开关必须为布尔值")
                values["oauth_daily_test_time"] = daily_test_time(values["oauth_daily_test_time"])
                limits = {"oauth_usage_refresh_concurrency": (1, 16), "oauth_recovery_test_concurrency": (1, 8),
                          "oauth_early_probe_batch_size": (1, 50)}
                for key, (low, high) in limits.items():
                    if isinstance(values[key], bool) or not isinstance(values[key], int) or not low <= values[key] <= high:
                        raise ValueError(f"{key} 必须在 {low}–{high} 之间")
                model = str(values["oauth_recovery_test_model_id"]).strip()
                if not model or len(model) > 160:
                    raise ValueError("测活模型无效")
                values["oauth_recovery_test_model_id"] = model
                from .recovery_policy import CONNECTION_IDS, MODEL_IDS, account_ids
                for field in (CONNECTION_IDS, MODEL_IDS):
                    values[field] = account_ids([] if values.get(field) is None else values[field])
                if set(values[CONNECTION_IDS]) & set(values[MODEL_IDS]):
                    raise ValueError("账号只能选择一种恢复验证方式")
                selected = set(values[CONNECTION_IDS]) | set(values[MODEL_IDS])
                if selected and ({CONNECTION_IDS, MODEL_IDS} & set(changes)):
                    from . import account_ops
                    eligible = {int(row["id"]) for row in account_ops.current_oauth_accounts(r.db)
                                if row.get("platform") == "openai" and row.get("type") == "oauth"
                                and not row.get("deleted_at") and not row.get("parent_account_id")
                                and not (row.get("extra") or {}).get("parent_account_id")}
                    # Keep stale IDs already saved for old clients; newly added
                    # accounts must belong to the current eligible inventory.
                    previous = set(current[section].get(CONNECTION_IDS) or []) | set(current[section].get(MODEL_IDS) or [])
                    if selected - previous - eligible:
                        raise ValueError("所选账号不属于可管理的 OpenAI OAuth 账号")
                payload = {**r.oauth_config_file(), **values, **stamp}
                from .recovery_policy import SEEN_IDS
                if SEEN_IDS in payload:
                    changed_selection = any(values[field] != (current[section].get(field) or [])
                                            for field in (CONNECTION_IDS, MODEL_IDS))
                    if expected_revision is None and changed_selection:
                        raise ConfigConflict("修改恢复账号前请刷新设置并提交配置版本")
                    # An explicit selection also counts as seen before the next
                    # inventory refresh, so manual removal is never undone.
                    payload[SEEN_IDS] = sorted(set(account_ids(payload[SEEN_IDS])) | selected)
                for retired in ("oauth_early_probe_interval_seconds", "oauth_recovery_push_enabled", "oauth_night_recovery_cooldown_enabled", "oauth_usage_refresh_enabled", "oauth_regular_refresh_interval_seconds"):
                    payload.pop(retired, None)
                monitor = getattr(r, 'oauth_monitor', None)
                if monitor and ({CONNECTION_IDS, MODEL_IDS} & set(changes)):
                    from .recovery_policy import recovery_method
                    from types import SimpleNamespace
                    following = SimpleNamespace(**values)
                    affected = (set(values[CONNECTION_IDS]) | set(values[MODEL_IDS])
                                | set(current[section].get(CONNECTION_IDS) or []) | set(current[section].get(MODEL_IDS) or []))
                    affected = {aid for aid in affected if recovery_method(s, aid) != recovery_method(following, aid)}
                    def invalidate(data):
                        for aid in affected:
                            meta = data['scheduler'].setdefault(str(aid), {})
                            meta['recovery_method_generation'] = int(meta.get('recovery_method_generation', 0)) + 1
                    if affected:
                        monitor.store.transaction(invalidate)
                r.save_oauth_runtime_config(payload)
                r.apply_oauth_runtime_config(payload)
            elif section == "bark":
                if set(changes) - {"enabled", "device_key", "server_url"}:
                    raise ValueError("未知 Bark 设置")
                with r.BARK_CONFIG_LOCK:
                    runtime = r.bark_notifier.runtime_config()
                    enabled = changes.get("enabled", runtime.enabled)
                    if not isinstance(enabled, bool):
                        raise ValueError("开关必须为布尔值")
                    key = str(changes.get("device_key") or "").strip() or runtime.device_key
                    url = str(changes.get("server_url") or "").strip()
                    try:
                        url = normalize_bark_server_url(url) if url else runtime.server_url or DEFAULT_BARK_SERVER_URL
                    except ValueError:
                        raise ValueError("Bark 服务 URL 无效；HTTP 仅允许 loopback") from None
                    if enabled and not key:
                        raise ValueError("启用 Bark 前需要填写 Device Key")
                    payload = {**r.bark_config_file(), "enabled": enabled, "device_key": key, "server_url": url, **stamp}
                    r.save_bark_runtime_config(payload)
                    r.apply_bark_runtime_config(payload)
            elif section == "key_fallback":
                if set(changes) - {"openai_enabled", "grok_enabled", "managed_account_ids", "coexist_account_ids"}:
                    raise ValueError("未知 Key 回退设置")
                if r.key_fallback_controller is None:
                    raise ValueError("Key 回退尚未就绪")
                values = {key: current[section][key] for key in ("openai_enabled", "grok_enabled", "managed_account_ids")}
                values.update(changes)
                values.setdefault("coexist_account_ids", [aid for aid in current[section].get("coexist_account_ids", []) if aid in values["managed_account_ids"]])
                self._validate_switches(values)
                r.key_fallback_controller.save_user_config(**values, user=user)
            write_audit(s.audit_path, f"{section}_config_update", {"user": user, "fields": sorted(changes)})
            return self.snapshot(section)[section]

    @staticmethod
    def _validate_switches(values: dict[str, Any]) -> None:
        for key in ("openai_enabled", "grok_enabled"):
            if key in values and not isinstance(values[key], bool):
                raise ValueError("开关必须为布尔值")
