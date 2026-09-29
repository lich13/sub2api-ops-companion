"""Read-only catalog compatibility layer; native Sub2API remains the auth authority."""
from __future__ import annotations

import copy
import json
import re
import threading
import time
from datetime import datetime, timezone
from urllib.parse import quote, urlsplit

import httpx
from fastapi import HTTPException

from .model_config_store import ModelConfigStore
from .model_rules import admitted, encoded, revision, transform, validate_overrides
from .model_reasoning import (FIELDS, complete_descriptor, forwarding, model_id, patch_descriptor,
                              reasoning_fields, reasoning_values, routing_binding, same_reasoning)

CLIENT_VERSION = "0.146.0"
MAX_BODY = 8 * 1024 * 1024
GROUP_FIELDS = "id,name,platform,model_allowlist,codex_models_manifest_config,max_reasoning_effort,max_reasoning_effort_over_limit,reasoning_effort_mappings,updated_at"


def caller_key(headers: dict) -> str:
    auth = headers.get("authorization", "")
    if auth.lower().startswith("bearer ") and auth[7:].strip():
        return auth[7:].strip()
    return headers.get("x-api-key") or headers.get("x-goog-api-key") or ""


def import_fields(entry: dict) -> dict:
    """Read only explicitly supplied reasoning information."""
    output = {k: copy.deepcopy(entry[k]) for k in FIELDS if k in entry}
    levels = output.get("supported_reasoning_levels")
    if isinstance(levels, list):
        output["supported_reasoning_levels"] = [{"effort": value, "description": ""} if isinstance(value, str) else value for value in levels]
    if entry.get("reasoning") is False and "supported_reasoning_levels" not in output:
        output["supported_reasoning_levels"] = [{"effort": "none", "description": ""}]
        output["default_reasoning_level"] = "none"
    elif "supported_reasoning_levels" not in output and isinstance(entry.get("reasoning_options"), list):
        values = []
        for option in entry["reasoning_options"]:
            if isinstance(option, dict) and option.get("type", "").lower() == "effort":
                values.extend(value for value in option.get("values", []) if isinstance(value, str))
        if values:
            output["supported_reasoning_levels"] = [{"effort": v, "description": ""} for v in dict.fromkeys(values)]
    return output


