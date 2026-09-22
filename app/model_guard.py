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
import re
import tempfile
import threading
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Callable
from zoneinfo import ZoneInfo

from . import account_ops
from .audit import write_audit
from .bark import sanitize_error_text

MAX_BATCH = 500
SCAN_INTERVAL_SECONDS = 10
HISTORY_DAYS = 7
OVERLAP_MINUTES = 15
RULE_VERSION = 2
PLATFORMS = ("openai", "grok")
OPENAI_RANK = {"luna": 0, "terra": 1, "sol": 2, "astra": 3}
OPENAI_MODEL = re.compile(r"^gpt-(6|5\.6)-(astra|sol|terra|luna)$")


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
    elif len(text) > 9 and text[-9] == "-" and text[-8:].isdigit():
        text = text[:-9]
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


def compare_openai_models(upstream: object, response: object, catalog: PriceCatalog) -> tuple[str, str]:
    left = OPENAI_MODEL.fullmatch(normalize_model(upstream))
    right = OPENAI_MODEL.fullmatch(normalize_model(response))
    if left and right:
        left_rank = (OPENAI_RANK[left.group(2)], 1 if left.group(1) == "6" else 0)
        right_rank = (OPENAI_RANK[right.group(2)], 1 if right.group(1) == "6" else 0)
        if right_rank < left_rank:
            return "confirmed", "OpenAI 档位或同档代际降低"
        return "ok", "OpenAI 档位或同档代际未降低"
    return catalog.compare(upstream, response)


