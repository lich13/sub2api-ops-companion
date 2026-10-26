//! Read-only API Key billing protocols ported from lich13-switch.
//! Credentials stay on the server; display evidence never changes scheduling.
use crate::{
    compat::{config_revision, string as text_value},
    error::{Error, Result},
    store::{valid_object, Store},
    validation,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{watch, Semaphore};
const LIMIT: usize = 2_000_000;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Debug, Serialize, Default, PartialEq)]
pub struct Plan {
    pub name: String,
    pub remaining: Option<f64>,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub unit: String,
    pub unlimited: bool,
    pub reset_at: Option<String>,
}
#[derive(Clone, Serialize, PartialEq)]
pub struct Subscription {
    pub name: Option<String>,
    pub status: Option<String>,
    pub expires_at: Option<String>,
    pub active: Option<bool>,
    pub plans: Vec<Plan>,
}
#[derive(Clone, Serialize, Default)]
pub struct Usage {
    pub requests: Option<f64>,
    pub tokens: Option<f64>,
    pub cost: Option<f64>,
}
#[derive(Clone, Serialize)]
pub struct QuotaView {
    #[serde(skip_serializing)]
    pub version: String,
    pub state: String,
    pub source: Option<String>,
    pub checked_at: Option<u64>,
    pub success_at: Option<u64>,
    pub retry_at: Option<u64>,
    pub next_refresh_at: Option<u64>,
    pub stale: bool,
    pub error: Option<String>,
    pub key_status: Option<String>,
    pub plans: Vec<Plan>,
    pub subscriptions: Vec<Subscription>,
    pub expires_at: Option<String>,
    pub expires_at_unix: Option<i64>,
    pub today: Option<Usage>,
    pub total_usage: Option<Usage>,
}
impl QuotaView {
    fn empty(version: &str) -> Self {
        Self {
            version: version.into(),
            state: "idle".into(),
            source: None,
            checked_at: None,
            success_at: None,
            retry_at: None,
            next_refresh_at: None,
            stale: false,
            error: None,
            key_status: None,
            plans: vec![],
            subscriptions: vec![],
            expires_at: None,
            expires_at_unix: None,
            today: None,
            total_usage: None,
        }
    }
}

