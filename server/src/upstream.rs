use crate::{
    compat::{now, sha256},
    error::{Error, Result},
    settings::Settings,
};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Probe {
    pub state: String,
    pub endpoint: String,
    pub status: Option<u16>,
    pub message: String,
    pub retryable: bool,
    pub checked_at: String,
}
impl Probe {
    fn new(
        state: &str,
        endpoint: &str,
        status: Option<u16>,
        message: &str,
        retryable: bool,
    ) -> Self {
        Self {
            state: state.into(),
            endpoint: endpoint.into(),
            status,
            message: message.into(),
            retryable,
            checked_at: now(),
        }
    }
    pub fn public(&self) -> Value {
        json!({"state":self.state,"endpoint":self.endpoint,"http_status":self.status,"message":self.message,"retryable":self.retryable,"checked_at":self.checked_at})
    }
    pub fn error(&self) -> Error {
        Error::new(
            if self.state == "auth_rejected" {
                401
            } else {
                502
            },
            json!({"code":self.state,"message":self.message,"connection":self.public()}),
        )
    }
}

struct Cache {
    active: Option<String>,
    status: Probe,
    auth: Option<(String, Instant)>,
    failure: Option<(String, Probe, u32, Instant)>,
    completed_probe: Option<(String, Probe, Instant)>,
}

impl Cache {
    fn cached(&self, signature: &str) -> Option<Result<Probe>> {
        if self.status.state == "ok"
            && self
                .auth
                .as_ref()
                .is_some_and(|(digest, until)| digest == signature && *until > Instant::now())
        {
            return Some(Ok(self.status.clone()));
        }
        self.failure
            .as_ref()
            .and_then(|(digest, result, _, until)| {
                (digest == signature && *until > Instant::now()).then(|| Err(result.error()))
            })
    }
}

#[derive(Clone)]
pub struct Upstream {
    settings: Arc<Settings>,
    cache: Arc<Mutex<Cache>>,
    probe_lock: Arc<tokio::sync::Mutex<()>>,
    pub client: reqwest::Client,
}

