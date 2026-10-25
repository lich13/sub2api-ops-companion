//! HTTP shape validation, before authentication or any domain access.
//! Business decisions stay in the authenticated handlers.
use crate::{
    actions::TEST_MODES,
    compat::string,
    error::{Error, Result},
    validation::{self, boolean, fields, integer, text},
};
use serde_json::{json, Value};

pub fn has_body(path: &[&str], method: &str) -> bool {
    if method == "GET" || method == "DELETE" && path.first() == Some(&"proxy-management") {
        return false;
    }
    matches!(
        path,
        ["proxy-management", ..]
            | ["sub2api-config", "validate" | "apply"]
            | ["account-creation", ..]
            | ["accounts", _]
            | [
                "accounts",
                _,
                "operations"
                    | "schedulable"
                    | "recover-state"
                    | "priority"
                    | "test"
                    | "model-tests"
                    | "usage-action"
                    | "groups"
                    | "degradation-mark"
                    | "model-detection"
                    | "keepalive"
                    | "key-quota-settings"
            ]
            | ["account-operations", "batch"]
            | ["account-priorities"]
            | ["account-priority-orders"]
            | ["account-priority-orders", _, "cancel"]
            | ["account-templates", ..]
            | ["model-tests", _, "retry"]
            | ["config", _]
            | ["model-groups", _, "reasoning"]
            | ["model-groups", _, "reasoning", "resolve"]
    )
}

fn version(value: &Value, name: &str) -> Result<()> {
    text(&value[name], 64, 64).map(|_| ())
}
fn object(value: &Value) -> Result<()> {
    value.as_object().map(|_| ()).ok_or_else(Error::invalid)
}
fn optional_bool(value: &Value, name: &str) -> Result<()> {
    value.get(name).map(boolean).transpose().map(|_| ())
}
fn choice(value: &Value, choices: &[&str]) -> Result<()> {
    if !choices.contains(&string(value)) {
        return Err(Error::invalid());
    }
    Ok(())
}
fn profile(value: &Value) -> Result<()> {
    for name in ["whitelist", "mappings"] {
        let Some(values) = value.get(name) else {
            continue;
        };
        let values = values
            .as_array()
            .filter(|items| items.len() <= 300)
            .ok_or_else(Error::invalid)?;
        for item in values {
            if name == "whitelist" {
                text(item, 0, usize::MAX)?;
            } else {
                fields(item, &["source", "target"], &["source", "target"])?;
                text(&item["source"], 1, 200)?;
                text(&item["target"], 1, 200)?;
            }
        }
    }
    Ok(())
}
pub fn template_id(value: &Value) -> Result<()> {
    let id = text(value, 4, 31)?;
    if matches!(id, "full" | "degraded" | "takeover")
        || id
            .strip_prefix("custom-")
            .is_some_and(|suffix| validation::hex(&json!(suffix), 24).is_ok())
    {
        Ok(())
    } else {
        Err(Error::invalid())
    }
}
pub fn batch(value: &Value, template: bool) -> Result<()> {
    let action = if template {
        "account_template"
    } else {
        string(&value["action"])
    };
    if template {
        let keys = [
            "template_id",
            "template_version",
            "request_id",
            "client_id",
            "accounts",
        ];
        fields(value, &keys, &keys)?;
        template_id(&value["template_id"])?;
        version(value, "template_version")?;
    } else {
        fields(
            value,
            &["action", "payload", "request_id", "client_id", "accounts"],
            &["action", "request_id", "client_id", "accounts"],
        )?;
        choice(
            &value["action"],
            &["test", "proxy", "groups", "delete", "recover"],
        )?;
        match action {
            "test" => {
                fields(&value["payload"], &["confirmed"], &["confirmed"])?;
                boolean(&value["payload"]["confirmed"])?;
            }
            "proxy" => {
                fields(&value["payload"], &["proxy_id"], &["proxy_id"])?;
                if !value["payload"]["proxy_id"].is_null() {
                    integer(&value["payload"]["proxy_id"], 1, i64::MAX)?;
                }
            }
            "recover" => {
                if let Some(payload) = value.get("payload") {
                    fields(payload, &[], &[])?;
                }
            }
            _ => {
                if let Some(payload) = value.get("payload") {
                    object(payload)?;
                }
            }
        }
    }
    for key in ["request_id", "client_id"] {
        if template {
            validation::hex(&value[key], 32)?;
        } else {
            validation::request_id(&value[key])?;
        }
    }
    let items = value["accounts"]
        .as_array()
        .filter(|items| (1..=1000).contains(&items.len()))
        .ok_or_else(Error::invalid)?;
    for item in items {
        let with_payload = matches!(action, "groups" | "delete");
        fields(
            item,
            if with_payload {
                &["account_id", "expected_version", "payload"]
            } else {
                &["account_id", "expected_version"]
            },
            &["account_id", "expected_version"],
        )?;
        if template {
            validation::coercible_integer(&item["account_id"], 1, i64::MAX)?;
            version(item, "expected_version")?;
        } else {
            integer(&item["account_id"], 1, i64::MAX)?;
            validation::hex(&item["expected_version"], 64)?;
        }
        if with_payload {
            if let Some(payload) = item.get("payload") {
                object(payload)?;
            }
        }
    }
    Ok(())
}