class ModelCatalogService:
    def __init__(self, runtime):
        self.r = runtime
        self.store = ModelConfigStore(runtime.settings.model_config_path)
        self.client = httpx.Client(timeout=12, follow_redirects=False, trust_env=False)
        self.lock = threading.RLock()
        self.group_locks: dict[int, threading.Lock] = {}
        self.baselines: dict[tuple[int, str, str], tuple[float, dict]] = {}
        self.cache: dict[tuple, tuple[float, bytes, str]] = {}
        self.version_cache: tuple[float, str] = (0, "")
        self.sources: dict[str, tuple[float, dict]] = {}
        self.status = {"state": "ready", "message": "", "updated_at": None}

    def close(self):
        self.client.close()

    def mark(self, message: str = ""):
        with self.lock:
            self.status = {"state": "fallback" if message else "ready", "message": message,
                           "updated_at": datetime.now(timezone.utc).isoformat()}

    def read_http(self, method: str, url: str, *, headers=None, payload=None, client=None) -> tuple[int, dict, bytes]:
        with (client or self.client).stream(method, url, headers=headers, json=payload) as response:
            content = bytearray()
            for part in response.iter_bytes():
                if len(content) + len(part) > MAX_BODY:
                    raise ValueError("目录响应过大")
                content.extend(part)
            return response.status_code, dict(response.headers), bytes(content)

    def admin(self, key: str, method: str, path: str, payload=None):
        try:
            status, _, raw = self.read_http(method, self.r.oauth_base_url() + "/api/v1/admin" + path,
                                           headers={"x-api-key": key}, payload=payload)
            body = json.loads(raw)
            if status >= 400 or body.get("code") != 0:
                raise HTTPException(401 if status in (401, 403) else 502, "Sub2API 分组操作未确认，请刷新核对")
            return body.get("data")
        except HTTPException:
            raise
        except Exception:
            raise HTTPException(502, "Sub2API 分组操作未确认，未自动重放") from None

    def group(self, group_id: int) -> dict:
        if group_id < 1:
            raise HTTPException(422, "分组 ID 无效")
        row = self.r.db.fetch_one(f"SELECT {GROUP_FIELDS} FROM groups WHERE id=%(id)s AND deleted_at IS NULL", {"id": group_id})
        if not row:
            raise HTTPException(404, "分组不存在")
        row["model_allowlist"] = row.get("model_allowlist") or {"enabled": False, "models": []}
        row["version"] = revision({"id": row["id"], "platform": row["platform"], "updated_at": str(row["updated_at"]),
                                    "model_allowlist": row["model_allowlist"], "manifest": row.get("codex_models_manifest_config")})
        return row

    def accounts(self, group_id: int) -> list:
        return self.r.db.fetch_all("""SELECT a.id,a.platform,a.type,a.status,a.schedulable,a.credentials,a.extra,a.proxy_id
          FROM accounts a JOIN account_groups ag ON ag.account_id=a.id
          WHERE ag.group_id=%(id)s AND a.deleted_at IS NULL ORDER BY a.priority,a.id""", {"id": group_id})

    def routes(self, group: dict) -> list:
        if group["platform"] != "composite":
            return []
        return self.r.db.fetch_all("SELECT id,public_model,match_type,target_platform,upstream_model,endpoint,priority,enabled FROM composite_model_routes WHERE group_id=%(id)s AND enabled=true", {"id": group["id"]})

    def proxy(self, path: str, query: str, headers: dict) -> tuple[int, dict, bytes]:
        forwarded = {k: v for k, v in headers.items() if k.lower() in {
            "authorization", "x-api-key", "x-goog-api-key", "user-agent", "originator", "version", "x-client-version",
            "x-forwarded-for", "x-real-ip", "x-forwarded-proto", "accept", "openai-beta", "chatgpt-account-id"}}
        url = self.r.oauth_base_url() + path + ("?" + query if query else "")
        try:
            status, upstream_headers, raw = self.read_http("GET", url, headers=forwarded)
        except Exception:
            self.mark("原始模型目录暂不可用")
            return 503, {"content-type": "application/json"}, b'{"error":"catalog_unavailable"}'
        outgoing = {k: v for k, v in upstream_headers.items() if k in {"content-type", "etag", "cache-control", "retry-after", "vary"}}
        if status != 200:
            return status, outgoing, raw  # Original authentication and restrictions are final.
        try:
            identity = self.r.db.fetch_one("SELECT group_id FROM api_keys WHERE key=%(key)s AND deleted_at IS NULL", {"key": caller_key(headers)})
            if not identity or not identity.get("group_id"):
                return status, outgoing, raw
            group = self.group(identity["group_id"])
            saved = self.store.group(group["id"])
            original = json.loads(raw)
            version = query
            with self.lock:
                self.baselines[(group["id"], group["version"], version)] = (time.monotonic(), copy.deepcopy(original))
                if len(self.baselines) > 128:
                    self.baselines.pop(next(iter(self.baselines)))
            accounts = self.accounts(group["id"]) if group["platform"] == "composite" or saved.get("reasoning") else []
            routes = self.routes(group)
            native_version = self.native_version() if saved.get("reasoning") else ""
            cache_key = (group["id"], group["version"], saved["revision"], version, native_version, revision(original), revision([accounts, routes]))
            with self.lock:
                cached = self.cache.get(cache_key)
            if cached and cached[0] > time.monotonic():
                body, etag = cached[1:]
            else:
                overrides, supplements = self.active_reasoning(original, group, saved, accounts, routes, native_version)
                final = transform(original, overrides, group["model_allowlist"], supplements)
                body = encoded(final)
                etag = '"' + revision(final) + '"'
                with self.lock:
                    self.cache = {k: v for k, v in self.cache.items() if v[0] > time.monotonic()}
                    if len(self.cache) >= 128:
                        self.cache.clear()
                    self.cache[cache_key] = (time.monotonic() + 15, body, etag)
            self.mark()
            outgoing.update({"etag": etag, "cache-control": "private, no-cache", "x-sub2ops-catalog": "active"})
            requested = [v.strip().removeprefix("W/") for v in headers.get("if-none-match", "").split(",")]
            return (304, outgoing, b"") if etag in requested or "*" in requested else (200, outgoing, body)
        except Exception:
            self.mark("兼容层暂不可用，已返回 Sub2API 原始目录")
            outgoing["x-sub2ops-catalog"] = "fallback"
            return status, outgoing, raw

    def baseline(self, group: dict) -> tuple[dict, str]:
        with self.lock:
            available = [(stamp, body) for (gid, version, _), (stamp, body) in self.baselines.items() if gid == group["id"] and version == group["version"] and time.monotonic() - stamp < 60]
        if available:
            return copy.deepcopy(max(available, key=lambda p: p[0])[1]), "native"
        # Admin preview uses an existing group identity only for this read-only
        # native catalog request. It is never substituted on the public proxy.
        keys = self.r.db.fetch_all("SELECT key FROM api_keys WHERE group_id=%(id)s AND deleted_at IS NULL AND status='active' ORDER BY id LIMIT 3", {"id": group["id"]})
        for row in keys:
            try:
                status, _, raw = self.read_http("GET", self.r.oauth_base_url() + "/v1/models?client_version=" + CLIENT_VERSION,
                                               headers={"Authorization": "Bearer " + row["key"]})
                body = json.loads(raw)
                if status == 200 and isinstance(body.get("models"), list):
                    with self.lock:
                        self.baselines[(group["id"], group["version"], "admin-preview")] = (time.monotonic(), copy.deepcopy(body))
                    return body, "native"
            except Exception:
                continue
        return {"models": []}, "unavailable"

    def list_groups(self) -> dict:
        rows = self.r.db.fetch_all("SELECT id,name,platform FROM groups WHERE deleted_at IS NULL ORDER BY id")
        return {"groups": rows, "status": self.status}

    def native_version(self, key: str = "") -> str:
        with self.lock:
            if self.version_cache[0] > time.monotonic():
                return self.version_cache[1]
        try:
            if not key:
                row = self.r.db.fetch_one("SELECT value FROM settings WHERE key='admin_api_key'") or {}
                key = row.get("value", "")
            value = self.admin(key, "GET", "/system/version") if key else {}
            version = str(value.get("version", "")).removeprefix("v")
            if not re.fullmatch(r"\d+\.\d+\.\d+", version):
                version = ""
        except Exception:
            version = ""
        with self.lock:
            self.version_cache = (time.monotonic() + (30 if version else 5), version)
        return version

    def active_reasoning(self, body: dict, group: dict, saved: dict, accounts: list, routes: list, version: str) -> tuple[dict, dict]:
        overrides = {model: {k: copy.deepcopy(v) for k, v in fields.items() if k not in FIELDS}
                     for model, fields in saved["overrides"].items()}
        supplements = {}
        native = {m["slug"]: m for m in body["models"]}
        for model, entry in saved.get("reasoning", {}).items():
            if entry.get("mode") != "active" or not admitted(group["model_allowlist"], model):
                continue
            binding, available = routing_binding(group, model, accounts, routes)
            if binding != entry.get("binding") or forwarding(group, model, entry["efforts"], available, version)["state"] != "verified":
                continue
            fields = reasoning_fields(entry["efforts"], entry["default_effort"])
            if same_reasoning(native.get(model), fields):
                continue
            base = native.get(model) or entry.get("descriptor")
            if not complete_descriptor(base, model):
                continue
            if model not in native:
                supplements[model] = base
            overrides[model] = {**overrides.get(model, {}), **patch_descriptor(base, fields)}
        return overrides, supplements

    @staticmethod
    def group_summary(group: dict) -> dict:
        return {key: group[key] for key in ("id", "name", "platform", "version")}

    def reasoning_item(self, model: str, entry: dict, group: dict, native: dict, accounts: list, routes: list, version: str) -> dict:
        binding, available = routing_binding(group, model, accounts, routes)
        checked = forwarding(group, model, entry["efforts"], available, version)
        fields = reasoning_fields(entry["efforts"], entry["default_effort"])
        state, reason = checked["state"], checked["reason"]
        if not admitted(group["model_allowlist"], model):
            state, reason = "unverified", "分组白名单尚未允许该模型"
        elif entry.get("binding") != binding:
            state, reason = "unverified", "路由或分组策略已变化，请重新核对并保存"
        elif state == "verified" and same_reasoning(native.get(model), fields):
            state, reason = "native", "原生目录已提供相同档位，可恢复原生"
        elif state == "verified" and not complete_descriptor(native.get(model) or entry.get("descriptor"), model):
            state, reason = "unverified", "缺少该模型自身的完整目录描述"
        elif state == "verified" and entry.get("mode") == "active":
            state = "active"
        elif state == "verified":
            state, reason = "draft", "草稿已具备生效条件，请核对后保存"
        return {"model": model, "efforts": entry["efforts"], "default_effort": entry["default_effort"],
                "state": state, "reason": reason, "source": entry.get("source", "manual"),
                "updated_at": entry.get("updated_at"), "version": version}

    def read_reasoning(self, group_id: int, key: str) -> dict:
        group, saved = self.group(group_id), self.store.group(group_id)
        entries = copy.deepcopy(saved.get("reasoning", {}))
        for model, fields in saved["overrides"].items():
            efforts, default = reasoning_values(fields)
            if model not in entries and efforts and default:
                entries[model] = {"efforts": efforts, "default_effort": default, "mode": "draft", "source": "legacy"}
        items = []
        if entries:
            body, _ = self.baseline(group)
            native = {m["slug"]: m for m in body["models"]}
            accounts, routes, version = self.accounts(group_id), self.routes(group), self.native_version(key)
            items = [self.reasoning_item(model, entry, group, native, accounts, routes, version) for model, entry in sorted(entries.items())]
        return {"group": self.group_summary(group), "revision": saved["revision"], "items": items, "status": self.status}

    def resolve_reasoning(self, group_id: int, key: str, payload: dict) -> dict:
        model = model_id(payload["model"])
        group, saved = self.group(group_id), self.store.group(group_id)
        body, _ = self.baseline(group)
        native = next((m for m in body["models"] if m["slug"] == model), None)
        accounts, routes = self.accounts(group_id), self.routes(group)
        binding, available = routing_binding(group, model, accounts, routes)
        descriptor, imported, source = native, {}, "manual"
        destinations = {(a["platform"], target) for a, target in available}
        if len(destinations) == 1:
            deadline = time.monotonic() + 8
            for account, target in available[:3]:
                try:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        break
                    data = self.source_catalog(account, timeout=min(3, remaining))
                    entries = data if isinstance(data, list) else data.get("models", data.get("data", []))
                    found = next((m for m in entries if isinstance(m, dict) and m.get("slug", m.get("id", m.get("modelId"))) == target), None)
                    if not found:
                        continue
                    imported = {k: v for k, v in import_fields(found).items() if k in FIELDS}
                    if complete_descriptor(found, target):
                        # Rename only the public identity of this actual target.
                        actual = {**found, "slug": model}
                        validate_overrides({model: {k: v for k, v in actual.items() if k not in {"id", "slug"}}})
                        descriptor = native or actual
                    if reasoning_values(imported)[0]:
                        source = "upstream"
                        break
                except Exception:
                    continue
        existing = saved.get("reasoning", {}).get(model)
        efforts, default = reasoning_values(imported)
        if not efforts:
            if existing:
                efforts, default, source = existing["efforts"], existing["default_effort"], existing.get("source", "manual")
            elif native:
                efforts, default = reasoning_values(native)
                source = "native"
        if payload.get("efforts") is not None:
            fields = reasoning_fields(payload["efforts"], payload.get("default_effort", ""))
            efforts, default = reasoning_values(fields)
            if not same_reasoning(imported, fields):
                source = "manual"
        checked = forwarding(group, model, efforts, available, self.native_version(key))
        public = {"group": self.group_summary(group), "revision": saved["revision"], "model": model,
                  "binding": binding, "efforts": efforts, "default_effort": default, "source": source,
                  "needs_allowlist": not admitted(group["model_allowlist"], model),
                  "native_efforts": reasoning_values(native)[0], "native_default": reasoning_values(native)[1],
                  "descriptor_available": complete_descriptor(descriptor, model), "forwarding": checked}
        # Private working material never crosses the desktop DTO boundary.
        return {**public, "_descriptor": descriptor}

    def save_reasoning(self, group_id: int, key: str, payload: dict) -> dict:
        model = model_id(payload["model"])
        fields = reasoning_fields(payload["efforts"], payload["default_effort"])
        with self.lock:
            lock = self.group_locks.setdefault(group_id, threading.Lock())
        with lock:
            group, saved = self.group(group_id), self.store.group(group_id)
            if group["version"] != payload["expected_version"] or saved["revision"] != payload["expected_revision"]:
                raise HTTPException(409, "配置已变更，请刷新后重新核对，草稿已保留")
            resolved = self.resolve_reasoning(group_id, key, payload)
            if resolved["binding"] != payload["expected_binding"]:
                raise HTTPException(409, "账号路由已变化，请重新核对，草稿已保留")
            native_fields = reasoning_fields(resolved["native_efforts"], resolved["native_default"]) if resolved["native_default"] else {}
            if same_reasoning(native_fields, fields) and not resolved["needs_allowlist"] and resolved["forwarding"]["state"] == "verified":
                if model in saved.get("reasoning", {}) or FIELDS.intersection(saved["overrides"].get(model, {})):
                    self.store.save_reasoning(group_id, model, None, saved["revision"])
                return {"outcome": "native", "message": "原生已支持，无需补全", "state": self.read_reasoning(group_id, key)}
            active = resolved["forwarding"]["state"] == "verified" and resolved["descriptor_available"]
            if active and resolved["needs_allowlist"] and not payload.get("confirm_allowlist"):
                raise HTTPException(409, "请确认仅将该模型追加到分组白名单")
            if self.group(group_id)["version"] != group["version"]:
                raise HTTPException(409, "分组已变更，请刷新后重新核对")
            appended = False
            if active and resolved["needs_allowlist"]:
                whitelist = {**group["model_allowlist"], "models": [*group["model_allowlist"]["models"], model]}
                self.admin(key, "PUT", f"/groups/{group_id}", {"model_allowlist": whitelist})
                group = self.group(group_id)
                if group["model_allowlist"] != whitelist:
                    raise HTTPException(409, "白名单结果未确认；补全未保存，请刷新核对，不要自动重试")
                appended = True
            try:
                # Recheck after the native write and before the local commit.
                binding, _ = routing_binding(group, model, self.accounts(group_id), self.routes(group))
                if binding != resolved["binding"] or self.group(group_id)["version"] != group["version"]:
                    raise HTTPException(409, "路由或分组已变更")
                entry = {"efforts": resolved["efforts"], "default_effort": resolved["default_effort"],
                         "binding": binding, "source": resolved["source"], "mode": "active" if active else "draft",
                         "descriptor": resolved["_descriptor"], "updated_at": datetime.now(timezone.utc).isoformat()}
                self.store.save_reasoning(group_id, model, entry, saved["revision"])
            except Exception:
                if not appended:
                    raise
                return {"outcome": "partial", "message": "白名单已添加，补全未保存；请重新核对后保存，未自动重试",
                        "state": self.read_reasoning(group_id, key)}
            with self.lock:
                self.cache.clear()
                self.baselines = {k: v for k, v in self.baselines.items() if k[0] != group_id}
            return {"outcome": "saved" if active else "draft", "message": "目录已补全" if active else "已保存草稿，未对外发布",
                    "state": self.read_reasoning(group_id, key)}

    def remove_reasoning(self, group_id: int, key: str, payload: dict) -> dict:
        model = model_id(payload["model"])
        with self.lock:
            lock = self.group_locks.setdefault(group_id, threading.Lock())
        with lock:
            if self.group(group_id)["version"] != payload["expected_version"]:
                raise HTTPException(409, "分组已变更，请刷新后重试")
            self.store.save_reasoning(group_id, model, None, payload["expected_revision"])
            with self.lock:
                self.cache.clear()
            return self.read_reasoning(group_id, key)


    def source_catalog(self, account: dict, *, timeout: float = 20) -> dict:
        identity = revision(account)
        with self.lock:
            cached = self.sources.get(identity)
        if cached and cached[0] > time.monotonic():
            return copy.deepcopy(cached[1])
        credentials = account.get("credentials") or {}
        platform, kind = account["platform"], account["type"]
        if platform not in {"openai", "grok", "anthropic", "gemini", "kimi", "zhipu", "deepseek", "minimax", "opencodego"}:
            raise ValueError("该平台没有只读目录协议")
        base = str(credentials.get("base_url") or "").rstrip("/")
        headers = {"Accept": "application/json"}
        token = credentials.get("access_token") if kind == "oauth" else credentials.get("api_key")
        if not token:
            raise ValueError("当前凭据不可用")
        if platform == "openai" and kind == "oauth":
            # Sub2API's OAuth directory uses the official Codex endpoint.
            base = "https://chatgpt.com/backend-api/codex"
            url = base + "/models?client_version=" + CLIENT_VERSION
            headers.update({"Authorization": "Bearer " + token, "Originator": "codex_cli_rs",
                            "Version": CLIENT_VERSION, "User-Agent": "codex_cli_rs/" + CLIENT_VERSION + " (Ubuntu 22.4.0; x86_64) xterm-256color"})
            if credentials.get("chatgpt_account_id"):
                headers["ChatGPT-Account-ID"] = credentials["chatgpt_account_id"]
        elif platform == "gemini":
            if kind != "apikey":
                raise ValueError("该凭据没有只读目录接口")
            base = base or "https://generativelanguage.googleapis.com/v1beta"
            url = base if base.endswith("/models") else base + ("/models" if base.endswith("/v1beta") else "/v1beta/models")
            headers["x-goog-api-key"] = token
        else:
            if platform == "grok" and kind == "oauth" and not base:
                setting = self.r.db.fetch_one("SELECT value FROM settings WHERE key='grok_default_base_url_mode'") or {}
                base = {"api": "https://api.x.ai/v1", "us-east-1": "https://us-east-1.api.x.ai/v1",
                        "us-west-2": "https://us-west-2.api.x.ai/v1", "eu-west-1": "https://eu-west-1.api.x.ai/v1"}.get(setting.get("value"), "https://cli-chat-proxy.grok.com/v1")
            base = base or {"openai": "https://api.openai.com/v1", "grok": "https://api.x.ai/v1", "anthropic": "https://api.anthropic.com/v1"}.get(platform, "")
            if not base:
                raise ValueError("未配置上游目录地址")
            # Mirror native buildOpenAIEndpointURL's explicit # suffix.
            explicit = base.endswith("#")
            if explicit:
                base = base[:-1].rstrip("/")
            url = base if base.endswith("/models") else base + ("/models" if explicit or base.endswith("/v1") else "/v1/models")
            if platform == "openai":
                url += "?client_version=" + CLIENT_VERSION
            headers["Authorization"] = "Bearer " + token
            if platform == "anthropic":
                headers["anthropic-version"] = "2023-06-01"
                if kind == "oauth":
                    headers["anthropic-beta"] = "oauth-2025-04-20"
                if kind == "apikey":
                    headers.pop("Authorization")
                    headers["x-api-key"] = token
            if platform == "grok" and kind == "oauth" and urlsplit(url).hostname == "cli-chat-proxy.grok.com":
                headers.update({"X-XAI-Token-Auth": "xai-grok-cli", "x-grok-client-version": "0.2.120",
                                "x-grok-client-identifier": "grok-shell", "User-Agent": "xai-grok-workspace/0.2.120"})
                for field, header in (("sub", "X-UserID"), ("email", "X-Email")):
                    if credentials.get(field):
                        headers[header] = credentials[field]
        parsed = urlsplit(url)
        if parsed.scheme not in {"https", "http"} or parsed.username or parsed.password or not parsed.hostname:
            raise ValueError("上游目录地址无效")
        proxy = None
        if account.get("proxy_id"):
            p = self.r.db.fetch_one("SELECT protocol,host,port,username,password FROM proxies WHERE id=%(id)s AND deleted_at IS NULL", {"id": account["proxy_id"]})
            if not p:
                raise ValueError("账号代理不可用")
            auth = (quote(p["username"], safe="") + ":" + quote(p.get("password") or "", safe="") + "@") if p.get("username") else ""
            proxy = f'{p["protocol"]}://{auth}{p["host"]}:{p["port"]}'
        with httpx.Client(timeout=max(0.1, timeout), follow_redirects=False, trust_env=False, proxy=proxy) as client:
            status, _, raw = self.read_http("GET", url, headers=headers, client=client)
        if status != 200:
            raise ValueError("上游目录暂不可用")
        data = json.loads(raw)
        # Credentials stay server-side even if a broken upstream echoes them.
        secrets = [str(v) for k, v in credentials.items() if k in {"api_key", "access_token", "refresh_token", "id_token"} and isinstance(v, str) and len(v) >= 8]
        def scrub(value):
            if isinstance(value, str):
                for secret in secrets:
                    value = value.replace(secret, "[redacted]")
                return value
            if isinstance(value, list):
                return [scrub(v) for v in value]
            if isinstance(value, dict):
                return {k: scrub(v) for k, v in value.items()}
            return value
        data = scrub(data)
        with self.lock:
            self.sources = {k: v for k, v in self.sources.items() if v[0] > time.monotonic()}
            if len(self.sources) >= 64:
                self.sources.clear()
            self.sources[identity] = (time.monotonic() + 60, data)
        return copy.deepcopy(data)
