use crate::compat::string;
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
