//! Serialization at the persisted Python/Rust boundary. Do not use a compact
//! serializer for optimistic versions: whitespace is part of their digest.
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub fn now() -> String {
    datetime(Utc::now())
}

pub fn datetime(value: DateTime<Utc>) -> String {
    let format = if value.timestamp_subsec_micros() == 0 {
        SecondsFormat::Secs
    } else {
        SecondsFormat::Micros
    };
    value.to_rfc3339_opts(format, false)
}

pub fn parse_datetime(value: &Value) -> Option<DateTime<Utc>> {
    let owned;
    let text = if let Some(text) = value.as_str() {
        text.trim()
    } else if value.is_number() {
        owned = value.to_string();
        &owned
    } else {
        return None;
    };
    if let Ok(mut epoch) = text.parse::<f64>() {
        if !epoch.is_finite() {
            return None;
        }
        if epoch.abs() >= 100_000_000_000.0 {
            epoch /= 1000.0;
        }
        let micros = (epoch * 1_000_000.0).round_ties_even();
        if micros < i64::MIN as f64 || micros > i64::MAX as f64 {
            return None;
        }
        return DateTime::from_timestamp_micros(micros as i64)
            .filter(|value| (1..=9999).contains(&value.year()));
    }
    DateTime::parse_from_rfc3339(&text.replacen(' ', "T", 1))
        .map(|v| v.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|v| v.and_utc())
        })
        .or_else(|| {
            NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|v| v.and_utc())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|v| v.and_utc())
        })
        // Python datetime rejects UTC values outside this range, including a
        // valid local timestamp whose offset crosses the start of year 1.
        .filter(|value| (1..=9999).contains(&value.year()))
}

pub fn future(value: &Value, at: DateTime<Utc>) -> bool {
    parse_datetime(value).is_some_and(|v| v > at)
}

pub fn text(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::String(v) => v.clone(),
        _ => py_json(value, false, false),
    }
}

pub fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
pub fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}
pub fn float(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|v| v != 0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}

