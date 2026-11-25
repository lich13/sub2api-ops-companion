//! Persisted timestamps and optimistic versions are shared across supported releases.
//! The original JSON whitespace and scalar representations are part of their digest.
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};
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

#[cfg(test)]
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

#[cfg(test)]
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

fn write_sorted_picked<W: std::io::Write>(out: &mut W, entries: &[(&str, &Value)], ascii: bool) {
    out.write_all(b"{").unwrap();
    for (idx, (k, v)) in entries.iter().enumerate() {
        if idx > 0 {
            out.write_all(b", ").unwrap();
        }
        write_json_str(out, k, ascii);
        out.write_all(b": ").unwrap();
        write_py_json(out, v, ascii, false);
    }
    out.write_all(b"}").unwrap();
}

fn write_sorted_group_refs<W: std::io::Write>(out: &mut W, row: &Value, ascii: bool) {
    out.write_all(b"[").unwrap();
    if let Some(items) = row["group_ids"].as_array() {
        let mut refs: Vec<&Value> = items.iter().collect();
        sort_group_refs(&mut refs);
        for (idx, item) in refs.into_iter().enumerate() {
            if idx > 0 {
                out.write_all(b", ").unwrap();
            }
            write_py_json(out, item, ascii, false);
        }
    }
    out.write_all(b"]").unwrap();
}

fn digest_raw_pair(first_bytes: &[u8], write_second: impl FnOnce(&mut HashWriter)) -> String {
    use std::io::Write;
    digest_py(|out| {
        out.write_all(b"[").unwrap();
        out.write_all(first_bytes).unwrap();
        out.write_all(b", ").unwrap();
        write_second(out);
        out.write_all(b"]").unwrap();
    })
}

