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

pub fn sanitize(value: &str, limit: usize) -> String {
    let mut text = if value.is_empty() {
        "-".to_owned()
    } else {
        value.replace('\0', " ")
    };
    for (pattern, replacement) in REDACTIONS.iter() {
        text = pattern.replace_all(&text, *replacement).into_owned();
    }
    text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.len() > limit {
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
        "-".into()
    } else {
        text
    }
}

pub fn clean(value: &Value, limit: usize) -> String {
    if value.is_null() || value == "" {
        String::new()
    } else {
        sanitize(&crate::compat::text(value), limit)
    }
}

pub fn safe_error(value: &Value, limit: usize) -> String {
    static REQUEST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)["']?\b(?:request(?:_body|body)?|body|messages|prompt|input|content|credentials|headers|authorization)["']?\s*[:=]"#).expect("request pattern")
    });
    let text = if value.is_null() {
        String::new()
    } else {
        crate::compat::text(value)
    };
    if REQUEST.is_match(&text.replace("\\\"", "\"")) {
        "上游错误包含请求或凭据数据，详细内容已隐藏".into()
    } else {
        clean(value, limit)
    }
}

pub fn safe_error_body(value: &Value) -> String {
    let parsed;
    let value = if let Some(text) = value.as_str() {
        match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                parsed = value;
                &parsed
            }
            Err(_) => {
                return safe_error(
                    &Value::String(text.lines().next().unwrap_or("").into()),
                    1200,
                )
            }
        }
    } else {
        value
    };
    fn scrub(value: &Value) -> Value {
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
                    .map(|(key, value)| (key.clone(), scrub(value)))
                    .collect::<Map<_, _>>(),
            ),
            Value::Array(items) => Value::Array(items.iter().take(10).map(scrub).collect()),
            _ => safe_error(value, 1500).into(),
        }
    }
    if !value.is_object() {
        return String::new();
    }
    let allowed = scrub(value);
    if allowed.as_object().is_none_or(|v| v.is_empty()) {
        return String::new();
    }
    sanitize(
        &serde_json::to_string_pretty(&allowed).expect("error DTO"),
        6000,
    )
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
