use crate::{
    compat::{integer, string, truthy},
    modeltrace::{NUMBERS, THOUGHT_TAG},
};
use futures_util::{future::BoxFuture, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

pub const MAX_TEXT: usize = 32 * 1024;
const MAX_FRAME: usize = 256 * 1024;
#[derive(Debug, Clone)]
pub struct Failure {
    pub code: &'static str,
    pub retryable: bool,
    pub uncertain: bool,
}
impl Failure {
    pub fn new(code: &'static str, retryable: bool) -> Self {
        Self {
            code,
            retryable,
            uncertain: matches!(
                code,
                "network_error"
                    | "incomplete_stream"
                    | "invalid_stream"
                    | "output_limit"
                    | "timeout"
                    | "protocol_mismatch"
            ),
        }
    }
}
pub type TransportResult<T> = std::result::Result<T, Failure>;
pub fn failure(status: u16, payload: &Value) -> Failure {
    let text = payload
        .get("error")
        .filter(|v| truthy(v))
        .unwrap_or(payload)
        .to_string()
        .to_lowercase();
    if matches!(status, 401..=403)
        || [
            "unauthorized",
            "invalid_api_key",
            "invalid api key",
            "authentication_error",
            "insufficient_quota",
            "billing",
            "token_expired",
        ]
        .iter()
        .any(|v| text.contains(v))
    {
        return Failure::new("auth_or_quota", false);
    }
    if status == 429
        || [
            "rate_limit",
            "usage_limit",
            "quota_exceeded",
            "concurrency limit exceeded",
            "concurrency_limit_exceeded",
        ]
        .iter()
        .any(|v| text.contains(v))
    {
        return Failure::new("rate_limited", false);
    }
    if matches!(status, 400 | 404 | 422)
        || ["model_not_found", "invalid_request", "invalid_parameter"]
            .iter()
            .any(|v| text.contains(v))
    {
        return Failure::new("invalid_model_or_request", false);
    }
    Failure::new("upstream_error", true)
}
pub fn isolated_answer(text: &str) -> String {
    let mut stack = vec![];
    let mut parts = String::new();
    let mut cursor = 0;
    for capture in THOUGHT_TAG.captures_iter(text) {
        let whole = capture.get(0).unwrap();
        if stack.is_empty() {
            parts.push_str(&text[cursor..whole.start()]);
        }
        let tag = capture[2].to_lowercase();
        if &capture[1] == "/" {
            if stack.last() == Some(&tag) {
                stack.pop();
            }
        } else {
            stack.push(tag);
        }
        cursor = whole.end();
    }
    if stack.is_empty() {
        let mut tail = &text[cursor..];
        if let Some(start) = tail.rfind('<') {
            if !tail[start..].contains('>') {
                tail = &tail[..start];
            }
        }
        parts.push_str(tail);
    }
    parts
}
#[derive(Clone)]
pub struct Collector {
    expected: usize,
    input_tokens: Option<i64>,
    cache_read: i64,
    cache_creation: i64,
    parts: BTreeMap<(i64, i64), String>,
    channels: BTreeMap<(i64, i64), bool>,
    pub completed: bool,
    pub returned_model: Option<String>,
    pub recognized: bool,
    pub protocol: Option<&'static str>,
    pub end_reason: String,
    pub metadata: Value,
}
impl Collector {
    pub fn new(expected: usize) -> Self {
        Self {
            expected,
            input_tokens: None,
            cache_read: 0,
            cache_creation: 0,
            parts: BTreeMap::new(),
            channels: BTreeMap::new(),
            completed: false,
            returned_model: None,
            recognized: false,
            protocol: None,
            end_reason: String::new(),
            metadata: json!({}),
        }
    }
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter(|(key, _)| !self.channels.get(key).copied().unwrap_or(false))
            .map(|(_, value)| value.as_str())
            .collect()
    }
    fn response_metadata(&mut self, response: &Value) {
        if matches!(
            string(&response["status"]),
            "completed" | "incomplete" | "failed" | "in_progress" | "queued" | "cancelled"
        ) {
            self.metadata["provider_status"] = response["status"].clone();
        }
        let usage = &response["usage"];
        if self.protocol == Some("messages") {
            let valid = |v: &Value| v.as_i64().filter(|n| (0..=1_000_000_000_000).contains(n));
            if let Some(n) = valid(&usage["input_tokens"]) {
                self.input_tokens = Some(n);
            }
            if let Some(n) = valid(&usage["cache_read_input_tokens"]) {
                self.cache_read = n;
            }
            if let Some(n) = valid(&usage["cache_creation_input_tokens"]) {
                self.cache_creation = n;
            }
        }
        let details = usage
            .get("output_tokens_details")
            .or_else(|| usage.get("completion_tokens_details"))
            .unwrap_or(&Value::Null);
        for (key, value) in [
            (
                "input_tokens",
                usage
                    .get("input_tokens")
                    .or_else(|| usage.get("prompt_tokens")),
            ),
            (
                "output_tokens",
                usage
                    .get("output_tokens")
                    .or_else(|| usage.get("completion_tokens")),
            ),
            ("reasoning_tokens", details.get("reasoning_tokens")),
        ] {
            if let Some(value) = value.filter(|v| {
                v.as_i64()
                    .is_some_and(|n| (0..=1_000_000_000_000).contains(&n))
            }) {
                self.metadata[key] = value.clone();
            }
        }
        if self.protocol == Some("messages") {
            if let Some(input) = self.input_tokens {
                self.metadata["input_tokens"] =
                    json!(input + self.cache_read + self.cache_creation);
            }
        }
    }
    fn incomplete(&mut self, response: &Value) -> TransportResult<()> {
        let reason = string(&response["incomplete_details"]["reason"]);
        if matches!(
            reason,
            "max_output_tokens" | "max_tokens" | "content_filter"
        ) {
            self.response_output(response)?;
            self.completed = true;
            self.end_reason = reason.into();
            self.metadata["incomplete_reason"] = reason.into();
            Ok(())
        } else {
            self.metadata["incomplete_reason"] = "unknown".into();
            Err(Failure::new("incomplete_response", false))
        }
    }
    pub fn update(
        &mut self,
        key: (i64, i64),
        value: &Value,
        snapshot: bool,
        reasoning: bool,
        guard: bool,
    ) -> TransportResult<()> {
        let Some(value) = value.as_str() else {
            return Ok(());
        };
        let text = if snapshot {
            value.to_owned()
        } else {
            format!(
                "{}{value}",
                self.parts.get(&key).map(String::as_str).unwrap_or("")
            )
        };
        if self
            .parts
            .iter()
            .filter(|(k, _)| **k != key)
            .map(|(_, v)| v.len())
            .sum::<usize>()
            + text.len()
            > MAX_TEXT
        {
            return Err(Failure::new("output_limit", true));
        }
        self.parts.insert(key, text);
        self.channels.insert(key, reasoning);
        if guard && !snapshot && !reasoning {
            let answer = isolated_answer(&self.text());
            let boundary =
                |c: char| c.is_alphanumeric() || c == '_' || matches!(c, '.' | '+' | '-');
            let count = NUMBERS
                .find_iter(&answer)
                .filter(|m| {
                    m.end() < answer.len()
                        && answer[..m.start()]
                            .chars()
                            .next_back()
                            .is_none_or(|c| !boundary(c))
                        && answer[m.end()..]
                            .chars()
                            .next()
                            .is_none_or(|c| !boundary(c))
                })
                .count();
            if count > self.expected * 2 {
                return Err(Failure::new("output_limit", true));
            }
        }
        Ok(())
    }
    fn response_output(&mut self, response: &Value) -> TransportResult<()> {
        if let Some(model) = response["model"].as_str() {
            self.returned_model = Some(model.chars().take(200).collect());
        }
        for (i, item) in response["output"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            if !item.is_object() {
                continue;
            }
            let reasoning = item["type"] == "reasoning"
                || !matches!(item.get("phase"), None | Some(Value::Null))
                    && !matches!(string(&item["phase"]), "final" | "final_answer");
            for (j, content) in item["content"].as_array().into_iter().flatten().enumerate() {
                if matches!(string(&content["type"]), "output_text" | "text") {
                    self.update(
                        (i as i64, j as i64),
                        &content["text"],
                        true,
                        reasoning,
                        false,
                    )?;
                }
            }
            for (j, content) in item["summary"].as_array().into_iter().flatten().enumerate() {
                if content.is_object() {
                    self.update(
                        (i as i64, 1000 + j as i64),
                        &content["text"],
                        true,
                        true,
                        false,
                    )?;
                }
            }
        }
        if response["output_text"].is_string() && !truthy(&response["output"]) {
            self.update((0, 0), &response["output_text"], true, false, false)?;
        }
        Ok(())
    }
    pub fn accept(&mut self, event: &Value, event_name: &str, whole: bool) -> TransportResult<()> {
        if !event.is_object() {
            return Err(Failure::new("protocol_mismatch", false));
        }
        let kind = event
            .get("type")
            .filter(|v| truthy(v))
            .map(|v| v.as_str())
            .unwrap_or(Some(event_name))
            .ok_or_else(|| Failure::new("protocol_mismatch", false))?;
        let empty = json!({});
        let response = event
            .get("response")
            .filter(|v| truthy(v))
            .unwrap_or(&empty);
        if !response.is_object() {
            return Err(Failure::new("protocol_mismatch", false));
        }
        if matches!(
            kind,
            "message"
                | "message_start"
                | "message_delta"
                | "message_stop"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
        ) {
            self.protocol = Some("messages");
            self.recognized = true;
        }
        if kind == "message_start" {
            self.response_metadata(&event["message"]);
        }
        self.response_metadata(if truthy(response) { response } else { event });
        if truthy(&event["error"])
            || truthy(&response["error"])
            || matches!(kind, "error" | "response.failed")
        {
            return Err(failure(0, if truthy(response) { response } else { event }));
        }
        if self.protocol == Some("messages") {
            let message = if kind == "message_start" {
                &event["message"]
            } else {
                event
            };
            if let Some(model) = message["model"].as_str() {
                self.returned_model = Some(model.chars().take(200).collect());
            }
            if whole && kind == "message" {
                for (i, block) in event["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let reasoning = block["type"] != "text";
                    self.update(
                        (i as i64, 0),
                        if reasoning {
                            &block["thinking"]
                        } else {
                            &block["text"]
                        },
                        true,
                        reasoning,
                        false,
                    )?;
                }
                self.completed = !event["stop_reason"].is_null();
                self.end_reason = string(&event["stop_reason"]).into();
            } else if kind == "content_block_start" {
                let i = event["index"].as_i64().unwrap_or(0);
                let block = &event["content_block"];
                let reasoning = block["type"] != "text";
                self.update(
                    (i, 0),
                    if reasoning {
                        &block["thinking"]
                    } else {
                        &block["text"]
                    },
                    true,
                    reasoning,
                    false,
                )?;
                self.channels.insert((i, 0), reasoning);
            } else if kind == "content_block_delta" {
                let i = event["index"].as_i64().unwrap_or(0);
                let delta = &event["delta"];
                let reasoning = delta["type"] != "text_delta"
                    || self.channels.get(&(i, 0)).copied().unwrap_or(false);
                self.update(
                    (i, 0),
                    if reasoning {
                        &delta["thinking"]
                    } else {
                        &delta["text"]
                    },
                    false,
                    reasoning,
                    true,
                )?;
            } else if kind == "message_delta" {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.end_reason = reason.chars().take(40).collect();
                }
            } else if kind == "message_stop" {
                self.completed = true;
                if self.end_reason.is_empty() {
                    self.end_reason = "message_stop".into();
                }
            }
            return Ok(());
        }
        if kind == "response.incomplete" {
            self.recognized = true;
            self.protocol = Some("responses");
            return self.incomplete(response);
        }
        if event.get("choices").is_some() {
            self.recognized = true;
            self.protocol = Some("chat_completions");
            if let Some(model) = event["model"].as_str() {
                self.returned_model = Some(model.chars().take(200).collect());
            }
            for choice in event["choices"].as_array().into_iter().flatten() {
                if !choice.is_object()
                    || integer(choice.get("index").unwrap_or(&json!(0))) != Some(0)
                {
                    continue;
                }
                let message = &choice[if whole { "message" } else { "delta" }];
                self.update((0, 0), &message["content"], whole, false, !whole)?;
                self.update(
                    (0, 1000),
                    message
                        .get("reasoning_content")
                        .unwrap_or(&message["reasoning"]),
                    whole,
                    true,
                    false,
                )?;
                if !choice["finish_reason"].is_null() {
                    self.completed = true;
                    self.end_reason = crate::compat::text(&choice["finish_reason"])
                        .chars()
                        .take(40)
                        .collect();
                }
            }
            return Ok(());
        }
        if whole
            && (event.get("output").is_some()
                || event.get("output_text").is_some()
                || event["object"] == "response")
        {
            self.recognized = true;
            self.protocol = Some("responses");
            if event["status"] == "failed" {
                return Err(failure(0, event));
            }
            if event["status"] == "incomplete" {
                return self.incomplete(event);
            }
            if event["status"] != "completed" {
                return Err(Failure::new("incomplete_stream", true));
            }
            self.response_output(event)?;
            self.completed = true;
            self.end_reason = "completed".into();
            return Ok(());
        }
        if kind.starts_with("response.") || kind.starts_with("reasoning.") {
            self.recognized = true;
            self.protocol = Some("responses");
        }
        let index = event
            .get("output_index")
            .map(|v| v.as_i64())
            .unwrap_or(Some(0))
            .ok_or_else(|| Failure::new("protocol_mismatch", false))?;
        let part = event
            .get("content_index")
            .map(|v| v.as_i64())
            .unwrap_or(Some(0))
            .ok_or_else(|| Failure::new("protocol_mismatch", false))?;
        let summary = event
            .get("summary_index")
            .map(|v| v.as_i64())
            .unwrap_or(Some(0))
            .ok_or_else(|| Failure::new("protocol_mismatch", false))?;
        if kind == "response.output_item.done" {
            for (j, content) in event["item"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if matches!(string(&content["type"]), "output_text" | "text") {
                    self.update(
                        (index, j as i64),
                        &content["text"],
                        true,
                        self.channels.get(&(index, 0)).copied().unwrap_or(false),
                        false,
                    )?;
                }
            }
        }
        if kind == "response.output_item.added" {
            let item = &event["item"];
            self.channels.insert(
                (index, 0),
                item["type"] == "reasoning"
                    || !item["phase"].is_null()
                        && !matches!(string(&item["phase"]), "final" | "final_answer"),
            );
        }
        if kind.starts_with("response.output_text.")
            && ["delta", "done", "snapshot"]
                .iter()
                .any(|suffix| kind.ends_with(suffix))
        {
            self.update(
                (index, part),
                event.get("delta").unwrap_or(&event["text"]),
                !kind.ends_with("delta"),
                self.channels.get(&(index, 0)).copied().unwrap_or(false),
                true,
            )?;
        } else if kind.contains("reasoning") && (kind.ends_with("delta") || kind.ends_with("done"))
        {
            self.update(
                (index, 1000 + summary),
                event.get("delta").unwrap_or(&event["text"]),
                kind.ends_with("done"),
                true,
                false,
            )?;
        }
        if kind == "response.completed" {
            if response["status"] == "incomplete" {
                return self.incomplete(response);
            }
            if !response["status"].is_null() && response["status"] != "completed" {
                return Err(Failure::new("incomplete_stream", true));
            }
            self.response_output(response)?;
            self.completed = true;
            self.end_reason = "completed".into();
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct Target {
    pub platform: String,
    pub url: String,
    pub headers: reqwest::header::HeaderMap,
    pub proxy: Option<String>,
    pub model: String,
    pub oauth: bool,
    pub protocol: String,
}
impl Target {
    pub fn client(&self) -> TransportResult<reqwest::Client> {
        let mut client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10));
        if let Some(proxy) = &self.proxy {
            client = client.proxy(
                reqwest::Proxy::all(proxy).map_err(|_| Failure::new("network_error", false))?,
            );
        }
        client
            .build()
            .map_err(|_| Failure::new("network_error", false))
    }
    pub fn payload(&self, prompt: &str) -> Value {
        if self.protocol == "messages" {
            json!({"model":self.model,"messages":[{"role":"user","content":prompt}],"stream":true,"max_tokens":4096})
        } else if self.protocol == "chat_completions" {
            let mut payload = json!({"model":self.model,"messages":[{"role":"user","content":prompt}],"stream":true});
            payload[if self.platform == "openai" {
                "max_completion_tokens"
            } else {
                "max_tokens"
            }] = 4096.into();
            if self.platform != "openai" {
                payload["stream_options"] = json!({"include_usage":true});
            }
            payload
        } else {
            let mut payload = json!({"model":self.model,"input":[{"role":"user","content":[{"type":"input_text","text":prompt}]}],"instructions":"You are a helpful assistant.","stream":true,"store":false});
            if !self.oauth {
                payload["max_output_tokens"] = 4096.into();
            }
            payload
        }
    }
}
pub type Progress =
    Arc<dyn Fn(Value) -> BoxFuture<'static, crate::error::Result<()>> + Send + Sync>;