pub fn operation_versions(row: &Value) -> Value {
    use std::io::Write;
    let extra = &row["extra"];
    let mut identity_bytes = Vec::with_capacity(192);
    write_sorted_picked(
        &mut identity_bytes,
        &[
            ("credential_version", &row["credential_version"]),
            ("id", &row["id"]),
            ("parent_account_id", &row["parent_account_id"]),
            ("platform", &row["platform"]),
            ("proxy_id", &row["proxy_id"]),
            ("type", &row["type"]),
        ],
        false,
    );
    let mut groups_bytes = Vec::with_capacity(32);
    write_sorted_group_refs(&mut groups_bytes, row, false);

    let mut limits_bytes = Vec::with_capacity(160);
    write_sorted_picked(
        &mut limits_bytes,
        &[
            ("error_message", &row["error_message"]),
            ("overload_until", &row["overload_until"]),
            ("rate_limit_reset_at", &row["rate_limit_reset_at"]),
            ("status", &row["status"]),
            ("temp_unschedulable_until", &row["temp_unschedulable_until"]),
        ],
        false,
    );

    let mapping = if truthy(&row["model_mapping_version"]) {
        &row["model_mapping_version"]
    } else {
        &row["credential_version"]
    };

    let mut result = Map::with_capacity(15);
    result.insert(
        "schedulable".to_owned(),
        digest_raw_pair(&identity_bytes, |out| {
            out.write_all(b"[").unwrap();
            write_py_json(out, &row["schedulable"], false, false);
            out.write_all(b"]").unwrap();
        })
        .into(),
    );
    result.insert(
        "groups".to_owned(),
        digest_raw_pair(&identity_bytes, |out| {
            out.write_all(&groups_bytes).unwrap();
        })
        .into(),
    );
    result.insert(
        "recover".to_owned(),
        digest_raw_pair(&identity_bytes, |out| {
            out.write_all(&limits_bytes).unwrap();
        })
        .into(),
    );
    result.insert(
        "delete".to_owned(),
        digest_raw_pair(&identity_bytes, |out| {
            out.write_all(b"[").unwrap();
            write_py_json(out, &row["name"], false, false);
            out.write_all(b", ").unwrap();
            write_py_json(out, &row["status"], false, false);
            out.write_all(b", ").unwrap();
            write_py_json(out, &row["schedulable"], false, false);
            out.write_all(b", ").unwrap();
            out.write_all(&groups_bytes).unwrap();
            out.write_all(b"]").unwrap();
        })
        .into(),
    );
    result.insert(
        "usage".to_owned(),
        digest_raw_pair(&identity_bytes, |out| {
            out.write_all(b"null").unwrap();
        })
        .into(),
    );
    result.insert(
        "reset_quota".to_owned(),
        digest_raw_pair(&identity_bytes, |out| {
            out.write_all(b"[").unwrap();
            out.write_all(&limits_bytes).unwrap();
            out.write_all(b", ").unwrap();
            let mut reset_entries: Vec<(&str, &Value)> = extra
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, _)| k.contains("reset_credit") || k.contains("reset_quota"))
                .map(|(k, v)| (k.as_str(), v))
                .collect();
            reset_entries.sort_by(|a, b| a.0.cmp(b.0));
            write_sorted_picked(out, &reset_entries, false);
            out.write_all(b"]").unwrap();
        })
        .into(),
    );
    let test_digest = digest_raw_pair(&identity_bytes, |out| {
        out.write_all(b"[").unwrap();
        out.write_all(&groups_bytes).unwrap();
        out.write_all(b", ").unwrap();
        write_py_json(out, &row["model_catalog_version"], false, false);
        out.write_all(b"]").unwrap();
    });
    result.insert("test".to_owned(), test_digest.clone().into());
    result.insert("model_test".to_owned(), test_digest.into());
    result.insert(
        "account_template".to_owned(),
        digest_raw_pair(&identity_bytes, |out| {
            out.write_all(b"[").unwrap();
            write_py_json(out, mapping, false, false);
            out.write_all(b"]").unwrap();
        })
        .into(),
    );

    let parent_id = if truthy(&row["parent_account_id"]) {
        &row["parent_account_id"]
    } else {
        &extra["parent_account_id"]
    };
    let mut stable_bytes = Vec::with_capacity(128);
    write_sorted_picked(
        &mut stable_bytes,
        &[
            ("id", &row["id"]),
            ("parent_account_id", parent_id),
            ("platform", &row["platform"]),
            ("type", &row["type"]),
        ],
        false,
    );
    let zero = Value::Number(0.into());
    for field in ["priority", "concurrency", "load_factor"] {
        let fallback = if field == "priority" {
            &zero
        } else {
            &Value::Null
        };
        let value = row.get(field).unwrap_or(fallback);
        result.insert(
            field.to_owned(),
            digest_raw_pair(&stable_bytes, |out| write_py_json(out, value, false, false)).into(),
        );
    }
    result.insert(
        "proxy".to_owned(),
        digest_raw_pair(&stable_bytes, |out| {
            write_py_json(out, &row["proxy_id"], false, false)
        })
        .into(),
    );
    let mut guard_bytes = Vec::with_capacity(stable_bytes.len() + 128);
    guard_bytes.extend_from_slice(b"[");
    guard_bytes.extend_from_slice(&stable_bytes);
    guard_bytes.extend_from_slice(b", ");
    write_py_json(
        &mut guard_bytes,
        &row["credential_settings_version"],
        false,
        false,
    );
    guard_bytes.extend_from_slice(b", ");
    write_sorted_picked(
        &mut guard_bytes,
        &[
            (
                "openai_oauth_passthrough",
                &extra["openai_oauth_passthrough"],
            ),
            ("openai_passthrough", &extra["openai_passthrough"]),
        ],
        false,
    );
    guard_bytes.extend_from_slice(b"]");

    let model_mapping = [&row["model_mapping_version"], &row["model_mapping"]]
        .into_iter()
        .find(|v| truthy(v));
    result.insert(
        "model_config".to_owned(),
        digest_raw_pair(&guard_bytes, |out| match model_mapping {
            Some(m) => write_py_json(out, m, false, false),
            None => out.write_all(b"{}").unwrap(),
        })
        .into(),
    );
    result.insert(
        "sync_models".to_owned(),
        digest_py(|out| {
            out.write_all(b"[").unwrap();
            out.write_all(&identity_bytes).unwrap();
            out.write_all(b", ").unwrap();
            out.write_all(&guard_bytes).unwrap();
            out.write_all(b", ").unwrap();
            write_py_json(out, &row["model_sync_project"], false, false);
            out.write_all(b"]").unwrap();
        })
        .into(),
    );
    Value::Object(result)
}

pub fn priority_version(row: &Value) -> String {
    let parent_id = if truthy(&row["parent_account_id"]) {
        &row["parent_account_id"]
    } else {
        &row["extra"]["parent_account_id"]
    };
    let zero = Value::Number(0.into());
    let value = row.get("priority").unwrap_or(&zero);
    digest_py(|out| {
        use std::io::Write;
        out.write_all(b"[").unwrap();
        write_sorted_picked(
            out,
            &[
                ("id", &row["id"]),
                ("parent_account_id", parent_id),
                ("platform", &row["platform"]),
                ("type", &row["type"]),
            ],
            false,
        );
        out.write_all(b", ").unwrap();
        write_py_json(out, value, false, false);
        out.write_all(b"]").unwrap();
    })
}

