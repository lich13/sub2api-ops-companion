//! Persisted timestamps and optimistic versions are shared across supported releases.
//! The original JSON whitespace and scalar representations are part of their digest.
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub type WallClock = std::sync::Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

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
    parse_datetime_text(text)
}

pub fn parse_datetime_text(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
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

struct HashWriter(Sha256);

impl std::io::Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_json_str<W: std::io::Write>(out: &mut W, value: &str, ascii: bool) {
    if !ascii {
        serde_json::to_writer(out, value).expect("string serialization");
        return;
    }
    let escaped = serde_json::to_string(value).expect("string serialization");
    if escaped.is_ascii() {
        out.write_all(escaped.as_bytes()).expect("in-memory write");
        return;
    }
    for ch in escaped.chars() {
        if (ch as u32) < 127 {
            let mut buf = [0u8; 4];
            out.write_all(ch.encode_utf8(&mut buf).as_bytes())
                .expect("in-memory write");
        } else {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                write!(out, "\\u{unit:04x}").expect("in-memory write");
            }
        }
    }
}

pub fn write_py_json<W: std::io::Write>(out: &mut W, value: &Value, ascii: bool, compact: bool) {
    let comma: &[u8] = if compact { b"," } else { b", " };
    let colon: &[u8] = if compact { b":" } else { b": " };
    match value {
        Value::String(v) => write_json_str(out, v, ascii),
        Value::Array(v) => {
            out.write_all(b"[").expect("in-memory write");
            for (idx, item) in v.iter().enumerate() {
                if idx > 0 {
                    out.write_all(comma).expect("in-memory write");
                }
                write_py_json(out, item, ascii, compact);
            }
            out.write_all(b"]").expect("in-memory write");
        }
        Value::Object(v) => {
            // Parsed objects retain the upstream order, which becomes visible
            // when model mappings are projected into arrays. Version digests
            // still follow Python's recursive sort_keys=True contract.
            let mut entries = v.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            out.write_all(b"{").expect("in-memory write");
            for (idx, (k, val)) in entries.into_iter().enumerate() {
                if idx > 0 {
                    out.write_all(comma).expect("in-memory write");
                }
                write_json_str(out, k, ascii);
                out.write_all(colon).expect("in-memory write");
                write_py_json(out, val, ascii, compact);
            }
            out.write_all(b"}").expect("in-memory write");
        }
        Value::Number(n) if n.is_f64() => {
            let v = n.as_f64().expect("float");
            if v != 0.0 && (v.abs() < 1e-4 || v.abs() >= 1e16) {
                let raw = format!("{v:e}");
                let (mantissa, exponent) = raw.split_once('e').expect("scientific notation");
                let exponent: i32 = exponent.parse().expect("exponent");
                write!(
                    out,
                    "{mantissa}e{}{:02}",
                    if exponent < 0 { "-" } else { "+" },
                    exponent.abs()
                )
                .expect("in-memory write");
            } else {
                let s = n.to_string();
                out.write_all(s.as_bytes()).expect("in-memory write");
                if !s.contains('.') {
                    out.write_all(b".0").expect("in-memory write");
                }
            }
        }
        Value::Bool(true) => out.write_all(b"true").expect("in-memory write"),
        Value::Bool(false) => out.write_all(b"false").expect("in-memory write"),
        Value::Null => out.write_all(b"null").expect("in-memory write"),
        Value::Number(n) => write!(out, "{n}").expect("in-memory write"),
    }
}

fn digest_py(write_fn: impl FnOnce(&mut HashWriter)) -> String {
    let mut writer = HashWriter(Sha256::new());
    write_fn(&mut writer);
    format!("{:x}", writer.0.finalize())
}

fn digest_pair(first: &Value, second: &Value) -> String {
    use std::io::Write;
    digest_py(|out| {
        out.write_all(b"[").unwrap();
        write_py_json(out, first, false, false);
        out.write_all(b", ").unwrap();
        write_py_json(out, second, false, false);
        out.write_all(b"]").unwrap();
    })
}

fn digest_slice(items: &[&Value], ascii: bool) -> String {
    use std::io::Write;
    digest_py(|out| {
        out.write_all(b"[").unwrap();
        for (idx, item) in items.iter().enumerate() {
            if idx > 0 {
                out.write_all(b", ").unwrap();
            }
            write_py_json(out, item, ascii, false);
        }
        out.write_all(b"]").unwrap();
    })
}

