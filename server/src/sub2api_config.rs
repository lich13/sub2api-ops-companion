//! Version-bound configuration editing. Deployment values never enter the catalogue.
mod constraints;
pub mod host;
pub mod service;

use crate::{
    compat::{sha256, string},
    error::{Error, Result},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::LazyLock};

pub static CATALOG: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../resources/sub2api-config.json"))
        .expect("reviewed configuration catalogue")
});
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

pub fn field(path: &str) -> Option<&'static Value> {
    CATALOG["fields"]
        .as_array()?
        .iter()
        .find(|f| f["path"] == path)
}
pub fn get<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |v, key| v.get(key))
}
fn set(value: &mut Value, path: &str, new: Option<Value>) -> Result<()> {
    let (key, tail) = path.split_once('.').unwrap_or((path, ""));
    let map = value
        .as_object_mut()
        .ok_or_else(|| Error::new(422, format!("{path} 的父节点不是映射")))?;
    if tail.is_empty() {
        if let Some(new) = new {
            map.insert(key.into(), new);
        } else {
            map.remove(key);
        }
    } else if new.is_some() || map.contains_key(key) {
        let child = map.entry(key).or_insert_with(|| json!({}));
        set(child, tail, new)?;
        if child.as_object().is_some_and(|o| o.is_empty()) {
            map.remove(key);
        }
    }
    Ok(())
}
pub fn parse_yaml(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(Error::new(422, "配置文件超过 1 MiB 上限"));
    }
    let value: yaml_serde::Value = yaml_serde::from_slice(bytes).map_err(|e| {
        let location = e
            .location()
            .map(|l| format!("（第 {} 行，第 {} 列）", l.line(), l.column()))
            .unwrap_or_default();
        Error::new(422, format!("YAML 语法或结构无效{location}"))
    })?;
    let value: Value = serde_json::to_value(value)
        .map_err(|_| Error::new(422, "YAML 需要字符串键名和普通配置值"))?;
    if !value.is_object() {
        return Err(Error::new(422, "配置根节点必须是映射"));
    }
    Ok(value)
}
pub fn validate_request(payload: &Value) -> Result<()> {
    crate::validation::fields(
        payload,
        &["expected_revision", "runtime_revision", "changes"],
        &["expected_revision", "runtime_revision", "changes"],
    )?;
    crate::validation::hex(&payload["expected_revision"], 64)?;
    crate::validation::hex(&payload["runtime_revision"], 64)?;
    let changes = payload["changes"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= CATALOG["fields"].as_array().unwrap().len())
        .ok_or_else(Error::invalid)?;
    let mut paths = std::collections::BTreeSet::new();
    for change in changes {
        crate::validation::fields(change, &["path", "remove", "value"], &["path", "remove"])?;
        let path = string(&change["path"]);
        if !paths.insert(path)
            || field(path).is_none_or(|f| f["editable"] != true)
            || !change["remove"].is_boolean()
            || (change["remove"] != true && change.get("value").is_none())
        {
            return Err(Error::invalid());
        }
    }
    Ok(())
}

