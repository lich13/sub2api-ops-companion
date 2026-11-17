use crate::compat::{parse_datetime, string};
use chrono::{DateTime, Duration as TimeDelta, Utc};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
static THROTTLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r####"(?i)(?:您\s*已\s*(?:达到|超过)\s*请求(?:数|数量)\s*限制|you\s*(?:have\s*)?(?:reached|exceeded)\s*(?:the\s*)?(?:request\s*)?(?:rate\s*)?limit)[^\n]{0,120}?(?:(?:1\s*分钟内|每\s*(?:1\s*)?分钟)\s*(?:最多|至多)?\s*请求\s*[0-9]+\s*次|[0-9]+\s*requests?\s*(?:per|\/|a)\s*minute)"####).expect("error matcher")
});
static THROTTLE_EN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r####"(?i)(?:request\s*(?:rate\s*)?limit|maximum\s+(?:number\s+of\s+)?requests?|requests?\s*(?:rate\s*)?limit)[^\n]{0,120}(?:maximum\s*)?[0-9]+\s*(?:requests?|reqs?)\s*(?:per|\/|a)\s*minute"####).expect("error matcher")
});
static CONCURRENCY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r####"(?i)(?:too\s+many\s+concurrent\s+requests\s+for\s+(?:user|client|api[ -]?key)|(?:user|client|api[ -]?key)\s+concurrency\s+limit\s+exceeded|(?:用户|客户端|密钥)\s*(?:并发请求|并发)\s*(?:数)?\s*(?:已)?(?:达到|超过|超出).{0,20}(?:限制|上限))"####).expect("error matcher")
});
pub const CAPACITY_MESSAGES: &[&str] = &[
    "Our servers are currently overloaded. Please try again later.",
    "Selected model is at capacity. Please try a different model.",
    "stream disconnected before completion: Concurrency limit exceeded for account, please retry later"
];

fn recorded_texts(value: &Value) -> Vec<String> {
    match value {
        Value::Array(values) => values.iter().flat_map(recorded_texts).collect(),
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => recorded_texts(&value),
            Err(_) => vec![text.clone()],
        },
        Value::Object(_) => {
            let mut texts = vec![];
            for field in ["message", "error_message"] {
                if let Some(text) = value[field].as_str() {
                    texts.push(text.into());
                }
            }
            if let Some(text) = value["error"]["message"].as_str() {
                texts.push(text.into());
            }
            if matches!(
                string(&value["type"]),
                "response.failed" | "response.incomplete"
            ) {
                if let Some(text) = value["response"]["error"]["message"].as_str() {
                    texts.push(text.into());
                }
            }
            texts
        }
        _ => vec![],
    }
}
pub fn local_throttle(row: &Value) -> bool {
    let local_concurrency = matches!(string(&row["error_owner"]), "client" | "platform")
        && matches!(
            string(&row["error_phase"]),
            "concurrency" | "rate_limit" | "admission"
        );
    [
        "upstream_error_message",
        "error_message",
        "message",
        "upstream_errors",
        "error_body",
        "upstream_error_detail",
    ]
    .into_iter()
    .flat_map(|key| recorded_texts(&row[key]))
    .any(|text| {
        THROTTLE.is_match(&text)
            || THROTTLE_EN.is_match(&text)
            || local_concurrency && CONCURRENCY.is_match(&text)
    })
}
fn normalized(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches([
            '.', '!', '?', '。', '！', '？', ',', '，', ';', '；', ':', '：', ' ',
        ])
        .to_owned()
}
pub fn recorded_message(value: &Value, plain: bool) -> Option<&'static str> {
    match value {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => recorded_message(&value, false),
            Err(_) => CAPACITY_MESSAGES
                .iter()
                .find(|m| plain && normalized(text) == normalized(m))
                .copied(),
        },
        Value::Array(values) => values.iter().find_map(|v| recorded_message(v, true)),
        Value::Object(_) => {
            for key in ["message", "error_message"] {
                if value[key].is_string() {
                    if let Some(found) = recorded_message(&value[key], true) {
                        return Some(found);
                    }
                }
            }
            if value["error"].is_object() {
                return recorded_message(&value["error"]["message"], true);
            }
            if matches!(
                string(&value["type"]),
                "response.failed" | "response.incomplete"
            ) {
                return recorded_message(&value["response"], false);
            }
            None
        }
        _ => None,
    }
}
pub fn match_message(row: &Value) -> Option<&'static str> {
    if row["account_platform"] != "openai"
        || !matches!(string(&row["account_type"]), "oauth" | "apikey")
    {
        return None;
    }
    let provider = row["error_owner"] == "provider" && row["error_phase"] == "upstream";
    let wrapped = row["error_owner"] == "platform"
        && row["error_phase"] == "internal"
        && row["error_source"] == "gateway";
    if !provider && !wrapped {
        return None;
    }
    for field in [
        "upstream_error_message",
        "error_message",
        "error_body",
        "upstream_error_detail",
    ] {
        if wrapped && field == "error_message" && row["stream"] != true {
            continue;
        }
        let candidate = serde_json::json!({field:row[field]});
        if local_throttle(&candidate) {
            continue;
        }
        if let Some(message) = recorded_message(&row[field], field.ends_with("_message")) {
            return Some(message);
        }
    }
    row["upstream_errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v.is_object() && !local_throttle(v))
        .find_map(|v| recorded_message(v, true))
}

