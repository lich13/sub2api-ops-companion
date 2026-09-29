"""Reasoning supplements, not model routing or request rewriting.

Forwarding contracts: native Sub2API 0.2.9 (4c00df2) and 0.2.10 (2f3fed2).
Unknown releases deliberately require verification instead of inheriting a guess.
"""
from __future__ import annotations

import copy
import re

from .model_rules import revision, targets

EFFORTS = ("none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra")
FIELDS = {"supported_reasoning_levels", "default_reasoning_level"}
VERIFIED_VERSIONS = {"0.2.9", "0.2.10"}
GROK_EFFORT_MODELS = {"grok-4.5", "grok-4.5-latest", "grok-4.6", "grok-4.6-latest",
                     "grok-4.7", "grok-4.7-latest", "grok-4.3", "grok-4.3-latest",
                     "grok-3-mini", "grok-3-mini-fast", "grok-4.20-0309-reasoning",
                     "grok-4.20-reasoning", "grok-4.20-multi-agent-0309"}
GROK_XHIGH_MODELS = {"grok-4.6", "grok-4.6-latest", "grok-4.7", "grok-4.7-latest"}


def model_id(value: str) -> str:
    if not isinstance(value, str) or not value or value != value.strip() or len(value) > 256 or any(c.isspace() or ord(c) < 32 for c in value) or "*" in value:
        raise ValueError("请输入精确模型 ID，不支持通配符")
    return value


def reasoning_fields(efforts: list[str], default: str) -> dict:
    if not isinstance(efforts, list) or not efforts or any(not isinstance(e, str) or e not in EFFORTS for e in efforts) or len(efforts) != len(set(efforts)):
        raise ValueError("请选择有效的思考档位")
    if default not in efforts:
        raise ValueError("默认档位必须属于所选档位")
    return {"supported_reasoning_levels": [{"effort": e, "description": ""} for e in EFFORTS if e in efforts],
            "default_reasoning_level": default}


def reasoning_values(descriptor: dict | None) -> tuple[list[str], str]:
    descriptor = descriptor or {}
    raw = descriptor.get("supported_reasoning_levels") or []
    levels = [v if isinstance(v, str) else v.get("effort") for v in raw if isinstance(v, (str, dict))]
    levels = list(dict.fromkeys(v for v in levels if v in EFFORTS))
    default = descriptor.get("default_reasoning_level")
    return levels, default if default in levels else ""


def same_reasoning(left: dict | None, right: dict) -> bool:
    a, ad = reasoning_values(left)
    b, bd = reasoning_values(right)
    return bool(a and ad) and set(a) == set(b) and ad == bd


def complete_descriptor(value: object, target: str) -> bool:
    # An id-only /models response does not establish a Codex descriptor. Never
    # clone an unrelated model's context window, tools or instructions.
    return (isinstance(value, dict) and value.get("slug") == target
            and isinstance(value.get("display_name"), str)
            and type(value.get("context_window")) is int and value["context_window"] > 0
            and isinstance(value.get("input_modalities"), list) and bool(value["input_modalities"])
            and isinstance(value.get("model_messages"), dict)
            and isinstance(value["model_messages"].get("instructions_template"), str))


def routing_binding(group: dict, model: str, accounts: list, routes: list) -> tuple[str, list]:
    # Pinned catalog readers are not the set of accounts that serve requests.
    # Include paused accounts so later enabling one cannot silently retarget a
    # supplement. Tokens, names and priority are unrelated to this binding.
    candidates = [{**a, "status": "active", "schedulable": True} for a in accounts]
    available = targets({**group, "codex_models_manifest_config": {}}, model, candidates, routes)
    facts = []
    for account, upstream in available:
        extra = account.get("extra") or {}
        credentials = account.get("credentials") or {}
        facts.append({"id": account["id"], "platform": account["platform"], "type": account.get("type"), "model": upstream,
                      "passthrough": extra.get("openai_passthrough", extra.get("openai_oauth_passthrough")) is True,
                      "base_url": credentials.get("base_url", ""),
                      "responses": extra.get("openai_responses_supported"),
                      "endpoint_mode": extra.get("openai_responses_mode")})
    policy = {k: group.get(k) for k in ("max_reasoning_effort", "max_reasoning_effort_over_limit", "reasoning_effort_mappings")}
    return revision({"platform": group["platform"], "model": model, "targets": sorted(facts, key=lambda v: v["id"]), "policy": policy}), available