fn json_string(value: &str, ascii: bool) -> String {
    let result = serde_json::to_string(value).expect("string serialization");
    if !ascii {
        return result;
    }
    let mut out = String::new();
    for ch in result.chars() {
        if (ch as u32) < 127 {
            out.push(ch);
        } else {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

pub fn py_json(value: &Value, ascii: bool, compact: bool) -> String {
    let comma = if compact { "," } else { ", " };
    let colon = if compact { ":" } else { ": " };
    match value {
        Value::String(v) => json_string(v, ascii),
        Value::Array(v) => format!(
            "[{}]",
            v.iter()
                .map(|v| py_json(v, ascii, compact))
                .collect::<Vec<_>>()
                .join(comma)
        ),
        Value::Object(v) => {
            // Parsed objects retain the upstream order, which becomes visible
            // when model mappings are projected into arrays. Version digests
            // still follow Python's recursive sort_keys=True contract.
            let mut entries = v.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(k, v)| format!(
                        "{}{colon}{}",
                        json_string(k, ascii),
                        py_json(v, ascii, compact)
                    ))
                    .collect::<Vec<_>>()
                    .join(comma)
            )
        }
        Value::Number(n) if n.is_f64() => {
            let v = n.as_f64().expect("float");
            if v != 0.0 && (v.abs() < 1e-4 || v.abs() >= 1e16) {
                let raw = format!("{v:e}");
                let (mantissa, exponent) = raw.split_once('e').expect("scientific notation");
                let exponent: i32 = exponent.parse().expect("exponent");
                format!(
                    "{mantissa}e{}{:02}",
                    if exponent < 0 { "-" } else { "+" },
                    exponent.abs()
                )
            } else {
                let s = n.to_string();
                if s.contains('.') {
                    s
                } else {
                    format!("{s}.0")
                }
            }
        }
        _ => value.to_string(),
    }
}

pub fn sha256(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
pub fn digest(value: &Value) -> String {
    sha256(py_json(value, false, false))
}
pub fn config_revision(value: &Value) -> String {
    sha256(py_json(value, true, false))
}
pub fn pick(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .map(|key| ((*key).to_owned(), value[*key].clone()))
            .collect(),
    )
}

pub fn operation_versions(row: &Value, managed: bool) -> Value {
    let identity = pick(
        row,
        &[
            "id",
            "platform",
            "type",
            "parent_account_id",
            "credential_version",
            "proxy_id",
        ],
    );
    let mut groups = row["group_ids"].as_array().cloned().unwrap_or_default();
    sort_groups(&mut groups);
    let groups = Value::Array(groups);
    let limits = pick(
        row,
        &[
            "status",
            "error_message",
            "rate_limit_reset_at",
            "overload_until",
            "temp_unschedulable_until",
        ],
    );
    let extra = &row["extra"];
    let reset: Map<String, Value> = extra
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| k.contains("reset_credit") || k.contains("reset_quota"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mapping = if truthy(&row["model_mapping_version"]) {
        row["model_mapping_version"].clone()
    } else {
        row["credential_version"].clone()
    };
    let values = json!({
        "schedulable":[row["schedulable"],managed],
        "groups":groups,
        "recover":limits,
        "delete":[row["name"],row["status"],row["schedulable"],groups,managed],
        "usage":null,
        "reset_quota":[limits,reset],
        "test":[groups,row["model_catalog_version"]],
        "model_test":[groups,row["model_catalog_version"]],
        "account_template":[mapping]
    });
    let mut result: Map<String, Value> = values
        .as_object()
        .expect("object")
        .iter()
        .map(|(k, v)| (k.clone(), digest(&json!([identity, v])).into()))
        .collect();
    let mut stable = pick(row, &["id", "platform", "type", "parent_account_id"]);
    if !truthy(&stable["parent_account_id"]) {
        stable["parent_account_id"] = extra["parent_account_id"].clone();
    }
    for field in ["priority", "concurrency", "load_factor"] {
        let fallback = if field == "priority" {
            json!(0)
        } else {
            Value::Null
        };
        let value = row.get(field).unwrap_or(&fallback);
        result.insert(field.to_owned(), digest(&json!([stable, value])).into());
    }
    result.insert(
        "proxy".to_owned(),
        digest(&json!([stable, row["proxy_id"]])).into(),
    );
    let guard = json!([
        stable,
        row["credential_settings_version"],
        pick(extra, &["openai_passthrough", "openai_oauth_passthrough"])
    ]);
    let model_mapping = [&row["model_mapping_version"], &row["model_mapping"]]
        .into_iter()
        .find(|v| truthy(v))
        .cloned()
        .unwrap_or(json!({}));
    result.insert(
        "model_config".to_owned(),
        digest(&json!([guard, model_mapping])).into(),
    );
    result.insert(
        "sync_models".to_owned(),
        digest(&json!([identity, guard, row["model_sync_project"]])).into(),
    );
    Value::Object(result)
}

pub fn account_version(row: &Value) -> String {
    let mut groups = row["group_ids"].as_array().cloned().unwrap_or_default();
    sort_groups(&mut groups);
    sha256(py_json(
        &json!([
            row["id"],
            row["platform"],
            row["type"],
            row["schedulable"],
            row["updated_at"],
            row["priority"],
            groups
        ]),
        true,
        false,
    ))
}

fn sort_groups(groups: &mut [Value]) {
    groups.sort_by(|a, b| match (a, b) {
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => a.as_i64().cmp(&b.as_i64()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_json_separators_unicode_and_float_boundaries() {
        assert_eq!(
            py_json(&json!({"你好":[1,true,null,"😀"]}), false, false),
            "{\"你好\": [1, true, null, \"😀\"]}"
        );
        assert_eq!(py_json(&json!("😀"), true, false), "\"\\ud83d\\ude00\"");
        assert_eq!(py_json(&json!(1.0), false, false), "1.0");
        assert_eq!(py_json(&json!(1e-5), false, false), "1e-05");
        assert_eq!(py_json(&json!(1e16), false, false), "1e+16");
    }
}
