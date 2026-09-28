"""Codex model metadata contract: Sub2API PR 7663 @ 89c39792.

Only catalog descriptions are transformed; request routing is never modified.
"""
from __future__ import annotations

import copy
import hashlib
import json
import re
from typing import Any


def encoded(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def revision(value: Any) -> str:
    return hashlib.sha256(encoded(value)).hexdigest()


def merge(base: dict, patch: dict) -> dict:
    result = copy.deepcopy(base)
    for key, value in patch.items():
        result[key] = merge(result[key], value) if isinstance(value, dict) and isinstance(result.get(key), dict) else copy.deepcopy(value)
    return result


STRINGS = {"display_name", "description", "shell_type", "visibility", "default_reasoning_summary", "web_search_tool_type"}
NULLABLE_STRINGS = {"default_reasoning_level", "multi_agent_reasoning_effort", "default_verbosity", "apply_patch_tool_type"}
BOOLEANS = {"supported_in_api", "include_skills_usage_instructions", "include_plugin_usage_instructions", "include_apps_usage_instructions",
            "supports_reasoning_summary_parameter", "support_verbosity", "supports_image_detail_original", "supports_parallel_tool_calls",
            "supports_search_tool", "use_responses_lite", "node_repl_auto_review_required", "node_repl_disabled"}
INTEGERS = {"priority", "context_window", "max_context_window", "effective_context_window_percent"}
STRING_LISTS = {"additional_speed_tiers", "experimental_supported_tools", "input_modalities"}


def validate_fields(fields: dict, base: dict | None = None) -> None:
    if not isinstance(fields, dict) or {"id", "slug"} & fields.keys():
        raise ValueError("模型身份字段 id / slug 不可修改")
    for key, value in fields.items():
        if key in STRINGS and not isinstance(value, str):
            raise ValueError(f"{key} 必须为字符串")
        if key in NULLABLE_STRINGS and value is not None and not isinstance(value, str):
            raise ValueError(f"{key} 必须为字符串或 null")
        if key in BOOLEANS and type(value) is not bool:
            raise ValueError(f"{key} 必须为布尔值")
        if key in INTEGERS and (type(value) is not int or abs(value) > 2**53 - 1):
            raise ValueError(f"{key} 必须为安全整数")
        if key in STRING_LISTS and (not isinstance(value, list) or any(not isinstance(v, str) for v in value)):
            raise ValueError(f"{key} 必须为字符串数组")
        if key in {"supported_reasoning_levels", "service_tiers"}:
            required = "effort" if key == "supported_reasoning_levels" else "name"
            if not isinstance(value, list) or any(not isinstance(v, dict) or not isinstance(v.get(required), str) or not isinstance(v.get("description", ""), str) for v in value):
                raise ValueError(f"{key} 格式无效")
            if key == "service_tiers" and any("id" in v and not isinstance(v["id"], str) for v in value):
                raise ValueError("service_tiers.id 必须为字符串")
        if key in {"model_messages", "truncation_policy"} and not isinstance(value, dict):
            raise ValueError(f"{key} 必须为对象")
        if key == "model_messages" and "instructions_template" in value and not isinstance(value["instructions_template"], str):
            raise ValueError("instructions_template 必须为字符串")
        if key == "truncation_policy":
            if "mode" in value and not isinstance(value["mode"], str) or "limit" in value and type(value["limit"]) is not int:
                raise ValueError("truncation_policy 格式无效")
    effective = merge(base or {}, fields)
    for key in ("context_window", "max_context_window"):
        if key in effective and (type(effective[key]) is not int or effective[key] <= 0):
            raise ValueError("上下文窗口必须大于 0")
    if "context_window" in effective and "max_context_window" in effective and effective["max_context_window"] < effective["context_window"]:
        raise ValueError("最大上下文不能小于上下文窗口")
    if "effective_context_window_percent" in effective and not 1 <= effective["effective_context_window_percent"] <= 100:
        raise ValueError("有效上下文百分比必须为 1–100")
    if "input_modalities" in effective and (not effective["input_modalities"] or any(v not in ("text", "image") for v in effective["input_modalities"])):
        raise ValueError("输入类型至少包含 text / image 中的一项")
    levels = effective.get("supported_reasoning_levels")
    default = effective.get("default_reasoning_level")
    if levels is not None and default and default not in {v["effort"] for v in levels}:
        raise ValueError("默认推理等级必须属于所选等级")


def validate_overrides(overrides: dict, bases: dict | None = None) -> None:
    if not isinstance(overrides, dict) or len(overrides) > 500 or len(encoded(overrides)) > 2 * 1024 * 1024:
        raise ValueError("最多保存 500 个模型，配置不能超过 2 MiB")
    for model, fields in overrides.items():
        if not model or model != model.strip() or "*" in model or len(model) > 256 or any(ord(c) < 32 for c in model):
            raise ValueError("模型 ID 无效")
        validate_fields(fields, (bases or {}).get(model))


def matches(pattern: str, model: str) -> bool:
    # Native model mappings support only *, not shell character classes or ?.
    return bool(re.fullmatch(".*".join(re.escape(p) for p in pattern.split("*")), model))


def admitted(allowlist: dict, model: str) -> bool:
    return not allowlist.get("enabled") or any(matches(p, model) for p in allowlist.get("models", []))


def mapped(account: dict, model: str) -> str | None:
    credentials = account.get("credentials") or {}
    extra = account.get("extra") or {}
    if account.get("platform") == "openai" and extra.get("openai_passthrough", extra.get("openai_oauth_passthrough")) is True:
        return model
    mapping = credentials.get("model_mapping") or {}
    if not mapping:
        return model
    if model in mapping:
        return mapping[model] or model
    for pattern in sorted(mapping, key=lambda p: (-len(p.replace("*", "")), p)):
        if "*" in pattern and matches(pattern, model):
            return mapping[pattern] or model
    return None


def composite_target(model: str, routes: list[dict], accounts: list[dict]) -> tuple[str, str] | None:
    matching = [r for r in routes if r.get("enabled") and
                (model.startswith(r["public_model"]) if r.get("match_type") == "prefix" else model == r["public_model"])]
    compatible = [r for r in matching if r.get("endpoint", "any") in ("any", "responses", "")]
    if compatible:
        # Same precedence as native matchCompositeRoute: exact before prefix,
        # longest prefix before priority; endpoint-specific before any.
        chosen = sorted(compatible, key=lambda r: (r.get("match_type") == "prefix", r.get("endpoint", "any") in ("", "any"),
                                                    -len(r["public_model"]), r.get("priority", 0), r.get("id", 0)))[0]
        return chosen["target_platform"], chosen.get("upstream_model") or model
    if matching:
        return None
    owners = {a["platform"] for a in accounts if model in ((a.get("credentials") or {}).get("model_mapping") or {})}
    if len(owners) == 1:
        return owners.pop(), model
    if owners:
        return None
    for prefix, platform in (("gpt-", "openai"), ("o1", "openai"), ("o3", "openai"), ("o4", "openai"), ("codex", "openai"), ("grok", "grok"), ("claude", "anthropic"), ("gemini", "gemini"), ("deepseek", "deepseek")):
        if model.lower().startswith(prefix):
            return platform, model
    return None


def targets(group: dict, model: str, accounts: list[dict], routes: list[dict]) -> list[tuple[dict, str]]:
    eligible = [a for a in accounts if a.get("status") == "active" and a.get("schedulable")]
    selected = group.get("codex_models_manifest_config") or {}
    if selected.get("enabled") and selected.get("account_ids"):
        eligible = [a for a in eligible if a["id"] in selected["account_ids"]]
    target = composite_target(model, routes, eligible) if group["platform"] == "composite" else (group["platform"], model)
    if not target:
        return []
    platform, upstream = target
    return [(a, m) for a in eligible if a["platform"] == platform and (m := mapped(a, upstream)) is not None]


def transform(body: dict, overrides: dict, allowlist: dict, aliases: dict | None = None) -> dict:
    if not isinstance(body.get("models"), list) or any(not isinstance(m, dict) or not isinstance(m.get("slug"), str) for m in body["models"]):
        raise ValueError("原始 Codex 目录格式无效")
    output = copy.deepcopy(body)
    present = {m["slug"] for m in output["models"]}
    for model, descriptor in (aliases or {}).items():
        if model not in present and admitted(allowlist, model):
            output["models"].append({**copy.deepcopy(descriptor), "slug": model})
    output["models"] = [merge(m, overrides.get(m["slug"], {})) for m in output["models"] if admitted(allowlist, m["slug"])]
    return output