def forwarding(group: dict, model: str, efforts: list[str], available: list, version: str) -> dict:
    def result(state, reason):
        return {"state": state, "reason": reason, "version": version}
    if not available:
        return result("unverified", "没有可解析的账号路由")
    destinations = {(a["platform"], target) for a, target in available}
    if len(destinations) != 1:
        return result("unverified", "账号映射指向不同模型，请先统一目标")
    platform, target = next(iter(destinations))
    if platform == "openai":
        ceiling = group.get("max_reasoning_effort") or ""
        if ceiling and ceiling not in EFFORTS:
            return result("unverified", "分组思考上限无法核实")
        if ceiling and any(EFFORTS.index(e) > EFFORTS.index(ceiling) for e in efforts):
            return result("limited", "分组思考上限会拒绝或降低所选档位")
        for effort in efforts:
            candidates = []
            for index, mapping in enumerate(group.get("reasoning_effort_mappings") or []):
                selector = str(mapping.get("model") or "").strip().lower()
                kind = str(mapping.get("match_type") or "exact").strip().lower()
                request = model.lower()
                applies = (not selector or (kind == "exact" and request == selector)
                           or (kind == "prefix" and request.startswith(selector))
                           or (kind == "suffix" and request.endswith(selector)))
                if applies and mapping.get("from") == effort:
                    strength = 1 if not selector else 3 if kind == "exact" else 2
                    candidates.append((strength, len(selector) if strength == 2 else 0, -index, mapping.get("to")))
            if candidates and max(candidates)[3] != effort:
                return result("limited", "分组映射会改写或拒绝所选思考档位")
    if version.removeprefix("v") not in VERIFIED_VERSIONS:
        return result("unverified", "当前 Sub2API 版本尚无已核对的转发契约")
    if any(a.get("type") not in {"oauth", "apikey"} for a, _ in available):
        return result("unverified", "当前账号类型的转发尚未核实")
    if platform == "grok":
        target = re.sub(r"^(?:xai|x-ai|grok)/", "", target.lower())
        if target not in GROK_EFFORT_MODELS:
            return result("limited", "原版 Sub2API 会移除该 Grok 模型的思考强度")
        accepted = {"none", "low", "medium", "high"}
        if target in GROK_XHIGH_MODELS:
            accepted.add("xhigh")
        if not set(efforts) <= accepted:
            return result("limited", "原版 Sub2API 会改写所选 Grok 思考档位")
    elif platform != "openai":
        return result("unverified", "当前平台的转发尚未核实")
    else:
        for account, _ in available:
            extra = account.get("extra") or {}
            mode = extra.get("openai_responses_mode")
            if account.get("type") == "apikey" and (mode == "force_chat_completions" or (mode != "force_responses" and extra.get("openai_responses_supported") is False)):
                return result("unverified", "部分账号会转为 Chat Completions，档位转发尚未核实")
    return result("verified", "当前版本可保留所选 Responses 思考档位")


def patch_descriptor(descriptor: dict, fields: dict) -> dict:
    result = copy.deepcopy(fields)
    # Keep original descriptions for unchanged effort choices.
    descriptions = {v.get("effort"): v.get("description", "") for v in descriptor.get("supported_reasoning_levels", []) if isinstance(v, dict)}
    for level in result["supported_reasoning_levels"]:
        level["description"] = descriptions.get(level["effort"], "")
    return result
