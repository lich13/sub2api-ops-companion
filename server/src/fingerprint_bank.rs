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
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub(crate) const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
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
/// A scoring source that differs from the pinned one only in ways this analyzer does not
/// read is listed in the manifest as compatible.
fn compatible_blob(manifest: &Value, name: &str, blob: &Value) -> bool {
    let file = &manifest["files"][name];
    blob == &file["gitBlob"]
        || file["compatibleGitBlobs"]
            .as_array()
            .is_some_and(|known| known.contains(blob))
}
fn bundled_provenance(manifest: &Value, version: &Value) -> Value {
    let mut provenance = version.clone();
    for name in ["core", "challenge"] {
        provenance[format!("{name}_blob")] = manifest["files"][name]["gitBlob"].clone();
    }
    provenance
}
fn bundled_state_meta(manifest: &Value, version: &Value) -> Value {
    let provenance = bundled_provenance(manifest, version);
    json!({"version":1,"active":{"version":provenance},"source":"bundled","checked_at":0,"synced_at":0,"cooldown_until":null,"last_error":null,"result":null})
}
fn bundled_state_with_raw(manifest: &Value, version: &Value) -> Value {
    let provenance = bundled_provenance(manifest, version);
    json!({"version":1,"active":{"raw_bank":include_str!("../resources/modeltrace/unified_bank.json"),"version":provenance},"source":"bundled","checked_at":0,"synced_at":0,"cooldown_until":null,"last_error":null,"result":null})
}
fn lazy_bundled_state() -> Value {
    let manifest = modeltrace::manifest();
    let version = modeltrace::bundled_version().expect("bundled modeltrace bank");
    bundled_state_with_raw(&manifest, &version)
}
fn strip_raw_bank(mut state: Value) -> Value {
    if let Some(active) = state["active"].as_object_mut() {
        active.shift_remove("raw_bank");
    }
    state
}
fn persist_bank_state(disk: &mut Value, next: Value) -> Result<()> {
    let preserved_raw = disk["active"].get("raw_bank").cloned();
    *disk = next;
    if disk["active"].get("raw_bank").is_none() {
        let expected_sha = &disk["active"]["version"]["sha256"];
        if let Some(raw) =
            preserved_raw.filter(|raw| raw.as_str().is_some_and(|s| sha256(s) == *expected_sha))
        {
            disk["active"]["raw_bank"] = raw;
        } else {
            let bundled_raw = include_str!("../resources/modeltrace/unified_bank.json");
            if sha256(bundled_raw) == *expected_sha {
                disk["active"]["raw_bank"] = bundled_raw.into();
            } else {
                return Err(Error::new(503, "指纹库缓存损坏"));
            }
        }
    }
    Ok(())
}
struct Active {
    version: Value,
    compiled: Arc<modeltrace::CompiledBank>,
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
        Self::with_clock(settings, Arc::new(chrono::Utc::now))
    }
    pub(crate) fn with_clock(
        settings: Arc<Settings>,
        clock: crate::compat::WallClock,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .user_agent("sub2ops-modeltrace")
            .build()?;
        Self::with_transport(
            settings,
            Arc::new(HttpTransport(client)),
            Arc::new(move || clock().timestamp_millis() as f64 / 1000.0),
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
        let bundled_version = modeltrace::bundled_version()?;
        let store = Store::new_lazy(
            settings.data_dir().join("modeltrace-bank-state.json"),
            lazy_bundled_state,
            valid_policy,
        );
        let cached = store.path().exists() || store.path().with_extension("lock").exists();
        let (compiled, version, state) = if !cached {
            let (bundled_compiled, _) = modeltrace::bundled_compiled()?;
            let mut defaults = bundled_state_meta(&manifest, &bundled_version);
            let v = crate::compat::pick(
                &defaults["active"]["version"],
                &["revision", "sha256", "built_at", "analyzer_version"],
            );
            defaults["source"] = "bundled".into();
            (Arc::new(bundled_compiled), v, defaults)
        } else {
            let state = store.read_shared()?;
            store.evict_cache();
            let mut state = Arc::unwrap_or_clone(state);
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
                    .any(|key| !compatible_blob(&manifest, key, &v[format!("{key}_blob")]))
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
            let cached_compiled = match modeltrace::CompiledBank::compile_str(raw) {
                Ok(c) => c,
                Err(e) => {
                    // Preserve exact serde_json parse error vs schema validation error semantics.
                    let bank_val: Value = serde_json::from_str(raw)?;
                    if bank_val["built_at"] != v["built_at"] {
                        return Err(Error::new(503, "指纹库缓存不兼容"));
                    }
                    return Err(e);
                }
            };
            if cached_compiled.built_at() != &v["built_at"] {
                return Err(Error::new(503, "指纹库缓存不兼容"));
            }
            // A cache older than the bundled bank is superseded by it. The file is replaced
            // by `adopt_bundled` once the service starts; validation alone writes nothing.
            let compiled = if parse_datetime(cached_compiled.built_at())
                < parse_datetime(&bundled_version["built_at"])
            {
                let (bundled_compiled, _) = modeltrace::bundled_compiled()?;
                state = bundled_state_meta(&manifest, &bundled_version);
                Arc::new(bundled_compiled)
            } else {
                state["source"] = "cache".into();
                state = strip_raw_bank(state);
                Arc::new(cached_compiled)
            };
            let v = crate::compat::pick(
                &state["active"]["version"],
                &["revision", "sha256", "built_at", "analyzer_version"],
            );
            (compiled, v, state)
        };
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
                version,
                compiled,
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
    /// Replaces a saved bank that is older than the bundled one, so later state writes
    /// keep the bank and its recorded version together.
    pub fn adopt_bundled(&self) -> Result<()> {
        if !self.store.path().exists() {
            return Ok(());
        }
        if self.active.read().unwrap_or_else(|e| e.into_inner()).state["source"] != "bundled" {
            return Ok(());
        }
        let manifest = modeltrace::manifest();
        let bundled_version = modeltrace::bundled_version()?;
        let built = parse_datetime(&bundled_version["built_at"]);
        let result = self.store.transaction(|state| {
            if parse_datetime(&state["active"]["version"]["built_at"]) < built {
                *state = bundled_state_with_raw(&manifest, &bundled_version);
            }
            Ok(())
        });
        self.store.evict_cache();
        result
    }
    pub fn compiled(&self) -> Arc<modeltrace::CompiledBank> {
        self.active
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .compiled
            .clone()
    }
    pub fn compiled_version(&self) -> (Value, Arc<modeltrace::CompiledBank>) {
        let active = self.active.read().unwrap_or_else(|e| e.into_inner());
        (active.version.clone(), active.compiled.clone())
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
        value["version"] = active.version.clone();
        value["status"] = active.status.clone().into();
        value["error"] = active.state["last_error"]
            .get("code")
            .or_else(|| active.state["last_error"].get("stage"))
            .cloned()
            .unwrap_or(Value::Null);
        value
    }
    pub fn request_sync(&self) -> Result<Value> {
        let this = self.clone();
        self.tasks.spawn(async move {
            this.sync(true).await;
        })?;
        Ok(self.public())
    }
    pub async fn close(&self, seconds: u64) {
        self.tasks.close(seconds).await;
    }
    fn audit_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp_millis((self.now() * 1000.0) as i64).unwrap_or_default()
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
            if !compatible_blob(manifest, name, &json!(git_blob_digest(&raw))) {
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
        current_version: &Value,
        current_built_at: &Value,
        rate: &mut Option<f64>,
    ) -> SyncResult<(Value, Option<(Arc<modeltrace::CompiledBank>, Value)>)> {
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
        if current_version["revision"] == revision {
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
                        } else if !compatible_blob(&manifest, name, &entry["sha"]) {
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
        let raw_digest = sha256(&raw);
        let raw_str = String::from_utf8(raw).map_err(|_| Failure::new("invalid-data"))?;
        let compiled = modeltrace::CompiledBank::compile_str(&raw_str)
            .map_err(|_| Failure::new("invalid-data"))?;
        if parse_datetime(compiled.built_at()) < parse_datetime(current_built_at) {
            return Err(Failure::new("incompatible"));
        }
        let version = json!({"revision":revision,"sha256":raw_digest,"built_at":compiled.built_at(),"analyzer_version":manifest["analyzerVersion"]});
        let provenance = bundled_provenance(&manifest, &version);
        let next = json!({"version":1,"active":{"raw_bank":raw_str,"version":provenance},"checked_at":now,"synced_at":self.now(),"source":"remote","last_error":null,"cooldown_until":rate,"rate_limit_until":rate,"result":"updated","retrieval":retrieval});
        Ok((next, Some((Arc::new(compiled), version))))
    }
    pub async fn sync(&self, force: bool) {
        if self.tasks.is_closed() {
            return;
        }
        let Ok(_guard) = self.lock.try_lock() else {
            return;
        };
        let (state, current_version, current_built_at) = {
            let mut active = self.active.write().unwrap_or_else(|e| e.into_inner());
            let now = self.now();
            let code = string(&active.state["last_error"]["code"]);
            let last = active.state["checked_at"].as_f64().unwrap_or(0.0);
            if active.state["rate_limit_until"]
                .as_f64()
                .is_some_and(|at| now < at)
                || active.state["cooldown_until"]
                    .as_f64()
                    .is_some_and(|at| now < at)
                    && (!force || code == "rate-limit")
                || !force
                    && last > 0.0
                    && (0.0..CHECK_INTERVAL.as_secs_f64()).contains(&(now - last))
            {
                return;
            }
            active.status = "checking".into();
            (
                active.state.clone(),
                active.version.clone(),
                active.compiled.built_at().clone(),
            )
        };
        let mut rate = None;
        let result = self
            .retrieve(&state, &current_version, &current_built_at, &mut rate)
            .await
            .and_then(|(next, compiled_and_version)| {
                let compiled_and_version =
                    compiled_and_version.map(|(compiled, version)| (version, compiled));
                self.store
                    .transaction(|v| persist_bank_state(v, next.clone()))
                    .map_err(|_| Failure::new("cache"))?;
                self.store.evict_cache();
                Ok((strip_raw_bank(next), compiled_and_version))
            });
        match result {
            Ok((next, compiled_and_version)) => {
                let event = string(&next["result"]).to_owned();
                let revision = next["active"]["version"]["revision"].clone();
                let mut active = self.active.write().unwrap_or_else(|e| e.into_inner());
                active.state = next;
                if let Some((version, compiled)) = compiled_and_version {
                    active.version = version;
                    active.compiled = compiled;
                }
                active.status = "idle".into();
                audit::write_at(
                    &self.settings.audit_path,
                    &format!("fingerprint_bank_{event}"),
                    json!({"revision":revision}),
                    self.audit_time(),
                );
            }
            Err(failure) => {
                let cooldown =
                    if matches!(failure.code, "network" | "timeout" | "rate-limit" | "cache") {
                        Some((self.now() + 300.0).max(failure.retry_at.unwrap_or(0.0)))
                    } else {
                        None
                    };
                let mut next = strip_raw_bank(state);
                next["checked_at"] = json!(self.now());
                next["last_error"] = json!({"code":failure.code});
                next["cooldown_until"] = json!(cooldown);
                next["result"] = "failed".into();
                if let Some(rate) = rate {
                    next["rate_limit_until"] = json!(rate);
                    next["cooldown_until"] = json!(cooldown.unwrap_or(0.0).max(rate));
                }
                let _ = self
                    .store
                    .transaction(|v| persist_bank_state(v, next.clone()));
                self.store.evict_cache();
                let mut active = self.active.write().unwrap_or_else(|e| e.into_inner());
                active.state = next;
                active.status = if failure.code == "incompatible" {
                    "upgrade-required"
                } else {
                    "error"
                }
                .into();
                audit::write_at(
                    &self.settings.audit_path,
                    "fingerprint_bank_sync_error",
                    json!({"code":failure.code}),
                    self.audit_time(),
                );
            }
        }
    }
}

#[cfg(test)]
mod compile_budget_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    struct NoopTransport;
    impl Transport for NoopTransport {
        fn get<'a>(
            &'a self,
            _url: reqwest::Url,
            _accept: &'a str,
        ) -> BoxFuture<'a, SyncResult<reqwest::Response>> {
            Box::pin(async { Err(Failure::new("network")) })
        }
    }

    #[test]
    fn cached_bank_startup_and_adopt_skip_compiling_bundled_bank() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::from_env(&std::collections::BTreeMap::from([
            ("DATABASE_URL".into(), "postgres://127.0.0.1/unused".into()),
            (
                "USAGE_QUERY_STATE_PATH".into(),
                dir.path().join("oauth-state.json").to_string_lossy().into(),
            ),
        ]))
        .unwrap();
        settings.audit_path = dir.path().join("audit.jsonl");
        let settings = Arc::new(settings);

        // Cold startup without cache compiles bundled bank exactly once.
        let before_cold = modeltrace::COMPILE_COUNT.load(Ordering::Relaxed);
        let cold = FingerprintBank::with_transport(
            settings.clone(),
            Arc::new(NoopTransport),
            Arc::new(|| 1_700_000_000.0),
        )
        .unwrap();
        assert_eq!(
            modeltrace::COMPILE_COUNT.load(Ordering::Relaxed) - before_cold,
            1
        );
        drop(cold);

        // Seed up-to-date disk cache using lazy_bundled_state (which uses bundled_version only).
        let before_lazy = modeltrace::COMPILE_COUNT.load(Ordering::Relaxed);
        let seeded = lazy_bundled_state();
        assert_eq!(
            modeltrace::COMPILE_COUNT.load(Ordering::Relaxed) - before_lazy,
            0
        );
        std::fs::write(
            settings.data_dir().join("modeltrace-bank-state.json"),
            serde_json::to_vec(&seeded).unwrap(),
        )
        .unwrap();

        // Warm startup over up-to-date cache + adopt_bundled compiles only the cache (1 total, not 2).
        let before_warm = modeltrace::COMPILE_COUNT.load(Ordering::Relaxed);
        let warm = FingerprintBank::with_transport(
            settings,
            Arc::new(NoopTransport),
            Arc::new(|| 1_700_000_000.0),
        )
        .unwrap();
        warm.adopt_bundled().unwrap();
        assert_eq!(
            modeltrace::COMPILE_COUNT.load(Ordering::Relaxed) - before_warm,
            1
        );
        assert_eq!(warm.public()["source"], "cache");
    }
}
