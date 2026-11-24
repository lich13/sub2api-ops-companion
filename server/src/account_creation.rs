//! Explicit Sub2API account creation. No credential-bearing request is queued or logged.
use crate::{
    actions::Actions,
    compat::string,
    error::{Error, Result},
    security::clean,
    upstream::read_limited,
    validation,
};
use reqwest::Method;
use serde_json::{json, Value};
use std::time::Duration;
use sub2ops_protocol::creation::{applies, pick, AUTH_EXTRA, IDENTITY};
pub use sub2ops_protocol::creation::{supported, SCHEMA};

const COMMON: &[&str] = &[
    "name",
    "notes",
    "proxy_id",
    "concurrency",
    "load_factor",
    "priority",
    "rate_multiplier",
    "group_ids",
    "expires_at",
    "auto_pause_on_expired",
];

#[cfg(test)]
use sub2ops_protocol::creation::insert;
pub use sub2ops_protocol::routes::auth_route;
use sub2ops_protocol::routes::{catalog_route, import_route};
pub fn reusable(row: &Value) -> Result<Value> {
    sub2ops_protocol::creation::reusable(row).ok_or_else(|| Error::new(422, "此账号类型不支持创建"))
}
fn common(value: &Value) -> Result<()> {
    for name in ["name", "notes"] {
        if let Some(v) = value.get(name).filter(|v| !v.is_null()) {
            validation::text(v, 0, 8192)?;
        }
    }
    for name in ["proxy_id", "load_factor", "expires_at"] {
        if let Some(v) = value.get(name).filter(|v| !v.is_null()) {
            validation::integer_in_range(v, 1, i64::MAX)?;
        }
    }
    for name in ["concurrency", "priority"] {
        if let Some(v) = value.get(name) {
            validation::integer_in_range(v, i64::from(name == "concurrency"), i32::MAX as i64)?;
        }
    }
    if let Some(v) = value.get("rate_multiplier") {
        if !v.as_f64().is_some_and(|v| v.is_finite() && v >= 0.) {
            return Err(Error::invalid());
        }
    }
    if let Some(v) = value.get("auto_pause_on_expired") {
        validation::boolean(v)?;
    }
    if let Some(v) = value.get("group_ids") {
        for id in v
            .as_array()
            .filter(|a| a.len() <= 1000)
            .ok_or_else(Error::invalid)?
        {
            validation::integer_in_range(id, 1, i64::MAX)?;
        }
    }
    Ok(())
}
fn configuration(value: &Value, platform: &str, kind: &str, auth: bool) -> Result<()> {
    for section in ["credentials", "extra"] {
        let Some(values) = value.get(section).filter(|v| !v.is_null()) else {
            continue;
        };
        for (key, v) in values.as_object().ok_or_else(Error::invalid)? {
            let path = format!("{section}.{key}");
            if let Some(f) = SCHEMA["fields"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["path"] == path && applies(f, platform, kind))
            {
                config_value(f, v)?;
                if f["kind"] == "headers" && !header_overrides_valid(v, kind == "apikey") {
                    return Err(Error::new(
                        422,
                        "请求头名称、值或模板变量无效；认证头不能覆写，仅 API Key 账号支持 {{user_id}}、{{api_key_id}}、{{group_id}}、{{request_id}}",
                    ));
                }
            } else if section == "extra"
                && platform == "openai"
                && key == &format!("openai_{kind}_responses_websockets_v2_enabled")
            {
                validation::boolean(v)?;
            } else if auth
                && (section == "credentials" && IDENTITY.contains(&key.as_str())
                    || section == "extra" && AUTH_EXTRA.contains(&key.as_str()))
            {
                if !v.is_null() && !v.is_string() && !v.is_number() && !v.is_boolean() {
                    return Err(Error::invalid());
                }
            } else {
                return Err(Error::new(422, "账号参数不适用于当前平台和类型"));
            }
        }
    }
    Ok(())
}
fn config_value(f: &Value, v: &Value) -> Result<()> {
    if v.is_null() {
        return Ok(());
    }
    let valid = match string(&f["kind"]) {
        "boolean" => v.is_boolean(),
        "stringbool" => matches!(v.as_str(), Some("true" | "false")),
        "number" => v.as_f64().is_some_and(|n| {
            n.is_finite()
                && f["min"].as_f64().is_none_or(|min| n >= min)
                && f["max"].as_f64().is_none_or(|max| n <= max)
        }),
        "tls" => v.as_i64().is_some_and(|id| id > 0),
        "select" => f["choices"]
            .as_array()
            .is_some_and(|choices| choices.contains(v)),
        "tier" | "text" | "url" => v.as_str().is_some_and(|v| v.len() <= 8192),
        "strings" => v.as_array().is_some_and(|a| {
            !a.is_empty()
                && a.iter()
                    .all(|v| f["choices"].as_array().is_some_and(|c| c.contains(v)))
        }),
        "numbers" => v.as_array().is_some_and(|a| {
            a.len() <= 500
                && a.iter().all(|n| {
                    n.as_i64().is_some_and(|n| {
                        n >= f["min"].as_i64().unwrap_or(0)
                            && n <= f["max"].as_i64().unwrap_or(i64::MAX)
                    })
                })
        }),
        "mapping" | "headers" | "protocols" => v.as_object().is_some_and(|o| {
            o.len() <= 300
                && o.iter()
                    .all(|(k, v)| k.len() <= 200 && v.as_str().is_some_and(|v| v.len() <= 8192))
        }),
        "pause-rules" | "protocol-rules" => v
            .as_array()
            .is_some_and(|a| a.len() <= 300 && a.iter().all(Value::is_object)),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(Error::new(422, format!("{}格式无效", string(&f["label"]))))
    }
}
fn header_overrides_valid(value: &Value, allow_variables: bool) -> bool {
    let Some(map) = value.as_object() else {
        return true;
    };
    if map.len() > 64 {
        return false;
    }
    let blocked = SCHEMA["blocked_headers"].as_array();
    let mut seen = std::collections::HashSet::with_capacity(map.len());
    for (key, raw) in map {
        let k = key.trim();
        let lower = k.to_ascii_lowercase();
        if k.is_empty()
            || k.len() > 200
            || !k.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                    )
            })
            || blocked.is_some_and(|list| list.iter().any(|item| item.as_str() == Some(&lower)))
            || !seen.insert(lower)
        {
            return false;
        }
        let Some(s) = raw.as_str() else {
            return false;
        };
        if s.len() > 8192 || s.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f) {
            return false;
        }
        if !allow_variables {
            if s.contains("{{") {
                return false;
            }
            continue;
        }
        let mut rest = s;
        while let Some(start) = rest.find("{{") {
            let after_open = &rest[start + 2..];
            let Some(end) = after_open.find("}}") else {
                return false;
            };
            let name = after_open[..end].trim();
            if !matches!(name, "user_id" | "api_key_id" | "group_id" | "request_id") {
                return false;
            }
            rest = &after_open[end + 2..];
        }
    }
    true
}
fn http_base_url_valid(value: &Value) -> bool {
    value.as_str().is_some_and(|raw| {
        let trimmed = raw.trim();
        !trimmed.is_empty()
            && trimmed.len() <= 8192
            && reqwest::Url::parse(trimmed).is_ok_and(|u| {
                matches!(u.scheme(), "http" | "https")
                    && u.host_str().is_some()
                    && u.username().is_empty()
                    && u.password().is_none()
            })
    })
}
pub fn validate(path: &[&str], value: &Value) -> Result<()> {
    let mut fields = COMMON.to_vec();
    match path {
        ["account-creation", "models"] => {
            validation::fields(
                value,
                &["platform", "type", "base_url", "api_key", "model_mapping"],
                &["platform", "type", "api_key"],
            )?;
            if !supported(string(&value["platform"]), string(&value["type"])) {
                return Err(Error::invalid());
            }
            validation::text(&value["api_key"], 1, 65536)?;
        }
        ["account-creation", "upstream-group"] => {
            validation::fields(
                value,
                &["platform", "base_url", "api_key", "proxy_id"],
                &["platform", "base_url", "api_key"],
            )?;
            if !supported(string(&value["platform"]), "apikey")
                || !http_base_url_valid(&value["base_url"])
            {
                return Err(Error::invalid());
            }
            validation::text(&value["api_key"], 1, 65536)?;
            common(value)?;
        }
        ["account-creation"] => {
            fields.extend([
                "platform",
                "type",
                "credentials",
                "extra",
                "upstream_billing_probe_enabled",
                "upstream_billing_group_sync_enabled",
                "confirm_mixed_channel_risk",
            ]);
            validation::fields(value, &fields, &["name", "platform", "type", "credentials"])?;
            if string(&value["name"]).trim().is_empty()
                || !supported(string(&value["platform"]), string(&value["type"]))
            {
                return Err(Error::invalid());
            }
            configuration(
                value,
                string(&value["platform"]),
                string(&value["type"]),
                true,
            )?;
            if !value["credentials"].is_object() {
                return Err(Error::invalid());
            }
            for flag in [
                "upstream_billing_probe_enabled",
                "upstream_billing_group_sync_enabled",
                "confirm_mixed_channel_risk",
            ] {
                if let Some(v) = value.get(flag) {
                    validation::boolean(v)?;
                }
            }
            if value.get("upstream_billing_group_sync_enabled") == Some(&json!(true))
                && (string(&value["type"]) != "apikey"
                    || value.get("upstream_billing_probe_enabled") == Some(&json!(false)))
            {
                return Err(Error::new(
                    422,
                    "自动同步上游分组名称与备注仅适用于已开启自动探测上游倍率的 API Key 账号",
                ));
            }
            common(value)?;
        }
        ["account-creation", "check-mixed-channel"] => {
            validation::fields(
                value,
                &["platform", "group_ids"],
                &["platform", "group_ids"],
            )?;
            validation::text(&value["platform"], 1, 40)?;
            common(value)?;
        }
        ["account-creation", "auth", platform, action] => {
            if auth_route(platform, action).is_none() {
                return Err(Error::invalid());
            }
            let allowed = match *action {
                "authorize" => vec!["proxy_id", "project_id", "oauth_type", "tier_id"],
                "exchange" => vec![
                    "session_id",
                    "state",
                    "code",
                    "proxy_id",
                    "oauth_type",
                    "tier_id",
                ],
                "refresh" => vec!["refresh_token", "proxy_id", "client_id"],
                "cookie" => vec!["code", "proxy_id"],
                _ => return Err(Error::invalid()),
            };
            let required: &[&str] = match *action {
                "exchange" => &["session_id", "code"],
                "refresh" => &["refresh_token"],
                "cookie" => &["code"],
                _ => &[],
            };
            validation::fields(value, &allowed, required)?;
            for (k, v) in value.as_object().unwrap() {
                if k != "proxy_id" {
                    validation::text(v, 1, 65536)?;
                }
            }
            common(value)?;
        }
        ["account-creation", "import", kind] => {
            if import_route(kind).is_none() {
                return Err(Error::invalid());
            }
            fields.extend(["extra", "credential_extras", "credentials"]);
            let key = match *kind {
                "codex-session" => {
                    fields.extend(["content", "update_existing"]);
                    "content"
                }
                "codex-pat" => {
                    fields.push("access_token");
                    "access_token"
                }
                _ => {
                    fields.push("sso_tokens");
                    "sso_tokens"
                }
            };
            validation::fields(value, &fields, &[key])?;
            common(value)?;
            let mut config = value.clone();
            if let Some(v) = value.get("credential_extras") {
                config["credentials"] = v.clone();
            }
            configuration(
                &config,
                if *kind == "grok-sso" {
                    "grok"
                } else {
                    "openai"
                },
                "oauth",
                false,
            )?;
            if key == "sso_tokens" {
                for v in value[key]
                    .as_array()
                    .filter(|a| !a.is_empty() && a.len() <= 1000)
                    .ok_or_else(Error::invalid)?
                {
                    validation::text(v, 1, 65536)?;
                }
            } else {
                validation::text(&value[key], 1, 1_000_000)?;
            }
            if let Some(v) = value.get("update_existing") {
                validation::boolean(v)?;
            }
        }
        _ => return Err(Error::invalid()),
    }
    Ok(())
}

