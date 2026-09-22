"""Usage-log based model downgrade protection.

This module deliberately does not call the model probe or account update APIs.  It
only reads usage logs and performs a conditional JSONB deletion for one exact
mapping entry when the evidence and the live account still agree.
"""
from __future__ import annotations

import hashlib
import json
import math
import os
import tempfile
import threading
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Callable

from . import account_ops
from .audit import write_audit
from .bark import sanitize_error_text

MAX_BATCH = 500
SCAN_INTERVAL_SECONDS = 10
HISTORY_DAYS = 7


def _atomic_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    temporary = Path(name)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            fd = -1
            json.dump(payload, handle, ensure_ascii=False, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        temporary.replace(path)
        path.chmod(0o600)
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if fd >= 0:
            os.close(fd)
        temporary.unlink(missing_ok=True)


def normalize_model(value: object) -> str:
    text = str(value or "").strip().lower()
    if not text:
        return ""
    # Sub2API records dated model aliases; the pricing snapshot stores their
    # canonical day-independent entry when the prices are identical.
    for separator in ("@", ":"):
        if separator in text:
            base, suffix = text.rsplit(separator, 1)
            if len(suffix) in {8, 10} and suffix.replace("-", "").isdigit():
                text = base
                break
    if len(text) > 11 and text[-11] == "-" and text[-10:].replace("-", "").isdigit():
        text = text[:-11]
    return text


@dataclass(frozen=True)
class ModelPrice:
    input_price: float
    output_price: float
    unit: str = "token"


class PriceCatalog:
    def __init__(self, prices: dict[str, ModelPrice], *, source_sha256: str = "", collected_at: str = ""):
        self.prices = {normalize_model(key): value for key, value in prices.items() if normalize_model(key)}
        self.source_sha256 = source_sha256
        self.collected_at = collected_at

    @classmethod
    def from_payload(cls, payload: object) -> "PriceCatalog":
        if not isinstance(payload, dict):
            return cls({})
        raw = payload.get("prices", payload)
        if not isinstance(raw, dict):
            return cls({})
        prices: dict[str, ModelPrice] = {}
        for name, value in raw.items():
            if not isinstance(value, dict):
                continue
            try:
                input_price = float(value.get("input_price", value.get("input_cost_per_token")))
                output_price = float(value.get("output_price", value.get("output_cost_per_token")))
            except (TypeError, ValueError):
                continue
            unit = str(value.get("unit", "token") or "token").strip().lower()
            if math.isfinite(input_price) and math.isfinite(output_price) and input_price > 0 and output_price > 0:
                prices[str(name)] = ModelPrice(input_price, output_price, unit)
        return cls(prices, source_sha256=str(payload.get("source_sha256") or ""), collected_at=str(payload.get("collected_at") or ""))

    @classmethod
    def load(cls, path: str | Path) -> "PriceCatalog":
        try:
            raw = json.loads(Path(path).read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError):
            return cls({})
        return cls.from_payload(raw)

    def compare(self, expected: object, response: object) -> tuple[str, str]:
        expected_key = normalize_model(expected)
        response_key = normalize_model(response)
        if not expected_key or not response_key:
            return "missing", "缺少响应模型"
        left = self.prices.get(expected_key)
        right = self.prices.get(response_key)
        if left is None or right is None:
            return "unconfirmed", "未知模型价格"
        if left.unit != right.unit:
            return "unconfirmed", "计价单位不可比"
        lower_input = right.input_price <= left.input_price
        lower_output = right.output_price <= left.output_price
        strictly_lower = right.input_price < left.input_price or right.output_price < left.output_price
        if lower_input and lower_output and strictly_lower:
            return "confirmed", "响应模型价格更低"
        return "unconfirmed", "模型价格未确认进一步降级"


def classify_model_event(log: dict[str, Any], catalog: PriceCatalog) -> dict[str, Any]:
    requested = str(log.get("requested_model") or log.get("model") or "").strip()
    upstream = str(log.get("upstream_model") or requested).strip()
    response = str(log.get("upstream_response_model") or "").strip()
    endpoint = str(log.get("inbound_endpoint") or "").lower()
    chain = str(log.get("model_mapping_chain") or "").strip()
    if normalize_model(upstream) and normalize_model(upstream) == normalize_model(response):
        status, reason = "ok", "响应模型与实际上游模型一致"
    else:
        status, reason = catalog.compare(upstream, response)
    if status == "confirmed":
        reason = "确认降级"
    return {
        "status": status,
        "reason": reason,
        "requested_model": requested,
        "upstream_model": upstream,
        "response_model": response,
        "log_id": int(log.get("id") or 0),
        "account_id": int(log.get("account_id") or 0),
        "created_at": str(log.get("created_at") or ""),
        "platform": str(log.get("platform") or "").lower(),
        "inbound_endpoint": endpoint,
        "model_mapping_chain": chain,
    }


def _mapping_from_account(row: dict[str, Any]) -> dict[str, Any] | None:
    for source in (row.get("credentials"), row.get("extra")):
        if isinstance(source, dict) and isinstance(source.get("model_mapping"), dict):
            return source["model_mapping"]
    return None


def precise_mapping_key(row: dict[str, Any], requested: str, upstream: str) -> str | None:
    for source in (row.get("extra"), row.get("credentials")):
        if isinstance(source, dict) and (
            source.get("openai_passthrough") is True
            or source.get("openai_oauth_passthrough") is True
            or source.get("grok_passthrough") is True
        ):
            return None
    mapping = _mapping_from_account(row)
    if not mapping or requested not in mapping:
        return None
    key = str(mapping.get(requested) or "").strip()
    if not key or key != upstream or "*" in requested or "*" in key:
        return None
    # An absent or one-item mapping means removing it would switch the whole
    # account to allow-all semantics, so it is intentionally fail-closed.
    if len(mapping) <= 1:
        return None
    return requested


def remove_mapping_transaction(db: Any, evidence: dict[str, Any], row: dict[str, Any]) -> tuple[bool, str]:
    account_id = int(evidence.get("account_id") or 0)
    key = precise_mapping_key(row, str(evidence.get("requested_model") or ""), str(evidence.get("upstream_model") or ""))
    if "messages" in str(evidence.get("inbound_endpoint") or "").lower():
        return False, "Messages 路由无法确定精确白名单入口"
    if account_id <= 0 or key is None:
        return False, "无法精确定位模型入口"
    chain = str(evidence.get("model_mapping_chain") or "").strip()
    if chain:
        parts = [item.strip() for item in chain.replace("->", "→").split("→") if item.strip()]
        if len(parts) < 2 or normalize_model(parts[0]) != normalize_model(str(evidence.get("requested_model") or "")) or normalize_model(parts[-1]) != normalize_model(str(evidence.get("upstream_model") or "")):
            return False, "模型映射链与日志不一致"
    platform = str(row.get("platform") or "").strip().lower()
    account_type = str(row.get("type") or "").strip().lower()
    try:
        with db.connection() as conn:
            with conn.transaction():
                with conn.cursor() as cur:
                    cur.execute("SET LOCAL statement_timeout = '3s'")
                    cur.execute(
                        "SELECT id, platform, type, credentials, extra FROM accounts WHERE id=%(id)s AND deleted_at IS NULL FOR UPDATE",
                        {"id": account_id},
                    )
                    current = cur.fetchone()
                    if not isinstance(current, dict):
                        return False, "账号已删除"
                    if str(current.get("platform") or "").lower() != platform or str(current.get("type") or "").lower() != account_type:
                        return False, "账号平台或类型已变化"
                    current_mapping = _mapping_from_account(current)
                    if not isinstance(current_mapping, dict) or current_mapping.get(key) != evidence.get("upstream_model"):
                        return False, "模型映射已变化"
                    if len(current_mapping) <= 1 or "*" in key:
                        return False, "白名单无法精确移除"
                    updated_mapping = dict(current_mapping)
                    updated_mapping.pop(key, None)
                    credentials = current.get("credentials") if isinstance(current.get("credentials"), dict) else {}
                    if isinstance(credentials.get("model_mapping"), dict):
                        credentials = {**credentials, "model_mapping": updated_mapping}
                        cur.execute(
                            "UPDATE accounts SET credentials=%(credentials)s::jsonb, updated_at=NOW() WHERE id=%(id)s AND deleted_at IS NULL",
                            {"id": account_id, "credentials": json.dumps(credentials, ensure_ascii=False)},
                        )
                    else:
                        extra = current.get("extra") if isinstance(current.get("extra"), dict) else {}
                        extra = {**extra, "model_mapping": updated_mapping}
                        cur.execute(
                            "UPDATE accounts SET extra=%(extra)s::jsonb, updated_at=NOW() WHERE id=%(id)s AND deleted_at IS NULL",
                            {"id": account_id, "extra": json.dumps(extra, ensure_ascii=False)},
                        )
                    cur.execute(
                        "INSERT INTO scheduler_outbox (event_type, account_id, payload, dedup_key) VALUES (%(event_type)s, %(account_id)s, %(payload)s::jsonb, %(dedup_key)s)",
                        {
                            "event_type": "account_changed",
                            "account_id": account_id,
                            "payload": json.dumps({"reason": "model_guard_mapping_removed", "model": key}),
                            # A consumed outbox row may remain in deployments with
                            # a partial unique index; leave deduplication to the
                            # companion incident state and keep scheduler events replayable.
                            "dedup_key": None,
                        },
                    )
        return True, "已移除精确模型入口"
    except Exception:
        return False, "事务更新失败"


@dataclass(frozen=True)
class ModelGuardConfig:
    enabled: bool = False
    auto_remove: bool = False
    config_version: int = 0
    updated_at: str = ""
    updated_by: str = ""
    valid: bool = True


class ModelGuard:
    def __init__(self, settings: Any, db: Any, *, catalog: PriceCatalog | None = None, account_reader: Callable[..., Any] = account_ops.fallback_account) -> None:
        self.settings = settings
        self.db = db
        self.account_reader = account_reader
        self._lock = threading.RLock()
        self._catalog = catalog
        self._catalog_signature: tuple[int, int] | None = None

    def _paths(self) -> tuple[Path, Path, Path]:
        return (Path(self.settings.model_guard_config_path), Path(self.settings.model_guard_state_path), Path(self.settings.model_guard_pricing_path))

    def config(self) -> ModelGuardConfig:
        path, _state, _pricing = self._paths()
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return ModelGuardConfig()
        except (OSError, UnicodeError, json.JSONDecodeError):
            return ModelGuardConfig(valid=False)
        if not isinstance(data, dict):
            return ModelGuardConfig(valid=False)
        def strict(value: object, default: bool) -> tuple[bool, bool]:
            if isinstance(value, bool): return value, True
            if value is None: return default, True
            if str(value).lower() in {"1", "true", "yes", "on"}: return True, True
            if str(value).lower() in {"0", "false", "no", "off"}: return False, True
            return default, False
        enabled, ok1 = strict(data.get("enabled"), False)
        auto, ok2 = strict(data.get("auto_remove"), False)
        return ModelGuardConfig(enabled, auto, int(data.get("config_version") or 0), str(data.get("updated_at") or ""), str(data.get("updated_by") or ""), ok1 and ok2)

    def save_config(self, *, enabled: bool, auto_remove: bool, user: str) -> ModelGuardConfig:
        with self._lock:
            current = self.config()
            value = ModelGuardConfig(bool(enabled), bool(auto_remove), current.config_version + 1, datetime.now(timezone.utc).isoformat(), str(user or ""), True)
            _atomic_json(Path(self.settings.model_guard_config_path), {"enabled": value.enabled, "auto_remove": value.auto_remove, "config_version": value.config_version, "updated_at": value.updated_at, "updated_by": value.updated_by})
        write_audit(self.settings.audit_path, "model_guard_config_update", {"user": str(user or ""), "enabled": value.enabled, "auto_remove": value.auto_remove, "config_version": value.config_version})
        return value

    def panel_snapshot(self) -> dict[str, Any]:
        config = self.config()
        state = self._read_state()
        incidents = list((state.get("incidents") or {}).values()) if isinstance(state.get("incidents"), dict) else []
        incidents.sort(key=lambda item: str(item.get("latest_at") or ""), reverse=True)
        return {"enabled": config.enabled and config.valid, "auto_remove": config.auto_remove and config.valid, "config_valid": config.valid, "config_version": config.config_version, "incidents": incidents[:200], "cursor": state.get("cursor", 0)}

    def _read_state(self) -> dict[str, Any]:
        _config, path, _pricing = self._paths()
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (FileNotFoundError, OSError, UnicodeError, json.JSONDecodeError):
            return {"cursor": 0, "seen": [], "incidents": {}, "pending_events": [], "history_bootstrapped": False}
        return data if isinstance(data, dict) else {"cursor": 0, "seen": [], "incidents": {}, "pending_events": [], "history_bootstrapped": False}

    def _write_state(self, state: dict[str, Any]) -> None:
        _atomic_json(self._paths()[1], state)

    def _load_catalog(self) -> PriceCatalog:
        pricing_path = self._paths()[2]
        try:
            stat = pricing_path.stat()
            signature = (int(stat.st_mtime_ns), int(stat.st_size))
        except OSError:
            signature = None
        if self._catalog is not None and (signature is None or signature == self._catalog_signature):
            return self._catalog
        self._catalog = PriceCatalog.load(pricing_path)
        self._catalog_signature = signature
        return self._catalog

    def _fetch_logs(self, cursor: int, *, limit: int = MAX_BATCH, since: datetime | None = None) -> list[dict[str, Any]]:
        clause = " AND created_at >= %(since)s" if since is not None else ""
        params: dict[str, Any] = {"cursor": int(cursor), "limit": int(limit)}
        if since is not None:
            params["since"] = since
        return self.db.fetch_all(
            f"SELECT id, account_id, requested_model, model, upstream_model, upstream_response_model, model_mapping_chain, inbound_endpoint, created_at FROM usage_logs WHERE id > %(cursor)s{clause} ORDER BY id LIMIT %(limit)s",
            params,
        )

    def _fetch_overlap_logs(self, cursor: int, now: datetime, *, limit: int = MAX_BATCH) -> list[dict[str, Any]]:
        # IDs can be allocated before a transaction commits. A short time
        # overlap catches such late rows without rewinding the durable cursor.
        return self.db.fetch_all(
            "SELECT id, account_id, requested_model, model, upstream_model, upstream_response_model, model_mapping_chain, inbound_endpoint, created_at FROM usage_logs WHERE id <= %(cursor)s AND created_at >= %(since)s ORDER BY id LIMIT %(limit)s",
            {"cursor": int(cursor), "since": now - timedelta(minutes=15), "limit": int(limit)},
        )

    def run_once(self, now: datetime | None = None) -> dict[str, Any]:
        with self._lock:
            config = self.config()
            if not config.valid or not config.enabled:
                return {"skipped": True, "reason": "disabled"}
            state = self._read_state()
            current_time = now or datetime.now(timezone.utc)
            history_scan = not bool(state.get("history_bootstrapped"))
            logs = self._fetch_logs(
                int(state.get("cursor") or 0),
                since=current_time - timedelta(days=HISTORY_DAYS) if history_scan else None,
            )
            if not history_scan and state.get("cursor"):
                logs.extend(self._fetch_overlap_logs(int(state.get("cursor") or 0), current_time))
                logs_by_id = {int(item.get("id") or 0): item for item in logs if int(item.get("id") or 0) > 0}
                logs = [logs_by_id[key] for key in sorted(logs_by_id)]
            if not logs:
                if history_scan and state.get("cursor"):
                    state["history_bootstrapped"] = True
                    self._write_state(state)
                return {"processed": 0, "events": []}
            catalog = self._load_catalog()
            seen = {int(item) for item in state.get("seen", []) if str(item).isdigit()}
            incidents = state.setdefault("incidents", {})
            events: list[dict[str, Any]] = []
            history_batch = not bool(state.get("history_bootstrapped"))
            history_notified = {int(item) for item in state.get("history_notified_accounts", []) if str(item).isdigit()}
            account_cache: dict[int, dict[str, Any] | None] = {}
            for log in logs:
                log_id = int(log.get("id") or 0)
                if log_id <= 0 or log_id in seen:
                    continue
                seen.add(log_id)
                evidence = classify_model_event(log, catalog)
                if evidence["status"] in {"missing", "ok"}:
                    continue
                account_id = evidence["account_id"]
                if account_id not in account_cache:
                    try:
                        account_cache[account_id] = self.account_reader(self.db, account_id)
                    except Exception:
                        account_cache[account_id] = None
                account = account_cache[account_id]
                if isinstance(account, dict):
                    evidence["platform"] = str(account.get("platform") or evidence.get("platform") or "").lower()
                    evidence["account_name"] = str(account.get("name") or "-")
                    evidence["account_type"] = str(account.get("type") or "").lower()
                key = f"{account_id}:{normalize_model(evidence['requested_model'])}"
                incident = incidents.setdefault(key, {"account_id": account_id, "account_name": evidence.get("account_name", "-"), "account_type": evidence.get("account_type", ""), "requested_model": evidence["requested_model"], "platform": evidence["platform"], "first_at": evidence["created_at"], "latest_at": evidence["created_at"], "count": 0, "history": history_batch, "status": evidence["status"], "action": "未执行", "reason": evidence["reason"]})
                incident["latest_at"] = evidence["created_at"]
                incident["count"] = int(incident.get("count") or 0) + 1
                incident["response_model"] = evidence["response_model"]
                incident["upstream_model"] = evidence["upstream_model"]
                incident["log_id"] = log_id
                current_config = self.config()
                if evidence["status"] == "confirmed" and current_config.config_version == config.config_version and current_config.auto_remove and not history_batch:
                    if (
                        isinstance(account, dict)
                        and str(account.get("platform") or "").lower() in {"openai", "grok"}
                        and str(account.get("type") or "").lower() in {"oauth", "apikey"}
                    ):
                        ok, action = remove_mapping_transaction(self.db, evidence, account)
                    elif isinstance(account, dict):
                        ok, action = False, "账号平台或类型不在监控范围"
                    else:
                        ok, action = False, "账号已删除"
                    incident["action"] = action
                    incident["status"] = "confirmed"
                    if ok:
                        incident["action"] = "已自动移除"
                elif evidence["status"] == "confirmed":
                    incident["action"] = "历史仅告警" if incident.get("history") else "未执行"
                else:
                    incident["action"] = "待核实"
                should_notify = incident.get("count") == 1 or incident.get("action") in {"已自动移除", "事务更新失败"}
                if history_batch and account_id in history_notified:
                    should_notify = False
                if should_notify:
                    events.append({**evidence, "incident": incident.copy(), "title": "确认模型降级" if evidence["status"] == "confirmed" else "模型不一致待核实"})
                    if history_batch:
                        history_notified.add(account_id)
            if history_batch and events:
                grouped: dict[int, dict[str, Any]] = {}
                for event in events:
                    account_id = int(event.get("account_id") or 0)
                    grouped.setdefault(account_id, event)
                events = list(grouped.values())
            state["cursor"] = max(int(state.get("cursor") or 0), max(int(row.get("id") or 0) for row in logs))
            state["seen"] = sorted(seen)[-10000:]
            state["incidents"] = incidents
            state["history_notified_accounts"] = sorted(history_notified)
            if history_batch and len(logs) < MAX_BATCH:
                state["history_bootstrapped"] = True
            pending = list(state.get("pending_events") or [])
            pending.extend(events)
            state["pending_events"] = pending[-500:]
            self._write_state(state)
            return {"processed": len(logs), "events": events, "cursor": state["cursor"]}

    def pending_events(self) -> list[dict[str, Any]]:
        return list(self._read_state().get("pending_events") or [])

    def mark_events_delivered(self, events: list[dict[str, Any]], *, suppressed: bool = False) -> None:
        if not events:
            return
        state = self._read_state()
        pending = list(state.get("pending_events") or [])
        ids = {int(item.get("log_id") or 0) for item in events}
        state["pending_events"] = [item for item in pending if int(item.get("log_id") or 0) not in ids]
        self._write_state(state)


def model_guard_event_message(event: dict[str, Any]) -> tuple[str, str]:
    incident = event.get("incident") if isinstance(event.get("incident"), dict) else {}
    confirmed = event.get("status") == "confirmed"
    title = "确认模型降级" if confirmed else "模型不一致待核实"
    body = "\n".join([
        f"账号 ID：{int(event.get('account_id') or 0)}",
        f"名称：{sanitize_error_text(event.get('account_name') or '-', 100)}",
        f"平台/类型：{sanitize_error_text(event.get('platform') or '-', 40)}/{sanitize_error_text(event.get('account_type') or '-', 40)}",
        f"模型链：{sanitize_error_text(event.get('requested_model'), 100)} → {sanitize_error_text(event.get('upstream_model'), 100)} → {sanitize_error_text(event.get('response_model') or '未知', 100)}",
        f"时间：{sanitize_error_text(event.get('created_at'), 80)}",
        f"日志 ID：{int(event.get('log_id') or 0)}",
        f"累计：{int(incident.get('count') or 1)} 次",
        f"处置：{sanitize_error_text(incident.get('action') or '未执行', 100)}",
        f"原因：{sanitize_error_text(incident.get('reason') or event.get('reason'), 180)}",
    ])
    return title, body
