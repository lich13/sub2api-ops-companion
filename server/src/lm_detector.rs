//! Paired, immutable LM Detector resources. SDK networking is deliberately unused.
use crate::{
    compat::{sha256, string},
    error::{Error, Result},
    fingerprint_bank::git_blob_digest,
    settings::Settings,
    store::{valid_object, Store},
    tokenizer,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

pub const ENGINE: &str = "lm_detector";
const REVISION: &str = "fd9b4caf6feafbec64d45a1552432515bf4efa39";
const PREVIOUS_BUNDLED_REVISION: &str = "567e11d65a48d4fbba2713cc4dfbe685cdd05a73";
const FILES: [(&str, &[u8], usize); 3] = [
    (
        "unified_bank.json",
        include_bytes!(concat!(env!("OUT_DIR"), "/lm-detector-bank.json")),
        32 * 1024 * 1024,
    ),
    (
        "shared_detector.bin",
        include_bytes!("../resources/lm-detector/shared_detector.bin"),
        32 * 1024 * 1024,
    ),
    (
        "tokenizer_bank.json",
        include_bytes!("../resources/lm-detector/tokenizer_bank.json"),
        2 * 1024 * 1024,
    ),
];
pub fn manifest() -> Value {
    serde_json::from_str(include_str!("../resources/lm-detector/manifest.json"))
        .expect("bundled manifest")
}
fn valid_revision(v: &str) -> bool {
    v.len() == 40
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn unavailable() -> Error {
    Error::new(503, "LM Detector 检测库无效或不兼容，请更新检测库或服务端")
}

pub struct Snapshot {
    pub detector: lmfpd::Detector,
    pub tokenizer: tokenizer::Bank,
    merged_models: BTreeSet<String>,
    pub version: Value,
}
impl Snapshot {
    fn bundled() -> Result<Self> {
        let bundled = manifest();
        let files: serde_json::Map<String, Value> = FILES
            .iter()
            .map(|(name, _, _)| {
                (
                    (*name).into(),
                    bundled["files"][format!("data/{name}")].clone(),
                )
            })
            .collect();
        let mut meta = json!({"revision":REVISION,"files":files});
        // The build verifies the full source bank; runtime verifies its compact
        // projection. Reports still identify the unchanged upstream source files.
        meta["files"]["unified_bank.json"]["sha256"] =
            include_str!(concat!(env!("OUT_DIR"), "/lm-detector-bank.sha256")).into();
        let mut snapshot = Self::load_bytes(FILES[0].1, FILES[1].1, FILES[2].1, &meta)?;
        snapshot.version["files"] = Value::Object(files);
        Ok(snapshot)
    }
    fn load_bytes(
        bank_bytes: &[u8],
        detector_bytes: &[u8],
        tokenizer_bytes: &[u8],
        meta: &Value,
    ) -> Result<Self> {
        for ((name, _, limit), bytes) in
            FILES
                .into_iter()
                .zip([bank_bytes, detector_bytes, tokenizer_bytes])
        {
            if bytes.len() > limit || sha256(bytes) != meta["files"][name]["sha256"] {
                return Err(unavailable());
            }
        }
        let detector =
            lmfpd::Detector::load_bytes(bank_bytes, detector_bytes).map_err(|_| unavailable())?;
        let models = detector.models();
        if models.is_empty() || models.len() > 256 {
            return Err(unavailable());
        }
        let merged_models = models
            .iter()
            .filter(|m| m.display_name.contains('/'))
            .map(|m| m.id.clone())
            .collect();
        let tokenizer = tokenizer::Bank::parse(tokenizer_bytes)?;
        let version = json!({"revision":meta["revision"],"sha256":meta["files"]["shared_detector.bin"]["sha256"],"files":meta["files"],"built_at":detector.built_at(),"tokenizer_built_at":tokenizer.built_at(),"analyzer_version":manifest()["analyzer_version"]});
        Ok(Self {
            detector,
            tokenizer,
            merged_models,
            version,
        })
    }
    fn load(path: &Path, meta: &Value) -> Result<Self> {
        let bank_bytes =
            std::fs::read(path.join("unified_bank.json")).map_err(|_| unavailable())?;
        let detector_bytes =
            std::fs::read(path.join("shared_detector.bin")).map_err(|_| unavailable())?;
        let tokenizer_bytes =
            std::fs::read(path.join("tokenizer_bank.json")).map_err(|_| unavailable())?;
        Self::load_bytes(&bank_bytes, &detector_bytes, &tokenizer_bytes, meta)
    }
    pub fn analyze(
        &self,
        samples: &BTreeMap<usize, String>,
        challenges: &[(usize, String)],
    ) -> Result<Value> {
        let outputs = samples
            .iter()
            .map(|(i, text)| lmfpd::Output {
                text: text.clone(),
                expected_count: challenges[*i].0 as u64,
            })
            .collect::<Vec<_>>();
        let mut report = serde_json::to_value(
            self.detector
                .analyze(&outputs, false)
                .map_err(|_| unavailable())?,
        )?;
        report["merged_class"] = json!(self.merged(string(&report["prediction"])));
        Ok(report)
    }
    fn merged(&self, id: &str) -> bool {
        self.merged_models.contains(id)
    }
}
#[derive(Default)]
struct Snapshots {
    active: Option<Arc<Snapshot>>,
    history: BTreeMap<String, Weak<Snapshot>>,
}
#[derive(Clone)]
pub struct Bank {
    settings: Arc<Settings>,
    state: Store,
    root: PathBuf,
    snapshots: Arc<Mutex<Snapshots>>,
    sync_lock: Arc<tokio::sync::Mutex<()>>,
    sync_error: Arc<Mutex<Option<String>>>,
    tasks: crate::tasks::Tasks,
}
impl Bank {
    pub fn new(settings: Arc<Settings>, supervisor: crate::supervision::Supervisor) -> Self {
        let root = settings.data_dir().join("lm-detector");
        Self {
            state: Store::new(
                root.join("state.json"),
                json!({"revision":REVISION,"checked_at":null,"synced_at":null,"error":""}),
                valid_object,
                settings.read_only,
            ),
            root,
            settings,
            snapshots: Arc::new(Mutex::new(Snapshots::default())),
            sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            sync_error: Arc::new(Mutex::new(None)),
            tasks: crate::tasks::Tasks::supervised(supervisor),
        }
    }
    pub fn revision(&self) -> Result<String> {
        let state = self.state.read_shared()?;
        let revision = string(&state["revision"]);
        if !valid_revision(revision) {
            return Err(unavailable());
        }
        Ok(revision.into())
    }
    pub fn snapshot(&self, revision: &str) -> Result<Arc<Snapshot>> {
        if !valid_revision(revision) {
            return Err(unavailable());
        }
        let mut cache = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
        let active = self.revision()?;
        cache.active = cache.history.get(&active).and_then(Weak::upgrade);
        cache
            .history
            .retain(|_, snapshot| snapshot.strong_count() > 0);
        if let Some(snapshot) = cache.history.get(revision).and_then(Weak::upgrade) {
            return Ok(snapshot);
        }
        let path = self.root.join(revision);
        let loaded = if revision == REVISION && !path.exists() {
            Arc::new(Snapshot::bundled()?)
        } else {
            let meta: Value = serde_json::from_slice(
                &std::fs::read(path.join("manifest.json")).map_err(|_| unavailable())?,
            )
            .map_err(|_| unavailable())?;
            if meta["revision"] != revision {
                return Err(unavailable());
            }
            Arc::new(Snapshot::load(&path, &meta)?)
        };
        cache
            .history
            .insert(revision.into(), Arc::downgrade(&loaded));
        if revision == active {
            cache.active = Some(loaded.clone());
        }
        Ok(loaded)
    }
    pub async fn load(&self, revision: String) -> Result<Arc<Snapshot>> {
        let this = self.clone();
        crate::supervision::join(tokio::task::spawn_blocking(move || {
            this.snapshot(&revision)
        }))
        .await
        .map_err(|_| unavailable())?
    }
    pub async fn migrate_bundled(&self) -> Result<()> {
        if self.settings.read_only || self.revision()? != PREVIOUS_BUNDLED_REVISION {
            return Ok(());
        }
        let _lock = self.sync_lock.lock().await;
        let this = self.clone();
        crate::supervision::join(tokio::task::spawn_blocking(move || {
            let snapshot = Arc::new(Snapshot::bundled()?);
            let changed = this.state.transaction(|state| {
                if state["revision"] != PREVIOUS_BUNDLED_REVISION {
                    return Ok(false);
                }
                state["revision"] = REVISION.into();
                state["synced_at"] = crate::compat::now().into();
                state["error"] = "".into();
                state["result"] = "updated".into();
                Ok(true)
            })?;
            if changed {
                let mut cache = this.snapshots.lock().unwrap_or_else(|e| e.into_inner());
                cache
                    .history
                    .insert(REVISION.into(), Arc::downgrade(&snapshot));
                cache.active = Some(snapshot);
            }
            Ok(())
        }))
        .await
        .map_err(|_| unavailable())?
    }
    pub async fn public(&self) -> Result<Value> {
        let mut state = self.state.read()?;
        if let Some(error) = self
            .sync_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            state["error"] = error.clone().into();
            state["result"] = "failed".into();
        }
        let snapshot = self.load(string(&state["revision"]).into()).await?;
        Ok(
            json!({"version":snapshot.version,"source":if state["revision"]==REVISION{"bundled"}else{"remote"},"checked_at":state["checked_at"],"synced_at":state["synced_at"],"error":state["error"],"result":state["result"],"status":if self.sync_lock.try_lock().is_err(){"syncing"}else{"idle"}}),
        )
    }
    pub async fn request_sync(&self) -> Result<Value> {
        if self.settings.read_only {
            return Err(Error::read_only());
        }
        let lock = self
            .sync_lock
            .clone()
            .try_lock_owned()
            .map_err(|_| Error::new(409, "检测库正在更新"))?;
        let this = self.clone();
        *self.sync_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.tasks.spawn(async move {
            let before = this.revision().ok();
            let result = tokio::time::timeout(Duration::from_secs(180), this.download())
                .await
                .unwrap_or_else(|_| Err(Error::new(504, "检测库更新超时，当前版本已保留")));
            let recorded = this.state.transaction(|state| {
                state["checked_at"] = crate::compat::now().into();
                state["error"] = result
                    .as_ref()
                    .err()
                    .map(|e| crate::security::safe_error(&e.detail, 200))
                    .unwrap_or_default()
                    .into();
                state["result"] = if result.is_err() {
                    json!("failed")
                } else if Some(string(&state["revision"])) == before.as_deref() {
                    json!("up-to-date")
                } else {
                    json!("updated")
                };
                Ok(())
            });
            if recorded.is_err() {
                *this.sync_error.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some("检测库更新状态无法保存，请检查服务端存储后重试".into());
            }
            drop(lock);
        })?;
        self.public().await
    }
    pub async fn close(&self, seconds: u64) {
        self.tasks.close(seconds).await;
    }
    async fn download(&self) -> Result<()> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(90))
            .user_agent("Sub2Ops-LM-Detector")
            .build()?;
        self.download_from(
            &client,
            "https://api.github.com/repos/Ikaleio/lm-detector",
            "https://raw.githubusercontent.com/Ikaleio/lm-detector",
            &manifest(),
        )
        .await
    }
    async fn download_from(
        &self,
        client: &reqwest::Client,
        api: &str,
        raw: &str,
        fixed: &Value,
    ) -> Result<()> {
        let head: Value = serde_json::from_slice(
            &fetch(client, &format!("{api}/commits/main"), 2 * 1024 * 1024).await?,
        )
        .map_err(|_| unavailable())?;
        let revision = string(&head["sha"]);
        if !valid_revision(revision) {
            return Err(unavailable());
        }
        if revision == self.revision()? {
            return Ok(());
        }
        // Resolve directories separately: the full repository tree can be much larger than the data itself.
        let root: Value = serde_json::from_slice(
            &fetch(
                client,
                &format!("{api}/git/trees/{revision}"),
                2 * 1024 * 1024,
            )
            .await?,
        )
        .map_err(|_| unavailable())?;
        let data_sha = tree_entry(&root, "data", "tree")?;
        let data: Value = serde_json::from_slice(
            &fetch(
                client,
                &format!("{api}/git/trees/{data_sha}"),
                2 * 1024 * 1024,
            )
            .await?,
        )
        .map_err(|_| unavailable())?;
        // Changes to the scoring/challenge/tokenizer algorithms require a service release.
        for (path, expected) in fixed["files"]
            .as_object()
            .ok_or_else(unavailable)?
            .iter()
            .filter(|(p, _)| {
                p.starts_with("shared/")
                    || p.starts_with("sdk/rust/src/")
                        && !p.ends_with("request.rs")
                        && !p.ends_with("sample.rs")
            })
        {
            let bytes = fetch(client, &format!("{raw}/{revision}/{path}"), 256 * 1024).await?;
            if sha256(bytes) != expected["sha256"] {
                return Err(Error::new(
                    409,
                    "上游 SDK 已变化，请升级服务端；当前检测库已保留",
                ));
            }
        }
        let blobs = FILES
            .iter()
            .map(|(name, _, _)| tree_entry(&data, name, "blob"))
            .collect::<Result<Vec<_>>>()?;
        if self.load(self.revision()?).await.is_ok_and(|active| {
            FILES.iter().zip(&blobs).all(|((name, _, _), blob)| {
                active.version["files"][*name]["git_blob"].as_str() == Some(blob.as_str())
            })
        }) {
            return Ok(());
        }
        std::fs::create_dir_all(&self.root)?;
        let stage = tempfile::tempdir_in(&self.root)?;
        let mut files = json!({});
        for ((name, _, limit), blob) in FILES.into_iter().zip(blobs) {
            let bytes = fetch(client, &format!("{raw}/{revision}/data/{name}"), limit).await?;
            if git_blob_digest(&bytes) != blob {
                return Err(unavailable());
            }
            files[name] = json!({"sha256":sha256(&bytes),"git_blob":blob,"bytes":bytes.len()});
            std::fs::write(stage.path().join(name), bytes)?;
        }
        let meta = json!({"revision":revision,"files":files});
        self.activate(stage, meta).await
    }
    async fn activate(&self, stage: tempfile::TempDir, meta: Value) -> Result<()> {
        let revision = string(&meta["revision"]);
        if !valid_revision(revision) {
            return Err(unavailable());
        }
        std::fs::write(
            stage.path().join("manifest.json"),
            serde_json::to_vec(&meta)?,
        )?;
        let dest = self.root.join(revision);
        let snapshot = if dest.exists() {
            let stage_path = stage.path().to_path_buf();
            let dest_path = dest.clone();
            let check = meta.clone();
            crate::supervision::join(tokio::task::spawn_blocking(move || -> Result<Snapshot> {
                for (name, _, limit) in FILES {
                    let bytes = std::fs::read(stage_path.join(name)).map_err(|_| unavailable())?;
                    if bytes.len() > limit || sha256(&bytes) != check["files"][name]["sha256"] {
                        return Err(unavailable());
                    }
                }
                Snapshot::load(&dest_path, &check)
            }))
            .await
            .map_err(|_| unavailable())??
        } else {
            let path = stage.path().to_path_buf();
            let check = meta.clone();
            let loaded = crate::supervision::join(tokio::task::spawn_blocking(move || {
                Snapshot::load(&path, &check)
            }))
            .await
            .map_err(|_| unavailable())??;
            std::fs::create_dir_all(&self.root)?;
            std::fs::rename(stage.path(), &dest)?;
            loaded
        };
        self.state.transaction(|state| {
            state["revision"] = revision.into();
            state["synced_at"] = crate::compat::now().into();
            Ok(())
        })?;
        let mut cache = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = Arc::new(snapshot);
        cache
            .history
            .insert(revision.into(), Arc::downgrade(&snapshot));
        cache.active = Some(snapshot);
        cache
            .history
            .retain(|_, snapshot| snapshot.strong_count() > 0);
        Ok(())
    }
}
fn tree_entry(tree: &Value, path: &str, kind: &str) -> Result<String> {
    tree["tree"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["path"] == path && v["type"] == kind)
        .and_then(|v| v["sha"].as_str())
        .filter(|s| valid_revision(s))
        .map(str::to_owned)
        .ok_or_else(unavailable)
}
async fn fetch(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>> {
    let response = client.get(url).send().await.map_err(|e| {
        Error::new(
            502,
            if e.is_timeout() {
                "检测库下载超时，当前版本已保留"
            } else {
                "检测库连接失败，当前版本已保留"
            },
        )
    })?;
    if !response.status().is_success() {
        return Err(Error::new(
            502,
            format!(
                "检测库下载失败（HTTP {}），当前版本已保留",
                response.status().as_u16()
            ),
        ));
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(Error::new(502, "检测库文件超过大小限制，当前版本已保留"));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = vec![];
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::new(502, "检测库下载中断，当前版本已保留"))?;
        if bytes.len() + chunk.len() > limit {
            return Err(Error::new(502, "检测库文件超过大小限制，当前版本已保留"));
        }
        bytes.extend(chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    const FULL_BANK: &[u8] = include_bytes!("../resources/lm-detector/unified_bank.json");

    fn full_files() -> [(&'static str, &'static [u8], usize); 3] {
        let mut files = FILES;
        files[0].1 = FULL_BANK;
        files
    }

    struct MockUpstream {
        base: String,
        requests: Arc<Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for MockUpstream {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl MockUpstream {
        async fn serve(responses: BTreeMap<String, (u16, Vec<u8>)>) -> Self {
            use axum::{body::Body, extract::Request, response::Response, routing::get, Router};
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            let responses = Arc::new(responses);
            let router = Router::new().route(
                "/{*path}",
                get(move |request: Request| {
                    let seen = seen.clone();
                    let responses = responses.clone();
                    async move {
                        let path = request.uri().path();
                        seen.lock().unwrap().push(path.into());
                        let (status, bytes) = responses.get(path).cloned().unwrap_or((404, vec![]));
                        Response::builder()
                            .status(status)
                            .body(Body::from(bytes))
                            .unwrap()
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            Self {
                base,
                requests,
                task,
            }
        }
        async fn update(&self, bank: &Bank) -> Result<()> {
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            bank.download_from(
                &client,
                &self.base,
                &self.base,
                &json!({"files":{
                    "sdk/rust/src/analysis.rs":{"sha256":sha256(b"fixture algorithm")}
                }}),
            )
            .await
        }
    }
    fn update_responses(revision: &str, changed: bool) -> BTreeMap<String, (u16, Vec<u8>)> {
        let tree = "d".repeat(40);
        let mut responses = BTreeMap::from([
            (
                "/commits/main".into(),
                (200, serde_json::to_vec(&json!({"sha":revision})).unwrap()),
            ),
            (
                format!("/git/trees/{revision}"),
                (
                    200,
                    serde_json::to_vec(&json!({"tree":[{"path":"data","type":"tree","sha":tree}]}))
                        .unwrap(),
                ),
            ),
            (
                format!("/{revision}/sdk/rust/src/analysis.rs"),
                (200, b"fixture algorithm".to_vec()),
            ),
        ]);
        let entries = full_files()
            .into_iter()
            .map(|(name, bytes, _)| {
                let mut bytes = bytes.to_vec();
                if changed && name == "unified_bank.json" {
                    bytes.push(b'\n');
                }
                let blob = git_blob_digest(&bytes);
                responses.insert(format!("/{revision}/data/{name}"), (200, bytes));
                json!({"path":name,"type":"blob","sha":blob})
            })
            .collect::<Vec<_>>();
        responses.insert(
            format!("/git/trees/{tree}"),
            (200, serde_json::to_vec(&json!({"tree":entries})).unwrap()),
        );
        responses
    }
    fn test_bank(root: &Path, read_only: bool) -> Bank {
        let env = BTreeMap::from([
            (
                "DATABASE_URL".into(),
                "postgres://example.invalid/sub2ops".into(),
            ),
            (
                "USAGE_QUERY_STATE_PATH".into(),
                root.join("usage.json").to_string_lossy().into_owned(),
            ),
            (
                "OAUTH_CONFIG_PATH".into(),
                root.join("oauth.json").to_string_lossy().into_owned(),
            ),
            (
                "BARK_CONFIG_PATH".into(),
                root.join("bark.json").to_string_lossy().into_owned(),
            ),
        ]);
        Bank::new(
            Arc::new(Settings::from_env(&env, read_only).unwrap()),
            crate::supervision::Supervisor::default(),
        )
    }
    #[tokio::test]
    async fn read_only_status_does_not_extract_into_the_data_directory() {
        let root = tempfile::tempdir().unwrap();
        let bank = test_bank(root.path(), true);
        assert_eq!(
            bank.public().await.unwrap()["version"]["revision"],
            REVISION
        );
        assert!(!bank.root.exists());
        assert!(bank.request_sync().await.is_err());
    }
    #[tokio::test]
    async fn known_bundled_upgrade_preserves_cached_and_pinned_history() {
        let root = tempfile::tempdir().unwrap();
        let bank = test_bank(root.path(), false);
        let (stage, mut meta) = fixture();
        meta["revision"] = PREVIOUS_BUNDLED_REVISION.into();
        bank.activate(stage, meta).await.unwrap();
        let pinned = bank.load(PREVIOUS_BUNDLED_REVISION.into()).await.unwrap();
        let old_version = pinned.version.clone();
        let old_bytes = std::fs::read(
            bank.root
                .join(PREVIOUS_BUNDLED_REVISION)
                .join("manifest.json"),
        )
        .unwrap();
        bank.state
            .transaction(|state| {
                state["error"] = "上游 SDK 已变化，请升级服务端；当前检测库已保留".into();
                state["result"] = "failed".into();
                Ok(())
            })
            .unwrap();
        bank.migrate_bundled().await.unwrap();
        let status = bank.public().await.unwrap();
        assert_eq!(status["version"]["revision"], REVISION);
        assert_eq!(status["version"]["analyzer_version"], "lmfpd-0.2.0");
        assert_eq!(status["source"], "bundled");
        assert_eq!(status["error"], "");
        assert_eq!(status["result"], "updated");
        assert_eq!(pinned.version, old_version);
        assert!(Arc::ptr_eq(
            &pinned,
            &bank.load(PREVIOUS_BUNDLED_REVISION.into()).await.unwrap()
        ));
        assert_eq!(
            std::fs::read(
                bank.root
                    .join(PREVIOUS_BUNDLED_REVISION)
                    .join("manifest.json")
            )
            .unwrap(),
            old_bytes
        );
        let saved = std::fs::read(bank.state.path()).unwrap();
        let restarted = test_bank(root.path(), false);
        restarted.migrate_bundled().await.unwrap();
        assert_eq!(std::fs::read(bank.state.path()).unwrap(), saved);
        assert_eq!(
            restarted.public().await.unwrap()["version"]["revision"],
            REVISION
        );
        assert_eq!(
            restarted
                .load(PREVIOUS_BUNDLED_REVISION.into())
                .await
                .unwrap()
                .version,
            old_version
        );
    }
    #[tokio::test]
    async fn migration_handles_unextracted_old_bundle_and_leaves_other_versions_alone() {
        let root = tempfile::tempdir().unwrap();
        let bank = test_bank(root.path(), false);
        bank.state
            .transaction(|state| {
                state["revision"] = PREVIOUS_BUNDLED_REVISION.into();
                Ok(())
            })
            .unwrap();
        assert!(!bank.root.join(PREVIOUS_BUNDLED_REVISION).exists());
        bank.migrate_bundled().await.unwrap();
        assert_eq!(
            bank.public().await.unwrap()["version"]["revision"],
            REVISION
        );
        let (stage, mut meta) = fixture();
        meta["revision"] = "3".repeat(40).into();
        bank.activate(stage, meta).await.unwrap();
        let saved = std::fs::read(bank.state.path()).unwrap();
        bank.migrate_bundled().await.unwrap();
        assert_eq!(std::fs::read(bank.state.path()).unwrap(), saved);
        assert_eq!(
            bank.public().await.unwrap()["version"]["revision"],
            "3".repeat(40)
        );
    }
    #[tokio::test]
    async fn migration_failure_and_read_only_mode_preserve_the_old_state() {
        let root = tempfile::tempdir().unwrap();
        let bank = test_bank(root.path(), false);
        bank.state
            .transaction(|state| {
                state["revision"] = PREVIOUS_BUNDLED_REVISION.into();
                Ok(())
            })
            .unwrap();
        let saved = std::fs::read(bank.state.path()).unwrap();
        let read_only = test_bank(root.path(), true);
        read_only.migrate_bundled().await.unwrap();
        let failing = Bank {
            state: Store::new(bank.state.path(), json!({}), valid_object, true),
            ..bank.clone()
        };
        assert!(failing.migrate_bundled().await.is_err());
        assert_eq!(std::fs::read(bank.state.path()).unwrap(), saved);
        assert_eq!(bank.revision().unwrap(), PREVIOUS_BUNDLED_REVISION);
        assert!(bank.snapshots.lock().unwrap().active.is_none());
    }
    #[tokio::test]
    async fn compatible_unchanged_data_skips_downloads_and_preserves_the_revision() {
        let root = tempfile::tempdir().unwrap();
        let bank = test_bank(root.path(), false);
        let revision = "1".repeat(40);
        let upstream = MockUpstream::serve(update_responses(&revision, false)).await;
        upstream.update(&bank).await.unwrap();
        assert_eq!(bank.revision().unwrap(), REVISION);
        assert!(!bank.root.exists());
        let requests = upstream.requests.lock().unwrap();
        assert!(requests.iter().any(|p| p.ends_with("analysis.rs")));
        assert!(!requests.iter().any(|p| p.contains("/data/")));
    }
    #[tokio::test]
    async fn changed_data_is_downloaded_once_and_reused_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let bank = test_bank(root.path(), false);
        let pinned = bank.load(REVISION.into()).await.unwrap();
        let revision = "1".repeat(40);
        let upstream = MockUpstream::serve(update_responses(&revision, true)).await;
        upstream.update(&bank).await.unwrap();
        assert_eq!(bank.revision().unwrap(), revision);
        assert_eq!(pinned.version["revision"], REVISION);
        assert_eq!(
            upstream
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p.contains("/data/"))
                .count(),
            3
        );
        let next_revision = "2".repeat(40);
        let next = MockUpstream::serve(update_responses(&next_revision, true)).await;
        let restarted = test_bank(root.path(), false);
        next.update(&restarted).await.unwrap();
        assert_eq!(restarted.revision().unwrap(), revision);
        assert!(!restarted.root.join(next_revision).exists());
        assert!(!next
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("/data/")));
    }
    #[tokio::test]
    async fn unreadable_active_cache_does_not_prevent_a_verified_new_revision() {
        let root = tempfile::tempdir().unwrap();
        let bank = test_bank(root.path(), false);
        let (stage, mut meta) = fixture();
        let old_revision = "1".repeat(40);
        meta["revision"] = old_revision.clone().into();
        bank.activate(stage, meta).await.unwrap();
        let old_detector = bank.root.join(&old_revision).join("shared_detector.bin");
        std::fs::write(&old_detector, b"damaged fixture cache").unwrap();
        drop(bank);

        let restarted = test_bank(root.path(), false);
        assert!(restarted.public().await.is_err());
        let revision = "2".repeat(40);
        let upstream = MockUpstream::serve(update_responses(&revision, false)).await;
        upstream.update(&restarted).await.unwrap();
        assert_eq!(
            restarted.public().await.unwrap()["version"]["revision"],
            revision
        );
        assert_eq!(
            upstream
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p.contains("/data/"))
                .count(),
            3
        );
        assert_eq!(
            std::fs::read(old_detector).unwrap(),
            b"damaged fixture cache"
        );
    }
    #[tokio::test]
    async fn incompatible_sdk_network_failure_and_invalid_downloads_keep_the_active_bank() {
        let revision = "1".repeat(40);
        for (path, status, bytes) in [
            (
                format!("/{revision}/sdk/rust/src/analysis.rs"),
                200,
                b"changed algorithm".to_vec(),
            ),
            (format!("/{revision}/data/shared_detector.bin"), 503, vec![]),
            (
                format!("/{revision}/data/shared_detector.bin"),
                200,
                b"invalid blob".to_vec(),
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let bank = test_bank(root.path(), false);
            let mut responses = update_responses(&revision, true);
            responses.insert(path.clone(), (status, bytes));
            let upstream = MockUpstream::serve(responses).await;
            let error = upstream.update(&bank).await.unwrap_err();
            if path.ends_with("analysis.rs") {
                assert_eq!(
                    error.detail,
                    "上游 SDK 已变化，请升级服务端；当前检测库已保留"
                );
                assert!(!upstream
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|p| p.contains("/data/")));
            }
            assert_eq!(bank.revision().unwrap(), REVISION);
            assert!(!bank.root.join(&revision).exists());
            assert_eq!(
                bank.public().await.unwrap()["version"]["revision"],
                REVISION
            );
        }
    }
    #[tokio::test]
    async fn updates_validate_all_files_and_keep_pinned_snapshots_across_restart() {
        let root = tempfile::tempdir().unwrap();
        let bank = test_bank(root.path(), false);
        let pinned = bank.load(REVISION.into()).await.unwrap();
        let (stage, mut meta) = fixture();
        meta["revision"] = "1".repeat(40).into();
        meta["files"]["tokenizer_bank.json"]["sha256"] = "0".repeat(64).into();
        assert!(bank.activate(stage, meta).await.is_err());
        assert_eq!(bank.revision().unwrap(), REVISION);
        let (stage, mut meta) = fixture();
        meta["revision"] = "2".repeat(40).into();
        bank.activate(stage, meta).await.unwrap();
        assert_eq!(bank.revision().unwrap(), "2".repeat(40));
        assert_eq!(pinned.version["revision"], REVISION);
        assert!(Arc::ptr_eq(
            &pinned,
            &bank.load(REVISION.into()).await.unwrap()
        ));
        let old = Arc::downgrade(&pinned);
        let restarted = test_bank(root.path(), false);
        assert_eq!(
            restarted.public().await.unwrap()["version"]["revision"],
            "2".repeat(40)
        );
        assert_eq!(
            restarted.load(REVISION.into()).await.unwrap().version,
            pinned.version
        );
        drop(pinned);
        assert!(
            old.upgrade().is_none(),
            "an unused historical detector is released immediately"
        );
        let active = bank.load("2".repeat(40)).await.unwrap();
        let active_weak = Arc::downgrade(&active);
        drop(active);
        assert!(
            active_weak.upgrade().is_some(),
            "the active detector stays warm"
        );
        assert_eq!(
            bank.load(REVISION.into()).await.unwrap().version["revision"],
            REVISION
        );
        let (stage, mut meta) = fixture();
        meta["revision"] = "3".repeat(40).into();
        let failing = Bank {
            state: Store::new(bank.state.path(), json!({}), valid_object, true),
            ..bank.clone()
        };
        assert!(failing.activate(stage, meta).await.is_err());
        assert_eq!(bank.revision().unwrap(), "2".repeat(40));
    }
    fn fixture() -> (tempfile::TempDir, Value) {
        let root = tempfile::tempdir().unwrap();
        let mut files = json!({});
        for (name, bytes, _) in full_files() {
            std::fs::write(root.path().join(name), bytes).unwrap();
            files[name] = manifest()["files"][format!("data/{name}")].clone();
        }
        (root, json!({"revision":REVISION,"files":files}))
    }
    #[test]
    fn pinned_sdk_conformance_covers_single_three_and_invalid_samples() {
        let (root, meta) = fixture();
        let snapshot = Snapshot::load(root.path(), &meta).unwrap();
        let compact = Snapshot::bundled().unwrap();
        assert_eq!(manifest()["revision"], REVISION);
        assert_eq!(compact.version, snapshot.version);
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/contracts/lm-detector/analyze.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let outputs: Vec<lmfpd::Output> =
                serde_json::from_value(case["outputs"].clone()).unwrap();
            let actual = serde_json::to_value(
                snapshot
                    .detector
                    .analyze(&outputs, case["strict"] == true)
                    .unwrap(),
            )
            .unwrap();
            assert_analysis(&actual, &case["expected"], string(&case["name"]));
            assert_eq!(
                actual,
                serde_json::to_value(
                    compact
                        .detector
                        .analyze(&outputs, case["strict"] == true)
                        .unwrap()
                )
                .unwrap()
            );
        }
        for count in [1, 3] {
            let c = lmfpd::generate_challenges(count).unwrap();
            assert_eq!(c.len(), count);
            assert!(c
                .iter()
                .all(|c| (292..=332).contains(&c.expected_count) && !c.prompt.is_empty()));
        }
        assert!(snapshot.merged("gpt-6-astra"));
    }
    fn assert_analysis(actual: &Value, expected: &Value, path: &str) {
        match (actual, expected) {
            (Value::Number(a), Value::Number(b)) => {
                let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
                assert!(
                    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0),
                    "{path}: {a} != {b}"
                );
            }
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len(), "{path}");
                for (i, (a, b)) in a.iter().zip(b).enumerate() {
                    assert_analysis(a, b, &format!("{path}[{i}]"));
                }
            }
            (Value::Object(a), Value::Object(b)) => {
                assert_eq!(a.len(), b.len(), "{path}");
                for (key, value) in b {
                    assert_analysis(a.get(key).unwrap(), value, &format!("{path}.{key}"));
                }
            }
            _ => assert_eq!(actual, expected, "{path}"),
        }
    }
    #[test]
    fn no_calibration_keeps_ranking_and_null_confidence() {
        let (root, mut meta) = fixture();
        let bytes = FILES[1].1;
        let n = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let mut header: Value = serde_json::from_slice(&bytes[4..4 + n]).unwrap();
        header.as_object_mut().unwrap().remove("calibration");
        let header = serde_json::to_vec(&header).unwrap();
        let mut changed = (header.len() as u32).to_le_bytes().to_vec();
        changed.extend(header);
        changed.resize(changed.len().div_ceil(4) * 4, 0);
        changed.extend_from_slice(&bytes[(4 + n).div_ceil(4) * 4..]);
        std::fs::write(root.path().join("shared_detector.bin"), &changed).unwrap();
        meta["files"]["shared_detector.bin"]["sha256"] = sha256(changed).into();
        let snapshot = Snapshot::load(root.path(), &meta).unwrap();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/contracts/lm-detector/analyze.json"
        ))
        .unwrap();
        let outputs =
            serde_json::from_value::<Vec<lmfpd::Output>>(fixture["cases"][0]["outputs"].clone())
                .unwrap();
        let report =
            serde_json::to_value(snapshot.detector.analyze(&outputs, false).unwrap()).unwrap();
        assert_eq!(report["decision"], "scored");
        assert!(report["probability"].is_null());
        assert!(!report["results"].as_array().unwrap().is_empty());
    }
    #[test]
    fn integrity_and_paired_model_order_are_required() {
        let (root, mut meta) = fixture();
        meta["files"]["shared_detector.bin"]["sha256"] = "0".repeat(64).into();
        assert!(Snapshot::load(root.path(), &meta).is_err());
        meta["files"]["shared_detector.bin"]["sha256"] = sha256(FILES[1].1).into();
        let mut bank: Value = serde_json::from_slice(FULL_BANK).unwrap();
        bank["models"].as_array_mut().unwrap().swap(0, 1);
        let bytes = serde_json::to_vec(&bank).unwrap();
        std::fs::write(root.path().join("unified_bank.json"), &bytes).unwrap();
        meta["files"]["unified_bank.json"]["sha256"] = sha256(bytes).into();
        assert!(Snapshot::load(root.path(), &meta).is_err());
    }
}