struct Entry {
    view: QuotaView,
    flight: Option<watch::Receiver<Option<QuotaView>>>,
}
struct Shared {
    entries: Mutex<HashMap<String, Entry>>,
    permits: Semaphore,
}
#[derive(Clone)]
pub struct Service {
    shared: Arc<Shared>,
    config: Store,
    client: reqwest::Client,
    tasks: crate::tasks::Tasks,
}
pub struct Query {
    pub id: String,
    pub version: String,
    pub base: String,
    pub token: String,
    pub quota_access_token: Option<String>,
    client: reqwest::Client,
}
fn seconds(raw: &Value) -> Result<u64> {
    raw["seconds"]
        .as_u64()
        .filter(|n| *n == 0 || (10..=86400).contains(n))
        .ok_or_else(|| Error::new(503, "余额刷新配置无效"))
}
fn token(raw: &Value, id: i64) -> Result<Option<String>> {
    match raw["accounts"].get(id.to_string()) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if !value.is_empty()
                && value.len() <= 8192
                && value.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            Ok(Some(value.clone()))
        }
        _ => Err(Error::new(503, "余额查询凭据配置无效")),
    }
}
pub fn settings_request(value: &Value, account: bool) -> Result<()> {
    validation::fields(
        value,
        if account {
            &["expected_revision", "access_token"]
        } else {
            &["expected_revision", "seconds"]
        },
        if account {
            &["expected_revision"]
        } else {
            &["expected_revision", "seconds"]
        },
    )?;
    validation::text(&value["expected_revision"], 64, 64)?;
    if account {
        if let Some(value) = value.get("access_token") {
            let v = validation::text(value, 0, 8192)?;
            if !v.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(Error::new(422, "账户访问令牌格式无效"));
            }
        }
    } else if seconds(value).is_err() {
        return Err(Error::new(422, "余额刷新间隔需为 10–86400 秒，或 0 关闭"));
    }
    Ok(())
}
impl Service {
    pub fn new(path: PathBuf, read_only: bool) -> Result<Self> {
        let service = Self {
            shared: Arc::new(Shared {
                entries: Mutex::new(HashMap::new()),
                permits: Semaphore::new(3),
            }),
            config: Store::new(
                path,
                json!({"seconds":60,"accounts":{}}),
                valid_object,
                read_only,
            ),
            tasks: crate::tasks::Tasks::default(),
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .build()?,
        };
        service.settings()?;
        Ok(service)
    }
    pub(crate) fn supervise(&mut self, supervisor: crate::supervision::Supervisor) {
        self.tasks = crate::tasks::Tasks::supervised(supervisor);
    }
    pub async fn close(&self) {
        self.shared.permits.close();
        self.tasks.close(0).await;
        self.shared
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
    pub fn forget(&self, id: i64) {
        self.shared
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id.to_string());
    }
    pub(crate) fn retain_accounts(&self, rows: &[Value]) {
        let ids: std::collections::HashSet<String> = rows
            .iter()
            .filter_map(|row| row["id"].as_i64().map(|id| id.to_string()))
            .collect();
        self.shared
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id, _| ids.contains(id));
    }
    pub fn settings(&self) -> Result<Value> {
        let raw = self.config.read()?;
        let seconds = seconds(&raw)?;
        if !raw["accounts"].is_object() {
            return Err(Error::new(503, "余额查询配置无效"));
        }
        Ok(json!({"seconds":seconds,"revision":config_revision(&json!(seconds))}))
    }
    pub fn account_settings(&self, id: i64) -> Result<Value> {
        let token = token(&self.config.read()?, id)?;
        Ok(
            json!({"account_id":id,"has_access_token":token.is_some(),"revision":config_revision(&json!([id,token]))}),
        )
    }
    pub fn account_access_token(&self, id: i64) -> Result<Value> {
        let value = token(&self.config.read()?, id)?
            .ok_or_else(|| Error::new(404, "尚未配置账户访问令牌"))?;
        Ok(json!({"account_id":id,"access_token":value}))
    }
    pub fn save_settings(&self, value: &Value, id: Option<i64>) -> Result<Value> {
        settings_request(value, id.is_some())?;
        let changed = self.config.transaction(|raw| {
            let revision = if let Some(id) = id {
                config_revision(&json!([id, token(raw, id)?]))
            } else {
                config_revision(&json!(seconds(raw)?))
            };
            if value["expected_revision"] != revision {
                return Err(Error::new(409, "余额设置已变化，请重新读取后保存"));
            }
            if let Some(id) = id {
                if let Some(value) = value.get("access_token") {
                    let accounts = raw["accounts"]
                        .as_object_mut()
                        .ok_or_else(|| Error::new(503, "余额查询配置无效"))?;
                    if value == "" {
                        accounts.remove(&id.to_string());
                    } else {
                        accounts.insert(id.to_string(), value.clone());
                    }
                }
            } else {
                raw["seconds"] = value["seconds"].clone();
            }
            Ok(if let Some(id) = id {
                config_revision(&json!([id, token(raw, id)?])) != revision
            } else {
                false
            })
        })?;
        if let Some(id) = id {
            if changed {
                self.shared.entries.lock().unwrap().remove(&id.to_string());
            }
            self.account_settings(id)
        } else {
            self.settings()
        }
    }
    pub fn input(&self, id: i64, credentials: &Value, owner: &Value) -> Result<Query> {
        let access = token(&self.config.read()?, id)?;
        let base = text_value(&credentials["base_url"])
            .trim_end_matches('/')
            .to_owned();
        let key = text_value(&credentials["api_key"]).to_owned();
        Ok(Query {
            id: id.to_string(),
            version: config_revision(&json!([owner, base, key, access])),
            base,
            token: key,
            quota_access_token: access,
            client: self.client.clone(),
        })
    }
    fn schedule(view: &mut QuotaView, interval: u64) {
        view.next_refresh_at = (interval > 0).then(|| {
            view.checked_at
                .unwrap_or(0)
                .saturating_add(interval)
                .max(view.retry_at.unwrap_or(0))
        });
        view.stale =
            view.success_at.is_some() && (view.state == "error" || view.state == "unsupported");
    }
    pub async fn query(&self, input: Query, force: bool) -> Result<QuotaView> {
        if self.tasks.is_closed() {
            return Err(Error::new(503, "服务正在停止"));
        }
        let interval = self.settings()?["seconds"].as_u64().unwrap();
        let mut rx = {
            let mut entries = self.shared.entries.lock().unwrap();
            let e = entries.entry(input.id.clone()).or_insert_with(|| Entry {
                view: QuotaView::empty(&input.version),
                flight: None,
            });
            if e.view.version != input.version {
                *e = Entry {
                    view: QuotaView::empty(&input.version),
                    flight: None,
                };
            }
            Self::schedule(&mut e.view, interval);
            if let Some(rx) = &e.flight {
                rx.clone()
            } else if e.view.retry_at.is_some_and(|at| at > now())
                || (!force
                    && (interval == 0 || e.view.next_refresh_at.is_some_and(|at| at > now())))
            {
                return Ok(e.view.clone());
            } else {
                let previous = e.view.clone();
                let (tx, rx) = watch::channel(None);
                e.flight = Some(rx.clone());
                let service = self.clone();
                let flight = rx.clone();
                if let Err(error) = self.tasks.spawn(async move {
                    let shared = service.shared.clone();
                    let id = input.id.clone();
                    let _cleanup = crate::supervision::on_drop(move || {
                        if let Some(entry) = shared
                            .entries
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .get_mut(&id)
                        {
                            if entry
                                .flight
                                .as_ref()
                                .is_some_and(|current| current.same_channel(&flight))
                            {
                                entry.flight = None;
                            }
                        }
                    });
                    let Ok(_permit) = service.shared.permits.acquire().await else {
                        return;
                    };
                    if service.tasks.is_closed() {
                        return;
                    }
                    let result = if input.token.is_empty() {
                        Err(Failure::new("账号 API Key 未配置"))
                    } else {
                        fetch(&input).await
                    };
                    let mut view = previous;
                    view.checked_at = Some(now());
                    view.retry_at = None;
                    match result {
                        Ok(Some(data)) => {
                            view.source = Some(data.source.into());
                            view.plans = data.plans;
                            view.subscriptions = data.subscriptions;
                            view.key_status = data.key_status;
                            view.expires_at = data.expires_at;
                            view.expires_at_unix = data.expires_at_unix;
                            view.today = data.today;
                            view.total_usage = data.total_usage;
                            view.state = "ok".into();
                            view.error = data.warning;
                            view.retry_at = data.retry_at;
                            view.success_at = view.checked_at;
                        }
                        Ok(None) => {
                            view.state = "unsupported".into();
                            view.error =
                                Some("不可查询：未识别 Sub2API 或 New API 额度接口".into());
                        }
                        Err(error) => {
                            view.state = "error".into();
                            view.error = Some(error.message);
                            view.retry_at = error.retry_at;
                        }
                    }
                    Self::schedule(&mut view, interval);
                    if let Some(e) = service
                        .shared
                        .entries
                        .lock()
                        .unwrap()
                        .get_mut(&input.id)
                        .filter(|e| {
                            e.view.version == input.version
                                && e.flight
                                    .as_ref()
                                    .is_some_and(|current| current.same_channel(&tx.subscribe()))
                        })
                    {
                        e.view = view.clone();
                        e.flight = None;
                    }
                    tx.send_replace(Some(view));
                }) {
                    e.flight = None;
                    return Err(error);
                }
                rx
            }
        };
        loop {
            if let Some(view) = rx.borrow().clone() {
                return Ok(view);
            }
            rx.changed()
                .await
                .map_err(|_| Error::new(502, "余额查询已中断，请重试"))?;
        }
    }
}
struct Failure {
    message: String,
    retry_at: Option<u64>,
}
impl Failure {
    fn new(s: &str) -> Self {
        Self {
            message: s.into(),
            retry_at: None,
        }
    }
}
struct Reply {
    status: u16,
    body: Option<Value>,
}

