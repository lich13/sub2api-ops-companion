use crate::{
    account_templates::profile,
    actions::TEST_MODES,
    compat::string,
    error::{Error, Result},
};
use serde_json::{json, Value};

pub fn fields(value: &Value, allowed: &[&str], required: &[&str]) -> Result<()> {
    let object = value.as_object().ok_or_else(Error::invalid)?;
    if object.keys().any(|k| !allowed.contains(&k.as_str()))
        || required.iter().any(|k| !object.contains_key(*k))
    {
        return Err(Error::invalid());
    }
    Ok(())
}
pub fn text(value: &Value, min: usize, max: usize) -> Result<&str> {
    value
        .as_str()
        .filter(|v| (min..=max).contains(&v.chars().count()))
        .ok_or_else(Error::invalid)
}
pub fn boolean(value: &Value) -> Result<bool> {
    value.as_bool().ok_or_else(Error::invalid)
}
pub fn coercible_boolean(value: &Value) -> Result<bool> {
    if let Some(value) = value.as_bool() {
        return Ok(value);
    }
    if let Some(value) = value.as_f64() {
        return match value {
            0.0 => Ok(false),
            1.0 => Ok(true),
            _ => Err(Error::invalid()),
        };
    }
    match value.as_str().map(str::to_ascii_lowercase).as_deref() {
        Some("false" | "0" | "off" | "no" | "f" | "n") => Ok(false),
        Some("true" | "1" | "on" | "yes" | "t" | "y") => Ok(true),
        _ => Err(Error::invalid()),
    }
}
pub fn integer(value: &Value, min: i64, max: i64) -> Result<i64> {
    value
        .as_i64()
        .filter(|v| (min..=max).contains(v))
        .ok_or_else(Error::invalid)
}
/// Match the non-strict Pydantic integer fields within PostgreSQL's ID range.
pub fn coercible_integer(value: &Value, min: i64, max: i64) -> Result<i64> {
    static INTEGER_TEXT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[+-]?[0-9](?:_?[0-9])*(?:\.0+)?$").unwrap()
    });
    let number = if let Some(value) = value.as_i64() {
        Some(value)
    } else if let Some(value) = value.as_bool() {
        Some(i64::from(value))
    } else if let Some(value) = value.as_f64() {
        (value.is_finite()
            && value.fract() == 0.0
            && value >= i64::MIN as f64
            && value < -(i64::MIN as f64))
            .then_some(value as i64)
    } else if let Some(value) = value.as_str().map(str::trim) {
        INTEGER_TEXT
            .is_match(value)
            .then(|| {
                value
                    .split('.')
                    .next()
                    .unwrap()
                    .replace('_', "")
                    .parse::<i64>()
                    .ok()
            })
            .flatten()
    } else {
        None
    };
    number
        .filter(|value| (min..=max).contains(value))
        .ok_or_else(Error::invalid)
}
pub fn request_id(value: &Value) -> Result<&str> {
    let id = text(value, 16, 64)?;
    if !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(Error::invalid());
    }
    Ok(id)
}
pub fn hex(value: &Value, len: usize) -> Result<&str> {
    let id = text(value, len, len)?;
    if !id
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::invalid());
    }
    Ok(id)
}
pub fn ids(value: &Value, min: usize, max: usize) -> Result<Vec<i64>> {
    let values = value
        .as_array()
        .filter(|v| (min..=max).contains(&v.len()))
        .ok_or_else(Error::invalid)?;
    values
        .iter()
        .map(|v| integer(v, i64::MIN, i64::MAX))
        .collect()
}
pub fn operation_payload(action: &str, payload: &Value) -> Result<Value> {
    let mut value = payload.clone();
    let (allowed, required): (&[&str], &[&str]) = match action {
        "priority" => (&["priority"], &["priority"]),
        "concurrency" => (&["concurrency"], &["concurrency"]),
        "load_factor" => (&["load_factor"], &["load_factor"]),
        "proxy" => (&["proxy_id"], &["proxy_id"]),
        "sync_models" | "recover" => (&[], &[]),
        "model_config" => (&["whitelist", "mappings"], &[]),
        "account_template" => (
            &["template_id", "template_version"],
            &["template_id", "template_version"],
        ),
        "groups" => (
            &["scope_group_ids", "group_ids"],
            &["scope_group_ids", "group_ids"],
        ),
        "test" => (
            &[
                "confirmed",
                "model_id",
                "mode",
                "prompt",
                "image_data_url",
                "audio_data_url",
            ],
            &[],
        ),
        "schedulable" => (&["schedulable"], &["schedulable"]),
        "delete" => (&[], &[]),
        "usage" | "reset_quota" => (&["action", "confirmed"], &["action"]),
        "degradation_mark" => (&["marked"], &["marked"]),
        _ => return Err(Error::invalid()),
    };
    // The prior request model accepts a version inside payload, but overwrites it
    // with the queue's field-scoped version before validation and persistence.
    if let Some(object) = value.as_object_mut() {
        object.shift_remove(if action == "degradation_mark" {
            "expected_mark_version"
        } else {
            "expected_version"
        });
    }
    fields(&value, allowed, required)?;
    match action {
        "priority" => {
            integer(&value["priority"], 0, i32::MAX as i64)?;
        }
        "concurrency" => {
            integer(&value["concurrency"], 1, i32::MAX as i64)?;
        }
        "load_factor" => {
            if !value["load_factor"].is_null() {
                integer(&value["load_factor"], 1, 10000)?;
            }
        }
        "proxy" => {
            if !value["proxy_id"].is_null() {
                integer(&value["proxy_id"], 1, i64::MAX)?;
            }
        }
        "model_config" => value = profile(&value)?,
        "groups" => {
            ids(&value["scope_group_ids"], 1, 100)?;
            ids(&value["group_ids"], 0, 100)?;
        }
        "account_template" => {
            let id = text(&value["template_id"], 4, 31)?;
            if !(matches!(id, "full" | "degraded" | "takeover")
                || id.starts_with("custom-") && hex(&json!(&id[7..]), 24).is_ok())
            {
                return Err(Error::invalid());
            }
            text(&value["template_version"], 64, 64)?;
        }
        "test" => {
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
            text(&value["model_id"], 0, 200)?;
            text(&value["prompt"], 0, 16000)?;
            for field in ["image_data_url", "audio_data_url"] {
                text(&value[field], 0, 16_777_216)?;
            }
            if !TEST_MODES.contains(&string(&value["mode"])) {
                return Err(Error::invalid());
            }
            if value["confirmed"] != true {
                return Err(Error::new(422, "请先确认该操作"));
            }
        }
        "schedulable" | "delete" => {
            if action == "schedulable" {
                boolean(&value["schedulable"])?;
            }
        }
        "usage" | "reset_quota" => {
            if value.get("confirmed").is_none() {
                value["confirmed"] = false.into();
            }
            boolean(&value["confirmed"])?;
            if !matches!(
                string(&value["action"]),
                "query_usage" | "query_reset_credits" | "reset_quota" | "probe_quota"
            ) {
                return Err(Error::invalid());
            }
            if action == "reset_quota" && value["action"] != "reset_quota" {
                return Err(Error::new(422, "用卡请求无效"));
            }
            if action == "usage" && value["action"] == "reset_quota" {
                return Err(Error::new(422, "用卡需要独立确认"));
            }
            if action == "reset_quota" && value["confirmed"] != true {
                return Err(Error::new(422, "请先确认该操作"));
            }
        }
        "degradation_mark" => {
            boolean(&value["marked"])?;
        }
        _ => {}
    }
    Ok(value)
}
