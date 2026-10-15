use crate::{
    audit,
    compat::{parse_datetime, sha256, string},
    error::{Error, Result},
    modeltrace,
    settings::Settings,
    store::{valid_policy, Store},
    upstream::read_limited,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::{
    collections::BTreeSet,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const MAX_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct Failure {
    pub code: &'static str,
    pub retry_at: Option<f64>,
}
impl Failure {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            retry_at: None,
        }
    }
}
pub type SyncResult<T> = std::result::Result<T, Failure>;

/// The source policy, redirects, limits and retry rules remain in the bank;
/// this boundary supplies only the response to one HTTP request.
pub trait Transport: Send + Sync {
    fn get<'a>(
        &'a self,
        url: reqwest::Url,
        accept: &'a str,
    ) -> BoxFuture<'a, SyncResult<reqwest::Response>>;
}

struct HttpTransport(reqwest::Client);
impl Transport for HttpTransport {
    fn get<'a>(
        &'a self,
        url: reqwest::Url,
        accept: &'a str,
    ) -> BoxFuture<'a, SyncResult<reqwest::Response>> {
        Box::pin(async move {
            self.0
                .get(url)
                .header("Accept", accept)
                .send()
                .await
                .map_err(|error| {
                    Failure::new(if error.is_timeout() {
                        "timeout"
                    } else {
                        "network"
                    })
                })
        })
    }
}
fn hex(value: &Value, length: usize) -> bool {
    string(value).len() == length
        && string(value)
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn vector(value: &Value, n: usize, predicate: impl Fn(f64) -> bool) -> bool {
    value.as_array().is_some_and(|v| {
        v.len() == n
            && v.iter()
                .all(|v| v.as_f64().is_some_and(|v| v.is_finite() && predicate(v)))
    })
}
fn matrix(value: &Value, n: usize, dim: usize) -> bool {
    value
        .as_array()
        .is_some_and(|v| v.len() == n && v.iter().all(|row| vector(row, dim, |_| true)))
}
pub fn validate(value: &Value) -> Result<()> {
    let invalid = || Error::new(503, "指纹库数据无效");
    let models = value["models"].as_array().ok_or_else(invalid)?;
    let robust = &value["robust"];
    let method = &value["method"];
    if value["schema"] != "robust-number-fingerprint-bank"
        || chrono::DateTime::parse_from_rfc3339(string(&value["built_at"])).is_err()
        || value["minimum_valid_numbers"] != 80
        || value["recommended_queries"] != 3
        || method["range"] != json!([1, 355])
        || method["alpha"] != 0.5
        || method["ordered_block_weight"] != 0.25
        || models.is_empty()
        || models.len() > 256
        || robust["robust_ready"] != true
    {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    for model in models {
        for field in ["id", "display_name", "family", "family_name"] {
            if model[field]
                .as_str()
                .is_none_or(|v| v.is_empty() || v.chars().count() > 256)
            {
                return Err(invalid());
            }
        }
        if !ids.insert(string(&model["id"]))
            || !vector(&model["counts"], 355, |v| {
                (0.0..=9007199254740991.0).contains(&v) && v.trunc() == v
            })
        {
            return Err(invalid());
        }
    }
    if robust["model_order"] != json!(models.iter().map(|m| &m["id"]).collect::<Vec<_>>()) {
        return Err(invalid());
    }
    for (key, dim) in [("hellinger", 355), ("ordered_blocks", 74)] {
        let feature = &robust[key];
        let basis = &feature["nuisance_basis"];
        if !vector(&feature["feature_mean"], dim, |_| true)
            || !vector(&feature["feature_scale"], dim, |v| v > 0.0)
            || basis
                .as_array()
                .is_none_or(|v| v.len() > dim || !matrix(basis, v.len(), dim))
            || !matrix(&feature["centroids"], models.len(), dim)
        {
            return Err(invalid());
        }
    }
    let ordered = &robust["ordered_blocks"];
    if ordered["weight"] != 0.25
        || ordered["environment_centroids"]
            .as_array()
            .is_none_or(|env| {
                env.is_empty()
                    || env.len() > 256
                    || !env.iter().all(|v| matrix(v, models.len(), 74))
            })
    {
        return Err(invalid());
    }
    for count in ["1", "2", "3"] {
        let c = &value["calibration"][count];
        if !c["beta"].as_f64().is_some_and(|v| v.is_finite() && v > 0.0)
            || !c["cv_accuracy"]
                .as_f64()
                .is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v))
        {
            return Err(invalid());
        }
    }
    Ok(())
}
pub fn git_blob_digest(raw: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", raw.len()));
    hash.update(raw);
    format!("{:x}", hash.finalize())
}
pub fn parse_git_refs(raw: &[u8], branch: &str) -> SyncResult<String> {
    let mut offset = 0;
    let mut found = vec![];
    while offset < raw.len() {
        let header = raw
            .get(offset..offset + 4)
            .and_then(|v| std::str::from_utf8(v).ok())
            .filter(|v| v.bytes().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| Failure::new("invalid-data"))?;
        let size = usize::from_str_radix(header, 16).map_err(|_| Failure::new("invalid-data"))?;
        if size == 0 {
            offset += 4;
            continue;
        }
        if size < 4 {
            return Err(Failure::new("invalid-data"));
        }
        let line = raw
            .get(offset + 4..offset + size)
            .and_then(|v| std::str::from_utf8(v).ok())
            .ok_or_else(|| Failure::new("invalid-data"))?;
        let wanted = format!(" refs/heads/{branch}");
        if line.get(..40).is_some_and(|head| hex(&json!(head), 40))
            && line[40..].starts_with(&wanted)
            && line[40 + wanted.len()..]
                .chars()
                .next()
                .is_none_or(|c| c == '\0' || matches!(c, ' ' | '\t' | '\r' | '\n'))
        {
            found.push(line[..40].to_owned());
        }
        offset += size;
    }
    if found.len() == 1 {
        Ok(found.remove(0))
    } else {
        Err(Failure::new("invalid-data"))
    }
}
struct Active {
    snapshot: Arc<(Value, Value)>,
    state: Value,
    status: String,
}
#[derive(Clone)]
pub struct FingerprintBank {
    store: Store,
    active: Arc<RwLock<Active>>,
    lock: Arc<Mutex<()>>,
    transport: Arc<dyn Transport>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    settings: Arc<Settings>,
    tasks: crate::tasks::Tasks,
}
impl FingerprintBank {
    pub(crate) fn supervise(&mut self, supervisor: crate::supervision::Supervisor) {
        self.tasks = crate::tasks::Tasks::supervised(supervisor);
    }