pub fn validate(path: &[&str], method: &str, value: &mut Value) -> Result<()> {
    if path.first() == Some(&"sub2api-config") {
        return crate::sub2api_config::validate_request(value);
    }
    if path.first() == Some(&"account-creation") {
        return crate::account_creation::validate(path, value);
    }
    if path.first() == Some(&"proxy-management") {
        return crate::proxy_management::validate_body(path, method, value);
    }
    match path {
        ["account-operations", "batch"] => batch(value, false)?,
        ["account-templates", "apply"] => batch(value, true)?,
        ["account-priorities"] if method == "PUT" => {
            crate::priority_orders::validate_parallelism_request(value)?
        }
        ["account-priority-orders"] => crate::priority_orders::validate_request(value)?,
        ["account-priority-orders", _, "cancel"] => fields(value, &[], &[])?,
        ["accounts", _, "operations"] => {
            fields(
                value,
                &[
                    "action",
                    "payload",
                    "expected_version",
                    "request_id",
                    "client_id",
                    "scheduled_at",
                    "repeat",
                ],
                &["action", "expected_version", "request_id", "client_id"],
            )?;
            choice(
                &value["action"],
                &[
                    "priority",
                    "groups",
                    "recover",
                    "usage",
                    "reset_quota",
                    "delete",
                    "test",
                    "schedulable",
                    "degradation_mark",
                    "account_template",
                    "concurrency",
                    "load_factor",
                    "model_config",
                    "proxy",
                    "sync_models",
                ],
            )?;
            version(value, "expected_version")?;
            for key in ["request_id", "client_id"] {
                validation::request_id(&value[key])?;
            }
            if let Some(at) = value.get("scheduled_at") {
                if value["action"] != "test" {
                    return Err(Error::invalid());
                }
                crate::operations::scheduled_time(at)?;
            }
            if let Some(repeat) = value.get("repeat") {
                choice(repeat, &["quota_cycle"])?;
                if value["action"] != "test" || value.get("scheduled_at").is_some() {
                    return Err(Error::invalid());
                }
            }
            if let Some(payload) = value.get("payload") {
                object(payload)?;
            }
        }
        ["accounts", _, "model-tests"] => {
            *value = crate::model_tests::model_request(value)?;
        }
        ["accounts", _, "model-detection"] => {
            *value = crate::model_detection::detection_request(value)?;
        }
        ["config", "key-quota"] => crate::key_quota::settings_request(value, false)?,
        ["accounts", _, "key-quota-settings"] => crate::key_quota::settings_request(value, true)?,
        ["accounts", _, "keepalive"] => crate::keepalive::request(value)?,
        ["model-tests", _, "retry"] => {
            fields(value, &["request_id"], &["request_id"])?;
            validation::request_id(&value["request_id"])?;
        }
        ["accounts", _, "priority"] => {
            fields(
                value,
                &["expected_version", "priority"],
                &["expected_version", "priority"],
            )?;
            version(value, "expected_version")?;
            integer(&value["priority"], 0, i32::MAX as i64)?;
        }
        ["accounts", _, "groups"] => {
            fields(
                value,
                &["expected_version", "scope_group_ids", "group_ids"],
                &["expected_version", "scope_group_ids", "group_ids"],
            )?;
            version(value, "expected_version")?;
            validation::ids(&value["scope_group_ids"], 1, 100)?;
            validation::ids(&value["group_ids"], 0, 100)?;
        }
        ["accounts", _, "degradation-mark"] => {
            fields(
                value,
                &["expected_mark_version", "marked"],
                &["expected_mark_version", "marked"],
            )?;
            version(value, "expected_mark_version")?;
            boolean(&value["marked"])?;
        }
        ["accounts", _, "schedulable"] => {
            fields(
                value,
                &["expected_version", "schedulable"],
                &["expected_version", "schedulable"],
            )?;
            version(value, "expected_version")?;
            boolean(&value["schedulable"])?;
        }
        ["accounts", _] => {
            fields(value, &["expected_version"], &["expected_version"])?;
            version(value, "expected_version")?;
        }
        ["accounts", _, "recover-state"] => {
            fields(value, &["expected_version"], &["expected_version"])?;
            version(value, "expected_version")?;
        }
        ["accounts", _, "usage-action"] => {
            fields(
                value,
                &["expected_version", "action", "confirmed"],
                &["expected_version", "action"],
            )?;
            version(value, "expected_version")?;
            choice(
                &value["action"],
                &[
                    "query_usage",
                    "query_reset_credits",
                    "reset_quota",
                    "probe_quota",
                ],
            )?;
            optional_bool(value, "confirmed")?;
        }
        ["accounts", _, "test"] => {
            fields(
                value,
                &[
                    "expected_version",
                    "confirmed",
                    "model_id",
                    "mode",
                    "prompt",
                    "image_data_url",
                    "audio_data_url",
                ],
                &["expected_version"],
            )?;
            version(value, "expected_version")?;
            for (key, default) in [
                ("confirmed", json!(false)),
                ("model_id", json!("")),
                ("mode", json!("default")),
                ("prompt", json!("")),
                ("image_data_url", json!("")),
                ("audio_data_url", json!("")),
            ] {
                if value.get(key).is_none() {
                    value[key] = default;
                }
            }
            boolean(&value["confirmed"])?;
            choice(&value["mode"], TEST_MODES)?;
            text(&value["model_id"], 0, 200)?;
            text(&value["prompt"], 0, 16000)?;
            for key in ["image_data_url", "audio_data_url"] {
                text(&value[key], 0, 16_777_216)?;
            }
        }
        ["config", _] => {
            fields(
                value,
                &["expected_revision", "changes"],
                &["expected_revision", "changes"],
            )?;
            version(value, "expected_revision")?;
            object(&value["changes"])?;
        }
        ["account-templates"] if method == "PUT" => {
            fields(
                value,
                &["expected_version", "full", "degraded", "takeover"],
                &["expected_version", "full", "degraded", "takeover"],
            )?;
            version(value, "expected_version")?;
            for key in ["full", "degraded", "takeover"] {
                fields(&value[key], &["whitelist", "mappings"], &[])?;
                profile(&value[key])?;
            }
        }
        ["account-templates", ..] if method == "DELETE" => {
            fields(value, &["expected_version"], &["expected_version"])?;
            version(value, "expected_version")?;
        }
        ["account-templates", ..] => {
            fields(
                value,
                &["expected_version", "name", "whitelist", "mappings"],
                &["expected_version", "name"],
            )?;
            version(value, "expected_version")?;
            text(&value["name"], 1, 40)?;
            profile(value)?;
        }
        ["model-groups", _, "reasoning", "resolve"] => reasoning(value, "resolve")?,
        ["model-groups", _, "reasoning"] => {
            reasoning(value, if method == "PUT" { "save" } else { "delete" })?
        }
        _ => {}
    }
    Ok(())
}