def classify_model_event(log: dict[str, Any], catalog: PriceCatalog) -> dict[str, Any]:
    requested = str(log.get("requested_model") or log.get("model") or "").strip()
    upstream = str(log.get("upstream_model") or "").strip()
    response = str(log.get("upstream_response_model") or "").strip()
    endpoint = str(log.get("inbound_endpoint") or "").lower()
    chain = str(log.get("model_mapping_chain") or "").strip()
    platform = str(log.get("platform") or "").lower()
    if not normalize_model(upstream):
        status, reason = "unconfirmed", "缺少实际上游模型"
    elif normalize_model(upstream) == normalize_model(response):
        status, reason = "ok", "响应模型与实际上游模型一致"
    else:
        status, reason = compare_openai_models(upstream, response, catalog) if platform == "openai" else catalog.compare(upstream, response)
    return {
        "status": status,
        "reason": reason,
        "requested_model": requested,
        "upstream_model": upstream,
        "response_model": response,
        "log_id": int(log.get("id") or 0),
        "account_id": int(log.get("account_id") or 0),
        "created_at": str(log.get("created_at") or ""),
        "platform": platform,
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
    if any(
        pattern != requested and pattern.endswith("*") and requested.startswith(pattern[:-1])
        for pattern in mapping
    ):
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
    if platform not in {"openai", "grok"} or account_type not in {"oauth", "apikey"}:
        return False, "账号平台或类型不在监控范围"
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
                    if precise_mapping_key(current, str(evidence.get("requested_model") or ""), str(evidence.get("upstream_model") or "")) != key:
                        return False, "模型入口或透传设置已变化"
                    current_mapping = _mapping_from_account(current)
                    if not isinstance(current_mapping, dict) or current_mapping.get(key) != evidence.get("upstream_model"):
                        return False, "模型映射已变化"
                    if len(current_mapping) <= 1 or "*" in key:
                        return False, "白名单无法精确移除"
                    credentials = current.get("credentials") if isinstance(current.get("credentials"), dict) else {}
                    if isinstance(credentials.get("model_mapping"), dict):
                        cur.execute(
                            "UPDATE accounts SET credentials=jsonb_set(credentials, '{model_mapping}', "
                            "(credentials->'model_mapping') - %(key)s), updated_at=NOW() "
                            "WHERE id=%(id)s AND deleted_at IS NULL AND lower(platform)=%(platform)s "
                            "AND lower(type)=%(type)s AND credentials->'model_mapping'->>%(key)s=%(upstream)s",
                            {"id": account_id, "key": key, "upstream": evidence["upstream_model"], "platform": platform, "type": account_type},
                        )
                    else:
                        cur.execute(
                            "UPDATE accounts SET extra=jsonb_set(extra, '{model_mapping}', "
                            "(extra->'model_mapping') - %(key)s), updated_at=NOW() "
                            "WHERE id=%(id)s AND deleted_at IS NULL AND lower(platform)=%(platform)s "
                            "AND lower(type)=%(type)s AND extra->'model_mapping'->>%(key)s=%(upstream)s",
                            {"id": account_id, "key": key, "upstream": evidence["upstream_model"], "platform": platform, "type": account_type},
                        )
                    if cur.rowcount != 1:
                        return False, "模型映射并发变化"
                    cur.execute(
                        "INSERT INTO scheduler_outbox (event_type, account_id, payload) VALUES (%(event_type)s, %(account_id)s, %(payload)s::jsonb)",
                        {
                            "event_type": "account_changed",
                            "account_id": account_id,
                            "payload": json.dumps({"reason": "model_guard_mapping_removed", "model": key}),
                        },
                    )
        return True, "已移除精确模型入口"
    except Exception:
        return False, "事务更新失败"


@dataclass(frozen=True)
class ModelGuardConfig:
    openai_enabled: bool = False
    grok_enabled: bool = False
    auto_remove: bool = False
    config_version: int = 0
    updated_at: str = ""
    updated_by: str = ""
    valid: bool = True

    def platform_enabled(self, platform: str) -> bool:
        return bool(self.valid and (self.openai_enabled if platform == "openai" else self.grok_enabled if platform == "grok" else False))


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
        legacy, legacy_ok = strict(data.get("enabled"), False)
        openai, openai_ok = strict(data.get("openai_enabled"), legacy)
        grok, grok_ok = strict(data.get("grok_enabled"), legacy)
        auto, auto_ok = strict(data.get("auto_remove"), False)
        try:
            version = int(data.get("config_version") or 0)
        except (TypeError, ValueError):
            version = 0
            legacy_ok = False
        return ModelGuardConfig(openai, grok, auto, version, str(data.get("updated_at") or ""), str(data.get("updated_by") or ""), legacy_ok and openai_ok and grok_ok and auto_ok)

    def save_config(self, *, openai_enabled: bool, grok_enabled: bool, auto_remove: bool, user: str) -> ModelGuardConfig:
        with self._lock:
            current = self.config()
            if not current.valid:
                raise ValueError("invalid model guard configuration")
            value = ModelGuardConfig(bool(openai_enabled), bool(grok_enabled), bool(auto_remove), current.config_version + 1, datetime.now(timezone.utc).isoformat(), str(user or ""), True)
            state = self._read_state()
            self._ensure_platform_state(state, current)
            for platform in PLATFORMS:
                if current.platform_enabled(platform) and not value.platform_enabled(platform):
                    state["platforms"][platform]["last_enabled"] = False
                    state["platforms"][platform]["disabled_at"] = value.updated_at
            self._write_state(state)
            _atomic_json(Path(self.settings.model_guard_config_path), {"openai_enabled": value.openai_enabled, "grok_enabled": value.grok_enabled, "auto_remove": value.auto_remove, "config_version": value.config_version, "updated_at": value.updated_at, "updated_by": value.updated_by})
        write_audit(self.settings.audit_path, "model_guard_config_update", {"user": str(user or ""), "openai_enabled": value.openai_enabled, "grok_enabled": value.grok_enabled, "auto_remove": value.auto_remove, "config_version": value.config_version})
        return value

    def panel_snapshot(self) -> dict[str, Any]:
        config = self.config()
        state = self._read_state()
        incidents = list((state.get("incidents") or {}).values()) if isinstance(state.get("incidents"), dict) else []
        for item in incidents:
            if item.get("history") and item.get("status") == "confirmed" and item.get("action") == "待核实":
                item["action"] = "历史仅告警"
        incidents.sort(key=lambda item: str(item.get("latest_at") or ""), reverse=True)
        return {"openai_enabled": config.platform_enabled("openai"), "grok_enabled": config.platform_enabled("grok"), "auto_remove": config.auto_remove and config.valid, "config_valid": config.valid, "config_version": config.config_version, "incidents": incidents[:200], "cursors": {platform: state.get("platforms", {}).get(platform, {}).get("cursor", state.get("cursor", 0)) for platform in PLATFORMS}}

    def _read_state(self) -> dict[str, Any]:
        _config, path, _pricing = self._paths()
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (FileNotFoundError, OSError, UnicodeError, json.JSONDecodeError):
            return {"incidents": {}, "pending_events": []}
        return data if isinstance(data, dict) else {"incidents": {}, "pending_events": []}

    def _ensure_platform_state(self, state: dict[str, Any], config: ModelGuardConfig) -> None:
        platforms = state.setdefault("platforms", {})
        legacy = "cursor" in state
        for platform in PLATFORMS:
            if platform in platforms:
                continue
            platforms[platform] = {
                "cursor": int(state.get("cursor") or 0) if legacy else 0,
                "history_bootstrapped": bool(state.get("history_bootstrapped")) if legacy else False,
                "last_enabled": config.platform_enabled(platform) if legacy else False,
                "seen_at": {str(item): datetime.now(timezone.utc).isoformat() for item in state.get("seen", []) if str(item).isdigit()} if legacy else {},
                "overlap_cursor": 0,
                "coverage": {"total": 0, "missing_response": 0},
            }
        state.setdefault("incidents", {})
        state.setdefault("pending_events", [])

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

    @staticmethod
    def _incident_key(evidence: dict[str, Any]) -> str:
        return ":".join(str(part) for part in (
            evidence.get("platform"), evidence.get("account_id"),
            normalize_model(evidence.get("requested_model")),
            normalize_model(evidence.get("upstream_model")),
            normalize_model(evidence.get("response_model")),
        ))

    def _fetch_legacy_logs(self, account_id: int, requested: str, after: int, upper_id: int, since: datetime) -> list[dict[str, Any]]:
        return self.db.fetch_all(
            "SELECT l.id, l.account_id, l.requested_model, l.model, l.upstream_model, l.upstream_response_model, "
            "l.model_mapping_chain, l.inbound_endpoint, l.created_at, lower(a.platform) AS platform "
            "FROM usage_logs l JOIN accounts a ON a.id = l.account_id "
            "WHERE l.account_id = %(legacy_account_id)s AND COALESCE(NULLIF(l.requested_model, ''), l.model) = %(requested)s "
            "AND l.id > %(after)s AND l.id <= %(upper_id)s AND l.created_at >= %(since)s ORDER BY l.id LIMIT %(limit)s",
            {"legacy_account_id": account_id, "requested": requested, "after": after, "upper_id": upper_id, "since": since, "limit": MAX_BATCH},
        )

    def _migrate_incidents(self, state: dict[str, Any], catalog: PriceCatalog, now: datetime) -> list[dict[str, Any]]:
        if int(state.get("rule_version") or 1) >= RULE_VERSION:
            return []
        old = state.get("incidents") if isinstance(state.get("incidents"), dict) else {}
        rebuilt: dict[str, dict[str, Any]] = {}
        newly_confirmed: dict[int, dict[str, Any]] = {}
        for item in old.values():
            if not isinstance(item, dict):
                continue
            account_id = int(item.get("account_id") or 0)
            requested = str(item.get("requested_model") or "")
            rows: list[dict[str, Any]] = []
            after = 0
            if account_id > 0 and requested:
                while True:
                    page = self._fetch_legacy_logs(account_id, requested, after, int(state.get("cursor") or item.get("log_id") or 0), now - timedelta(days=HISTORY_DAYS))
                    rows.extend(page)
                    if len(page) < MAX_BATCH:
                        break
                    after = int(page[-1]["id"])
            fallback = not rows
            if fallback:
                rows = [{
                    "id": item.get("log_id"), "account_id": account_id, "requested_model": requested,
                    "upstream_model": item.get("upstream_model"), "upstream_response_model": item.get("response_model"),
                    "created_at": item.get("latest_at"), "platform": item.get("platform"),
                }]
            for row in rows:
                evidence = classify_model_event(row, catalog)
                if evidence["status"] not in {"confirmed", "unconfirmed"}:
                    continue
                key = self._incident_key(evidence)
                first = key not in rebuilt
                converted = rebuilt.setdefault(key, {
                    "account_id": account_id, "account_name": item.get("account_name", "-"),
                    "account_type": item.get("account_type", ""), "platform": evidence["platform"],
                    "requested_model": evidence["requested_model"], "upstream_model": evidence["upstream_model"],
                    "response_model": evidence["response_model"], "first_at": evidence["created_at"],
                    "latest_at": evidence["created_at"], "count": 0, "history": True,
                    "status": evidence["status"], "action": "历史仅告警" if evidence["status"] == "confirmed" else "待核实",
                    "reason": evidence["reason"], "log_id": evidence["log_id"],
                })
                converted["count"] += 1
                if str(evidence["created_at"]) < str(converted["first_at"]):
                    converted["first_at"] = evidence["created_at"]
                if str(evidence["created_at"]) >= str(converted["latest_at"]):
                    converted["latest_at"] = evidence["created_at"]
                    converted["log_id"] = evidence["log_id"]
                if fallback and int(item.get("count") or 0) > 1:
                    converted["legacy_aggregate_count"] = int(item["count"])
                if item.get("action") == "已自动移除" and evidence["log_id"] == int(item.get("log_id") or 0):
                    converted["action"] = "已自动移除"
                prior_status = catalog.compare(evidence["upstream_model"], evidence["response_model"])[0]
                if evidence["status"] == "confirmed" and prior_status != "confirmed":
                    newly_confirmed.setdefault(account_id, {**evidence, "account_name": item.get("account_name", "-"), "account_type": item.get("account_type", ""), "incident": converted.copy()})
                if first and evidence["status"] == "confirmed" and prior_status != "confirmed":
                    converted["action"] = "历史重判仅告警"
        state["legacy_incidents"] = old
        state["incidents"] = rebuilt
        state["rule_version"] = RULE_VERSION
        events = []
        for account_id, evidence in newly_confirmed.items():
            events.append({**evidence, "kind": "history_reclassified", "event_id": f"reclassified:v{RULE_VERSION}:{account_id}"})
        return events

    def _fetch_logs(self, platform: str, cursor: int, *, since: datetime | None = None, upper_id: int | None = None) -> list[dict[str, Any]]:
        clause = " AND l.created_at >= %(since)s" if since is not None else ""
        clause += " AND l.id <= %(upper_id)s" if upper_id is not None else ""
        params: dict[str, Any] = {"platform": platform, "cursor": int(cursor), "limit": MAX_BATCH}
        if since is not None:
            params["since"] = since
        if upper_id is not None:
            params["upper_id"] = int(upper_id)
        return self.db.fetch_all(
            "SELECT l.id, l.account_id, l.requested_model, l.model, l.upstream_model, l.upstream_response_model, "
            "l.model_mapping_chain, l.inbound_endpoint, l.created_at, lower(a.platform) AS platform "
            "FROM usage_logs l JOIN accounts a ON a.id = l.account_id "
            f"WHERE lower(a.platform) = %(platform)s AND lower(a.type) IN ('oauth', 'apikey') AND l.id > %(cursor)s{clause} ORDER BY l.id LIMIT %(limit)s",
            params,
        )

    def _fetch_overlap_logs(self, platform: str, cursor: int, after: int, now: datetime) -> list[dict[str, Any]]:
        return self.db.fetch_all(
            "SELECT l.id, l.account_id, l.requested_model, l.model, l.upstream_model, l.upstream_response_model, "
            "l.model_mapping_chain, l.inbound_endpoint, l.created_at, lower(a.platform) AS platform "
            "FROM usage_logs l JOIN accounts a ON a.id = l.account_id "
            "WHERE lower(a.platform) = %(platform)s AND lower(a.type) IN ('oauth', 'apikey') "
            "AND l.id > %(after)s AND l.id <= %(cursor)s AND l.created_at >= %(since)s ORDER BY l.id LIMIT %(limit)s",
            {"platform": platform, "after": after, "cursor": cursor, "since": now - timedelta(minutes=OVERLAP_MINUTES), "limit": MAX_BATCH},
        )

    def _max_log_id(self, platform: str) -> int:
        row = self.db.fetch_one("SELECT COALESCE(MAX(l.id), 0) AS id FROM usage_logs l JOIN accounts a ON a.id = l.account_id WHERE lower(a.platform) = %(platform)s AND lower(a.type) IN ('oauth', 'apikey')", {"platform": platform})
        return int((row or {}).get("id") or 0)

    @staticmethod
    def _queue_event(state: dict[str, Any], event: dict[str, Any]) -> None:
        pending = state.setdefault("pending_events", [])
        event_id = event.get("event_id")
        if not event_id or not any(item.get("event_id") == event_id for item in pending):
            pending.append(event)

    def _queue_migration_summaries(self, state: dict[str, Any]) -> list[dict[str, Any]]:
        if int(state.get("migration_summary_version") or 0) >= RULE_VERSION or not state.get("legacy_incidents"):
            return []
        grouped: dict[int, dict[str, Any]] = {}
        for incident in state.get("incidents", {}).values():
            if not isinstance(incident, dict) or incident.get("status") != "confirmed" or not incident.get("history"):
                continue
            account_id = int(incident.get("account_id") or 0)
            if account_id <= 0:
                continue
            event = grouped.setdefault(account_id, {
                "kind": "history_summary", "event_id": f"migration-history:v{RULE_VERSION}:{account_id}",
                "account_id": account_id, "account_name": incident.get("account_name", "-"),
                "account_type": incident.get("account_type", ""), "platform": incident.get("platform", ""),
                "requested_model": incident.get("requested_model", ""),
                "upstream_model": incident.get("upstream_model", ""), "response_model": incident.get("response_model", ""),
                "created_at": incident.get("latest_at", ""), "log_id": incident.get("log_id", 0),
                "status": "confirmed", "incident": dict(incident), "count": 0,
            })
            event["count"] += int(incident.get("count") or 0)
            if str(incident.get("latest_at") or "") > str(event["created_at"]):
                event.update(created_at=incident.get("latest_at", ""), log_id=incident.get("log_id", 0),
                             requested_model=incident.get("requested_model", ""), upstream_model=incident.get("upstream_model", ""),
                             response_model=incident.get("response_model", ""), incident=dict(incident))
        state["migration_summary_version"] = RULE_VERSION
        existing_accounts = {
            int(item.get("account_id") or 0) for item in state.get("pending_events", [])
            if item.get("kind") == "history_reclassified"
        }
        events = [event for account_id, event in grouped.items() if account_id not in existing_accounts]
        for event in events:
            self._queue_event(state, event)
        return events

    def _reconcile_claims(self, state: dict[str, Any]) -> None:
        for claim in state.setdefault("action_claims", {}).values():
            if claim.get("status") != "started":
                continue
            claim["status"] = "uncertain"
            evidence = claim.get("evidence") or {}
            incident = state.get("incidents", {}).get(claim.get("incident_key"))
            if isinstance(incident, dict):
                incident["action"] = "处置结果待核实"
            self._queue_event(state, {**evidence, "incident": dict(incident or {}), "event_id": f"claim-uncertain:{claim.get('id')}", "kind": "action_uncertain"})

    def run_once(self, now: datetime | None = None) -> dict[str, Any]:
        with self._lock:
            config = self.config()
            if not config.valid:
                return {"skipped": True, "reason": "invalid_config"}
            state = self._read_state()
            self._ensure_platform_state(state, config)
            current_time = now or datetime.now(timezone.utc)
            catalog = self._load_catalog()
            events = self._migrate_incidents(state, catalog, current_time)
            for legacy_key in ("cursor", "seen", "history_bootstrapped", "history_notified_accounts"):
                state.pop(legacy_key, None)
            for event in events:
                self._queue_event(state, event)
            events.extend(self._queue_migration_summaries(state))
            self._reconcile_claims(state)
            claims = state.setdefault("action_claims", {})
            for claim_id, claim in list(claims.items()):
                if claim.get("status") == "started":
                    continue
                try:
                    expired = datetime.fromisoformat(str(claim.get("at") or "")) < current_time - timedelta(days=HISTORY_DAYS)
                except ValueError:
                    expired = False
                if expired:
                    claims.pop(claim_id, None)
            processed = 0
            for platform in PLATFORMS:
                progress = state["platforms"][platform]
                if not config.platform_enabled(platform):
                    progress["last_enabled"] = False
                    progress.setdefault("disabled_at", current_time.isoformat())
                    continue
                if not progress.get("last_enabled"):
                    disabled_at = progress.pop("disabled_at", "")
                    since = current_time - timedelta(days=HISTORY_DAYS)
                    try:
                        since = max(since, datetime.fromisoformat(disabled_at)) if disabled_at else since
                    except ValueError:
                        pass
                    upper_id = self._max_log_id(platform)
                    progress["history"] = {"cursor": 0, "upper_id": upper_id, "since": since.isoformat(), "accounts": {}, "batch_id": f"{platform}:{upper_id}:{since.isoformat()}"}
                    progress["last_enabled"] = True
                history = progress.get("history")
                if isinstance(history, dict):
                    upper_id = int(history.get("upper_id") or 0)
                    logs = self._fetch_logs(platform, int(history.get("cursor") or 0), since=datetime.fromisoformat(history["since"]), upper_id=upper_id) if upper_id else []
                    history_batch = True
                else:
                    cursor = int(progress.get("cursor") or 0)
                    logs = self._fetch_logs(platform, cursor)
                    history_batch = False
                    if cursor and len(logs) < MAX_BATCH:
                        after = int(progress.get("overlap_cursor") or 0)
                        overlap = self._fetch_overlap_logs(platform, cursor, after, current_time)
                        if not overlap and after:
                            progress["overlap_cursor"] = 0
                        elif overlap:
                            progress["overlap_cursor"] = int(overlap[-1]["id"])
                            logs.extend(overlap)
                    logs = list({int(row["id"]): row for row in logs}.values())
                    logs.sort(key=lambda row: int(row["id"]))
                seen_at = progress.setdefault("seen_at", {})
                cutoff = current_time - timedelta(minutes=OVERLAP_MINUTES)
                seen_at_copy = dict(seen_at)
                for log_id, checked_at in seen_at_copy.items():
                    try:
                        if datetime.fromisoformat(checked_at) < cutoff:
                            seen_at.pop(log_id, None)
                    except (TypeError, ValueError):
                        seen_at.pop(log_id, None)
                account_cache: dict[int, dict[str, Any] | None] = {}
                for log in logs:
                    log_id = int(log.get("id") or 0)
                    if log_id <= 0 or str(log_id) in seen_at:
                        continue
                    seen_at[str(log_id)] = current_time.isoformat()
                    processed += 1
                    coverage = progress.setdefault("coverage", {"total": 0, "missing_response": 0})
                    coverage["total"] += 1
                    evidence = classify_model_event(log, catalog)
                    if evidence["status"] == "missing":
                        coverage["missing_response"] += 1
                    if evidence["status"] in {"missing", "ok"}:
                        continue
                    account_id = evidence["account_id"]
                    if account_id not in account_cache:
                        try:
                            account_cache[account_id] = self.account_reader(self.db, account_id)
                        except Exception:
                            account_cache[account_id] = None
                    account = account_cache[account_id]
                    evidence["account_name"] = str((account or {}).get("name") or "-")
                    evidence["account_type"] = str((account or {}).get("type") or "").lower()
                    key = self._incident_key(evidence)
                    incidents = state["incidents"]
                    first = key not in incidents
                    incident = incidents.setdefault(key, {"account_id": account_id, "account_name": evidence["account_name"], "account_type": evidence["account_type"], "requested_model": evidence["requested_model"], "platform": platform, "first_at": evidence["created_at"], "count": 0, "history": history_batch, "status": evidence["status"], "action": "未执行"})
                    previous_action = incident.get("action")
                    incident.update(latest_at=evidence["created_at"], count=int(incident.get("count") or 0) + 1, status=evidence["status"], reason=evidence["reason"], upstream_model=evidence["upstream_model"], response_model=evidence["response_model"], log_id=log_id)
                    if not history_batch:
                        incident["history"] = False
                    if evidence["status"] == "confirmed" and not history_batch and config.auto_remove:
                        current_config = self.config()
                        if current_config.config_version == config.config_version and current_config.platform_enabled(platform) and current_config.auto_remove:
                            claim_id = f"{platform}:{log_id}"
                            claims = state.setdefault("action_claims", {})
                            if claim_id not in claims:
                                claims[claim_id] = {"id": claim_id, "at": current_time.isoformat(), "status": "started", "evidence": evidence, "incident_key": key}
                                incident["action"] = "处置进行中"
                                self._write_state(state)
                                if isinstance(account, dict) and str(account.get("platform") or "").lower() == platform and evidence["account_type"] in {"oauth", "apikey"}:
                                    ok, action = remove_mapping_transaction(self.db, evidence, account)
                                else:
                                    ok, action = False, "账号已删除或平台类型变化"
                                incident["action"] = "已自动移除" if ok else action
                                claims[claim_id]["status"] = "done"
                                claims[claim_id]["result"] = incident["action"]
                            else:
                                incident["action"] = "处置结果待核实"
                        else:
                            incident["action"] = "配置已变化，未执行"
                    elif evidence["status"] == "confirmed" and history_batch:
                        incident["action"] = previous_action if previous_action == "已自动移除" else "历史仅告警"
                    elif evidence["status"] == "confirmed":
                        incident["action"] = "未执行"
                    else:
                        incident["action"] = "待核实"
                    event = {**evidence, "incident": incident.copy(), "event_id": f"log:v{RULE_VERSION}:{platform}:{log_id}"}
                    if history_batch:
                        summaries = history["accounts"]
                        summary = summaries.setdefault(str(account_id), {**event, "count": 0})
                        summary["count"] += 1
                        if evidence["status"] == "confirmed":
                            summary.update(event)
                    elif first or incident["action"] != previous_action:
                        self._queue_event(state, event)
                        events.append(event)
                if history_batch:
                    if logs:
                        history["cursor"] = max(int(history.get("cursor") or 0), int(logs[-1]["id"]))
                    if len(logs) < MAX_BATCH:
                        for account_id, summary in history["accounts"].items():
                            summary["kind"] = "history_summary"
                            summary["event_id"] = f"history:v{RULE_VERSION}:{history['batch_id']}:{account_id}"
                            self._queue_event(state, summary)
                            events.append(summary)
                        progress["cursor"] = max(int(progress.get("cursor") or 0), int(history["upper_id"]))
                        progress["history_bootstrapped"] = True
                        progress.pop("history", None)
                elif logs:
                    progress["cursor"] = max(int(progress.get("cursor") or 0), max(int(row["id"]) for row in logs))
            self._write_state(state)
            if not any(config.platform_enabled(platform) for platform in PLATFORMS):
                return {"skipped": True, "reason": "disabled", "events": events}
            return {"processed": processed, "events": events, "cursors": {platform: state["platforms"][platform]["cursor"] for platform in PLATFORMS}}

    def pending_events(self) -> list[dict[str, Any]]:
        return list(self._read_state().get("pending_events") or [])

    def mark_events_delivered(self, events: list[dict[str, Any]], *, suppressed: bool = False) -> None:
        if not events:
            return
        with self._lock:
            state = self._read_state()
            pending = list(state.get("pending_events") or [])
            ids = {(item.get("event_id") or f"legacy:{item.get('log_id')}:{item.get('title')}") for item in events}
            state["pending_events"] = [item for item in pending if (item.get("event_id") or f"legacy:{item.get('log_id')}:{item.get('title')}") not in ids]
            self._write_state(state)


def model_guard_event_message(event: dict[str, Any]) -> tuple[str, str]:
    incident = event.get("incident") if isinstance(event.get("incident"), dict) else {}
    confirmed = event.get("status") == "confirmed"
    title = "确认模型降级" if confirmed else "模型不一致待核实"
    if event.get("kind") in {"history_summary", "history_reclassified"}:
        title = "历史模型降级汇总" if confirmed else "历史模型不一致汇总"
    elif event.get("kind") == "action_uncertain":
        title = "模型降级处置结果待核实"
    try:
        event_time = datetime.fromisoformat(str(event.get("created_at") or ""))
        if event_time.tzinfo is None:
            event_time = event_time.replace(tzinfo=timezone.utc)
        display_time = event_time.astimezone(ZoneInfo("Asia/Shanghai")).strftime("%Y-%m-%d %H:%M:%S")
    except ValueError:
        display_time = sanitize_error_text(event.get("created_at"), 80)
    lines = [
        f"账号 ID：{int(event.get('account_id') or 0)}",
        f"名称：{sanitize_error_text(event.get('account_name') or '-', 100)}",
        f"平台/类型：{sanitize_error_text(event.get('platform') or '-', 40)}/{sanitize_error_text(event.get('account_type') or '-', 40)}",
        f"模型链：{sanitize_error_text(event.get('requested_model'), 100)} → {sanitize_error_text(event.get('upstream_model'), 100)} → {sanitize_error_text(event.get('response_model') or '未知', 100)}",
        f"北京时间：{display_time}",
        f"日志 ID：{int(event.get('log_id') or 0)}",
        f"累计：{int(incident.get('count') or 1)} 次",
        f"处置：{sanitize_error_text(incident.get('action') or '未执行', 100)}",
        f"原因：{sanitize_error_text(incident.get('reason') or event.get('reason'), 180)}",
    ]
    if event.get("kind") == "history_summary":
        lines.insert(-2, f"历史汇总：{int(event.get('count') or 0)} 次")
    return title, "\n".join(lines)