pub fn account_version(row: &Value) -> String {
    digest_py(|out| {
        use std::io::Write;
        out.write_all(b"[").unwrap();
        for item in [
            &row["id"],
            &row["platform"],
            &row["type"],
            &row["schedulable"],
            &row["updated_at"],
            &row["priority"],
        ] {
            write_py_json(out, item, true, false);
            out.write_all(b", ").unwrap();
        }
        write_sorted_group_refs(out, row, true);
        out.write_all(b"]").unwrap();
    })
}

fn sort_group_refs(groups: &mut [&Value]) {
    groups.sort_by(|a, b| match (a, b) {
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => a.as_i64().cmp(&b.as_i64()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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

    #[test]
    fn pre_serialized_operation_and_account_versions_match_reference_digests() {
        let cases = [
            json!({}),
            json!({
                "id": 42,
                "name": "Codex · 主力 😀",
                "platform": "openai",
                "type": "oauth",
                "status": "active",
                "schedulable": true,
                "priority": 3,
                "concurrency": 5,
                "load_factor": 1,
                "proxy_id": 9,
                "parent_account_id": null,
                "credential_version": "cred-v1",
                "credential_settings_version": "cset-v2",
                "model_mapping_version": "map-v3",
                "model_catalog_version": "cat-v4",
                "model_sync_project": "proj-x",
                "updated_at": "2026-10-01T08:00:00Z",
                "group_ids": [9, 2, 5],
                "extra": {
                    "parent_account_id": 7,
                    "reset_quota_b": 2,
                    "reset_credit_a": 1,
                    "openai_passthrough": true,
                    "openai_oauth_passthrough": false,
                    "unrelated": "ignored"
                }
            }),
            json!({
                "id": 99,
                "platform": "anthropic",
                "type": "apikey",
                "parent_account_id": 12,
                "group_ids": ["z", "a"],
                "model_mapping": {"gpt-5": "claude"},
                "extra": {}
            }),
        ];
        for row in &cases {
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
            groups.sort_by(|a, b| match (a, b) {
                (Value::String(a), Value::String(b)) => a.cmp(b),
                _ => a.as_i64().cmp(&b.as_i64()),
            });
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
            let mut expected = Map::new();
            let schedulable = json!([row["schedulable"]]);
            let delete_val = json!([row["name"], row["status"], row["schedulable"], groups]);
            let reset_quota = json!([limits, reset]);
            let test_val = json!([groups, row["model_catalog_version"]]);
            let tpl_val = json!([mapping]);
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
                expected.insert(k.to_owned(), digest_pair(&identity, v).into());
            }
            let mut stable = pick(row, &["id", "platform", "type", "parent_account_id"]);
            if !truthy(&stable["parent_account_id"]) {
                stable["parent_account_id"] = extra["parent_account_id"].clone();
            }
            let zero = json!(0);
            for f in ["priority", "concurrency", "load_factor"] {
                let fb = if f == "priority" { &zero } else { &Value::Null };
                expected.insert(
                    f.to_owned(),
                    digest_pair(&stable, row.get(f).unwrap_or(fb)).into(),
                );
            }
            expected.insert(
                "proxy".to_owned(),
                digest_pair(&stable, &row["proxy_id"]).into(),
            );
            let guard = json!([
                stable.clone(),
                row["credential_settings_version"],
                pick(extra, &["openai_passthrough", "openai_oauth_passthrough"]),
            ]);
            let empty_obj = json!({});
            let mm = [&row["model_mapping_version"], &row["model_mapping"]]
                .into_iter()
                .find(|v| truthy(v))
                .unwrap_or(&empty_obj);
            expected.insert("model_config".to_owned(), digest_pair(&guard, mm).into());
            expected.insert(
                "sync_models".to_owned(),
                digest_slice(&[&identity, &guard, &row["model_sync_project"]], false).into(),
            );
            assert_eq!(operation_versions(row), Value::Object(expected));
            assert_eq!(
                priority_version(row),
                digest_pair(&stable, row.get("priority").unwrap_or(&zero))
            );
            assert_eq!(
                account_version(row),
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
            );
        }
    }
}