async fn get(input: &Query, url: String, auth: bool) -> std::result::Result<Reply, Failure> {
    get_with_token(input, url, auth.then_some(input.token.as_str())).await
}
async fn get_with_token(
    input: &Query,
    url: String,
    token: Option<&str>,
) -> std::result::Result<Reply, Failure> {
    let request = async {
        let mut req = input
            .client
            .get(url)
            .header("Accept", "application/json")
            .header("Accept-Encoding", "identity")
            .header("User-Agent", concat!("Sub2Ops/", env!("CARGO_PKG_VERSION")));
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let mut response = req
            .send()
            .await
            .map_err(|_| Failure::new("额度接口连接失败，请检查网络或证书"))?;
        let status = response.status().as_u16();
        if status == 429 || (status >= 400 && response.headers().contains_key("retry-after")) {
            let retry = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    let value = v.trim();
                    let duration = value
                        .parse::<f64>()
                        .ok()
                        .filter(|n| {
                            n.is_finite()
                                && *n >= 0.0
                                && value.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                        })
                        .map(|n| n.ceil().max(1.0) as u64)
                        .or_else(|| {
                            httpdate::parse_http_date(value).ok().map(|at| {
                                at.duration_since(SystemTime::now())
                                    .unwrap_or_default()
                                    .as_secs()
                                    .saturating_add(1)
                            })
                        });
                    duration.map(|seconds| now().saturating_add(seconds))
                });
            return Err(Failure {
                message: if status == 429 {
                    "额度查询被限流"
                } else {
                    "额度服务暂时不可用"
                }
                .into(),
                retry_at: Some(retry.unwrap_or_else(|| now() + 60)),
            });
        }
        if (300..400).contains(&status) {
            return Err(Failure::new("额度接口要求跳转，已停止发送凭据"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Failure::new("额度响应中断"))?
        {
            if bytes.len() + chunk.len() > LIMIT {
                return Err(Failure::new("额度响应超过 2 MB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Reply {
            status,
            body: serde_json::from_slice(&bytes).ok().map(|mut value| {
                redact_echoed_key(&mut value, &input.token);
                if let Some(token) = &input.quota_access_token {
                    redact_echoed_key(&mut value, token);
                }
                value
            }),
        })
    };
    tokio::time::timeout(Duration::from_secs(10), request)
        .await
        .map_err(|_| Failure::new("额度查询超时"))?
}
fn redact_echoed_key(value: &mut Value, key: &str) {
    if key.is_empty() {
        return;
    }
    match value {
        Value::String(s) => *s = s.replace(key, "[已隐藏]"),
        Value::Array(values) => values.iter_mut().for_each(|v| redact_echoed_key(v, key)),
        Value::Object(values) => values.values_mut().for_each(|v| redact_echoed_key(v, key)),
        _ => {}
    }
}
fn urls(base: &str) -> std::result::Result<(String, String, String), Failure> {
    let mut u = reqwest::Url::parse(base).map_err(|_| Failure::new("供应商地址无效"))?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(Failure::new("供应商地址无效"));
    }
    let path = u.path().trim_end_matches('/').to_owned();
    let prefix = path.strip_suffix("/v1").unwrap_or(&path);
    let sub = if path.ends_with("/v1") {
        format!("{path}/usage")
    } else {
        format!("{path}/v1/usage")
    };
    u.set_path(&sub);
    let a = u.to_string();
    u.set_path(&format!("{prefix}/api/usage/token/"));
    let b = u.to_string();
    u.set_path(&format!("{prefix}/api/status"));
    Ok((a, b, u.to_string()))
}
#[derive(Default)]
struct Data {
    source: &'static str,
    plans: Vec<Plan>,
    subscriptions: Vec<Subscription>,
    key_status: Option<String>,
    expires_at: Option<String>,
    expires_at_unix: Option<i64>,
    today: Option<Usage>,
    total_usage: Option<Usage>,
    warning: Option<String>,
    retry_at: Option<u64>,
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|x| x.is_finite())
}
fn n(v: &Value, k: &str) -> Option<f64> {
    number(&v[k])
}
fn string(v: &Value, k: &str) -> Option<String> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().filter(|c| !c.is_control()).take(120).collect())
}
fn metadata_text(v: &Value, key: &str) -> Option<String> {
    let text = v[key].as_str()?.trim();
    if text.chars().count() > 80 || text.chars().any(char::is_control) {
        return None;
    }
    let safe = crate::security::clean(&json!(text), 320);
    (!safe.is_empty() && safe == text && !safe.contains("[已隐藏]")).then_some(safe)
}
fn subscription_expiry(value: &Value) -> Option<String> {
    if let Some(seconds) = value.as_i64().filter(|s| *s > 0) {
        return chrono::DateTime::from_timestamp(seconds, 0).map(|d| d.to_rfc3339());
    }
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|d| d.to_rfc3339())
}
// Explicit subscription metadata only. A token's name, balance or expiration
// never proves which subscription it belongs to.
fn subscriptions(value: &Value, unit: &str) -> Vec<Subscription> {
    let nodes: Vec<&Value> = if let Some(items) = value["subscriptions"].as_array() {
        items.iter().filter(|v| v.is_object()).take(32).collect()
    } else if value["subscription"].is_object() {
        vec![&value["subscription"]]
    } else if metadata_text(value, "planName")
        .or_else(|| metadata_text(value, "plan_name"))
        .is_some()
    {
        vec![value]
    } else {
        vec![]
    };
    let single = nodes.len() == 1;
    nodes
        .into_iter()
        .map(|node| {
            let sub = node
                .get("subscription")
                .filter(|v| v.is_object())
                .unwrap_or(node);
            let root = std::ptr::eq(node, value);
            let name = metadata_text(node, "planName")
                .or_else(|| metadata_text(node, "plan_name"))
                .or_else(|| metadata_text(&node["plan"], "name"))
                .or_else(|| metadata_text(&node["plan"], "title"))
                .or_else(|| (!root).then(|| metadata_text(sub, "name")).flatten())
                .or_else(|| {
                    single
                        .then(|| {
                            metadata_text(value, "planName")
                                .or_else(|| metadata_text(value, "plan_name"))
                        })
                        .flatten()
                });
            let status = (!root).then(|| metadata_text(sub, "status")).flatten();
            let active =
                (!root)
                    .then(|| sub["active"].as_bool())
                    .flatten()
                    .or(match status.as_deref() {
                        Some("active") => Some(true),
                        Some("expired" | "disabled" | "cancelled" | "canceled") => Some(false),
                        _ => None,
                    });
            let expires_at = (!root)
                .then(|| {
                    ["expires_at", "expiresAt", "end_time"]
                        .iter()
                        .find_map(|key| subscription_expiry(&sub[*key]))
                })
                .flatten();
            let mut plans = Vec::new();
            for (label, used, total) in [
                ("每日", "daily_usage_usd", "daily_limit_usd"),
                ("每周", "weekly_usage_usd", "weekly_limit_usd"),
                ("每月", "monthly_usage_usd", "monthly_limit_usd"),
            ] {
                if let Some(total) = n(sub, total).filter(|v| *v >= 0.) {
                    let used = n(sub, used);
                    plans.push(plan(
                        label,
                        used.map(|u| (total - u).max(0.)),
                        used,
                        Some(total),
                        "USD",
                        false,
                    ));
                }
            }
            if let Some(total) = n(sub, "amount_total") {
                let used = n(sub, "amount_used");
                plans.push(plan(
                    "套餐额度",
                    used.map(|u| (total - u).max(0.)),
                    used,
                    Some(total),
                    &metadata_text(sub, "unit").unwrap_or_else(|| unit.into()),
                    false,
                ));
            }
            Subscription {
                name,
                status,
                expires_at,
                active,
                plans,
            }
        })
        .collect()
}
fn plan(
    name: &str,
    remaining: Option<f64>,
    used: Option<f64>,
    total: Option<f64>,
    unit: &str,
    unlimited: bool,
) -> Plan {
    Plan {
        name: name.into(),
        remaining: if unlimited { None } else { remaining },
        used,
        total,
        unit: unit.into(),
        unlimited,
        reset_at: None,
    }
}
fn usage(v: &Value) -> Option<Usage> {
    if !v.is_object() {
        return None;
    }
    let u = Usage {
        requests: n(v, "requests"),
        tokens: n(v, "total_tokens"),
        cost: n(v, "actual_cost").or_else(|| n(v, "cost")),
    };
    (u.requests.is_some() || u.tokens.is_some() || u.cost.is_some()).then_some(u)
}
fn sub2(v: &Value) -> Option<Data> {
    if !v.is_object() || v.get("error").is_some() {
        return None;
    }
    let status = v["status"].as_str().unwrap_or("");
    let known_status = matches!(
        status,
        "active" | "disabled" | "expired" | "quota_exhausted"
    );
    if !(v["subscription"].is_object()
        || v["subscriptions"].is_array()
        || v["quota"].is_object()
        || v["rate_limits"].is_array()
        || n(v, "remaining").is_some()
        || n(v, "balance").is_some()
        || (known_status && v["isValid"].is_boolean()))
    {
        return None;
    }
    let unit = string(v, "unit").unwrap_or_else(|| "USD".into());
    let mut d = Data {
        source: "sub2api",
        subscriptions: subscriptions(v, &unit),
        expires_at: string(v, "expires_at"),
        today: usage(&v["usage"]["today"]),
        total_usage: usage(&v["usage"]["total"]),
        ..Default::default()
    };
    d.key_status = match status {
        "disabled" => Some("已禁用"),
        "expired" => Some("已到期"),
        "quota_exhausted" => Some("额度用尽"),
        _ if v["isValid"] == false => Some("Key 无效"),
        _ => None,
    }
    .map(str::to_owned);
    if v["subscription"].is_object() {
        let sub = &v["subscription"];
        let mut windows = vec![];
        for (label, u, l) in [
            ("每日", "daily_usage_usd", "daily_limit_usd"),
            ("每周", "weekly_usage_usd", "weekly_limit_usd"),
            ("每月", "monthly_usage_usd", "monthly_limit_usd"),
        ] {
            if let Some(limit) = n(sub, l).filter(|x| *x > 0.) {
                let used = n(sub, u).unwrap_or(0.);
                windows.push(plan(
                    label,
                    Some((limit - used).max(0.)),
                    Some(used),
                    Some(limit),
                    &unit,
                    false,
                ));
            }
        }
        let unlimited = n(v, "remaining").is_some_and(|n| n < 0.);
        let worst = windows.iter().max_by(|a, b| {
            (a.used.unwrap_or(0.) / a.total.unwrap())
                .total_cmp(&(b.used.unwrap_or(0.) / b.total.unwrap()))
        });
        let remaining = n(v, "remaining")
            .or_else(|| windows.iter().filter_map(|p| p.remaining).reduce(f64::min));
        d.plans.push(plan(
            &d.subscriptions
                .first()
                .and_then(|s| s.name.clone())
                .unwrap_or_else(|| "订阅".into()),
            remaining,
            worst.and_then(|p| p.used),
            worst.and_then(|p| p.total),
            &unit,
            unlimited,
        ));
        d.plans.extend(windows);
    } else if v["quota"].is_object() || v["rate_limits"].is_array() {
        if v["quota"].is_object() {
            let q = &v["quota"];
            let total = n(q, "limit");
            let used = n(q, "used");
            let remaining = n(q, "remaining").or_else(|| Some((total? - used?).max(0.)));
            d.plans.push(plan(
                "API Key 配额",
                remaining,
                used,
                total,
                &string(q, "unit").unwrap_or_else(|| unit.clone()),
                total.is_some_and(|n| n <= 0.),
            ));
        }
        if let Some(rates) = v["rate_limits"].as_array() {
            let mut rates: Vec<_> = rates.iter().filter(|r| r.is_object()).collect();
            rates.sort_by_key(|r| match r["window"].as_str() {
                Some("5h") => 0,
                Some("1d") => 1,
                Some("7d") => 2,
                _ => 3,
            });
            for r in rates {
                let label = match r["window"].as_str() {
                    Some("5h") => "5 小时",
                    Some("1d") => "1 天",
                    Some("7d") => "7 天",
                    _ => "用量窗口",
                };
                let used = n(r, "used").unwrap_or(0.);
                let total = n(r, "limit");
                let remaining = n(r, "remaining").or_else(|| total.map(|t| (t - used).max(0.)));
                let mut p = plan(
                    label,
                    remaining,
                    Some(used),
                    total,
                    &string(r, "unit").unwrap_or_else(|| unit.clone()),
                    false,
                );
                p.reset_at = string(r, "reset_at");
                d.plans.push(p);
            }
        }
    } else if let Some(amount) = n(v, "remaining").or_else(|| n(v, "balance")) {
        d.plans.push(plan(
            "钱包余额",
            Some(amount),
            None,
            None,
            &unit,
            amount < 0.,
        ));
    }
    Some(d)
}
fn newapi(v: &Value) -> Option<Data> {
    let body = &v["data"];
    if !(v["code"] == true || v["success"] == true) || body["object"] != "token_usage" {
        return None;
    }
    let unlimited = body["unlimited_quota"] == true;
    let remaining = n(body, "total_available");
    let used = n(body, "total_used");
    let total = n(body, "total_granted");
    if !unlimited && remaining.is_none() {
        return None;
    }
    let expires = body["expires_at"].as_i64().filter(|t| *t > 0);
    let mut data = Data {
        source: "newapi",
        subscriptions: subscriptions(body, "额度单位"),
        plans: vec![plan(
            "Key 额度",
            remaining,
            used,
            total,
            "额度单位",
            unlimited,
        )],
        expires_at_unix: expires,
        key_status: expires
            .filter(|t| *t < now() as i64)
            .map(|_| "已到期".into()),
        ..Default::default()
    };
    if let Some(remaining) = n(&body["user_quota"], "total_available") {
        data.plans.insert(
            0,
            plan(
                "账户余额",
                Some(remaining),
                n(&body["user_quota"], "total_used"),
                None,
                "额度单位",
                false,
            ),
        );
    }
    Some(data)
}
fn convert(d: &mut Data, status: &Value) {
    let s = &status["data"];
    let Some(per) = n(s, "quota_per_unit").filter(|n| *n > 0.) else {
        return;
    };
    let kind = s["quota_display_type"]
        .as_str()
        .or_else(|| (s["display_in_currency"] == true).then_some("USD"));
    let (unit, rate) = match kind {
        Some("USD") => ("USD".to_string(), Some(1.)),
        Some("CNY") => ("CNY".to_string(), n(s, "usd_exchange_rate")),
        Some("CUSTOM") => (
            string(s, "custom_currency_symbol").unwrap_or_else(|| "自定义单位".into()),
            n(s, "custom_currency_exchange_rate"),
        ),
        _ => return,
    };
    let Some(rate) = rate.filter(|r| *r > 0.) else {
        return;
    };
    for p in &mut d.plans {
        p.unit = unit.clone();
        p.remaining = p.remaining.map(|n| n / per * rate);
        p.used = p.used.map(|n| n / per * rate);
        p.total = p.total.map(|n| n / per * rate);
    }
    for subscription in &mut d.subscriptions {
        for p in subscription
            .plans
            .iter_mut()
            .filter(|p| p.unit == "额度单位")
        {
            p.unit = unit.clone();
            p.remaining = p.remaining.map(|n| n / per * rate);
            p.used = p.used.map(|n| n / per * rate);
            p.total = p.total.map(|n| n / per * rate);
        }
    }
}
async fn fetch(input: &Query) -> std::result::Result<Option<Data>, Failure> {
    let (sub, new, status) = urls(&input.base)?;
    if let Some(token) = input.quota_access_token.as_deref() {
        let account = format!("{}/user/self", status.trim_end_matches("/status"));
        let response = get_with_token(input, account, Some(token)).await?;
        let body = response.body.as_ref();
        let message = match response.status {
            401 => "账户余额认证失败，请更新账号访问令牌",
            403 => "账号访问令牌无账户余额查询权限",
            500..=599 => "账户余额服务暂时不可用",
            _ => "账户余额接口未返回有效额度，请检查账号访问令牌",
        };
        let value = body
            .filter(|v| (200..300).contains(&response.status) && v["success"] == true)
            .ok_or_else(|| Failure::new(message))?;
        let remaining = n(&value["data"], "quota").ok_or_else(|| Failure::new(message))?;
        let used = n(&value["data"], "used_quota");
        let mut data = Data {
            source: "newapi",
            plans: vec![plan(
                "账户余额",
                Some(remaining),
                used,
                None,
                "额度单位",
                false,
            )],
            total_usage: Some(Usage {
                requests: n(&value["data"], "request_count"),
                ..Default::default()
            }),
            ..Default::default()
        };
        // Resolve units before the optional Key query so its failure cannot
        // change a successfully read account balance into raw quota units.
        let units = match get(input, status, false).await {
            Ok(reply) if reply.status == 200 => reply.body,
            Err(failure) if failure.retry_at.is_some() => {
                data.warning = Some("账户余额已更新，单位与 Key 配额查询被限流".into());
                data.retry_at = failure.retry_at;
                return Ok(Some(data));
            }
            _ => None,
        };
        match get(input, new, true).await {
            Ok(reply) => {
                if let Some(key) = reply
                    .body
                    .as_ref()
                    .filter(|_| (200..300).contains(&reply.status))
                    .and_then(newapi)
                {
                    data.plans
                        .extend(key.plans.into_iter().filter(|p| p.name == "Key 额度"));
                    data.expires_at_unix = key.expires_at_unix;
                    data.key_status = key.key_status;
                    data.subscriptions = key.subscriptions;
                } else {
                    data.warning = Some("账户余额已更新，Key 配额查询失败".into());
                }
            }
            Err(failure) => {
                data.warning = Some(format!(
                    "账户余额已更新，Key 配额查询失败：{}",
                    failure.message
                ));
                data.retry_at = failure.retry_at;
            }
        }
        if let Some(value) = units {
            convert(&mut data, &value);
        }
        return Ok(Some(data));
    }
    let first = get(input, sub, true).await?;
    if (200..300).contains(&first.status) || [401, 403].contains(&first.status) {
        if let Some(data) = first.body.as_ref().and_then(sub2) {
            return Ok(Some(data));
        }
    }
    let second = get(input, new, true).await?;
    if (200..300).contains(&second.status) {
        if let Some(mut data) = second.body.as_ref().and_then(newapi) {
            match get(input, status, false).await {
                Ok(response) if response.status == 200 => {
                    if let Some(body) = response.body {
                        convert(&mut data, &body);
                    }
                }
                Err(failure) => {
                    data.warning = Some(format!("额度已更新，单位查询失败：{}", failure.message));
                    data.retry_at = failure.retry_at;
                }
                _ => {}
            }
            return Ok(Some(data));
        }
    }
    if [first.status, second.status]
        .iter()
        .any(|s| [401, 403].contains(s))
    {
        return Err(Failure::new("额度接口认证失败，请检查 Key 或访问权限"));
    }
    if [first.status, second.status]
        .iter()
        .any(|s| *s >= 500 && *s != 501)
    {
        return Err(Failure::new("额度服务暂时不可用"));
    }
    if second
        .body
        .as_ref()
        .is_some_and(|v| v["success"] == false || v["code"] == false)
    {
        return Err(Failure::new("额度接口拒绝查询，请检查 Key 状态"));
    }
    Ok(None)
}