impl Upstream {
    pub fn new(settings: Arc<Settings>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            settings,
            client,
            probe_lock: Arc::new(tokio::sync::Mutex::new(())),
            cache: Arc::new(Mutex::new(Cache {
                active: None,
                auth: None,
                failure: None,
                completed_probe: None,
                status: Probe::new("network_unreachable", "none", None, "尚未检查上游", false),
            })),
        })
    }

    fn candidates(&self) -> Vec<(String, String)> {
        let Ok(config) = self.settings.connection_config() else {
            return vec![];
        };
        let mut values = Vec::new();
        for (kind, key) in [("verify", "verify_base_url"), ("public", "base_url")] {
            let value = config[key].as_str().unwrap_or("");
            if !value.is_empty() && !values.iter().any(|(_, v)| v == value) {
                values.push((kind.to_owned(), value.to_owned()));
            }
        }
        values
    }

    pub fn active_url(&self) -> String {
        let values = self.candidates();
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(active) = &cache.active {
            if values.iter().any(|(_, v)| v == active) {
                return active.clone();
            }
        }
        values.first().map(|(_, v)| v.clone()).unwrap_or_default()
    }
    pub fn status(&self) -> Value {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .public()
    }
    pub fn invalidate(&self) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.active = None;
        cache.auth = None;
        cache.failure = None;
        cache.completed_probe = None;
    }

    async fn probe(&self, endpoint: &str, base: &str, key: &str) -> Probe {
        let request = self
            .client
            .get(format!("{base}/api/v1/admin/groups/all"))
            .header("x-api-key", key)
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(5));
        let response = request.send().await;
        let Ok(response) = response else {
            return Probe::new(
                "network_unreachable",
                endpoint,
                None,
                "无法连接 Sub2API 上游",
                true,
            );
        };
        let status = response.status().as_u16();
        match status {
            401 | 403 => {
                return Probe::new(
                    "auth_rejected",
                    endpoint,
                    Some(status),
                    "管理员 API Key 被上游拒绝，请重新连接",
                    false,
                )
            }
            404 => {
                return Probe::new(
                    "route_not_found",
                    endpoint,
                    Some(status),
                    "Sub2API 管理接口路径不存在",
                    true,
                )
            }
            200..=299 => (),
            _ => {
                return Probe::new(
                    "upstream_error",
                    endpoint,
                    Some(status),
                    &format!("Sub2API 返回 HTTP {status}"),
                    status >= 500,
                )
            }
        }
        let body = read_limited(response, 2_000_000)
            .await
            .and_then(|v| serde_json::from_slice::<Value>(&v).map_err(Into::into));
        let Ok(body) = body else {
            return Probe::new(
                "protocol_mismatch",
                endpoint,
                Some(status),
                "Sub2API 返回内容不是有效 JSON",
                false,
            );
        };
        if body["code"].as_i64().is_some_and(|v| v != 0) {
            return Probe::new(
                "upstream_error",
                endpoint,
                Some(status),
                "Sub2API 管理接口返回错误",
                false,
            );
        }
        if !(body.is_array() || (body["code"].as_i64() == Some(0) && body["data"].is_array())) {
            return Probe::new(
                "protocol_mismatch",
                endpoint,
                Some(status),
                "Sub2API 管理响应结构不兼容",
                false,
            );
        }
        Probe::new(
            "ok",
            endpoint,
            Some(status),
            "Sub2API 管理权限已验证",
            false,
        )
    }

    pub async fn authenticate(&self, key: &str, fresh: bool) -> Result<Probe> {
        let requested = Instant::now();
        if key.is_empty() || key.chars().count() > 4096 || key.chars().any(|c| (c as u32) < 33) {
            let _guard = self.probe_lock.lock().await;
            let result = Probe::new(
                "auth_rejected",
                "none",
                Some(401),
                "管理员 API Key 无效",
                false,
            );
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.auth = None;
            cache.completed_probe = None;
            cache.status = result.clone();
            return Err(result.error());
        }
        let candidates = self.candidates();
        let signature = sha256(format!(
            "{}\0{key}",
            candidates
                .iter()
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
                .join("\0")
        ));
        // Cached reads must not wait behind a slow, fresh write authorization.
        if !fresh {
            if let Some(result) = self
                .cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cached(&signature)
            {
                return result;
            }
        }
        let _guard = self.probe_lock.lock().await;
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            // Only a probe completed after this request began can authorize a
            // fresh caller. Sequential writes still perform a fresh check.
            if let Some((digest, result, completed)) = &cache.completed_probe {
                if digest == &signature && *completed >= requested {
                    return if result.state == "ok" {
                        Ok(result.clone())
                    } else {
                        Err(result.error())
                    };
                }
            }
            if !fresh {
                if let Some(result) = cache.cached(&signature) {
                    return result;
                }
            }
        }
        if candidates.is_empty() {
            let result = Probe::new(
                "network_unreachable",
                "none",
                None,
                "未配置 Sub2API 服务地址",
                false,
            );
            self.cache.lock().unwrap_or_else(|e| e.into_inner()).status = result.clone();
            return Err(result.error());
        }
        let mut last = Probe::new(
            "network_unreachable",
            &candidates[0].0,
            None,
            "无法连接 Sub2API 上游",
            true,
        );
        for (endpoint, base) in candidates {
            last = self.probe(&endpoint, &base, key).await;
            if last.state == "ok" {
                let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                cache.active = Some(base);
                cache.status = last.clone();
                cache.auth = Some((signature.clone(), Instant::now() + Duration::from_secs(15)));
                cache.completed_probe = Some((signature, last.clone(), Instant::now()));
                cache.failure = None;
                return Ok(last);
            }
            if !matches!(
                last.state.as_str(),
                "network_unreachable" | "route_not_found"
            ) {
                break;
            }
        }
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.status = last.clone();
        cache.auth = None;
        cache.completed_probe = Some((signature.clone(), last.clone(), Instant::now()));
        cache.failure = if last.retryable {
            let count = cache
                .failure
                .as_ref()
                .filter(|(digest, _, _, _)| digest == &signature)
                .map(|(_, _, count, _)| count.saturating_add(1))
                .unwrap_or(1);
            let until =
                Instant::now() + Duration::from_secs((5 * 2u64.pow((count - 1).min(4))).min(60));
            Some((signature, last.clone(), count, until))
        } else {
            None
        };
        Err(last.error())
    }

    /// One dispatch only. Failover is an authentication decision and never a
    /// retry of an uncertain business write.
    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        key: &str,
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<reqwest::Response> {
        if self.settings.read_only
            && method != reqwest::Method::GET
            && method != reqwest::Method::HEAD
        {
            return Err(Error::read_only());
        }
        let base = self.active_url();
        if base.is_empty() {
            return Err(Error::new(502, "未配置 Sub2API 服务地址"));
        }
        let mut request = self
            .client
            .request(method, format!("{base}{path}"))
            .header("x-api-key", key)
            .header("Accept", "application/json")
            .timeout(timeout);
        if let Some(body) = body {
            request = request.json(&body);
        }
        Ok(request.send().await?)
    }
}

pub async fn read_limited(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > limit {
            return Err(Error::new(502, "上游响应超过限制"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