static OVERLOAD_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:currently overloaded|servers?\s+(?:is|are)\s+overloaded|at capacity|model is overloaded|concurrency limit|too many concurrent requests)\b")
        .expect("overload matcher")
});

static RPM_TPM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\b(?:tokens|requests|images)\s+per\s+(?:min(?:ute)?|sec(?:ond)?)\b|\bon\s+(?:tokens|requests|images)\s+per\s+(?:min|sec|day)\b|\b(?:rpm|tpm|tps|rps)\b)")
        .expect("rpm/tpm matcher")
});

static SHORT_COOLDOWN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:please\s+)?(?:try again|retry)\s+(?:in|after)\s+[0-9]+(?:\.[0-9]+)?\s*(?:ms|milliseconds?|s|sec|secs|seconds?)\b")
        .expect("short cooldown matcher")
});

fn collect_error_objects(value: &Value, depth: usize, out: &mut Vec<Value>) {
    if depth > 6 {
        return;
    }
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.starts_with(['{', '[']) {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    collect_error_objects(&parsed, depth + 1, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_error_objects(item, depth + 1, out);
            }
        }
        Value::Object(map) => {
            out.push(value.clone());
            for key in [
                "error",
                "detail",
                "response",
                "rate_limit",
                "data",
                "primary_window",
                "secondary_window",
            ] {
                if let Some(child) = map.get(key) {
                    collect_error_objects(child, depth + 1, out);
                }
            }
        }
        _ => {}
    }
}

fn error_objects(row: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for field in [
        "error_body",
        "upstream_error_detail",
        "upstream_error_message",
        "error_message",
        "upstream_errors",
    ] {
        collect_error_objects(&row[field], 0, &mut out);
    }
    out
}

fn parse_epoch_or_iso(value: &Value) -> Option<DateTime<Utc>> {
    if let Some(dt) = parse_datetime(value) {
        return Some(dt);
    }
    let number = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<f64>().ok()))?;
    if !number.is_finite() {
        return None;
    }
    if (1_000_000_000.0..10_000_000_000.0).contains(&number) {
        DateTime::from_timestamp_millis((number * 1000.0) as i64)
    } else if (1_000_000_000_000.0..10_000_000_000_000.0).contains(&number) {
        DateTime::from_timestamp_millis(number as i64)
    } else {
        None
    }
}

fn parse_relative_seconds(value: &Value) -> Option<f64> {
    if let Some(secs) = value.as_f64() {
        return (secs.is_finite() && (0.0..=31_536_000.0).contains(&secs)).then_some(secs);
    }
    let raw = value.as_str()?.trim().to_lowercase();
    let (num, scale) = if let Some(ms) = raw.strip_suffix("ms") {
        (ms.trim(), 0.001)
    } else if let Some(s) = raw.strip_suffix('s') {
        (s.trim(), 1.0)
    } else {
        (raw.as_str(), 1.0)
    };
    let parsed = num.parse::<f64>().ok()? * scale;
    (parsed.is_finite() && (0.0..=31_536_000.0).contains(&parsed)).then_some(parsed)
}

pub fn error_reset_at(row: &Value) -> Option<DateTime<Utc>> {
    let objects = error_objects(row);
    for obj in &objects {
        for key in ["resets_at", "reset_at"] {
            if let Some(dt) = parse_epoch_or_iso(&obj[key]) {
                return Some(dt);
            }
        }
    }
    let base = parse_datetime(&row["created_at"])?;
    for obj in &objects {
        for key in [
            "resets_in_seconds",
            "reset_after_seconds",
            "retry_after_seconds",
            "retry_after",
        ] {
            if let Some(secs) = parse_relative_seconds(&obj[key]) {
                return Some(base + TimeDelta::milliseconds((secs * 1000.0) as i64));
            }
        }
        for key in ["retry_after_ms", "reset_after_ms"] {
            if let Some(ms) = obj[key]
                .as_f64()
                .filter(|v| v.is_finite() && (0.0..=31_536_000_000.0).contains(v))
            {
                return Some(base + TimeDelta::milliseconds(ms as i64));
            }
        }
    }
    None
}

