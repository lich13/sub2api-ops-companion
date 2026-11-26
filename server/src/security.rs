use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;
static REDACTIONS: LazyLock<Vec<(Regex, &str)>> = LazyLock::new(|| {
    vec![
    (Regex::new(r####"(?s)-----BEGIN [^-\r\n]*PRIVATE KEY-----.*?-----END [^-\r\n]*PRIVATE KEY-----"####).expect("redaction pattern"), "[REDACTED PRIVATE KEY]"),
    (Regex::new(r####"(?i)(?:[\"']?)(authorization|proxy-authorization|x-api-key|x-auth-token|api[ _-]?key|access[ _-]?(?:key|token)|private[ _-]?key|token|secret|password|set-cookie|cookie)(?:[\"']?)(\s*[:=]\s*)(?:\"[^\"]*\"|'[^']*'|[^,;}\r\n]+)"####).expect("redaction pattern"), "${1}${2}[REDACTED]"),
    (Regex::new(r####"(?i)\b(api[ _-]?key|access[ _-]?(?:key|token)|private[ _-]?key|token|secret|password)\b(\s+(?:provided|is|was|equals?|value(?:\s+is)?)\s*[:=]?\s*)(?:\"[^\"]*\"|'[^']*'|[^\s,;}\]\)]+)"####).expect("redaction pattern"), "${1}${2}[REDACTED]"),
    (Regex::new(r####"(?im)\b(set-cookie|cookie)(\s*:\s*)[^\r\n]+"####).expect("redaction pattern"), "${1}${2}[REDACTED]"),
    (Regex::new(r####"(?i)\b(https?://)[^/@\s]+@"####).expect("redaction pattern"), "${1}[REDACTED]@"),
    (Regex::new(r####"(?i)\bbearer\s+[^\s,;]+"####).expect("redaction pattern"), "Bearer [REDACTED]"),
    (Regex::new(r####"\b[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b"####).expect("redaction pattern"), "[REDACTED]"),
    (Regex::new(r####"(?i)([?&](?:api_?key|access_?token|token|secret|password)=)[^&#\s]+"####).expect("redaction pattern"), "${1}[REDACTED]"),
    (Regex::new(r####"(?i)\b(authorization|api[ _-]?key|access[ _-]?(?:key|token)|private[ _-]?key|token|secret|password)(\s*[:=]\s*)([^\s,;]+)"####).expect("redaction pattern"), "${1}${2}[REDACTED]"),
    (Regex::new(r####"(?i)\b(?:sk-(?:proj-|live-|test-)?[A-Za-z0-9_-]{6,}|gh[pousr]_[A-Za-z0-9_]{10,}|xox[abprs]-[A-Za-z0-9-]{10,})\b"####).expect("redaction pattern"), "[REDACTED]"),
]
});

fn may_need_redaction(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut dots = 0usize;
    let mut has_unicode_ws_lead = false;
    for &b in bytes {
        if matches!(
            b,
            b':' | b'=' | b'@' | b' ' | b'\t' | b'\r' | b'\n' | 0x0B | 0x0C
        ) {
            return true;
        }
        if matches!(b, 0xC2 | 0xE1 | 0xE2 | 0xE3) {
            has_unicode_ws_lead = true;
        }
        if b == b'.' {
            dots += 1;
            if dots >= 2 && bytes.len() >= 26 {
                return true;
            }
        }
    }
    if has_unicode_ws_lead && value.chars().any(char::is_whitespace) {
        return true;
    }
    if bytes.len() >= 9 {
        // Check token prefixes (`sk-`, `gh[pousr]_`, `xox[abprs]-`, `-----BEGIN`) case-insensitively
        for w in bytes.windows(4) {
            let c0 = w[0].to_ascii_lowercase();
            let c1 = w[1].to_ascii_lowercase();
            let c2 = w[2].to_ascii_lowercase();
            if (c0 == b's' && c1 == b'k' && c2 == b'-')
                || (c0 == b'g'
                    && c1 == b'h'
                    && matches!(c2, b'p' | b'o' | b'u' | b's' | b'r')
                    && w[3] == b'_')
                || (c0 == b'x' && c1 == b'o' && c2 == b'x')
                || w == b"----"
            {
                return true;
            }
        }
    }
    false
}

fn redact_text(value: &str) -> String {
    if !may_need_redaction(value) {
        return value.to_owned();
    }
    let mut text = value.to_owned();
    for (pattern, replacement) in REDACTIONS.iter() {
        if let std::borrow::Cow::Owned(next) = pattern.replace_all(&text, *replacement) {
            text = next;
        }
    }
    text
}

fn contains_request_data(text: &str) -> bool {
    if !text.as_bytes().iter().any(|&b| b == b':' || b == b'=') {
        return false;
    }
    static REQUEST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)["']?\b(?:request(?:_body|body)?|body|messages|prompt|input|content|credentials|headers|authorization)["']?\s*[:=]"#).expect("request pattern")
    });
    if text.contains("\\\"") {
        REQUEST.is_match(&text.replace("\\\"", "\""))
    } else {
        REQUEST.is_match(text)
    }
}

/// Redact at the audit boundary without changing ordinary values, keys or
/// scalar types. Error-body projection is deliberately not used here: it
/// drops fields that are necessary to inspect operation receipts.
pub fn redact_json(value: &Value) -> Value {
    static SENSITIVE_KEY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:authorization|proxy-authorization|x-api-key|x-auth-token|api[ _-]?key|access[ _-]?(?:key|token)|refresh[ _-]?token|private[ _-]?key|(?:bark[ _-]?)?device[ _-]?key|token|secret|password|set-cookie|cookie|credentials|headers|request(?:_body|body)?|body|messages|prompt|input|content)$").expect("audit key pattern")
    });
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if SENSITIVE_KEY.is_match(key) {
                        Value::String("[REDACTED]".into())
                    } else {
                        redact_json(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_json).collect()),
        Value::String(text) if contains_request_data(text) => {
            "上游错误包含请求或凭据数据，详细内容已隐藏".into()
        }
        Value::String(text) => redact_text(text).into(),
        _ => value.clone(),
    }
}

fn sanitize_tracked(value: &str, limit: usize) -> (String, bool) {
    let mut text = if value.is_empty() {
        "-".to_owned()
    } else {
        value.replace('\0', " ")
    };
    text = redact_text(&text);
    text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut truncated = false;
    if text.len() > limit {
        truncated = true;
        if limit <= 3 {
            text = ".".repeat(limit);
        } else {
            let mut end = limit - 3;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("...");
        }
    }
    if text.is_empty() {
        ("-".into(), truncated)
    } else {
        (text, truncated)
    }
}

pub fn sanitize(value: &str, limit: usize) -> String {
    sanitize_tracked(value, limit).0
}

pub fn clean(value: &Value, limit: usize) -> String {
    if value.is_null() || value == "" {
        String::new()
    } else {
        sanitize(&crate::compat::text(value), limit)
    }
}

fn safe_error_tracked(value: &Value, limit: usize) -> (String, bool) {
    if value.is_null() {
        return (String::new(), false);
    }
    let text = crate::compat::text(value);
    if text.is_empty() {
        (String::new(), false)
    } else if contains_request_data(&text) {
        ("上游错误包含请求或凭据数据，详细内容已隐藏".into(), false)
    } else {
        sanitize_tracked(&text, limit)
    }
}

pub fn safe_error(value: &Value, limit: usize) -> String {
    safe_error_tracked(value, limit).0
}

pub(crate) fn safe_error_body_limited(value: &Value) -> (String, bool) {
    let parsed;
    let value = if let Some(text) = value.as_str() {
        match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                parsed = value;
                &parsed
            }
            Err(_) => {
                let first_line = text.lines().next().unwrap_or("");
                let (out, truncated) = safe_error_tracked(&Value::String(first_line.into()), 1200);
                return (out, truncated);
            }
        }
    } else {
        value
    };
    fn scrub(value: &Value, limited: &mut bool) -> Value {
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .filter(|(key, _)| {
                        matches!(
                            key.as_str(),
                            "error" | "code" | "type" | "message" | "detail" | "reason" | "status"
                        )
                    })
                    .map(|(key, value)| (key.clone(), scrub(value, limited)))
                    .collect::<Map<_, _>>(),
            ),
            Value::Array(items) => {
                if items.len() > 10 {
                    *limited = true;
                }
                Value::Array(items.iter().take(10).map(|v| scrub(v, limited)).collect())
            }
            _ => {
                let (s, truncated) = safe_error_tracked(value, 1500);
                *limited |= truncated;
                s.into()
            }
        }
    }
    if !value.is_object() {
        return (String::new(), false);
    }
    let mut limited = false;
    let allowed = scrub(value, &mut limited);
    if allowed.as_object().is_none_or(|v| v.is_empty()) {
        return (String::new(), false);
    }
    let (body, outer_truncated) = sanitize_tracked(
        &serde_json::to_string_pretty(&allowed).expect("error DTO"),
        6000,
    );
    (body, limited || outer_truncated)
}

pub fn safe_error_body(value: &Value) -> String {
    safe_error_body_limited(value).0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn error_dumps_do_not_leak_opaque_credentials_or_payloads() {
        assert_eq!(
            safe_error(
                &Value::String("request_body={private: opaque_fixture}".into()),
                600
            ),
            "上游错误包含请求或凭据数据，详细内容已隐藏"
        );
        assert!(
            !sanitize("authorization: Bearer synthetic-fixture", 600).contains("synthetic-fixture")
        );
        assert_eq!(sanitize("中文中文", 8), "中...");
    }
}