#[cfg(test)]
#[path = "key_quota_subscription_tests.rs"]
mod quota_subscription_v021_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn protocol_urls_preserve_deployment_prefix() {
        for base in [
            "https://example.invalid/site",
            "https://example.invalid/site/v1",
            "https://example.invalid/site/v1/",
        ] {
            assert_eq!(
                urls(base).unwrap_or_else(|_| panic!("url")),
                (
                    "https://example.invalid/site/v1/usage".into(),
                    "https://example.invalid/site/api/usage/token/".into(),
                    "https://example.invalid/site/api/status".into()
                )
            );
        }
    }
    #[test]
    fn sub2_wallet_subscription_key_and_rate_windows() {
        let wallet=sub2(&json!({"isValid":true,"mode":"balance","balance":3.25,"usage":{"today":{"requests":3,"actual_cost":0.15,"total_tokens":12000}}})).unwrap();
        assert_eq!(wallet.plans[0].remaining, Some(3.25));
        assert_eq!(wallet.today.unwrap().cost, Some(0.15));
        let subscription=sub2(&json!({"remaining":5,"subscription":{"daily_limit_usd":10,"daily_usage_usd":5,"weekly_limit_usd":100,"weekly_usage_usd":30,"monthly_limit_usd":300,"monthly_usage_usd":60}})).unwrap();
        assert_eq!(subscription.plans.len(), 4);
        assert_eq!(subscription.plans[0].used, Some(5.));
        let key=sub2(&json!({"quota":{"limit":20,"used":7.5},"rate_limits":[{"window":"7d","limit":100,"used":12},{"window":"5h","limit":10,"used":8,"reset_at":"2026-09-28T12:00:00Z"}]})).unwrap();
        assert_eq!(key.plans[0].remaining, Some(12.5));
        assert_eq!(key.plans[1].name, "5 小时");
        assert!(key.plans[1].reset_at.is_some());
        for v in [
            json!({"remaining":-1}),
            json!({"subscription":{},"remaining":-1}),
            json!({"quota":{"limit":0}}),
        ] {
            assert!(sub2(&v).unwrap().plans[0].unlimited);
        }
        let empty = sub2(&json!({"quota":{"limit":10,"used":10,"remaining":0}})).unwrap();
        assert_eq!(empty.plans[0].remaining, Some(0.));
    }
    #[test]
    fn invalid_key_state_is_recognized_without_fabricating_balance() {
        let data = sub2(&json!({"isValid":false,"status":"disabled"})).unwrap();
        assert_eq!(data.key_status.as_deref(), Some("已禁用"));
        assert!(data.plans.is_empty());
        for v in [
            json!({}),
            json!({"data":{"balance":50}}),
            json!({"error":{"message":"secret"}}),
            json!("html"),
        ] {
            assert!(sub2(&v).is_none());
        }
    }
    #[test]
    fn newapi_units_expiry_and_unlimited_are_not_assumed() {
        let body = json!({"code":true,"data":{"object":"token_usage","total_available":1000,"total_used":500,"total_granted":1500,"unlimited_quota":false,"expires_at":1}});
        let d = newapi(&body).unwrap();
        assert_eq!(d.key_status.as_deref(), Some("已到期"));
        assert_eq!(d.plans[0].unit, "额度单位");
        for (kind, unit, remaining) in [
            ("USD", "USD", 10.),
            ("CNY", "CNY", 70.),
            ("CUSTOM", "点", 20.),
            ("TOKENS", "额度单位", 1000.),
        ] {
            let mut d = newapi(&body).unwrap();
            convert(
                &mut d,
                &json!({"data":{"quota_per_unit":100,"quota_display_type":kind,"usd_exchange_rate":7,"custom_currency_symbol":"点","custom_currency_exchange_rate":2}}),
            );
            assert_eq!(d.plans[0].unit, unit);
            assert_eq!(d.plans[0].remaining, Some(remaining));
        }
        let mut d = newapi(&body).unwrap();
        convert(
            &mut d,
            &json!({"data":{"quota_per_unit":0,"quota_display_type":"USD"}}),
        );
        assert_eq!(d.plans[0].unit, "额度单位");
        assert!(
            newapi(&json!({"success":true,"data":{"object":"token_usage","unlimited_quota":true}}))
                .unwrap()
                .plans[0]
                .unlimited
        );
        assert!(newapi(
            &json!({"success":false,"data":{"object":"token_usage","total_available":10}})
        )
        .is_none());
    }

    #[test]
    fn newapi_user_quota_is_account_balance_not_unlimited_key_placeholder() {
        for kind in ["USD", "CNY", "CUSTOM", "TOKENS"] {
            let mut value = json!({"code":true,"data":{"object":"token_usage","unlimited_quota":true,"total_available":100000000,"user_quota":{"total_available":1000,"total_used":500}}});
            let mut data = newapi(&value).unwrap();
            convert(
                &mut data,
                &json!({"data":{"quota_per_unit":100,"quota_display_type":kind,"usd_exchange_rate":7,"custom_currency_symbol":"点","custom_currency_exchange_rate":2}}),
            );
            assert_eq!(data.plans[0].name, "账户余额");
            assert!(!data.plans[0].unlimited);
            assert_eq!(
                data.plans[0].remaining,
                Some(match kind {
                    "USD" => 10.,
                    "CNY" => 70.,
                    "CUSTOM" => 20.,
                    _ => 1000.,
                })
            );
            assert!(data.plans[1].unlimited);
            value["data"].as_object_mut().unwrap().remove("user_quota");
            let key_only = newapi(&value).unwrap();
            assert_eq!(key_only.plans.len(), 1);
            assert_eq!(key_only.plans[0].name, "Key 额度");
        }
        assert!(
            newapi(&json!({"object":"billing_subscription","hard_limit_usd":100000000})).is_none()
        );
    }
}

