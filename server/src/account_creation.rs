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
use std::{sync::LazyLock, time::Duration};

pub static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("account_creation_fields.json"))
        .expect("account field catalog")
});
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
const IDENTITY: &[&str] = &[
    "api_key",
    "access_token",
    "refresh_token",
    "id_token",
    "token_type",
    "expires_at",
    "expires_in",
    "scope",
    "email",
    "email_address",
    "org_uuid",
    "account_uuid",
    "chatgpt_account_id",
    "chatgpt_user_id",
    "organization_id",
    "plan_type",
    "subscription_expires_at",
    "client_id",
    "project_id",
    "client_email",
    "tier_id",
    "oauth_type",
    "sub",
    "team_id",
    "subscription_tier",
    "entitlement_status",
    "aws_access_key_id",
    "aws_secret_access_key",
    "aws_session_token",
    "service_account_json",
];
const AUTH_EXTRA: &[&str] = &[
    "email",
    "email_address",
    "name",
    "privacy_mode",
    "org_uuid",
    "account_uuid",
    "subscription_type",
    "subscription_tier",
    "entitlement_status",
    "drive_storage_limit",
    "drive_storage_usage",
    "drive_tier_updated_at",
];

fn pick(value: &Value, fields: &[&str]) -> Value {
    Value::Object(
        fields
            .iter()
            .filter_map(|k| value.get(*k).map(|v| ((*k).to_string(), v.clone())))
            .collect(),
    )
}