async fn received(
    collector: &mut Collector,
    diag: &mut Value,
    event: &Value,
    name: &str,
    whole: bool,
    started: Instant,
    callback: &Option<Progress>,
) -> TransportResult<()> {
    diag["events"] = json!(integer(&diag["events"]).unwrap_or(0) + 1);
    let event_type =
        event["type"]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or(if !name.is_empty() {
                name
            } else if event.get("choices").is_some() {
                "chat.completion"
            } else if whole {
                "response"
            } else {
                "unknown"
            });
    let event_type = if !event_type.is_empty()
        && event_type.len() <= 80
        && event_type
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
    {
        event_type
    } else {
        "unknown"
    };
    let event_type = crate::security::clean(&json!(event_type), 80);
    if diag.get("first_event_type").is_none() {
        diag["first_event_type"] = event_type.clone().into();
        diag["first_event_ms"] = json!(started.elapsed().as_millis());
    }
    diag["last_event_type"] = event_type.into();
    let result = collector.accept(event, name, whole);
    diag.as_object_mut()
        .unwrap()
        .extend(collector.metadata.as_object().unwrap().clone());
    diag["response_protocol"] = json!(collector.protocol);
    result?;
    if !isolated_answer(&collector.text()).is_empty() && diag.get("ttft_ms").is_none() {
        diag["ttft_ms"] = json!(started.elapsed().as_millis());
        if let Some(callback) = callback {
            callback(json!({"ttft_ms":diag["ttft_ms"],"diagnostics":diag}))
                .await
                .map_err(|_| Failure::new("internal_error", false))?;
        }
    }
    Ok(())
}
async fn frame(
    lines: &[Vec<u8>],
    collector: &mut Collector,
    diag: &mut Value,
    started: Instant,
    callback: &Option<Progress>,
) -> TransportResult<()> {
    let mut name = String::new();
    let mut data = vec![];
    for line in lines {
        if let Some(value) = line.strip_prefix(b"event:") {
            name = String::from_utf8_lossy(value).trim().into();
        } else if let Some(value) = line.strip_prefix(b"data:") {
            data.push(
                value
                    .iter()
                    .copied()
                    .skip_while(|b| *b == b' ')
                    .collect::<Vec<_>>(),
            );
        }
    }
    if data.is_empty() {
        return Ok(());
    }
    let raw = data.join(&b'\n');
    let raw = raw.trim_ascii();
    if raw == b"[DONE]" {
        if collector.protocol == Some("chat_completions") && !collector.text().is_empty() {
            collector.completed = true;
            collector.end_reason = "done".into();
        }
        return Ok(());
    }
    let event: Value =
        serde_json::from_slice(raw).map_err(|_| Failure::new("invalid_stream", true))?;
    received(collector, diag, &event, &name, false, started, callback).await
}
pub async fn execute(
    target: &Target,
    prompt: &str,
    expected: usize,
    client: &reqwest::Client,
    callback: Option<Progress>,
    diag: &mut Value,
) -> TransportResult<(String, Option<String>)> {
    execute_request(target, prompt, expected, client, callback, diag, false).await
}