pub fn non_weekly_429(row: &Value, expected_7d_reset: Option<DateTime<Utc>>) -> bool {
    if local_throttle(row) {
        return true;
    }
    let objects = error_objects(row);
    let mut texts: Vec<String> = [
        "upstream_error_message",
        "error_message",
        "message",
        "upstream_errors",
        "error_body",
        "upstream_error_detail",
    ]
    .into_iter()
    .flat_map(|key| recorded_texts(&row[key]))
    .collect();
    for obj in &objects {
        if let Some(detail) = obj["detail"].as_str() {
            texts.push(detail.to_owned());
        }
    }

    let mut codes = Vec::new();
    for key in ["provider_error_code", "provider_error_type", "error_type"] {
        let val = string(&row[key]).trim().to_lowercase();
        if !val.is_empty() {
            codes.push(val);
        }
    }
    for obj in &objects {
        for key in ["code", "type", "error_code", "error_type"] {
            let val = string(&obj[key]).trim().to_lowercase();
            if !val.is_empty()
                && !matches!(
                    val.as_str(),
                    "response.failed" | "response.incomplete" | "error"
                )
            {
                codes.push(val);
            }
        }
    }

    let mut models = Vec::new();
    for key in ["model", "requested_model", "upstream_model"] {
        let val = string(&row[key]).trim().to_lowercase();
        if !val.is_empty() {
            models.push(val);
        }
    }
    for obj in &objects {
        for key in ["model", "limit_name", "quota_scope", "feature"] {
            let val = string(&obj[key]).trim().to_lowercase();
            if !val.is_empty() {
                models.push(val);
            }
        }
    }

    // 1. Capacity / overload / concurrency
    if [
        ("upstream_error_message", true),
        ("error_message", true),
        ("error_body", false),
        ("upstream_error_detail", false),
    ]
    .into_iter()
    .any(|(field, plain)| recorded_message(&row[field], plain).is_some())
        || row["upstream_errors"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|v| recorded_message(v, true).is_some())
        || texts
            .iter()
            .any(|t| CONCURRENCY.is_match(t) || OVERLOAD_TEXT.is_match(t))
        || codes.iter().any(|c| {
            matches!(
                c.as_str(),
                "server_is_overloaded"
                    | "overloaded"
                    | "overloaded_error"
                    | "model_at_capacity"
                    | "at_capacity"
                    | "capacity_exceeded"
                    | "concurrency_limit_exceeded"
                    | "too_many_concurrent_requests"
                    | "slow_down"
                    | "engine_overloaded"
            )
        })
    {
        return true;
    }

    // 2. Model-specific quota (-spark / gpt-image / GoUsageLimitError)
    let model_specific = |s: &str| {
        s.contains("-spark") || s.contains("gpt-image") || s.contains("gousagelimiterror")
    };
    if models.iter().any(|m| model_specific(m))
        || codes.iter().any(|c| model_specific(c))
        || texts.iter().any(|t| model_specific(&t.to_lowercase()))
        || [&row["error_body"], &row["upstream_error_detail"]]
            .into_iter()
            .filter_map(Value::as_str)
            .any(|s| s.to_lowercase().contains("gousagelimiterror"))
    {
        return true;
    }

    // 3. Explicit 5h window or reset anchor deviating > 300s from 7d reset
    for obj in &objects {
        if obj["limit_window_seconds"].as_f64() == Some(18000.0)
            || obj["window_minutes"].as_f64() == Some(300.0)
            || matches!(
                string(&obj["window"]).trim().to_lowercase().as_str(),
                "5h" | "five_hour" | "primary_window"
            )
        {
            return true;
        }
    }
    let extracted_reset = error_reset_at(row);
    if let (Some(er), Some(r7)) = (extracted_reset, expected_7d_reset) {
        if (r7 - er).num_seconds().abs() > 300 {
            return true;
        }
    }

    let aligned_7d_reset = extracted_reset
        .zip(expected_7d_reset)
        .is_some_and(|(er, r7)| (r7 - er).num_seconds().abs() <= 300);
    let has_usage_limit_reached = codes.iter().any(|c| c == "usage_limit_reached")
        || texts.iter().any(|t| {
            let lower = t.to_lowercase();
            lower.contains("usage_limit_reached")
                || lower.contains("usage limit has been reached")
                || lower.contains("weekly limit reached")
        });

    // 4. RPM / TPM / short cooldown
    if texts.iter().any(|t| RPM_TPM.is_match(t)) {
        return true;
    }
    if !aligned_7d_reset && texts.iter().any(|t| SHORT_COOLDOWN.is_match(t)) {
        return true;
    }
    if !has_usage_limit_reached
        && !aligned_7d_reset
        && codes
            .iter()
            .any(|c| matches!(c.as_str(), "rate_limit_exceeded" | "requests" | "tokens"))
    {
        return true;
    }

    // 5. rate_limit_error without usage_limit_reached and without 7d reset alignment
    if codes.iter().any(|c| c == "rate_limit_error")
        && !has_usage_limit_reached
        && !aligned_7d_reset
    {
        return true;
    }

    false
}