    pub fn new(settings: Arc<Settings>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .user_agent("sub2ops-modeltrace")
            .build()?;
        Self::with_transport(
            settings,
            Arc::new(HttpTransport(client)),
            Arc::new(|| chrono::Utc::now().timestamp_millis() as f64 / 1000.0),
        )
    }
    pub fn with_transport(
        settings: Arc<Settings>,
        transport: Arc<dyn Transport>,
        clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    ) -> Result<Self> {
        let manifest = modeltrace::manifest();
        if manifest["repository"] != "Hanmo123/ModelTrace" || manifest["branch"] != "hanmo" {
            return Err(Error::new(503, "指纹库来源无效"));
        }
        let bundled = modeltrace::bundled()?;
        validate(&bundled.0)?;
        let mut provenance = bundled.1.clone();
        for name in ["core", "challenge"] {
            provenance[format!("{name}_blob")] = manifest["files"][name]["gitBlob"].clone();
        }
        let defaults = json!({"version":1,"active":{"raw_bank":include_str!("../resources/modeltrace/unified_bank.json"),"version":provenance},"source":"bundled","checked_at":0,"synced_at":0,"cooldown_until":null,"last_error":null,"result":null});
        let store = Store::new(
            settings.data_dir().join("modeltrace-bank-state.json"),
            defaults,
            valid_policy,
            settings.read_only,
        );
        let cached = store.path().exists();
        let mut state = store.read()?;
        let v = &state["active"]["version"];
        let raw = state["active"]["raw_bank"]
            .as_str()
            .ok_or_else(|| Error::new(503, "指纹库缓存损坏"))?;
        if raw.len() > MAX_BYTES
            || !hex(&v["revision"], 40)
            || !hex(&v["sha256"], 64)
            || v["analyzer_version"] != manifest["analyzerVersion"]
            || ["core", "challenge"]
                .iter()
                .any(|key| v[format!("{key}_blob")] != manifest["files"][key]["gitBlob"])
            || ["checked_at", "synced_at"].iter().any(|key| {
                !state[key]
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n >= 0.0)
            })
            || !state["cooldown_until"].is_null()
                && !state["cooldown_until"].as_f64().is_some_and(f64::is_finite)
            || sha256(raw) != v["sha256"]
        {
            return Err(Error::new(503, "指纹库缓存损坏"));
        }
        let bank: Value = serde_json::from_str(raw)?;
        validate(&bank)?;
        if bank["built_at"] != v["built_at"]
            || parse_datetime(&bank["built_at"]) < parse_datetime(&bundled.0["built_at"])
        {
            return Err(Error::new(503, "指纹库缓存不兼容"));
        }
        let snapshot = Arc::new((
            bank,
            crate::compat::pick(v, &["revision", "sha256", "built_at", "analyzer_version"]),
        ));
        if cached {
            state["source"] = "cache".into();
        }
        let code = string(&state["last_error"]["code"]);
        let status = if code.is_empty() {
            "idle"
        } else if code == "incompatible" {
            "upgrade-required"
        } else {
            "error"
        };
        let status = status.into();
        Ok(Self {
            store,
            active: Arc::new(RwLock::new(Active {
                snapshot,
                state,
                status,
            })),
            lock: Arc::new(Mutex::new(())),
            transport,
            clock,
            settings,
            tasks: Default::default(),
        })
    }
    pub fn snapshot(&self) -> Arc<(Value, Value)> {
        self.active
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone()
    }
    pub fn public(&self) -> Value {
        let active = self.active.read().unwrap_or_else(|e| e.into_inner());
        let mut value = crate::compat::pick(
            &active.state,
            &[
                "source",
                "checked_at",
                "synced_at",
                "cooldown_until",
                "result",
                "retrieval",
            ],
        );
        value["version"] = active.snapshot.1.clone();
        value["status"] = active.status.clone().into();
        value["error"] = active.state["last_error"]
            .get("code")
            .or_else(|| active.state["last_error"].get("stage"))
            .cloned()
            .unwrap_or(Value::Null);
        value
    }
    pub fn request_sync(&self) -> Result<Value> {
        if self.settings.read_only {
            return Err(Error::read_only());
        }
        let this = self.clone();
        self.tasks.spawn(async move {
            this.sync(true).await;
        })?;
        Ok(self.public())
    }
    pub async fn close(&self, seconds: u64) {
        self.tasks.close(seconds).await;
    }
    fn now(&self) -> f64 {
        (self.clock)()
    }
    fn url(url: &str) -> SyncResult<reqwest::Url> {
        let url = reqwest::Url::parse(url).map_err(|_| Failure::new("invalid-data"))?;
        if url.scheme() != "https"
            || !matches!(
                url.host_str(),
                Some("api.github.com" | "github.com" | "raw.githubusercontent.com")
            )
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some_and(|port| port != 443)
        {
            return Err(Failure::new("invalid-data"));
        }
        Ok(url)
    }
    async fn read(&self, url: &str, accept: &str, deadline: Instant) -> SyncResult<Vec<u8>> {
        let mut url = Self::url(url)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Failure::new("timeout"));
        }
        let result = tokio::time::timeout(remaining.min(Duration::from_secs(15)), async {
            for _ in 0..4 {
                Self::url(url.as_str())?;
                let response = self.transport.get(url.clone(), accept).await?;
                let status = response.status().as_u16();
                if [301, 302, 303, 307, 308].contains(&status) {
                    url = url
                        .join(
                            response
                                .headers()
                                .get("location")
                                .and_then(|v| v.to_str().ok())
                                .ok_or_else(|| Failure::new("invalid-data"))?,
                        )
                        .map_err(|_| Failure::new("invalid-data"))?;
                    continue;
                }
                if !(200..300).contains(&status) {
                    let retry = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| {
                            v.parse::<f64>()
                                .ok()
                                .map(|n| self.now() + n.max(0.0))
                                .or_else(|| {
                                    httpdate::parse_http_date(v)
                                        .ok()
                                        .and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok())
                                        .map(|v| v.as_secs_f64())
                                })
                        })
                        .or_else(|| {
                            response
                                .headers()
                                .get("x-ratelimit-reset")
                                .and_then(|v| v.to_str().ok())
                                .and_then(|v| v.parse::<f64>().ok())
                        })
                        .filter(|v| v.is_finite());
                    return Err(Failure {
                        code: if matches!(status, 403 | 429) {
                            "rate-limit"
                        } else if matches!(status, 408 | 504) {
                            "timeout"
                        } else if status >= 500 {
                            "network"
                        } else {
                            "invalid-data"
                        },
                        retry_at: retry,
                    });
                }
                if response.headers().get("content-length").is_some_and(|v| {
                    v.to_str()
                        .ok()
                        .and_then(|v| v.parse::<usize>().ok())
                        .is_none_or(|n| n > MAX_BYTES)
                }) {
                    return Err(Failure::new("invalid-data"));
                }
                return read_limited(response, MAX_BYTES)
                    .await
                    .map(|v| v.to_vec())
                    .map_err(|_| Failure::new("invalid-data"));
            }
            Err(Failure::new("invalid-data"))
        })
        .await;
        result.unwrap_or_else(|_| Err(Failure::new("timeout")))
    }
    async fn read_retry(&self, url: &str, accept: &str, deadline: Instant) -> SyncResult<Vec<u8>> {
        match self.read(url, accept, deadline).await {
            Err(failure) if matches!(failure.code, "network" | "timeout") => {
                self.read(url, accept, deadline).await
            }
            other => other,
        }
    }
    async fn pinned(&self, revision: &str, path: &str, deadline: Instant) -> SyncResult<Vec<u8>> {
        let repo = "Hanmo123/ModelTrace";
        match self
            .read_retry(
                &format!("https://raw.githubusercontent.com/{repo}/{revision}/{path}"),
                "text/plain",
                deadline,
            )
            .await
        {
            Err(failure) if matches!(failure.code, "network" | "timeout") => {
                self.read_retry(
                    &format!("https://github.com/{repo}/raw/{revision}/{path}"),
                    "text/plain",
                    deadline,
                )
                .await
            }
            other => other,
        }
    }
    async fn fallback_files(
        &self,
        revision: &str,
        manifest: &Value,
        deadline: Instant,
    ) -> SyncResult<Vec<u8>> {
        for name in ["core", "challenge"] {
            let file = &manifest["files"][name];
            let raw = self
                .pinned(revision, string(&file["path"]), deadline)
                .await?;
            if git_blob_digest(&raw) != file["gitBlob"]
                || crate::compat::sha256(&raw) != file["sha256"]
            {
                return Err(Failure::new("incompatible"));
            }
        }
        self.pinned(
            revision,
            string(&manifest["files"]["bank"]["path"]),
            deadline,
        )
        .await
    }
    async fn retrieve(
        &self,
        state: &Value,
        snapshot: &(Value, Value),
        rate: &mut Option<f64>,
    ) -> SyncResult<(Value, Option<(Value, Value)>)> {
        let now = self.now();
        let deadline = Instant::now() + Duration::from_secs(60);
        let manifest = modeltrace::manifest();
        let base = "https://api.github.com/repos/Hanmo123/ModelTrace";
        let mut retrieval = "api_tree";
        let revision = match self
            .read_retry(
                &format!("{base}/commits/{}", string(&manifest["branch"])),
                "application/vnd.github+json",
                deadline,
            )
            .await
        {
            Ok(raw) => {
                let head: Value =
                    serde_json::from_slice(&raw).map_err(|_| Failure::new("invalid-data"))?;
                if !hex(&head["sha"], 40) {
                    return Err(Failure::new("invalid-data"));
                }
                string(&head["sha"]).to_owned()
            }
            Err(failure) if matches!(failure.code, "network" | "timeout" | "rate-limit") => {
                if failure.code == "rate-limit" {
                    *rate = Some(failure.retry_at.unwrap_or(now + 300.0));
                }
                retrieval = "git_refs";
                parse_git_refs(&self.read_retry("https://github.com/Hanmo123/ModelTrace.git/info/refs?service=git-upload-pack","application/x-git-upload-pack-advertisement",deadline).await?,string(&manifest["branch"]))?
            }
            Err(failure) => return Err(failure),
        };
        if snapshot.1["revision"] == revision {
            let mut next = state.clone();
            next.as_object_mut().unwrap().extend(json!({"checked_at":now,"cooldown_until":null,"last_error":null,"result":"up-to-date","rate_limit_until":rate}).as_object().unwrap().clone());
            if let Some(rate) = rate {
                next["cooldown_until"] = json!(*rate);
            }
            return Ok((next, None));
        }
        let mut expected = None;
        let raw = if retrieval == "api_tree" {
            let tree = self
                .read_retry(
                    &format!("{base}/git/trees/{revision}?recursive=1"),
                    "application/vnd.github+json",
                    deadline,
                )
                .await;
            match tree {
                Ok(raw) => {
                    let tree: Value =
                        serde_json::from_slice(&raw).map_err(|_| Failure::new("invalid-data"))?;
                    let entries = tree["tree"]
                        .as_array()
                        .filter(|_| tree["truncated"] != true)
                        .ok_or_else(|| Failure::new("invalid-data"))?;
                    for name in ["core", "challenge", "bank"] {
                        let entry = entries
                            .iter()
                            .find(|entry| entry["path"] == manifest["files"][name]["path"])
                            .ok_or_else(|| Failure::new("invalid-data"))?;
                        if entry["type"] != "blob"
                            || entry["mode"] != "100644"
                            || !hex(&entry["sha"], 40)
                        {
                            return Err(Failure::new("invalid-data"));
                        }
                        if name == "bank" {
                            expected = Some(string(&entry["sha"]).to_owned());
                        } else if entry["sha"] != manifest["files"][name]["gitBlob"] {
                            return Err(Failure::new("incompatible"));
                        }
                    }
                    match self
                        .pinned(
                            &revision,
                            string(&manifest["files"]["bank"]["path"]),
                            deadline,
                        )
                        .await
                    {
                        Err(failure)
                            if matches!(failure.code, "network" | "timeout" | "rate-limit") =>
                        {
                            if failure.code == "rate-limit" {
                                *rate = Some(failure.retry_at.unwrap_or(now + 300.0));
                            }
                            retrieval = "git_refs";
                            self.fallback_files(&revision, &manifest, deadline).await?
                        }
                        other => other?,
                    }
                }
                Err(failure) if matches!(failure.code, "network" | "timeout" | "rate-limit") => {
                    if failure.code == "rate-limit" {
                        *rate = Some(failure.retry_at.unwrap_or(now + 300.0));
                    }
                    retrieval = "git_refs";
                    self.fallback_files(&revision, &manifest, deadline).await?
                }
                Err(failure) => return Err(failure),
            }
        } else {
            self.fallback_files(&revision, &manifest, deadline).await?
        };
        if expected.is_some_and(|digest| git_blob_digest(&raw) != digest) {
            return Err(Failure::new("invalid-data"));
        }
        let bank: Value = serde_json::from_slice(&raw).map_err(|_| Failure::new("invalid-data"))?;
        validate(&bank).map_err(|_| Failure::new("invalid-data"))?;
        if parse_datetime(&bank["built_at"]) < parse_datetime(&snapshot.0["built_at"]) {
            return Err(Failure::new("incompatible"));
        }
        let version = json!({"revision":revision,"sha256":sha256(&raw),"built_at":bank["built_at"],"analyzer_version":manifest["analyzerVersion"]});
        let mut provenance = version.clone();
        for key in ["core", "challenge"] {
            provenance[format!("{key}_blob")] = manifest["files"][key]["gitBlob"].clone();
        }
        let next = json!({"version":1,"active":{"raw_bank":String::from_utf8(raw).map_err(|_|Failure::new("invalid-data"))?,"version":provenance},"checked_at":now,"synced_at":self.now(),"source":"remote","last_error":null,"cooldown_until":rate,"rate_limit_until":rate,"result":"updated","retrieval":retrieval});
        Ok((next, Some((bank, version))))
    }
    pub async fn sync(&self, force: bool) {
        if self.settings.read_only || self.tasks.is_closed() {
            return;
        }
        let Ok(_guard) = self.lock.try_lock() else {
            return;
        };
        let (state, snapshot) = {
            let mut active = self.active.write().unwrap_or_else(|e| e.into_inner());
            let now = self.now();
            let code = string(&active.state["last_error"]["code"]);
            let last = active.state["checked_at"].as_f64().unwrap_or(0.0);
            let interval = if matches!(code, "network" | "timeout" | "rate-limit" | "cache") {
                300.0
            } else {
                3600.0
            };
            if active.state["rate_limit_until"]
                .as_f64()
                .is_some_and(|at| now < at)
                || active.state["cooldown_until"]
                    .as_f64()
                    .is_some_and(|at| now < at)
                    && (!force || code == "rate-limit")
                || !force && last > 0.0 && (0.0..interval).contains(&(now - last))
            {
                return;
            }
            active.status = "checking".into();
            (active.state.clone(), active.snapshot.clone())
        };
        let mut rate = None;
        let result = self
            .retrieve(&state, &snapshot, &mut rate)
            .await
            .and_then(|(next, bank)| {
                self.store
                    .transaction(|v| {
                        *v = next.clone();
                        Ok(())
                    })
                    .map_err(|_| Failure::new("cache"))?;
                Ok((next, bank))
            });
        match result {
            Ok((next, bank)) => {
                let event = string(&next["result"]).to_owned();
                let revision = next["active"]["version"]["revision"].clone();
                let mut active = self.active.write().unwrap_or_else(|e| e.into_inner());
                active.state = next;
                if let Some(bank) = bank {
                    active.snapshot = Arc::new(bank);
                }
                active.status = "idle".into();
                audit::write(
                    &self.settings.audit_path,
                    &format!("fingerprint_bank_{event}"),
                    json!({"revision":revision}),
                    false,
                );
            }
            Err(failure) => {
                let cooldown =
                    if matches!(failure.code, "network" | "timeout" | "rate-limit" | "cache") {
                        Some((self.now() + 300.0).max(failure.retry_at.unwrap_or(0.0)))
                    } else {
                        None
                    };
                let mut next = state;
                next["checked_at"] = json!(self.now());
                next["last_error"] = json!({"code":failure.code});
                next["cooldown_until"] = json!(cooldown);
                next["result"] = "failed".into();
                if let Some(rate) = rate {
                    next["rate_limit_until"] = json!(rate);
                    next["cooldown_until"] = json!(cooldown.unwrap_or(0.0).max(rate));
                }
                let _ = self.store.transaction(|v| {
                    *v = next.clone();
                    Ok(())
                });
                let mut active = self.active.write().unwrap_or_else(|e| e.into_inner());
                active.state = next;
                active.status = if failure.code == "incompatible" {
                    "upgrade-required"
                } else {
                    "error"
                }
                .into();
                audit::write(
                    &self.settings.audit_path,
                    "fingerprint_bank_sync_error",
                    json!({"code":failure.code}),
                    false,
                );
            }
        }
    }
}
