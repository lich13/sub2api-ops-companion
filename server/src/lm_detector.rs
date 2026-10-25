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
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

pub const ENGINE: &str = "lm_detector";
const REVISION: &str = "567e11d65a48d4fbba2713cc4dfbe685cdd05a73";
const FILES: [(&str, &[u8], usize); 3] = [
    (
        "unified_bank.json",
        include_bytes!("../resources/lm-detector/unified_bank.json"),
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
    pub bank: Value,
    pub version: Value,
}
impl Snapshot {
    fn load(path: &Path, meta: &Value) -> Result<Self> {
        for (name, _, limit) in FILES {
            let bytes = std::fs::read(path.join(name)).map_err(|_| unavailable())?;
            if bytes.len() > limit || sha256(&bytes) != meta["files"][name]["sha256"] {
                return Err(unavailable());
            }
        }
        let detector = lmfpd::Detector::load(&lmfpd::LoadOptions {
            sources: vec![path.to_string_lossy().into_owned()],
            cache: PathBuf::new(),
            ttl: 0.0,
        })
        .map_err(|_| unavailable())?;
        let bank: Value = serde_json::from_slice(&std::fs::read(path.join("unified_bank.json"))?)
            .map_err(|_| unavailable())?;
        if bank["models"]
            .as_array()
            .is_none_or(|v| v.is_empty() || v.len() > 256)
        {
            return Err(unavailable());
        }
        let tokenizer = tokenizer::Bank::parse(&std::fs::read(path.join("tokenizer_bank.json"))?)?;
        let version = json!({"revision":meta["revision"],"sha256":meta["files"]["shared_detector.bin"]["sha256"],"files":meta["files"],"built_at":detector.built_at(),"tokenizer_built_at":tokenizer.built_at(),"analyzer_version":"lmfpd-0.1.0"});
        Ok(Self {
            detector,
            tokenizer,
            bank,
            version,
        })
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
        self.bank["models"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|m| m["id"] == id)
            .is_some_and(|m| string(&m["display_name"]).contains('/'))
    }
}
#[derive(Clone)]
pub struct Bank {
    settings: Arc<Settings>,
    state: Store,
    root: PathBuf,
    snapshots: Arc<Mutex<BTreeMap<String, Arc<Snapshot>>>>,
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
            snapshots: Arc::new(Mutex::new(BTreeMap::new())),
            sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            sync_error: Arc::new(Mutex::new(None)),
            tasks: crate::tasks::Tasks::supervised(supervisor),
        }
    }
    pub fn revision(&self) -> Result<String> {
        let state = self.state.read()?;
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
        if let Some(snapshot) = cache.get(revision) {
            return Ok(snapshot.clone());
        }
        let mut path = self.root.join(revision);
        let mut temporary = None;
        if revision == REVISION && !path.exists() {
            let stage = if self.settings.read_only {
                tempfile::tempdir()?
            } else {
                std::fs::create_dir_all(&self.root)?;
                tempfile::tempdir_in(&self.root)?
            };
            let bundled = manifest();
            let mut files = json!({});
            for (name, bytes, _) in FILES {
                std::fs::write(stage.path().join(name), bytes)?;
                files[name] = bundled["files"][format!("data/{name}")].clone();
            }
            std::fs::write(
                stage.path().join("manifest.json"),
                serde_json::to_vec(&json!({"revision":revision,"files":files}))?,
            )?;
            if self.settings.read_only {
                path = stage.path().to_path_buf();
                temporary = Some(stage);
            } else {
                std::fs::rename(stage.path(), &path)?;
            }
        }
        let meta: Value = serde_json::from_slice(
            &std::fs::read(path.join("manifest.json")).map_err(|_| unavailable())?,
        )
        .map_err(|_| unavailable())?;
        if meta["revision"] != revision {
            return Err(unavailable());
        }
        let loaded = Arc::new(Snapshot::load(&path, &meta)?);
        drop(temporary);
        cache.insert(revision.into(), loaded.clone());
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
        let api = "https://api.github.com/repos/Ikaleio/lm-detector";
        let head: Value = serde_json::from_slice(
            &fetch(&client, &format!("{api}/commits/main"), 2 * 1024 * 1024).await?,
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
                &client,
                &format!("{api}/git/trees/{revision}"),
                2 * 1024 * 1024,
            )
            .await?,
        )
        .map_err(|_| unavailable())?;
        let data_sha = tree_entry(&root, "data", "tree")?;
        let data: Value = serde_json::from_slice(
            &fetch(
                &client,
                &format!("{api}/git/trees/{data_sha}"),
                2 * 1024 * 1024,
            )
            .await?,
        )
        .map_err(|_| unavailable())?;
        // Changes to the scoring/challenge/tokenizer algorithms require a service release.
        let fixed = manifest();
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
            let bytes = fetch(
                &client,
                &format!("https://raw.githubusercontent.com/Ikaleio/lm-detector/{revision}/{path}"),
                256 * 1024,
            )
            .await?;
            if sha256(bytes) != expected["sha256"] {
                return Err(Error::new(
                    409,
                    "上游算法已变化，请升级服务端；当前检测库已保留",
                ));
            }
        }
        std::fs::create_dir_all(&self.root)?;
        let stage = tempfile::tempdir_in(&self.root)?;
        let mut files = json!({});
        for (name, _, limit) in FILES {
            let blob = tree_entry(&data, name, "blob")?;
            let bytes = fetch(
                &client,
                &format!(
                    "https://raw.githubusercontent.com/Ikaleio/lm-detector/{revision}/data/{name}"
                ),
                limit,
            )
            .await?;
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
        let path = stage.path().to_path_buf();
        let check = meta.clone();
        let snapshot = crate::supervision::join(tokio::task::spawn_blocking(move || {
            Snapshot::load(&path, &check)
        }))
        .await
        .map_err(|_| unavailable())??;
        let dest = self.root.join(revision);
        if !dest.exists() {
            std::fs::rename(stage.path(), &dest)?;
        } else {
            Snapshot::load(&dest, &meta)?;
        }
        self.state.transaction(|state| {
            state["revision"] = revision.into();
            state["synced_at"] = crate::compat::now().into();
            Ok(())
        })?;
        self.snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(revision.into(), Arc::new(snapshot));
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
        let restarted = test_bank(root.path(), false);
        assert_eq!(
            restarted.public().await.unwrap()["version"]["revision"],
            "2".repeat(40)
        );
        assert_eq!(
            restarted.load(REVISION.into()).await.unwrap().version,
            pinned.version
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
        for (name, bytes, _) in FILES {
            std::fs::write(root.path().join(name), bytes).unwrap();
            files[name] = json!({"sha256":sha256(bytes)});
        }
        (root, json!({"revision":REVISION,"files":files}))
    }
    #[test]
    fn pinned_sdk_conformance_covers_single_three_and_invalid_samples() {
        let (root, meta) = fixture();
        let snapshot = Snapshot::load(root.path(), &meta).unwrap();
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
            let expected = &case["expected"];
            for key in [
                "decision",
                "prediction",
                "prediction_name",
                "used_outputs",
                "probability_status",
                "diagnostics",
                "sequence",
            ] {
                if key != "sequence" {
                    assert_eq!(actual[key], expected[key], "{} {key}", case["name"]);
                }
            }
            for (a, b) in actual["results"]
                .as_array()
                .unwrap()
                .iter()
                .zip(expected["results"].as_array().unwrap())
            {
                assert_eq!(a["model"], b["model"]);
                let (x, y) = (a["score"].as_f64().unwrap(), b["score"].as_f64().unwrap());
                assert!(
                    (x - y).abs() <= 1e-9_f64 * x.abs().max(y.abs()).max(1.0),
                    "{} {} delta={}",
                    case["name"],
                    a["model"],
                    (x - y).abs()
                );
                if let Some(probability) = b["probability"].as_f64() {
                    assert!((a["probability"].as_f64().unwrap() - probability).abs() < 1e-9);
                } else {
                    assert!(a["probability"].is_null());
                }
            }
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
        let mut bank: Value = serde_json::from_slice(FILES[0].1).unwrap();
        bank["models"].as_array_mut().unwrap().swap(0, 1);
        let bytes = serde_json::to_vec(&bank).unwrap();
        std::fs::write(root.path().join("unified_bank.json"), &bytes).unwrap();
        meta["files"]["unified_bank.json"]["sha256"] = sha256(bytes).into();
        assert!(Snapshot::load(root.path(), &meta).is_err());
    }
}
