use serde_json::{json, Value};
use std::sync::LazyLock;

pub static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../src/account_creation_fields.json"))
        .expect("account field catalog")
});
pub const IDENTITY: &[&str] = &[
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
pub const AUTH_EXTRA: &[&str] = &[
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

pub fn pick(value: &Value, fields: &[&str]) -> Value {
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
pub fn applies(field: &Value, platform: &str, kind: &str) -> bool {
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
pub fn insert(out: &mut Value, path: &str, value: Value) {
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
pub fn reusable(row: &Value) -> Option<Value> {
    let platform = row["platform"].as_str()?;
    let kind = row["type"].as_str()?;
    if !supported(platform, kind) {
        return None;
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
        let path = f["path"].as_str()?;
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
    out.as_object_mut()
        .unwrap()
        .remove("upstream_billing_group_sync_enabled");
    Some(out)
}

pub fn authorization_url(value: &str) -> bool {
    value.len() <= 16384
        && url::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        })
}

pub fn auth_reply(value: &Value, authorize: bool) -> Option<Value> {
    if authorize {
        if !authorization_url(value["auth_url"].as_str()?)
            || value["session_id"].as_str()?.is_empty()
        {
            return None;
        }
        return Some(pick(value, &["auth_url", "session_id", "state"]));
    }
    if value["access_token"].as_str()?.is_empty() {
        return None;
    }
    let mut out = pick(value, IDENTITY);
    for key in ["state", "session_id", "name", "privacy_mode"] {
        if let Some(v) = value.get(key) {
            out[key] = v.clone();
        }
    }
    if !out
        .as_object()?
        .values()
        .all(|v| v.is_null() || v.is_string() || v.is_number() || v.is_boolean())
    {
        return None;
    }
    if value["extra"].is_object() {
        let extra = pick(&value["extra"], AUTH_EXTRA);
        if !extra
            .as_object()?
            .values()
            .all(|v| v.is_null() || v.is_string() || v.is_number() || v.is_boolean())
        {
            return None;
        }
        out["extra"] = extra;
    }
    Some(out)
}