pub fn supported(platform: &str, kind: &str) -> bool {
    SCHEMA["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == platform && p["types"].as_array().unwrap().iter().any(|t| t == kind))
}
fn applies(field: &Value, platform: &str, kind: &str) -> bool {
    [("platforms", platform), ("types", kind)]
        .iter()
        .all(|(key, value)| {
            field[*key]
                .as_array()
                .is_none_or(|a| a.iter().any(|v| v == *value))
        })
}
fn field_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    match path.split_once('.') {
        Some((section, key)) => value.get(section)?.get(key),
        None => value.get(path),
    }
}
fn insert(out: &mut Value, path: &str, value: Value) {
    match path.split_once('.') {
        Some((section, key)) => {
            if !out[section].is_object() {
                out[section] = json!({});
            }
            out[section][key] = value;
        }
        None => out[path] = value,
    }
}
pub fn reusable(row: &Value) -> Result<Value> {
    let platform = string(&row["platform"]);
    let kind = string(&row["type"]);
    if !supported(platform, kind) {
        return Err(Error::new(422, "此账号类型不支持创建"));
    }
    let mut out = pick(
        row,
        &[
            "notes",
            "proxy_id",
            "concurrency",
            "load_factor",
            "priority",
            "rate_multiplier",
            "group_ids",
            "expires_at",
            "auto_pause_on_expired",
        ],
    );
    if !out["group_ids"].is_array() {
        out["group_ids"] = json!(row["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| g["id"].as_i64())
            .collect::<Vec<_>>());
    }
    out["platform"] = row["platform"].clone();
    out["type"] = row["type"].clone();
    out["credentials"] = json!({});
    out["extra"] = json!({});
    for f in SCHEMA["fields"].as_array().unwrap() {
        let path = string(&f["path"]);
        if applies(f, platform, kind) {
            if let Some(value) = field_value(row, path) {
                insert(&mut out, path, value.clone());
            }
        }
    }
    // Account creation parameters are selected positively; identity and live quota
    // state never ride along with the source account's credentials or extra.
    if kind == "service_account" {
        out["credentials"]
            .as_object_mut()
            .unwrap()
            .remove("project_id");
    }
    if kind == "apikey" {
        if let Some(v) = row["extra"].get("upstream_billing_probe_enabled") {
            out["upstream_billing_probe_enabled"] = v.clone();
        }
    }
    Ok(out)
}

pub fn auth_route(platform: &str, action: &str) -> Option<&'static str> {
    match (platform, action) {
        ("anthropic", "authorize") => Some("accounts/generate-auth-url"),
        ("anthropic", "exchange") => Some("accounts/exchange-code"),
        ("anthropic", "cookie") => Some("accounts/cookie-auth"),
        ("setup-token", "authorize") => Some("accounts/generate-setup-token-url"),
        ("setup-token", "exchange") => Some("accounts/exchange-setup-token-code"),
        ("setup-token", "cookie") => Some("accounts/setup-token-cookie-auth"),
        ("openai", "authorize") => Some("openai/generate-auth-url"),
        ("openai", "exchange") => Some("openai/exchange-code"),
        ("openai", "refresh") => Some("openai/refresh-token"),
        ("gemini", "authorize") => Some("gemini/oauth/auth-url"),
        ("gemini", "exchange") => Some("gemini/oauth/exchange-code"),
        ("antigravity", "authorize") => Some("antigravity/oauth/auth-url"),
        ("antigravity", "exchange") => Some("antigravity/oauth/exchange-code"),
        ("antigravity", "refresh") => Some("antigravity/oauth/refresh-token"),
        ("grok", "authorize") => Some("grok/oauth/auth-url"),
        ("grok", "exchange") => Some("grok/oauth/exchange-code"),
        ("grok", "refresh") => Some("grok/oauth/refresh-token"),
        _ => None,
    }
}
fn import_route(kind: &str) -> Option<&'static str> {
    match kind {
        "codex-session" => Some("accounts/import/codex-session"),
        "codex-pat" => Some("openai/create-from-codex-pat"),
        "grok-sso" => Some("grok/sso-to-oauth"),
        _ => None,
    }
}
fn catalog_route(kind: &str) -> Option<&'static str> {
    match kind {
        "gemini" => Some("gemini/oauth/capabilities"),
        "grok" => Some("grok/oauth/capabilities"),
        "tls" => Some("tls-fingerprint-profiles"),
        "settings" => Some("settings"),
        "web-search" => Some("settings/web-search-emulation"),
        "antigravity-models" => Some("accounts/antigravity/default-model-mapping"),
        _ => None,
    }
}
pub fn allowed(path: &[&str]) -> Option<&'static [&'static str]> {
    match path {
        ["account-creation"] | ["account-creation", "check-mixed-channel" | "models"] => {
            Some(&["POST"])
        }
        ["account-creation", "options"] => Some(&["GET"]),
        ["account-creation", "source", id] if id.parse::<i64>().is_ok_and(|v| v > 0) => {
            Some(&["GET"])
        }
        ["account-creation", "catalog", kind] if catalog_route(kind).is_some() => Some(&["GET"]),
        ["account-creation", "auth", platform, action]
            if auth_route(platform, action).is_some() =>
        {
            Some(&["POST"])
        }
        ["account-creation", "import", kind] if import_route(kind).is_some() => Some(&["POST"]),
        _ => None,
    }
}
fn common(value: &Value) -> Result<()> {
    for name in ["name", "notes"] {
        if let Some(v) = value.get(name).filter(|v| !v.is_null()) {
            validation::text(v, 0, 8192)?;
        }
    }
    for name in ["proxy_id", "load_factor", "expires_at"] {
        if let Some(v) = value.get(name).filter(|v| !v.is_null()) {
            validation::integer(v, 1, i64::MAX)?;
        }
    }
    for name in ["concurrency", "priority"] {
        if let Some(v) = value.get(name) {
            validation::integer(v, i64::from(name == "concurrency"), i32::MAX as i64)?;
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
            validation::integer(id, 1, i64::MAX)?;
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
        ["account-creation"] => {
            fields.extend([
                "platform",
                "type",
                "credentials",
                "extra",
                "upstream_billing_probe_enabled",
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
                "confirm_mixed_channel_risk",
            ] {
                if let Some(v) = value.get(flag) {
                    validation::boolean(v)?;
                }
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
    let response = actions
        .request(method, path, key, body, Duration::from_secs(120), false)
        .await?;
    let status = response.status().as_u16();
    let bytes = read_limited(response, 8_000_000).await?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::new(502, "响应无法确认，请刷新账号核对；未重放请求"))?;
    if !(200..300).contains(&status) || value["code"] != 0 {
        if value.to_string().contains("mixed_channel_warning") {
            return Err(Error::new(
                409,
                "mixed_channel_warning：这些分组包含不同渠道账号，确认后才能创建",
            ));
        }
        let message = if status >= 500 {
            "创建结果未确认，请刷新账号核对；未重放请求"
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
