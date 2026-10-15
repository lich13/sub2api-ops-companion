use crate::{
    compat::{integer, pick, string, truthy},
    error::{Error, Result},
    model_rules::{revision, targets},
};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];
pub const FIELDS: &[&str] = &["supported_reasoning_levels", "default_reasoning_level"];
pub const VERIFIED_SUB2API_VERSIONS: &[&str] = &["0.2.9", "0.2.10"];
pub fn model_id(value: &Value) -> Result<&str> {
    let value = value.as_str().filter(|v| {
        !v.is_empty()
            && v.trim() == *v
            && v.chars().count() <= 256
            && !v.chars().any(|v| v.is_whitespace() || (v as u32) < 32)
            && !v.contains('*')
    });
    value.ok_or_else(|| Error::new(422, "请输入精确模型 ID，不支持通配符"))
}
pub fn reasoning_fields(efforts: &Value, default: &Value) -> Result<Value> {
    let efforts = efforts
        .as_array()
        .filter(|v| {
            !v.is_empty()
                && v.iter()
                    .all(|e| e.as_str().is_some_and(|e| EFFORTS.contains(&e)))
                && v.iter().map(string).collect::<BTreeSet<_>>().len() == v.len()
        })
        .ok_or_else(|| Error::new(422, "请选择有效的思考档位"))?;
    if !efforts.contains(default) {
        return Err(Error::new(422, "默认档位必须属于所选档位"));
    }
    Ok(
        json!({"supported_reasoning_levels":EFFORTS.iter().filter(|e|efforts.contains(&json!(e))).map(|e|json!({"effort":e,"description":""})).collect::<Vec<_>>(),"default_reasoning_level":default}),
    )
}
pub fn reasoning_values(descriptor: &Value) -> (Vec<String>, String) {
    let mut levels = vec![];
    for value in descriptor["supported_reasoning_levels"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let level = if value.is_string() {
            string(value)
        } else {
            string(&value["effort"])
        };
        if EFFORTS.contains(&level) && !levels.iter().any(|v| v == level) {
            levels.push(level.to_owned());
        }
    }
    let default = string(&descriptor["default_reasoning_level"]);
    let default = if levels.iter().any(|v| v == default) {
        default.into()
    } else {
        String::new()
    };
    (levels, default)
}
pub fn same_reasoning(a: &Value, b: &Value) -> bool {
    let (a, ad) = reasoning_values(a);
    let (b, bd) = reasoning_values(b);
    !a.is_empty()
        && !ad.is_empty()
        && ad == bd
        && a.into_iter().collect::<BTreeSet<_>>() == b.into_iter().collect()
}
pub fn complete_descriptor(value: &Value, target: &str) -> bool {
    value.is_object()
        && value["slug"] == target
        && value["display_name"].is_string()
        && value["context_window"].as_i64().is_some_and(|v| v > 0)
        && value["input_modalities"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
        && value["model_messages"].is_object()
        && value["model_messages"]["instructions_template"].is_string()
}
pub fn routing_binding(
    group: &Value,
    model: &str,
    accounts: &[Value],
    routes: &[Value],
) -> (String, Vec<(Value, String)>) {
    let candidates: Vec<Value> = accounts
        .iter()
        .map(|v| {
            let mut v = v.clone();
            v["status"] = "active".into();
            v["schedulable"] = true.into();
            v
        })
        .collect();
    let mut group = group.clone();
    group["codex_models_manifest_config"] = json!({});
    let available = targets(&group, model, &candidates, routes);
    let mut facts: Vec<Value> = available.iter().map(|(account,target)| {
        let extra = &account["extra"]; let credentials = &account["credentials"];
        json!({"id":account["id"],"platform":account["platform"],"type":account["type"],"model":target,
            "passthrough":extra.get("openai_passthrough").unwrap_or(&extra["openai_oauth_passthrough"]) == &Value::Bool(true),
            "base_url":credentials.get("base_url").cloned().unwrap_or(json!("")),"responses":extra["openai_responses_supported"],"endpoint_mode":extra["openai_responses_mode"]})
    }).collect();
    facts.sort_by_key(|v| integer(&v["id"]));
    (
        revision(
            &json!({"platform":group["platform"],"model":model,"targets":facts,"policy":pick(&group,&["max_reasoning_effort","max_reasoning_effort_over_limit","reasoning_effort_mappings"])}),
        ),
        available,
    )
}

pub fn forwarding(
    group: &Value,
    model: &str,
    efforts: &[String],
    available: &[(Value, String)],
    version: &str,
) -> Value {
    let result =
        |state: &str, reason: &str| json!({"state":state,"reason":reason,"version":version});
    if available.is_empty() {
        return result("unverified", "没有可解析的账号路由");
    }
    let destinations: BTreeSet<(&str, &str)> = available
        .iter()
        .map(|(a, m)| (string(&a["platform"]), m.as_str()))
        .collect();
    if destinations.len() != 1 {
        return result("unverified", "账号映射指向不同模型，请先统一目标");
    }
    let (platform, target) = destinations.into_iter().next().unwrap();
    if platform == "openai" {
        let ceiling = string(&group["max_reasoning_effort"]);
        if !ceiling.is_empty() && !EFFORTS.contains(&ceiling) {
            return result("unverified", "分组思考上限无法核实");
        }
        if !ceiling.is_empty()
            && efforts.iter().any(|e| {
                EFFORTS.iter().position(|v| *v == e).unwrap_or(usize::MAX)
                    > EFFORTS.iter().position(|v| *v == ceiling).unwrap()
            })
        {
            return result("limited", "分组思考上限会拒绝或降低所选档位");
        }
        for effort in efforts {
            let mut candidates = vec![];
            for (index, mapping) in group["reasoning_effort_mappings"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                let selector = string(&mapping["model"]).trim().to_lowercase();
                let kind = if truthy(&mapping["match_type"]) {
                    string(&mapping["match_type"])
                } else {
                    "exact"
                }
                .trim()
                .to_lowercase();
                let request = model.to_lowercase();
                let applies = selector.is_empty()
                    || kind == "exact" && request == selector
                    || kind == "prefix" && request.starts_with(&selector)
                    || kind == "suffix" && request.ends_with(&selector);
                if applies && mapping["from"] == *effort {
                    let strength = if selector.is_empty() {
                        1
                    } else if kind == "exact" {
                        3
                    } else {
                        2
                    };
                    candidates.push((
                        strength,
                        if strength == 2 {
                            selector.chars().count()
                        } else {
                            0
                        },
                        std::cmp::Reverse(index),
                        &mapping["to"],
                    ));
                }
            }
            if candidates
                .iter()
                .max_by_key(|v| (v.0, v.1, v.2))
                .is_some_and(|v| v.3 != effort)
            {
                return result("limited", "分组映射会改写或拒绝所选思考档位");
            }
        }
    }
    if !VERIFIED_SUB2API_VERSIONS.contains(&version.strip_prefix('v').unwrap_or(version)) {
        return result("unverified", "当前 Sub2API 版本尚无已核对的转发契约");
    }
    if available
        .iter()
        .any(|(a, _)| !matches!(string(&a["type"]), "oauth" | "apikey"))
    {
        return result("unverified", "当前账号类型的转发尚未核实");
    }
    if platform == "grok" {
        let lower = target.to_lowercase();
        let target = ["xai/", "x-ai/", "grok/"]
            .into_iter()
            .find_map(|p| lower.strip_prefix(p))
            .unwrap_or(&lower);
        let supported = [
            "grok-4.5",
            "grok-4.5-latest",
            "grok-4.6",
            "grok-4.6-latest",
            "grok-4.7",
            "grok-4.7-latest",
            "grok-4.3",
            "grok-4.3-latest",
            "grok-3-mini",
            "grok-3-mini-fast",
            "grok-4.20-0309-reasoning",
            "grok-4.20-reasoning",
            "grok-4.20-multi-agent-0309",
        ];
        if !supported.contains(&target) {
            return result("limited", "原版 Sub2API 会移除该 Grok 模型的思考强度");
        }
        let xhigh =
            ["grok-4.6", "grok-4.6-latest", "grok-4.7", "grok-4.7-latest"].contains(&target);
        if efforts.iter().any(|e| {
            !(matches!(e.as_str(), "none" | "low" | "medium" | "high") || xhigh && e == "xhigh")
        }) {
            return result("limited", "原版 Sub2API 会改写所选 Grok 思考档位");
        }
    } else if platform != "openai" {
        return result("unverified", "当前平台的转发尚未核实");
    } else {
        for (account, _) in available {
            let extra = &account["extra"];
            let mode = &extra["openai_responses_mode"];
            if account["type"] == "apikey"
                && (mode == "force_chat_completions"
                    || mode != "force_responses" && extra["openai_responses_supported"] == false)
            {
                return result(
                    "unverified",
                    "部分账号会转为 Chat Completions，档位转发尚未核实",
                );
            }
        }
    }
    result("verified", "当前版本可保留所选 Responses 思考档位")
}
pub fn patch_descriptor(descriptor: &Value, fields: &Value) -> Value {
    let mut fields = fields.clone();
    if let Some(levels) = fields["supported_reasoning_levels"].as_array_mut() {
        for level in levels {
            level["description"] = descriptor["supported_reasoning_levels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|v| v.is_object())
                .find(|v| v["effort"] == level["effort"])
                .and_then(|v| v.get("description"))
                .cloned()
                .unwrap_or(json!(""));
        }
    }
    fields
}