pub fn override_value(f: &Value, env: &BTreeMap<String, String>) -> Option<(String, Value)> {
    for key in f["env"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let Some(raw) = env.get(key) else { continue };
        // Viper ignores empty AutomaticEnv values. These two loaders explicitly
        // use LookupEnv, so even an empty string overrides YAML with an empty list.
        let explicit_list = matches!(
            key,
            "SERVER_TRUSTED_PROXIES" | "SECURITY_FORWARDED_CLIENT_IP_HEADERS"
        );
        if raw.is_empty() && !explicit_list || key == "TZ" && raw.trim().is_empty() {
            continue;
        }
        let value = if explicit_list {
            json!(raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>())
        } else {
            match string(&f["type"]) {
                "integer" => raw
                    .parse::<i64>()
                    .map(Value::from)
                    .unwrap_or_else(|_| json!(raw)),
                "number" => raw
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .unwrap_or_else(|| json!(raw)),
                "boolean" => match raw.to_lowercase().as_str() {
                    "true" | "t" | "1" => json!(true),
                    "false" | "f" | "0" => json!(false),
                    _ => json!(raw),
                },
                "array" if f["items_type"] == "string" => json!(raw.split(',').collect::<Vec<_>>()),
                _ => json!(if key == "TZ" { raw.trim() } else { raw }),
            }
        };
        return Some((key.into(), value));
    }
    None
}
pub fn environment_override(
    f: &Value,
    config: &Value,
    env: &BTreeMap<String, String>,
) -> Option<(String, Value)> {
    if let Some(value) = override_value(f, env) {
        return Some(value);
    }
    let path = string(&f["path"]);
    let key = path.strip_prefix("wechat_connect.")?;
    let explicit = |key: &str| {
        get(config, &format!("wechat_connect.{key}")).is_some()
            || env.contains_key(&format!("WECHAT_CONNECT_{}", key.to_uppercase()))
    };
    if explicit(key) {
        return None;
    }
    let legacy = |mode: &str, credential: &str| -> Option<(String, String)> {
        if explicit(&format!("{mode}_{credential}")) || explicit(credential) {
            return None;
        }
        let name = format!(
            "WECHAT_OAUTH_{}_{}",
            mode.to_uppercase(),
            credential.to_uppercase()
        );
        let value = env.get(&name)?.trim();
        (!value.is_empty()).then(|| (name, value.to_owned()))
    };
    for mode in ["open", "mp"] {
        for credential in ["app_id", "app_secret"] {
            if key == format!("{mode}_{credential}") {
                return legacy(mode, credential).map(|(name, value)| (name, json!(value)));
            }
        }
    }
    if key == "frontend_redirect_url" {
        let name = "WECHAT_OAUTH_FRONTEND_REDIRECT_URL";
        return env
            .get(name)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|v| (name.into(), json!(v)));
    }
    let enabled = |mode: &str| {
        legacy(mode, "app_id")
            .zip(legacy(mode, "app_secret"))
            .map(|(id, secret)| format!("{} + {}", id.0, secret.0))
    };
    let open = enabled("open");
    let mp = enabled("mp");
    match key {
        "open_enabled" => open.map(|name| (name, json!(true))),
        "mp_enabled" => mp.map(|name| (name, json!(true))),
        "enabled" => open.or(mp).map(|name| (name, json!(true))),
        "mode" | "scopes" => open
            .map(|name| (name, "open"))
            .or_else(|| mp.map(|name| (name, "mp")))
            .map(|(name, mode)| {
                (
                    name,
                    json!(if key == "mode" {
                        mode
                    } else if mode == "open" {
                        "snsapi_login"
                    } else {
                        "snsapi_userinfo"
                    }),
                )
            }),
        _ => None,
    }
}
pub fn effective(config: &Value, env: &BTreeMap<String, String>) -> Value {
    let mut values = json!({});
    for f in CATALOG["fields"].as_array().expect("fields") {
        if f["internal"] == true {
            continue;
        }
        let path = string(&f["path"]);
        let value = environment_override(f, config, env)
            .map(|(_, v)| v)
            .or_else(|| get(config, path).cloned())
            .or_else(|| (f["has_default"] == true).then(|| f["default_value"].clone()));
        if let Some(v) = value {
            let _ = set(&mut values, path, Some(v));
        }
    }
    values
}
fn numeric(v: &Value, duration: bool) -> Option<f64> {
    if duration {
        return v.as_str().and_then(duration_seconds);
    }
    v.as_f64()
}
pub fn duration_seconds(s: &str) -> Option<f64> {
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(\d+(?:\.\d*)?|\.\d+)(ns|us|µs|μs|ms|s|m|h)").unwrap()
    });
    let (sign, s) = s
        .strip_prefix('-')
        .map(|s| (-1.0, s))
        .unwrap_or((1.0, s.strip_prefix('+').unwrap_or(s)));
    if s == "0" {
        return Some(0.0);
    }
    let mut end = 0;
    let mut total = 0.0;
    for cap in RE.captures_iter(s) {
        let m = cap.get(0)?;
        if m.start() != end {
            return None;
        }
        end = m.end();
        total += cap[1].parse::<f64>().ok()?
            * match &cap[2] {
                "h" => 3600.0,
                "m" => 60.0,
                "s" => 1.0,
                "ms" => 0.001,
                "us" | "µs" | "μs" => 0.000001,
                _ => 0.000000001,
            };
    }
    (end == s.len() && end > 0 && total.is_finite() && total <= i64::MAX as f64 / 1e9)
        .then_some(total * sign)
}
fn type_valid(f: &Value, v: &Value) -> bool {
    if v.is_null() && f["nullable"] == true {
        return true;
    }
    match string(&f["type"]) {
        "string" => v.is_string(),
        "duration" => v.as_str().and_then(duration_seconds).is_some(),
        "boolean" => v.is_boolean(),
        "integer" => {
            if f["go_type"] == "uint32" {
                v.as_u64().is_some_and(|n| n <= u32::MAX as u64)
            } else {
                v.is_i64()
            }
        }
        "number" => v.as_f64().is_some_and(f64::is_finite),
        "object" => v.is_object(),
        "array" => v.as_array().is_some_and(|a| {
            a.iter().all(|v| match string(&f["items_type"]) {
                "string" => v.is_string(),
                "integer" => {
                    if f["go_type"] == "[]uint16" {
                        v.as_u64().is_some_and(|n| n <= u16::MAX as u64)
                    } else {
                        v.is_i64()
                    }
                }
                _ => v.is_object(),
            })
        }),
        _ => false,
    }
}
pub fn violations(config: &Value, env: &BTreeMap<String, String>) -> Vec<Value> {
    let values = effective(config, env);
    let mut errors = vec![];
    for f in CATALOG["fields"].as_array().expect("fields") {
        if f["internal"] == true {
            fn entries<'a>(value: &'a Value, parts: &[&str]) -> Vec<&'a Value> {
                if parts.is_empty() {
                    return vec![value];
                }
                if parts[0] == "*" {
                    return match value {
                        Value::Object(map) => {
                            map.values().flat_map(|v| entries(v, &parts[1..])).collect()
                        }
                        Value::Array(array) => {
                            array.iter().flat_map(|v| entries(v, &parts[1..])).collect()
                        }
                        _ => vec![],
                    };
                }
                value
                    .get(parts[0])
                    .map(|v| entries(v, &parts[1..]))
                    .unwrap_or_default()
            }
            static WILDCARD: LazyLock<regex::Regex> =
                LazyLock::new(|| regex::Regex::new(r"<[^>]+>").unwrap());
            let path = string(&f["path"]);
            let pattern = WILDCARD.replace_all(path, "*").replace("[]", ".*");
            if entries(&values, &pattern.split('.').collect::<Vec<_>>())
                .iter()
                .any(|v| !type_valid(f, v))
            {
                errors.push(json!({"path":path.split("[]").next().unwrap().split(".<").next().unwrap(),"message":format!("内部字段 {path} 需要{}",string(&f["type_label"]))}));
            }
            continue;
        }
        if f["status"] == "reserved" {
            continue;
        }
        let path = string(&f["path"]);
        let Some(v) = get(&values, path) else {
            continue;
        };
        let mut reason = None;
        if !type_valid(f, v) {
            reason = Some(format!("需要{}", string(&f["type_label"])));
        }
        if (!f["choices_when"].is_object()
            || get(&values, string(&f["choices_when"]["path"]))
                == Some(&f["choices_when"]["equals"]))
            && f["choices"].as_array().is_some_and(|a| !a.contains(v))
        {
            reason = Some(format!(
                "允许值：{}",
                f["choices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        }
        for rule in f["rules"].as_array().into_iter().flatten() {
            if rule["allow_empty"] == true && v.as_str().is_some_and(|s| s.trim().is_empty()) {
                continue;
            }
            if rule["when"].is_object()
                && get(&values, string(&rule["when"]["path"])) != Some(&rule["when"]["equals"])
            {
                continue;
            }
            let n = if rule["length_bytes"] == true {
                v.as_str().map(|s| s.trim().len() as f64)
            } else {
                numeric(v, f["type"] == "duration")
            };
            let limit = if rule["value"].is_object() {
                get(&values, string(&rule["value"]["path"])).and_then(Value::as_f64)
            } else {
                rule["value"].as_f64()
            };
            if let (Some(n), Some(limit)) = (n, limit) {
                if rule["allow_zero"] == true && n == 0.0 {
                    continue;
                }
                let ok = match string(&rule["op"]) {
                    "ge" => n >= limit,
                    "gt" => n > limit,
                    "le" => n <= limit,
                    "lt" => n < limit,
                    _ => false,
                };
                if !ok {
                    reason = Some(string(&f["range"]).into());
                }
            }
        }
        if let Some(message) = reason {
            errors.push(json!({"path":path,"message":message}));
        }
    }
    let mut add = |path: &str, message: &str| errors.push(json!({"path":path,"message":message}));
    if get(&values, "log.output.to_stdout") == Some(&json!(false))
        && get(&values, "log.output.to_file") == Some(&json!(false))
    {
        add("log.output.to_stdout", "标准输出与文件输出不能同时关闭");
    }
    let weights = ["priority", "load", "queue", "error_rate", "ttft"];
    if weights.iter().all(|k| {
        get(
            &values,
            &format!("gateway.openai_ws.scheduler_score_weights.{k}"),
        )
        .and_then(Value::as_f64)
            == Some(0.0)
    }) {
        add(
            "gateway.openai_ws.scheduler_score_weights.priority",
            "基础评分权重之和必须大于 0",
        );
    }
    if let Some(s) = get(&values, "timezone").and_then(Value::as_str) {
        if s.parse::<chrono_tz::Tz>().is_err() {
            add("timezone", "需要有效的 IANA 时区");
        }
    }
    errors.extend(constraints::check(&values));
    errors
}
pub fn prepare(
    bytes: &[u8],
    env: &BTreeMap<String, String>,
    payload: &Value,
) -> Result<(Vec<u8>, Value)> {
    validate_request(payload)?;
    if sha256(bytes) != payload["expected_revision"] {
        return Err(Error::conflict("配置已被其他操作修改，请刷新后重试"));
    }
    let mut config = parse_yaml(bytes)?;
    for change in payload["changes"].as_array().unwrap() {
        let path = string(&change["path"]);
        let f = field(path).unwrap();
        if environment_override(f, &config, env).is_some() {
            return Err(Error::new(
                422,
                json!({"errors":[{"path":path,"message":"此字段由进程环境变量覆盖，只读"}]}),
            ));
        }
        set(
            &mut config,
            path,
            if change["remove"] == true {
                None
            } else {
                Some(change["value"].clone())
            },
        )?;
    }
    let errors = violations(&config, env);
    if !errors.is_empty() {
        return Err(Error::new(422, json!({"errors":errors})));
    }
    let output = yaml_serde::to_string(&config)
        .map_err(|_| Error::internal())?
        .into_bytes();
    if output.len() > MAX_CONFIG_BYTES {
        return Err(Error::new(422, "配置文件超过 1 MiB 上限"));
    }
    Ok((output, config))
}
