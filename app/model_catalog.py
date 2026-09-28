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
from .model_rules import admitted, encoded, merge, revision, targets, transform, validate_overrides

CLIENT_VERSION = "0.146.0"
MAX_BODY = 8 * 1024 * 1024
GROUP_FIELDS = "id,name,platform,model_allowlist,codex_models_manifest_config,updated_at"


def caller_key(headers: dict) -> str:
    auth = headers.get("authorization", "")
    if auth.lower().startswith("bearer ") and auth[7:].strip():
        return auth[7:].strip()
    return headers.get("x-api-key") or headers.get("x-goog-api-key") or ""


def allowlist(value: object) -> dict:
    if not isinstance(value, dict) or type(value.get("enabled")) is not bool or not isinstance(value.get("models"), list):
        raise ValueError("白名单格式无效")
    models = value["models"]
    if len(models) > 1000 or any(not isinstance(m, str) or not m.strip() or len(m) > 256 or any(ord(c) < 32 for c in m) for m in models):
        raise ValueError("白名单模型 ID 无效")
    return {"enabled": value["enabled"], "models": list(dict.fromkeys(m.strip() for m in models))}


def import_fields(entry: dict) -> dict:
    if "slug" in entry:
        return {k: copy.deepcopy(v) for k, v in entry.items() if k not in {"id", "slug"}}
    output = {}
    for source, dest in (("name", "display_name"), ("display_name", "display_name"), ("description", "description"),
                         ("context_window", "context_window"), ("max_context_window", "max_context_window"),
                         ("supported_reasoning_levels", "supported_reasoning_levels"), ("default_reasoning_level", "default_reasoning_level"),
                         ("input_modalities", "input_modalities")):
        if source in entry:
            output[dest] = entry[source]
    levels = output.get("supported_reasoning_levels")
    if isinstance(levels, list):
        output["supported_reasoning_levels"] = [{"effort": value, "description": ""} if isinstance(value, str) else value for value in levels]
    context = (entry.get("limit") or {}).get("context")
    if type(context) is int and context > 0:
        output["context_window"] = context
        output["max_context_window"] = context
    modalities = (entry.get("modalities") or {}).get("input")
    if isinstance(modalities, list):
        supported = [m for m in modalities if m in ("text", "image")]
        if supported:
            output["input_modalities"] = supported
    if "context_window" in output and "max_context_window" not in output:
        output["max_context_window"] = output["context_window"]
    if entry.get("reasoning") is False:
        output["supported_reasoning_levels"] = [{"effort": "none", "description": ""}]
        output["default_reasoning_level"] = "none"
    elif isinstance(entry.get("reasoning_options"), list):
        levels = []
        for option in entry["reasoning_options"]:
            if isinstance(option, dict) and option.get("type", "").lower() == "effort":
                levels.extend("none" if value is None else value for value in option.get("values", []) if value is None or isinstance(value, str))
        levels = list(dict.fromkeys(levels))
        if levels:
            output["supported_reasoning_levels"] = [{"effort": value, "description": ""} for value in levels]
            output["default_reasoning_level"] = levels[0]
    # Unspecified capability values do not become false on import.
    for key in ("supports_parallel_tool_calls", "supports_reasoning_summary_parameter", "support_verbosity", "supports_search_tool"):
        if key in entry:
            output[key] = entry[key]
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
        self.catalog_cache: tuple[float, list] = (0, [])
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

    def aliases(self, body: dict, group: dict, accounts: list, routes: list, whitelist: dict, *, draft: bool = False) -> dict:
        if group["platform"] != "composite" and not draft:
            return {}
        descriptors = {m["slug"]: m for m in body.get("models", []) if isinstance(m, dict) and isinstance(m.get("slug"), str)}
        candidates = set(whitelist.get("models", []))
        if group["platform"] == "composite":
            candidates.update(r["public_model"] for r in routes if r.get("match_type") == "exact")
            for a in accounts:
                candidates.update(((a.get("credentials") or {}).get("model_mapping") or {}).keys())
        output = {}
        fetched = {}
        deadline = time.monotonic() + 8
        for model in sorted(candidates):
            if "*" in model or model in descriptors or not admitted(whitelist, model):
                continue
            available = targets(group, model, accounts, routes)
            # The descriptor must come from the real source model. Never clone
            # an unrelated template to advertise an unverified capability.
            for account, upstream in available:
                if upstream in descriptors:
                    output[model] = {**descriptors[upstream], "slug": model}
                    break
                try:
                    identity = account["id"]
                    if identity not in fetched:
                        if time.monotonic() >= deadline:
                            break
                        fetched[identity] = None
                        fetched[identity] = self.source_catalog(account, timeout=min(3, deadline - time.monotonic()))
                    catalog = fetched[identity]
                    if not isinstance(catalog, dict):
                        continue
                    # Supplement only a complete, real Codex descriptor. An
                    # ordinary id-only list cannot prove client capabilities.
                    descriptor = next((entry for entry in catalog.get("models", []) if isinstance(entry, dict) and entry.get("slug") == upstream), None)
                    if descriptor:
                        validate_overrides({model: import_fields(descriptor)})
                        output[model] = {**descriptor, "slug": model}
                        break
                except Exception:
                    continue
        return output

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
            accounts = self.accounts(group["id"]) if group["platform"] == "composite" else []
            routes = self.routes(group)
            cache_key = (group["id"], group["version"], saved["revision"], version, revision(original), revision([accounts, routes]))
            with self.lock:
                cached = self.cache.get(cache_key)
            if cached and cached[0] > time.monotonic():
                body, etag = cached[1:]
            else:
                aliases = self.aliases(original, group, accounts, routes, group["model_allowlist"])
                final = transform(original, saved["overrides"], group["model_allowlist"], aliases)
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

    def read_group(self, group_id: int, key: str) -> dict:
        group = self.group(group_id)
        saved = self.store.group(group_id)
        body, source = self.baseline(group)
        candidates = self.admin(key, "GET", f"/groups/{group_id}/model-allowlist-candidates") or []
        if isinstance(candidates, dict):
            candidates = candidates.get("models", [])
        candidates = [v if isinstance(v, str) else v.get("id") for v in candidates]
        candidates = sorted({v for v in candidates if isinstance(v, str)})
        accounts, routes = self.accounts(group_id), self.routes(group)
        aliases = self.aliases(body, group, accounts, routes, group["model_allowlist"])
        baseline = transform(body, {}, group["model_allowlist"], aliases)
        final = transform(body, saved["overrides"], group["model_allowlist"], aliases)
        return {"group": {k: group[k] for k in ("id", "name", "platform", "version", "model_allowlist")},
                **saved, "candidates": candidates, "baseline": baseline, "effective": final,
                "baseline_status": source, "status": self.status}

    def preview(self, group_id: int, payload: dict) -> dict:
        group = self.group(group_id)
        body, source = self.baseline(group)
        draft = allowlist(payload["allowlist"])
        overrides = payload["overrides"]
        accounts, routes = self.accounts(group_id), self.routes(group)
        aliases = self.aliases(body, group, accounts, routes, draft, draft=True)
        bases = {m["slug"]: m for m in body.get("models", [])} | aliases
        validate_overrides(overrides, bases)
        final = transform(body, overrides, draft, aliases)
        missing = [m for m in draft["models"] if "*" not in m and m not in {v["slug"] for v in final["models"]}]
        return {"effective": final, "baseline_status": source, "pending_models": missing}

    def save_overrides(self, group_id: int, payload: dict) -> dict:
        with self.lock:
            lock = self.group_locks.setdefault(group_id, threading.Lock())
        with lock:
            group = self.group(group_id)
            if group["version"] != payload["expected_version"]:
                raise HTTPException(409, "分组已变更，请刷新后重试")
            saved = self.store.group(group_id)
            body, source = self.baseline(group)
            accounts, routes = self.accounts(group_id), self.routes(group)
            aliases = self.aliases(body, group, accounts, routes, group["model_allowlist"])
            bases = {m["slug"]: m for m in transform(body, {}, group["model_allowlist"], aliases)["models"]}
            validate_overrides(payload["overrides"], bases)
            for model, fields in payload["overrides"].items():
                if fields == saved["overrides"].get(model):
                    continue
                if not admitted(group["model_allowlist"], model) or model not in bases:
                    raise HTTPException(409, "模型尚未进入当前目录，请先保存白名单并刷新目录")
            if self.group(group_id)["version"] != group["version"]:
                raise HTTPException(409, "分组已变更，请刷新后重试")
            return self.store.save(group_id, payload["overrides"], payload["expected_revision"])

    def save_allowlist(self, group_id: int, key: str, payload: dict) -> dict:
        draft = allowlist(payload["allowlist"])
        with self.lock:
            lock = self.group_locks.setdefault(group_id, threading.Lock())
        with lock:
            group = self.group(group_id)
            if group["version"] != payload["expected_version"]:
                raise HTTPException(409, "分组已变更，请刷新后重试")
            self.admin(key, "PUT", f"/groups/{group_id}", {"model_allowlist": draft})
            actual = self.group(group_id)
            with self.lock:
                self.baselines = {k: v for k, v in self.baselines.items() if k[0] != group_id}
                self.cache.clear()
            if actual["model_allowlist"] != draft:
                raise HTTPException(409, "白名单保存结果与草稿不同，请刷新核对")
            return {"version": actual["version"], "model_allowlist": actual["model_allowlist"]}

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

    def upstream_import(self, group_id: int, model: str) -> dict:
        group = self.group(group_id)
        if not admitted(group["model_allowlist"], model):
            raise HTTPException(409, "请先保存该模型的白名单")
        accounts, routes = self.accounts(group_id), self.routes(group)
        deadline = time.monotonic() + 45
        for account, upstream in targets(group, model, accounts, routes):
            try:
                if time.monotonic() >= deadline:
                    break
                body = self.source_catalog(account, timeout=min(20, deadline - time.monotonic()))
                entries = body if isinstance(body, list) else body.get("models", body.get("data", []))
                for entry in entries:
                    if isinstance(entry, dict) and entry.get("slug", entry.get("id", entry.get("modelId"))) == upstream:
                        fields = import_fields(entry)
                        validate_overrides({model: fields})
                        if fields:
                            return {"model": model, "fields": fields, "source": "upstream", "account_id": account["id"], "upstream_model": upstream}
            except Exception:
                continue
        raise HTTPException(502, "可路由上游没有返回该模型的有效元数据，配置未修改")

    def catalog(self) -> dict:
        with self.lock:
            if self.catalog_cache[0] > time.monotonic():
                return {"items": self.catalog_cache[1]}
        try:
            status, _, raw = self.read_http("GET", "https://models.dev/api.json")
            data = json.loads(raw)
            if status != 200 or not isinstance(data, dict):
                raise ValueError()
            items = []
            for provider, record in data.items():
                for model, entry in record.get("models", {}).items():
                    fields = import_fields(entry)
                    try:
                        validate_overrides({model: fields})
                    except ValueError:
                        continue
                    items.append({"provider": provider, "provider_name": record.get("name", provider), "id": model,
                                  "name": entry.get("name", model), "fields": fields})
            items.sort(key=lambda i: (i["provider"], i["id"]))
            with self.lock:
                self.catalog_cache = (time.monotonic() + 21600, items)
            return {"items": items}
        except Exception:
            raise HTTPException(502, "models.dev 目录暂不可用，现有草稿已保留") from None
