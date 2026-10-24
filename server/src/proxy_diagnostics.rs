use crate::{
    actions::Actions,
    compat::{sha256, string},
    error::{Error, Result},
    upstream::read_limited,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
fn text(value: &Value) -> String {
    let Some(value) = value.as_str() else {
        return String::new();
    };
    value.to_owned()
}
fn number(value: &Value, maximum: f64) -> Value {
    if value
        .as_f64()
        .is_some_and(|n| n.is_finite() && n >= 0.0 && n <= maximum)
    {
        value.clone()
    } else {
        Value::Null
    }
}
fn ip(value: &Value) -> String {
    string(value)
        .parse::<IpAddr>()
        .map(|v| v.to_string())
        .unwrap_or_default()
}
pub fn safe_result(action: &str, data: &Value) -> Result<Value> {
    let fail = || {
        Error::new(
            502,
            if action == "test" {
                "代理检测响应格式不兼容"
            } else {
                "代理质量响应格式不兼容"
            },
        )
    };
    if !data.is_object() {
        return Err(fail());
    }
    if action == "test" {
        if !data["success"].is_boolean() {
            return Err(fail());
        }
        let mut result = json!({"success":data["success"],"message":text(&data["message"]),"latency_ms":number(&data["latency_ms"],1e12),"ip_address":ip(&data["ip_address"])});
        for key in ["city", "region", "country", "country_code"] {
            result[key] = text(&data[key]).into();
        }
        return Ok(result);
    }
    let items = data["items"].as_array().ok_or_else(fail)?;
    if number(&data["score"], 100.0).is_null() || !data["grade"].is_string() || items.len() > 32 {
        return Err(fail());
    }
    let items=items.iter().map(|item| {
        if !item.is_object() || !matches!(string(&item["status"]),"pass"|"warn"|"fail"|"challenge") {return Err(fail());}
        Ok(json!({"target":text(&item["target"]),"status":item["status"],"http_status":number(&item["http_status"],599.0),"latency_ms":number(&item["latency_ms"],1e12),"message":text(&item["message"])}))
    }).collect::<Result<Vec<_>>>()?;
    let mut result = json!({"score":data["score"],"grade":text(&data["grade"]),"summary":text(&data["summary"]),"exit_ip":ip(&data["exit_ip"]),"country":text(&data["country"]),"country_code":text(&data["country_code"]),"base_latency_ms":number(&data["base_latency_ms"],1e12),"items":items});
    for key in [
        "passed_count",
        "warn_count",
        "failed_count",
        "challenge_count",
        "checked_at",
    ] {
        result[key] = number(&data[key], 1e12);
    }
    Ok(result)
}
type Identity = (String, i64, String);
type Request = tokio::sync::watch::Receiver<Option<Result<Value>>>;
#[derive(Clone)]
pub struct ProxyDiagnostics {
    actions: Actions,
    running: Arc<Mutex<BTreeMap<Identity, Request>>>,
    tasks: crate::tasks::Tasks,
}
impl ProxyDiagnostics {
    pub fn new(actions: Actions) -> Self {
        let tasks = crate::tasks::Tasks::supervised(actions.supervisor.clone());
        Self {
            actions,
            running: Arc::new(Mutex::new(BTreeMap::new())),
            tasks,
        }
    }
    pub async fn close(&self, seconds: u64) {
        self.tasks.close(seconds).await;
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
    pub async fn run(&self, key: &str, id: i64, action: &str) -> Result<Value> {
        if id <= 0 || !matches!(action, "test" | "quality-check") {
            return Err(Error::new(422, "代理检测参数无效"));
        }
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        let identity = (sha256(key), id, action.to_owned());
        let mut receiver = {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(future) = running.get(&identity) {
                future.clone()
            } else {
                let actions = self.actions.clone();
                let key = key.to_owned();
                let action = action.to_owned();
                let future = async move {
                    tokio::time::timeout(Duration::from_secs(90), async {
                        let response = actions
                            .request(
                                reqwest::Method::POST,
                                &format!("proxies/{id}/{action}"),
                                &key,
                                Some(&json!({})),
                                Duration::from_secs(90),
                                false,
                            )
                            .await?;
                        let status = response.status().as_u16();
                        let bytes = read_limited(response, 1024 * 1024).await?;
                        if !(200..300).contains(&status) {
                            return Err(Error::new(
                                502,
                                format!(
                                    "代理检测失败 [http_{status}]\n{}",
                                    String::from_utf8_lossy(&bytes)
                                ),
                            ));
                        }
                        let body: Value = serde_json::from_slice(&bytes).map_err(|_| {
                            Error::new(
                                502,
                                format!(
                                    "代理检测响应格式无效\n{}",
                                    String::from_utf8_lossy(&bytes)
                                ),
                            )
                        })?;
                        if body["code"].as_i64() != Some(0) {
                            return Err(Error::new(
                                502,
                                String::from_utf8_lossy(&bytes).to_string(),
                            ));
                        }
                        safe_result(&action, &body["data"]).map_err(|e| {
                            Error::new(
                                502,
                                format!(
                                    "{}\n{}",
                                    e.detail.as_str().unwrap_or("代理检测响应格式不兼容"),
                                    String::from_utf8_lossy(&bytes)
                                ),
                            )
                        })
                    })
                    .await
                    .map_err(|_| Error::new(504, "代理检测超时，未重放请求"))?
                };
                let (send, receive) = tokio::sync::watch::channel(None);
                running.insert(identity.clone(), receive.clone());
                let weak = Arc::downgrade(&self.running);
                let task_identity = identity.clone();
                if let Err(error) = self.tasks.spawn(async move {
                    let result = future.await;
                    let _ = send.send(Some(result));
                    if let Some(running) = weak.upgrade() {
                        running
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&task_identity);
                    }
                }) {
                    running.remove(&identity);
                    return Err(error);
                }
                receive
            }
        };
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            if receiver.changed().await.is_err() {
                return Err(Error::new(502, "代理检测已中断，请核对结果；未重放请求"));
            }
        }
    }
}
