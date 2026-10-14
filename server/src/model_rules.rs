use crate::{
    compat::{integer, py_json, sha256, string, truthy},
    error::{Error, Result},
};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub fn revision(value: &Value) -> String {
    sha256(py_json(value, false, true))
}
pub fn merge(base: &Value, patch: &Value) -> Value {
    let mut base = base
        .as_object()
        .cloned()
        .map(Value::Object)
        .unwrap_or(json!({}));
    for (key, value) in patch.as_object().into_iter().flatten() {
        base[key] = if value.is_object() && base[key].is_object() {
            merge(&base[key], value)
        } else {
            value.clone()
        };
    }
    base
}
pub fn validate_fields(fields: &Value, base: &Value) -> Result<()> {
    let invalid = |message: &str| Error::new(422, message);
    let object = fields
        .as_object()
        .filter(|v| !v.contains_key("id") && !v.contains_key("slug"))
        .ok_or_else(|| invalid("模型身份字段 id / slug 不可修改"))?;
    for (key, value) in object {
        let key = key.as_str();
        if matches!(
            key,
            "display_name"
                | "description"
                | "shell_type"
                | "visibility"
                | "default_reasoning_summary"
                | "web_search_tool_type"
        ) && !value.is_string()
        {
            return Err(invalid(&format!("{key} 必须为字符串")));
        }
        if matches!(
            key,
            "default_reasoning_level"
                | "multi_agent_reasoning_effort"
                | "default_verbosity"
                | "apply_patch_tool_type"
        ) && !value.is_null()
            && !value.is_string()
        {
            return Err(invalid(&format!("{key} 必须为字符串或 null")));
        }
        if matches!(
            key,
            "supported_in_api"
                | "include_skills_usage_instructions"
                | "include_plugin_usage_instructions"
                | "include_apps_usage_instructions"
                | "supports_reasoning_summary_parameter"
                | "support_verbosity"
                | "supports_image_detail_original"
                | "supports_parallel_tool_calls"
                | "supports_search_tool"
                | "use_responses_lite"
                | "node_repl_auto_review_required"
                | "node_repl_disabled"
        ) && !value.is_boolean()
        {
            return Err(invalid(&format!("{key} 必须为布尔值")));
        }
        if matches!(
            key,
            "priority"
                | "context_window"
                | "max_context_window"
                | "effective_context_window_percent"
        ) && value
            .as_i64()
            .is_none_or(|v| v.unsigned_abs() > (1u64 << 53) - 1)
        {
            return Err(invalid(&format!("{key} 必须为安全整数")));
        }
        if matches!(
            key,
            "additional_speed_tiers" | "experimental_supported_tools" | "input_modalities"
        ) && value
            .as_array()
            .is_none_or(|v| v.iter().any(|v| !v.is_string()))
        {
            return Err(invalid(&format!("{key} 必须为字符串数组")));
        }
        if matches!(key, "supported_reasoning_levels" | "service_tiers") {
            let required = if key == "supported_reasoning_levels" {
                "effort"
            } else {
                "name"
            };
            if value.as_array().is_none_or(|v| {
                v.iter().any(|v| {
                    !v.is_object()
                        || !v[required].is_string()
                        || v.get("description").is_some_and(|v| !v.is_string())
                })
            }) {
                return Err(invalid(&format!("{key} 格式无效")));
            }
            if key == "service_tiers"
                && value
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v.get("id").is_some_and(|v| !v.is_string()))
            {
                return Err(invalid("service_tiers.id 必须为字符串"));
            }
        }
        if matches!(key, "model_messages" | "truncation_policy") && !value.is_object() {
            return Err(invalid(&format!("{key} 必须为对象")));
        }
        if key == "model_messages"
            && value
                .get("instructions_template")
                .is_some_and(|v| !v.is_string())
        {
            return Err(invalid("instructions_template 必须为字符串"));
        }
        if key == "truncation_policy"
            && (value.get("mode").is_some_and(|v| !v.is_string())
                || value.get("limit").is_some_and(|v| v.as_i64().is_none()))
        {
            return Err(invalid("truncation_policy 格式无效"));
        }
    }
    let effective = merge(base, fields);
    for key in ["context_window", "max_context_window"] {
        if effective
            .get(key)
            .is_some_and(|v| v.as_i64().is_none_or(|v| v <= 0))
        {
            return Err(invalid("上下文窗口必须大于 0"));
        }
    }
    if effective.get("context_window").is_some()
        && effective.get("max_context_window").is_some()
        && integer(&effective["max_context_window"]) < integer(&effective["context_window"])
    {
        return Err(invalid("最大上下文不能小于上下文窗口"));
    }
    if effective
        .get("effective_context_window_percent")
        .is_some_and(|v| v.as_i64().is_none_or(|v| !(1..=100).contains(&v)))
    {
        return Err(invalid("有效上下文百分比必须为 1–100"));
    }
    if effective.get("input_modalities").is_some_and(|v| {
        v.as_array()
            .is_none_or(|v| v.is_empty() || v.iter().any(|v| v != "text" && v != "image"))
    }) {
        return Err(invalid("输入类型至少包含 text / image 中的一项"));
    }
    if !effective["supported_reasoning_levels"].is_null()
        && truthy(&effective["default_reasoning_level"])
        && !effective["supported_reasoning_levels"]
            .as_array()
            .is_some_and(|v| {
                v.iter()
                    .any(|v| v["effort"] == effective["default_reasoning_level"])
            })
    {
        return Err(invalid("默认推理等级必须属于所选等级"));
    }
    Ok(())
}
pub fn validate_overrides(overrides: &Value, bases: &Value) -> Result<()> {
    let object = overrides
        .as_object()
        .filter(|v| v.len() <= 500 && py_json(overrides, false, true).len() <= 2 * 1024 * 1024)
        .ok_or_else(|| Error::new(422, "最多保存 500 个模型，配置不能超过 2 MiB"))?;
    for (model, fields) in object {
        if model.is_empty()
            || model.trim() != model
            || model.contains('*')
            || model.chars().count() > 256
            || model.chars().any(|v| (v as u32) < 32)
        {
            return Err(Error::new(422, "模型 ID 无效"));
        }
        validate_fields(fields, &bases[model])?;
    }
    Ok(())
}
pub fn matches(pattern: &str, model: &str) -> bool {
    let regex = format!(
        "^(?:{})$",
        pattern
            .split('*')
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join(".*")
    );
    regex::Regex::new(&regex).is_ok_and(|v| v.is_match(model))
}
pub fn admitted(allowlist: &Value, model: &str) -> bool {
    !truthy(&allowlist["enabled"])
        || allowlist["models"].as_array().is_some_and(|v| {
            v.iter()
                .any(|v| v.as_str().is_some_and(|v| matches(v, model)))
        })
}
pub fn mapped(account: &Value, model: &str) -> Option<String> {
    let extra = &account["extra"];
    if account["platform"] == "openai"
        && extra
            .get("openai_passthrough")
            .unwrap_or(&extra["openai_oauth_passthrough"])
            == &Value::Bool(true)
    {
        return Some(model.into());
    }
    let mapping = &account["credentials"]["model_mapping"];
    if !truthy(mapping) {
        return Some(model.into());
    }
    if let Some(target) = mapping.get(model) {
        return Some(
            if truthy(target) {
                string(target)
            } else {
                model
            }
            .into(),
        );
    }
    let mut patterns: Vec<&String> = mapping.as_object()?.keys().collect();
    patterns.sort_by_key(|p| (std::cmp::Reverse(p.replace('*', "").chars().count()), *p));
    for pattern in patterns {
        if pattern.contains('*') && matches(pattern, model) {
            return Some(
                if truthy(&mapping[pattern]) {
                    string(&mapping[pattern])
                } else {
                    model
                }
                .into(),
            );
        }
    }
    None
}
pub fn composite_target(
    model: &str,
    routes: &[Value],
    accounts: &[Value],
) -> Option<(String, String)> {
    let matching: Vec<_> = routes
        .iter()
        .filter(|r| {
            truthy(&r["enabled"])
                && if r["match_type"] == "prefix" {
                    model.starts_with(string(&r["public_model"]))
                } else {
                    model == string(&r["public_model"])
                }
        })
        .collect();
    let mut compatible: Vec<_> = matching
        .iter()
        .copied()
        .filter(|r| {
            r.get("endpoint").is_none()
                || matches!(string(&r["endpoint"]), "any" | "responses" | "")
        })
        .collect();
    compatible.sort_by_key(|r| {
        (
            r["match_type"] == "prefix",
            r.get("endpoint").is_none() || matches!(string(&r["endpoint"]), "" | "any"),
            std::cmp::Reverse(string(&r["public_model"]).chars().count()),
            integer(&r["priority"]).unwrap_or(0),
            integer(&r["id"]).unwrap_or(0),
        )
    });
    if let Some(chosen) = compatible.first() {
        return Some((
            string(&chosen["target_platform"]).into(),
            if truthy(&chosen["upstream_model"]) {
                string(&chosen["upstream_model"])
            } else {
                model
            }
            .into(),
        ));
    }
    if !matching.is_empty() {
        return None;
    }
    let owners: BTreeSet<&str> = accounts
        .iter()
        .filter(|a| a["credentials"]["model_mapping"].get(model).is_some())
        .map(|a| string(&a["platform"]))
        .collect();
    if owners.len() == 1 {
        return Some((owners.into_iter().next().unwrap().into(), model.into()));
    }
    if !owners.is_empty() {
        return None;
    }
    for (prefix, platform) in [
        ("gpt-", "openai"),
        ("o1", "openai"),
        ("o3", "openai"),
        ("o4", "openai"),
        ("codex", "openai"),
        ("grok", "grok"),
        ("claude", "anthropic"),
        ("gemini", "gemini"),
        ("deepseek", "deepseek"),
    ] {
        if model.to_lowercase().starts_with(prefix) {
            return Some((platform.into(), model.into()));
        }
    }
    None
}
pub fn targets(
    group: &Value,
    model: &str,
    accounts: &[Value],
    routes: &[Value],
) -> Vec<(Value, String)> {
    let mut eligible: Vec<_> = accounts
        .iter()
        .filter(|a| a["status"] == "active" && truthy(&a["schedulable"]))
        .cloned()
        .collect();
    let selected = &group["codex_models_manifest_config"];
    if truthy(&selected["enabled"]) {
        let mut pinned: Vec<_> = selected["account_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|id| eligible.iter().find(|a| a["id"] == *id).cloned())
            .collect();
        if truthy(&selected["fallback_to_scheduler"]) {
            let remaining: Vec<_> = eligible
                .into_iter()
                .filter(|a| !pinned.contains(a))
                .collect();
            pinned.extend(remaining);
        }
        eligible = pinned;
    }
    let target = if group["platform"] == "composite" {
        composite_target(model, routes, &eligible)
    } else {
        Some((string(&group["platform"]).into(), model.into()))
    };
    let Some((platform, upstream)) = target else {
        return vec![];
    };
    eligible
        .into_iter()
        .filter(|a| a["platform"] == platform)
        .filter_map(|a| mapped(&a, &upstream).map(|m| (a, m)))
        .collect()
}
pub fn transform(
    body: &Value,
    overrides: &Value,
    allowlist: &Value,
    aliases: &Value,
) -> Result<Value> {
    let mut models = body["models"]
        .as_array()
        .filter(|v| v.iter().all(|m| m.is_object() && m["slug"].is_string()))
        .cloned()
        .ok_or_else(|| Error::new(422, "原始 Codex 目录格式无效"))?;
    let present: BTreeSet<String> = models.iter().map(|v| string(&v["slug"]).into()).collect();
    for (model, descriptor) in aliases.as_object().into_iter().flatten() {
        if !present.contains(model) && admitted(allowlist, model) {
            let mut value = descriptor.clone();
            value["slug"] = model.clone().into();
            models.push(value);
        }
    }
    let mut entries = vec![];
    for model in models {
        if !admitted(allowlist, string(&model["slug"])) {
            continue;
        }
        let patch = &overrides[string(&model["slug"])];
        if truthy(patch) {
            validate_fields(patch, &model)?;
        }
        entries.push(merge(&model, patch));
    }
    let mut output = body.clone();
    output["models"] = entries.into();
    Ok(output)
}