pub fn py_json(value: &Value, ascii: bool, compact: bool) -> String {
    let mut out = Vec::with_capacity(128);
    write_py_json(&mut out, value, ascii, compact);
    String::from_utf8(out).expect("valid utf-8 json")
}

pub fn sha256(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
pub fn digest(value: &Value) -> String {
    digest_py(|out| write_py_json(out, value, false, false))
}
pub fn config_revision(value: &Value) -> String {
    digest_py(|out| write_py_json(out, value, true, false))
}
pub fn pick(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .map(|key| ((*key).to_owned(), value[*key].clone()))
            .collect(),
    )
}

pub fn operation_versions(row: &Value) -> Value {
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
    let reset = Value::Object(
        extra
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, _)| k.contains("reset_credit") || k.contains("reset_quota"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    let mapping = if truthy(&row["model_mapping_version"]) {
        &row["model_mapping_version"]
    } else {
        &row["credential_version"]
    };
    let schedulable = Value::Array(vec![row["schedulable"].clone()]);
    let delete_val = Value::Array(vec![
        row["name"].clone(),
        row["status"].clone(),
        row["schedulable"].clone(),
        groups.clone(),
    ]);
    let reset_quota = Value::Array(vec![limits.clone(), reset]);
    let test_val = Value::Array(vec![groups.clone(), row["model_catalog_version"].clone()]);
    let tpl_val = Value::Array(vec![mapping.clone()]);
    let mut result = Map::with_capacity(15);
    for (k, v) in [
        ("schedulable", &schedulable),
        ("groups", &groups),
        ("recover", &limits),
        ("delete", &delete_val),
        ("usage", &Value::Null),
        ("reset_quota", &reset_quota),
        ("test", &test_val),
        ("model_test", &test_val),
        ("account_template", &tpl_val),
    ] {
        result.insert(k.to_owned(), digest_pair(&identity, v).into());
    }
    let mut stable = pick(row, &["id", "platform", "type", "parent_account_id"]);
    if !truthy(&stable["parent_account_id"]) {
        stable["parent_account_id"] = extra["parent_account_id"].clone();
    }
    let zero = json!(0);
    for field in ["priority", "concurrency", "load_factor"] {
        let fallback = if field == "priority" {
            &zero
        } else {
            &Value::Null
        };
        let value = row.get(field).unwrap_or(fallback);
        result.insert(field.to_owned(), digest_pair(&stable, value).into());
    }
    result.insert(
        "proxy".to_owned(),
        digest_pair(&stable, &row["proxy_id"]).into(),
    );
    let guard = Value::Array(vec![
        stable,
        row["credential_settings_version"].clone(),
        pick(extra, &["openai_passthrough", "openai_oauth_passthrough"]),
    ]);
    let empty_obj = Value::Object(Map::new());
    let model_mapping = [&row["model_mapping_version"], &row["model_mapping"]]
        .into_iter()
        .find(|v| truthy(v))
        .unwrap_or(&empty_obj);
    result.insert(
        "model_config".to_owned(),
        digest_pair(&guard, model_mapping).into(),
    );
    result.insert(
        "sync_models".to_owned(),
        digest_slice(&[&identity, &guard, &row["model_sync_project"]], false).into(),
    );
    Value::Object(result)
}

pub fn priority_version(row: &Value) -> String {
    let mut stable = pick(row, &["id", "platform", "type", "parent_account_id"]);
    if !truthy(&stable["parent_account_id"]) {
        stable["parent_account_id"] = row["extra"]["parent_account_id"].clone();
    }
    let fallback = json!(0);
    let value = row.get("priority").unwrap_or(&fallback);
    digest_pair(&stable, value)
}

pub fn account_version(row: &Value) -> String {
    let mut groups = row["group_ids"].as_array().cloned().unwrap_or_default();
    sort_groups(&mut groups);
    let groups = Value::Array(groups);
    digest_slice(
        &[
            &row["id"],
            &row["platform"],
            &row["type"],
            &row["schedulable"],
            &row["updated_at"],
            &row["priority"],
            &groups,
        ],
        true,
    )
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