pub async fn execute_probe(
    target: &Target,
    prompt: &str,
    client: &reqwest::Client,
    diag: &mut Value,
) -> TransportResult<i64> {
    execute_request(target, prompt, 3000, client, None, diag, true).await?;
    integer(&diag["input_tokens"]).ok_or_else(|| Failure::new("no_usage", false))
}

async fn execute_request(
    target: &Target,
    prompt: &str,
    expected: usize,
    client: &reqwest::Client,
    callback: Option<Progress>,
    diag: &mut Value,
    probe: bool,
) -> TransportResult<(String, Option<String>)> {
    let started = Instant::now();
    let mut collector = Collector::new(expected);
    let mut payload = target.payload(prompt);
    if probe {
        payload
            .as_object_mut()
            .unwrap()
            .shift_remove("max_output_tokens");
        payload
            .as_object_mut()
            .unwrap()
            .shift_remove("max_completion_tokens");
        if target.protocol == "chat_completions" {
            payload.as_object_mut().unwrap().shift_remove("max_tokens");
            payload["stream_options"] = json!({"include_usage":true});
        }
    }
    *diag = json!({"protocol":target.protocol,"bytes":0,"events":0});
    if !target.oauth && !probe {
        diag["max_output_tokens"] = 4096.into();
    }
    let result = async {
        let response = client
            .post(&target.url)
            .headers(target.headers.clone())
            .json(&payload)
            .send()
            .await
            .map_err(|_| Failure::new("network_error", true))?;
        let status = response.status().as_u16();
        diag["http_status"] = status.into();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect::<String>();
        diag["content_type"] = crate::security::clean(&json!(content_type), 80).into();
        diag["headers_ms"] = json!(started.elapsed().as_millis());
        if let Some(callback) = &callback {
            callback(json!({"status":"receiving","diagnostics":diag}))
                .await
                .map_err(|_| Failure::new("internal_error", false))?;
        }
        let mut chunks = response.bytes_stream();
        if status != 200 {
            let mut raw = vec![];
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(|_| Failure::new("network_error", true))?;
                raw.extend(chunk.iter().take(MAX_TEXT - raw.len()));
                if raw.len() >= MAX_TEXT {
                    break;
                }
            }
            return Err(failure(
                status,
                &serde_json::from_slice(&raw).unwrap_or(json!({})),
            ));
        }
        let mut buffer = vec![];
        let mut lines = vec![];
        let mut frame_size = 0;
        let mut mode = None;
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| Failure::new("network_error", true))?;
            diag["bytes"] = json!(integer(&diag["bytes"]).unwrap_or(0) + chunk.len() as i64);
            buffer.extend_from_slice(&chunk);
            if mode.is_none() && !buffer.trim_ascii().is_empty() {
                mode = Some(buffer.trim_ascii_start().starts_with(b"{"));
            }
            if mode == Some(true) {
                if buffer.len() > MAX_FRAME {
                    return Err(Failure::new("output_limit", true));
                }
                continue;
            }
            while let Some(index) = buffer.iter().position(|v| *v == b'\n') {
                let mut line = buffer.drain(..=index).collect::<Vec<_>>();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                frame_size += line.len();
                if frame_size > MAX_FRAME {
                    return Err(Failure::new("output_limit", true));
                }
                if line.is_empty() {
                    frame(&lines, &mut collector, diag, started, &callback).await?;
                    lines.clear();
                    frame_size = 0;
                    if collector.completed && (!probe || integer(&diag["input_tokens"]).is_some()) {
                        return Ok((collector.text(), collector.returned_model.clone()));
                    }
                } else {
                    lines.push(line);
                }
            }
            if buffer.len() + frame_size > MAX_FRAME {
                return Err(Failure::new("output_limit", true));
            }
        }
        if mode == Some(true) {
            let event = serde_json::from_slice(&buffer)
                .map_err(|_| Failure::new("invalid_stream", true))?;
            received(&mut collector, diag, &event, "", true, started, &callback).await?;
        } else {
            if !buffer.is_empty() {
                if buffer.last() == Some(&b'\r') {
                    buffer.pop();
                }
                lines.push(buffer);
            }
            frame(&lines, &mut collector, diag, started, &callback).await?;
        }
        if collector.completed {
            Ok((collector.text(), collector.returned_model.clone()))
        } else {
            Err(Failure::new(
                if collector.recognized {
                    "incomplete_stream"
                } else {
                    "protocol_mismatch"
                },
                collector.recognized,
            ))
        }
    }
    .await;
    diag["duration_ms"] = json!(started.elapsed().as_millis());
    diag["end_reason"] = match &result {
        Ok(_) => collector.end_reason,
        Err(failure) => failure.code.into(),
    }
    .into();
    result
}