async fn upstream(
    actions: &Actions,
    key: &str,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let creating = matches!(
        path,
        "accounts"
            | "accounts/import/codex-session"
            | "openai/create-from-codex-pat"
            | "grok/sso-to-oauth"
    );
    let response = actions
        .request(method, path, key, body, Duration::from_secs(120), false)
        .await?;
    let status = response.status().as_u16();
    let bytes = read_limited(response, 8_000_000).await?;
    if !creating && status == 404 {
        let message = if path == "accounts/upstream-billing-probe/group-preview" {
            "当前 Sub2API 版本不支持同步上游分组信息"
        } else {
            "上游资源不存在，请刷新后重试"
        };
        return Err(Error::new(404, message));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        if creating {
            Error::new(502, "响应无法确认，请刷新账号核对；未重放请求")
        } else {
            Error::new(
                if (400..500).contains(&status) {
                    status
                } else {
                    502
                },
                "上游返回无效响应，请检查 Sub2API 服务状态",
            )
        }
    })?;
    if !(200..300).contains(&status) || value["code"] != 0 {
        if value.to_string().contains("mixed_channel_warning") {
            return Err(Error::new(
                409,
                "mixed_channel_warning：这些分组包含不同渠道账号，确认后才能创建",
            ));
        }
        let message = if creating && status >= 500 {
            "创建结果未确认，请刷新账号核对；未重放请求"
        } else if path == "accounts/upstream-billing-probe/group-preview" && status >= 500 {
            "无法从上游读取分组信息，请检查 API 地址、密钥与代理"
        } else if status >= 500 {
            "上游服务暂时不可用，请稍后重试"
        } else {
            "上游拒绝操作，请检查账号参数或授权凭据"
        };
        return Err(Error::new(
            if (400..500).contains(&status) {
                status
            } else {
                502
            },
            message,
        ));
    }
    Ok(value["data"].clone())
}
fn clean_group_text(value: &Value, limit: usize, multiline: bool) -> String {
    let Some(raw) = value.as_str() else {
        return String::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || (!multiline && trimmed.len() > limit)
        || trimmed
            .chars()
            .any(|c| c.is_control() && (!multiline || !matches!(c, '\n' | '\r' | '\t')))
    {
        return String::new();
    }
    clean(&Value::String(trimmed.to_owned()), limit)
}
fn account_result(value: &Value) -> Value {
    json!({"id":value["id"],"name":clean(&value["name"],512),"platform":value["platform"],"type":value["type"]})
}
fn names(value: &Value, fields: &[&str]) -> Result<Value> {
    let rows = value
        .as_array()
        .or_else(|| value["items"].as_array())
        .ok_or_else(|| Error::new(502, "账号选项响应无效"))?;
    Ok(json!(rows
        .iter()
        .map(|v| pick(v, fields))
        .collect::<Vec<_>>()))
}
fn model_warnings(value: &Value) -> Vec<&'static str> {
    let mut warnings = Vec::new();
    for warning in value["warnings"].as_array().into_iter().flatten() {
        let text = match string(&warning["code"]) {
            "upstream_model_metadata_incomplete" => "模型已同步，上游未提供完整模型元数据",
            "upstream_model_metadata_partial" => "模型已同步，部分上游模型元数据缺失",
            _ => "上游模型同步返回提示，请核对模型配置",
        };
        if !warnings.contains(&text) {
            warnings.push(text);
        }
    }
    warnings
}
pub async fn handle(
    actions: &Actions,
    key: &str,
    path: &[&str],
    body: &Value,
    sync_models: bool,
) -> Result<Value> {
    match path {
        ["account-creation", "models"] => {
            let value = upstream(
                actions,
                key,
                Method::POST,
                "accounts/models/sync-upstream-preview",
                Some(body),
            )
            .await?;
            Ok(json!({"models":value["models"],"warnings":model_warnings(&value)}))
        }
        ["account-creation", "upstream-group"] => {
            let value = upstream(
                actions,
                key,
                Method::POST,
                "accounts/upstream-billing-probe/group-preview",
                Some(body),
            )
            .await?;
            Ok(json!({
                "group_name": clean_group_text(&value["group_name"], 100, false),
                "group_description": clean_group_text(&value["group_description"], 8192, true),
            }))
        }
        ["account-creation", "options"] => {
            let (groups, proxies) = tokio::try_join!(
                upstream(actions, key, Method::GET, "groups/all", None),
                upstream(actions, key, Method::GET, "proxies/all", None)
            )?;
            Ok(
                json!({"groups":names(&groups,&["id","name","platform","status","long_context_pricing_enabled"] )?,"proxies":names(&proxies,&["id","name","status"])?}),
            )
        }
        ["account-creation", "source", id] => {
            reusable(&upstream(actions, key, Method::GET, &format!("accounts/{id}"), None).await?)
        }
        ["account-creation", "catalog", kind] => {
            let value = upstream(
                actions,
                key,
                Method::GET,
                catalog_route(kind).ok_or_else(Error::invalid)?,
                None,
            )
            .await?;
            Ok(match *kind {
                "gemini" => pick(&value, &["ai_studio_oauth_enabled"]),
                "grok" => pick(&value, &["password_auth_enabled"]),
                "settings" => pick(&value, &["account_quota_notify_enabled"]),
                "web-search" => {
                    json!({"enabled": value["enabled"] == true && value["providers"].as_array().is_some_and(|providers| !providers.is_empty())})
                }
                "tls" => names(&value, &["id", "name"])?,
                _ => value,
            })
        }
        ["account-creation", "check-mixed-channel"] => {
            let value = upstream(
                actions,
                key,
                Method::POST,
                "accounts/check-mixed-channel",
                Some(body),
            )
            .await?;
            if !value["has_risk"].is_boolean() {
                return Err(Error::new(502, "混合渠道检查响应无效"));
            }
            Ok(pick(&value, &["has_risk"]))
        }
        ["account-creation", "auth", platform, action] => {
            let value = upstream(
                actions,
                key,
                Method::POST,
                auth_route(platform, action).ok_or_else(Error::invalid)?,
                Some(body),
            )
            .await?;
            let mut fields = IDENTITY.to_vec();
            fields.extend([
                "auth_url",
                "session_id",
                "state",
                "extra",
                "name",
                "privacy_mode",
            ]);
            let mut result = pick(&value, &fields);
            if result["extra"].is_object() {
                result["extra"] = pick(&result["extra"], AUTH_EXTRA);
            }
            Ok(result)
        }
        ["account-creation", "import", kind] => {
            let value = upstream(
                actions,
                key,
                Method::POST,
                import_route(kind).ok_or_else(Error::invalid)?,
                Some(body),
            )
            .await?;
            if *kind == "codex-pat" {
                return Ok(json!({"account":account_result(&value),"warnings":[]}));
            }
            if *kind == "grok-sso" {
                let created: Vec<Value> = value["created"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| json!({"index":v["index"],"account":account_result(&v["account"])}))
                    .collect();
                let failed: Vec<Value> = value["failed"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| json!({"index":v["index"],"error":"SSO 导入失败，请检查凭据后重试"}))
                    .collect();
                return Ok(json!({"created":created,"failed":failed}));
            }
            let mut result = pick(&value, &["created", "updated", "skipped", "failed"]);
            for name in ["errors", "warnings"] {
                result[name] = json!(value[name].as_array().into_iter().flatten().map(|v| json!({"index":v["index"],"message":if name == "errors" {"凭据导入失败，请检查格式及授权状态"}else{"此项存在授权限制，请在 Sub2API 核对账号详情"}})).collect::<Vec<_>>());
            }
            Ok(result)
        }
        ["account-creation"] => {
            let value = upstream(actions, key, Method::POST, "accounts", Some(body)).await?;
            let id = value["id"]
                .as_i64()
                .filter(|v| *v > 0)
                .ok_or_else(|| Error::new(502, "创建结果未确认，请刷新账号核对；未重放请求"))?;
            let mut warnings = Vec::new();
            if body["type"] == "apikey" {
                if sync_models
                    || body["credentials"]["model_mapping"]
                        .as_object()
                        .is_some_and(|m| {
                            m.values().any(|b| {
                                b.as_str()
                                    .is_some_and(|b| !b.is_empty() && !b.contains('*'))
                            })
                        })
                {
                    match upstream(
                        actions,
                        key,
                        Method::POST,
                        &format!("accounts/{id}/models/sync-upstream"),
                        Some(&json!({})),
                    )
                    .await
                    {
                        Ok(value) => warnings.extend(model_warnings(&value)),
                        Err(_) => warnings.push("账号已创建，模型同步失败，可在账号设置中重试"),
                    }
                }
                if body["upstream_billing_probe_enabled"] == true
                    && upstream(
                        actions,
                        key,
                        Method::POST,
                        &format!("accounts/{id}/upstream-billing-probe"),
                        Some(&json!({})),
                    )
                    .await
                    .is_err()
                {
                    warnings.push("账号已创建，上游倍率探测失败");
                }
            }
            Ok(json!({"account":account_result(&value),"warnings":warnings}))
        }
        _ => Err(Error::invalid()),
    }
}

#[cfg(test)]
#[path = "account_creation_tests.rs"]
mod tests;