impl crate::runtime::Runtime {
    pub(crate) async fn key_quota_input(&self, id: i64) -> Result<Query> {
        let row = self.db.one("SELECT a.id,a.platform,a.type,coalesce(nullif(to_jsonb(a)->>'parent_account_id','')::bigint,nullif(a.extra->>'parent_account_id','')::bigint) AS parent_account_id,p.id AS effective_parent_id,coalesce(p.credentials,a.credentials) AS credentials FROM accounts a LEFT JOIN accounts p ON p.id=coalesce(nullif(to_jsonb(a)->>'parent_account_id','')::bigint,nullif(a.extra->>'parent_account_id','')::bigint) AND p.deleted_at IS NULL WHERE a.id=$1 AND a.deleted_at IS NULL", &[id.into()]).await?.ok_or_else(|| Error::new(404,"账号不存在"))?;
        if row["type"] != "apikey" {
            return Err(Error::new(422, "该账号不是 API Key 账号"));
        }
        if !row["parent_account_id"].is_null() && row["effective_parent_id"].is_null() {
            return Err(Error::new(409, "父账号不可用，请检查账号配置"));
        }
        self.key_quota.input(
            id,
            &row["credentials"],
            &crate::compat::pick(
                &row,
                &[
                    "id",
                    "platform",
                    "type",
                    "parent_account_id",
                    "effective_parent_id",
                ],
            ),
        )
    }
    pub async fn key_quota_settings(&self, id: i64, value: Option<&Value>) -> Result<Value> {
        self.key_quota_input(id).await?;
        if let Some(value) = value {
            self.key_quota.save_settings(value, Some(id))
        } else {
            self.key_quota.account_settings(id)
        }
    }
}

#[cfg(test)]
#[path = "key_quota_tests.rs"]
mod service_tests;
