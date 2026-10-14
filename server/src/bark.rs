use crate::{
    compat::{integer, parse_datetime, string, text, truthy},
    error::{Error, Result},
    security::sanitize,
    settings::Settings,
    store::read_config,
    upstream::read_limited,
};
use serde_json::{json, Value};
use std::{
    net::IpAddr,
    sync::{Arc, RwLock},
    time::Duration,
};
pub const DEFAULT_URL: &str = "https://api.day.app";
pub fn normalize_url(value: &str) -> Result<String> {
    let raw = value.trim();
    let invalid = || Error::new(422, "Bark 服务 URL 无效；HTTP 仅允许 loopback");
    if raw.is_empty()
        || raw
            .chars()
            .any(|c| c.is_whitespace() || c < ' ' || c == '\u{7f}')
    {
        return Err(invalid());
    }
    let parsed = reqwest::Url::parse(raw).map_err(|_| invalid())?;
    let hostname = parsed
        .host_str()
        .ok_or_else(invalid)?
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_lowercase();
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid());
    }
    if parsed.scheme() == "http"
        && hostname != "localhost"
        && !hostname.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    {
        return Err(invalid());
    }
    let path = parsed.path().trim_end_matches('/');
    let bytes = path.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
        {
            return Err(invalid());
        }
    }
    let mut result = parsed.clone();
    result.set_path(path);
    Ok(result.as_str().trim_end_matches('/').to_owned())
}
pub fn split_chunks(text: &str, max: usize) -> Result<Vec<String>> {
    if max == 0 {
        return Err(Error::invalid());
    }
    let mut output = vec![];
    let mut current = String::new();
    for ch in text.chars() {
        if !current.is_empty() && current.len() + ch.len_utf8() > max {
            output.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }
    if !current.is_empty() || output.is_empty() {
        output.push(current);
    }
    Ok(output)
}
pub fn body_parts(text: &str, max: usize) -> Result<Vec<String>> {
    let mut chunks = split_chunks(text, max)?;
    if chunks.len() == 1 {
        return Ok(chunks);
    }
    let mut total = chunks.len();
    loop {
        let prefix = format!("第 {total}/{total} 段\n").len();
        if prefix >= max {
            return Err(Error::invalid());
        }
        chunks = split_chunks(text, max - prefix)?;
        if chunks.len() == total {
            break;
        }
        total = chunks.len();
    }
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(i, v)| format!("第 {}/{total} 段\n{v}", i + 1))
        .collect())
}
pub fn beijing_time(value: &Value) -> String {
    if value.is_null() || value == "" {
        "-".into()
    } else if let Some(at) = parse_datetime(value) {
        at.with_timezone(&chrono_tz::Asia::Shanghai)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    } else {
        sanitize(&text(value), 120)
    }
}
fn safe(value: &Value, limit: usize) -> String {
    sanitize(
        &if truthy(value) {
            text(value)
        } else {
            String::new()
        },
        limit,
    )
}
pub fn oauth_message(event: &Value) -> (String, String) {
    let status = string(&event["status"]);
    let credit = &event["reset_credit"];
    if status == "recovered"
        && event["test_success"] == true
        && truthy(&event["recovered_at"])
        && credit["consumed"] == true
        && truthy(&credit["completed_at"])
    {
        return (
            "✅ OAuth 用卡成功，已恢复调度".into(),
            format!(
                "账号：{} #{}\n验证方式：{}\n模型：{}\n用卡时间：{}\n恢复时间：{}",
                safe(&event["account_name"], 120),
                integer(&event["account_id"]).unwrap_or(0),
                if credit["verification_method"] == "model" {
                    "模型测试"
                } else {
                    "测试连接"
                },
                safe(&event["model_id"], 120),
                beijing_time(&credit["completed_at"]),
                beijing_time(&event["recovered_at"])
            ),
        );
    }
    let title = match status {
        "recovered" => "OAuth 账号额度已恢复可用",
        "test_failed" => "OAuth 账号额度恢复后测试失败",
        "daily_test_failed" => "OAuth 账号定时测活失败",
        "recovery_failed" => "OAuth 账号自动恢复失败",
        _ => "OAuth 账号认证异常",
    };
    let labels = event["window_labels"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| safe(v, 80))
        .collect::<Vec<_>>()
        .join(" / ");
    let mut lines = vec![
        format!("账号 ID：{}", integer(&event["account_id"]).unwrap_or(0)),
        format!("名称：{}", safe(&event["account_name"], 120)),
        format!(
            "套餐：{}",
            if truthy(&event["plan_type"]) {
                safe(&event["plan_type"], 80)
            } else {
                "oauth".into()
            }
        ),
        format!("窗口：{}", if labels.is_empty() { "-" } else { &labels }),
        format!("模型：{}", safe(&event["model_id"], 120)),
        format!("错误码：{}", safe(&event["error_code"], 120)),
    ];
    if matches!(
        status,
        "test_failed" | "daily_test_failed" | "recovery_failed" | "auth_failed"
    ) || status.is_empty()
    {
        lines.push(format!("错误：{}", safe(&event["error"], 400)));
    }
    if status == "auth_failed" || status.is_empty() {
        lines.push(format!(
            "阶段：{}",
            if event["stage"] == "active_usage" {
                "active usage".into()
            } else {
                safe(&event["stage"], 80)
            }
        ));
        lines.push(format!("时间：{}", beijing_time(&event["checked_at"])));
    }
    (title.into(), lines.join("\n"))
}
pub fn credit_message(event: &Value) -> (String, String) {
    let outcome = string(&event["outcome"]);
    let label = result_label(outcome).unwrap_or("待确认");
    let title = if outcome == "restored" {
        "✅ OAuth 用卡成功，已恢复调度".into()
    } else {
        format!(
            "⚠️ OAuth {}",
            result_label(outcome).unwrap_or("用卡结果待确认")
        )
    };
    let mut lines = vec![
        format!(
            "账号：{} #{}",
            safe(&event["account_name"], 120),
            integer(&event["account_id"]).unwrap_or(0)
        ),
        format!("触发时间：{}", beijing_time(&event["trigger_at"])),
        format!(
            "执行时间：{}",
            if truthy(&event["attempt_at"]) {
                beijing_time(&event["attempt_at"])
            } else {
                "未执行".into()
            }
        ),
        format!(
            "验证方式：{}",
            if event["verification_method"] == "model" {
                "模型测试"
            } else {
                "测试连接"
            }
        ),
        format!("结果：{label}"),
    ];
    if truthy(&event["reason"]) && outcome != "restored" {
        lines.push(format!("原因：{}", safe(&event["reason"], 240)));
    }
    if truthy(&event["degradation_confirmed"]) {
        lines.push("模型验证已判定降智，降智标记与停止调度均已确认".into());
        if truthy(&event["prediction"]) {
            lines.push(format!("首组指纹推测：{}", safe(&event["prediction"], 120)));
        }
    }
    if truthy(&event["model_id"]) {
        lines.push(format!("模型：{}", safe(&event["model_id"], 120)));
    }
    if truthy(&event["reset_completed_at"]) {
        lines.push(format!(
            "用卡完成时间：{}",
            beijing_time(&event["reset_completed_at"])
        ));
    }
    lines.push(format!(
        "结束时间：{}",
        beijing_time(&event["completed_at"])
    ));
    (title, lines.join("\n"))
}
pub fn result_label(outcome: &str) -> Option<&'static str> {
    match outcome {
        "restored" => Some("用卡成功，已恢复调度"),
        "not_executed" => Some("未执行用卡"),
        "consume_failed" => Some("用卡明确失败"),
        "unrecovered" => Some("用卡成功，恢复未完成"),
        "unknown" => Some("消费结果不确定，禁止重复用卡"),
        _ => None,
    }
}
#[derive(Clone)]
pub struct Config {
    pub enabled: bool,
    pub valid: bool,
    pub key: String,
    pub url: String,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct PushResult {
    pub success: bool,
    pub error_code: String,
    pub sent_parts: usize,
    pub total_parts: usize,
}
impl PushResult {
    fn failed(code: &str, sent: usize, total: usize) -> Self {
        Self {
            success: false,
            error_code: code.into(),
            sent_parts: sent,
            total_parts: total,
        }
    }
}
#[derive(Clone)]
pub struct Bark {
    pub config: Arc<RwLock<Config>>,
    client: reqwest::Client,
    settings: Arc<Settings>,
}
impl Bark {
    pub fn new(settings: Arc<Settings>) -> Result<Self> {
        let config = Config {
            enabled: settings.bark["enabled"] == true,
            valid: settings.bark_valid,
            key: string(&settings.bark["device_key"]).into(),
            url: string(&settings.bark["server_url"]).into(),
        };
        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            settings,
        })
    }
    pub fn runtime(&self) -> Config {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn configure(&self, value: &Value, valid: bool) {
        let mut config = self.config.write().unwrap_or_else(|e| e.into_inner());
        config.valid = valid;
        config.enabled = value["enabled"] == true;
        config.key = string(&value["device_key"]).into();
        config.url = string(&value["server_url"]).into();
    }
    pub fn panel(&self) -> Result<Value> {
        let current = self.runtime();
        // An invalid optional notification config must remain visible in the
        // panel. Its validity was captured when loading the runtime config;
        // reading its metadata must not hide the rest of the settings page.
        let raw = read_config(&self.settings.bark_path).unwrap_or(json!({}));
        let url_valid = normalize_url(&current.url).is_ok();
        let key_set = !current.key.trim().is_empty();
        Ok(
            json!({"configured":current.valid && current.enabled && key_set && url_valid,"config_valid":current.valid,"enabled":current.enabled,"device_key_set":key_set,"device_key_status":if key_set {"已设置"} else {"未设置"},"server_url_valid":url_valid,"config_updated_at":raw["updated_at"]}),
        )
    }
    pub async fn push(&self, title: &str, body: &str, seconds: u64, options: &Value) -> PushResult {
        self.push_with_config(title, body, seconds, options, &self.runtime())
            .await
    }
    pub async fn push_with_config(
        &self,
        title: &str,
        body: &str,
        seconds: u64,
        options: &Value,
        config: &Config,
    ) -> PushResult {
        if self.settings.read_only {
            return PushResult::failed("read_only", 0, 0);
        }
        if !config.valid {
            return PushResult::failed("invalid_config", 0, 0);
        }
        let key = config.key.trim();
        if key.is_empty() {
            return PushResult::failed("missing_device_key", 0, 0);
        }
        let Ok(url) = normalize_url(if config.url.is_empty() {
            DEFAULT_URL
        } else {
            &config.url
        }) else {
            return PushResult::failed("invalid_server_url", 0, 0);
        };
        let Ok(chunks) = body_parts(body, 2400) else {
            return PushResult::failed("invalid_payload_encoding", 0, 0);
        };
        let total = chunks.len();
        for (index, body) in chunks.into_iter().enumerate() {
            let title = if total > 1 {
                format!("{title} ({}/{total})", index + 1)
            } else {
                title.to_owned()
            };
            let mut payload = json!({"device_key":key,"title":title,"body":body});
            for field in ["level", "sound", "group"] {
                if let Some(value) = options.get(field) {
                    payload[field] = value.clone();
                }
            }
            let response = match self
                .client
                .post(format!("{url}/push"))
                .json(&payload)
                .timeout(Duration::from_secs(seconds))
                .send()
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    return PushResult::failed(
                        if e.is_timeout() {
                            "timeout"
                        } else {
                            "request_error"
                        },
                        index,
                        total,
                    )
                }
            };
            if !response.status().is_success() {
                return PushResult::failed("http_status", index, total);
            }
            let raw = match read_limited(response, 65536).await {
                Ok(v) => v,
                Err(_) => return PushResult::failed("response_decode", index, total),
            };
            let body: Value = match serde_json::from_slice(&raw) {
                Ok(v) => v,
                Err(_) => return PushResult::failed("response_decode", index, total),
            };
            if body["code"] != 200 {
                return PushResult::failed("bark_response_code", index, total);
            }
        }
        PushResult {
            success: true,
            error_code: String::new(),
            sent_parts: total,
            total_parts: total,
        }
    }
    pub async fn notify_oauth(&self, events: &[Value], config: Option<&Config>) -> Vec<Value> {
        let runtime = config.cloned().unwrap_or_else(|| self.runtime());
        if !runtime.valid || !runtime.enabled {
            return vec![];
        }
        let mut delivered = vec![];
        for event in events {
            if !event.is_object()
                || truthy(&event["account_id"]) && integer(&event["account_id"]).is_none()
            {
                continue;
            }
            let (title, body) = oauth_message(event);
            if self
                .push_with_config(&title, &body, 3, &Value::Null, &runtime)
                .await
                .success
            {
                delivered.push(event.clone());
            }
        }
        delivered
    }
    pub async fn notify_credit(&self, event: &Value, config: Option<&Config>) -> bool {
        let runtime = config.cloned().unwrap_or_else(|| self.runtime());
        if !runtime.valid || !runtime.enabled {
            return false;
        }
        let (title, body) = credit_message(event);
        self.push_with_config(
            &title,
            &body,
            3,
            &json!({"level":"critical","sound":"alarm","group":"oauth-reset-credit"}),
            &runtime,
        )
        .await
        .success
    }
}
