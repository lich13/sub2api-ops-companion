//! HTTP shape validation, before authentication or any domain access.
//! Business decisions stay in the authenticated handlers.
use crate::{
    compat::string,
    error::{Error, Result},
    validation::{self, boolean, fields, integer_in_range, text},
};
use serde_json::{json, Value};
use sub2ops_protocol::routes::RequestBody;

fn version(value: &Value, name: &str) -> Result<()> {
    text(&value[name], 64, 64).map(|_| ())
}
fn object(value: &Value) -> Result<()> {
    value.as_object().map(|_| ()).ok_or_else(Error::invalid)
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
    if sub2ops_protocol::semantics::valid_template_id(id) {
        Ok(())
    } else {
        Err(Error::invalid())
    }
}
pub fn batch(value: &mut Value, template: bool) -> Result<()> {
    let action = if template {
        "account_template"
    } else {
        string(&value["action"])
    }
    .to_owned();
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
        if matches!(action.as_str(), "delete" | "groups" | "recover")
            && value.get("payload").is_none()
        {
            value["payload"] = json!({});
        }
        match action.as_str() {
            "test" => {
                fields(&value["payload"], &["confirmed"], &["confirmed"])?;
                boolean(&value["payload"]["confirmed"])?;
            }
            "proxy" => {
                fields(&value["payload"], &["proxy_id"], &["proxy_id"])?;
                if !value["payload"]["proxy_id"].is_null() {
                    integer_in_range(&value["payload"]["proxy_id"], 1, i64::MAX)?;
                }
            }
            "recover" => {
                if let Some(payload) = value.get("payload") {
                    fields(payload, &[], &[])?;
                }
            }
            _ => object(&value["payload"])?,
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
        .as_array_mut()
        .filter(|items| (1..=1000).contains(&items.len()))
        .ok_or_else(Error::invalid)?;
    for item in items {
        let with_payload = matches!(action.as_str(), "groups" | "delete");
        fields(
            item,
            if with_payload {
                &["account_id", "expected_version", "payload"]
            } else {
                &["account_id", "expected_version"]
            },
            &["account_id", "expected_version"],
        )?;
        let id = if template {
            version(item, "expected_version")?;
            validation::coercible_integer(&item["account_id"], 1, i64::MAX)?
        } else {
            validation::hex(&item["expected_version"], 64)?;
            integer_in_range(&item["account_id"], 1, i64::MAX)?
        };
        item["account_id"] = id.into();
        if with_payload {
            if let Some(payload) = item.get("payload") {
                object(payload)?;
            } else {
                item["payload"] = json!({});
            }
        }
    }
    Ok(())
}

pub struct Submission<'a> {
    pub(crate) action: &'a str,
    pub(crate) rid: &'a str,
    pub(crate) client: &'a str,
    pub(crate) expected: &'a str,
    pub(crate) payload: Option<&'a Value>,
    pub(crate) scheduled: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) repeat: Option<&'a Value>,
}

pub fn submission(value: &Value) -> Result<Submission<'_>> {
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
    let action = string(&value["action"]);
    let scheduled = value
        .get("scheduled_at")
        .map(crate::operations::scheduled_time)
        .transpose()?;
    let repeat = value.get("repeat");
    if (repeat.is_some() || scheduled.is_some()) && action != "test"
        || repeat.is_some_and(|v| v != "quota_cycle" || scheduled.is_some())
    {
        return Err(Error::invalid());
    }
    let payload = value.get("payload");
    if let Some(payload) = payload {
        object(payload)?;
    }
    Ok(Submission {
        action,
        expected: text(&value["expected_version"], 64, 64)?,
        rid: validation::request_id(&value["request_id"])?,
        client: validation::request_id(&value["client_id"])?,
        scheduled,
        repeat,
        payload,
    })
}

pub fn validate(kind: RequestBody, path: &[&str], method: &str, value: &mut Value) -> Result<()> {
    match kind {
        RequestBody::None => {}
        RequestBody::HostConfig => crate::sub2api_config::validate_request(value)?,
        RequestBody::Creation => crate::account_creation::validate(path, value)?,
        RequestBody::Proxy => crate::proxy_management::validate_body(path, method, value)?,
        RequestBody::Credentials => crate::oauth_reauthorization::validate(value)?,
        RequestBody::Batch => batch(value, false)?,
        RequestBody::TemplateBatch => batch(value, true)?,
        RequestBody::Parallelism => crate::priority_orders::validate_parallelism_request(value)?,
        RequestBody::PriorityOrder => crate::priority_orders::validate_request(value)?,
        RequestBody::CancelOrder => fields(value, &[], &[])?,
        // The dispatch future captures the parsed submission before authentication.
        RequestBody::Operation => {}
        RequestBody::ModelTest => {
            *value = crate::model_tests::model_request(value)?;
        }
        RequestBody::Detection => {
            *value = crate::model_detection::detection_request(value)?;
        }
        RequestBody::GlobalQuota => crate::key_quota::settings_request(value, false)?,
        RequestBody::AccountQuota => crate::key_quota::settings_request(value, true)?,
        RequestBody::Keepalive => crate::keepalive::request(value)?,
        RequestBody::Retry => {
            fields(value, &["request_id"], &["request_id"])?;
            validation::request_id(&value["request_id"])?;
        }
        RequestBody::Config => {
            let body: sub2ops_protocol::messages::ConfigUpdateBody =
                serde::Deserialize::deserialize(&*value).map_err(|_| Error::invalid())?;
            if body.expected_revision.chars().count() != 64 {
                return Err(Error::invalid());
            }
        }
        RequestBody::DeleteTemplate => {
            fields(value, &["expected_version"], &["expected_version"])?;
            version(value, "expected_version")?;
        }
        RequestBody::Template => {
            fields(
                value,
                &["expected_version", "name", "whitelist", "mappings"],
                &["expected_version", "name"],
            )?;
            version(value, "expected_version")?;
            text(&value["name"], 1, 40)?;
            profile(value)?;
            for field in ["whitelist", "mappings"] {
                if value.get(field).is_none() {
                    value[field] = json!([]);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn scheduled_connection_request_accepts_second_precision_only_for_test_operations() {
        use serde_json::json;
        let base = json!({"action":"test","expected_version":"a".repeat(64),"request_id":"fixture-scheduled-request","client_id":"fixture-scheduled-client","payload":{"mode":"default"},"scheduled_at":"2030-01-02T09:31:00+08:00"});
        assert!(super::submission(&base).is_ok());
        let mut seconds = base.clone();
        seconds["scheduled_at"] = "2030-01-02T09:31:38+08:00".into();
        assert!(super::submission(&seconds).is_ok());
        for at in [
            "2030-01-02T09:31:01.001+08:00",
            "2030-02-30T09:31:00+08:00",
            "2030-01-02T09:31",
            "invalid",
        ] {
            let mut bad = base.clone();
            bad["scheduled_at"] = at.into();
            assert!(super::submission(&bad).is_err());
        }
        let mut bad = base;
        bad["action"] = "delete".into();
        assert!(super::submission(&bad).is_err());
    }
}