pub fn reasoning(value: &mut Value, kind: &str) -> Result<()> {
    let (allowed, required): (&[&str], &[&str]) = match kind {
        "resolve" => (&["model", "efforts", "default_effort"], &["model"]),
        "save" => (
            &[
                "model",
                "expected_version",
                "expected_revision",
                "expected_binding",
                "efforts",
                "default_effort",
                "confirm_allowlist",
            ],
            &[
                "model",
                "expected_version",
                "expected_revision",
                "expected_binding",
                "efforts",
                "default_effort",
            ],
        ),
        _ => (
            &["model", "expected_version", "expected_revision"],
            &["model", "expected_version", "expected_revision"],
        ),
    };
    fields(value, allowed, required)?;
    text(&value["model"], 1, 256)?;
    for key in ["expected_version", "expected_revision", "expected_binding"] {
        if required.contains(&key) {
            version(value, key)?;
        }
    }
    let efforts = [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ];
    if let Some(items) = value.get("efforts").filter(|v| !v.is_null()) {
        let items = items.as_array().ok_or_else(Error::invalid)?;
        if kind == "save" && !(1..=8).contains(&items.len()) {
            return Err(Error::invalid());
        }
        for item in items {
            choice(item, &efforts)?;
        }
    } else if kind == "save" {
        return Err(Error::invalid());
    }
    if kind == "save" {
        choice(&value["default_effort"], &efforts)?;
        if let Some(input) = value.get("confirm_allowlist") {
            value["confirm_allowlist"] = validation::coercible_boolean(input)?.into();
        }
    } else if kind == "resolve" {
        if value.get("default_effort").is_none() {
            value["default_effort"] = "".into();
        }
        text(&value["default_effort"], 0, usize::MAX)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn scheduled_connection_request_accepts_second_precision_only_for_test_operations() {
        use serde_json::json;
        let path = ["accounts", "42", "operations"];
        let base = json!({"action":"test","expected_version":"a".repeat(64),"request_id":"fixture-scheduled-request","client_id":"fixture-scheduled-client","payload":{"mode":"default"},"scheduled_at":"2030-01-02T09:31:00+08:00"});
        assert!(super::validate(&path, "POST", &mut base.clone()).is_ok());
        let mut seconds = base.clone();
        seconds["scheduled_at"] = "2030-01-02T09:31:38+08:00".into();
        assert!(super::validate(&path, "POST", &mut seconds).is_ok());
        for at in [
            "2030-01-02T09:31:01.001+08:00",
            "2030-02-30T09:31:00+08:00",
            "2030-01-02T09:31",
            "invalid",
        ] {
            let mut bad = base.clone();
            bad["scheduled_at"] = at.into();
            assert!(super::validate(&path, "POST", &mut bad).is_err());
        }
        let mut bad = base;
        bad["action"] = "delete".into();
        assert!(super::validate(&path, "POST", &mut bad).is_err());
    }
}
