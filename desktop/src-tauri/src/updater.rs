#![cfg_attr(not(mobile), allow(dead_code))]

use crate::{parsed_release_version, HttpFailure, RELEASES};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{watch, Mutex};

const RELEASES_API: &str =
    "https://api.github.com/repos/lich13/sub2api-ops-companion/releases?per_page=20";
const MAX_APK_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SUMS_BYTES: usize = 64 * 1024;
const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppUpdateSnapshot {
    pub phase: &'static str,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub message: String,
}

impl AppUpdateSnapshot {
    pub fn idle(current_version: &str) -> Self {
        Self {
            phase: "idle",
            current_version: current_version.to_string(),
            latest_version: None,
            downloaded_bytes: 0,
            total_bytes: None,
            message: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AndroidReleaseCandidate {
    pub version: String,
    pub tag: String,
    pub apk_name: String,
    pub apk_url: String,
    pub apk_size: u64,
    pub sha256: Option<String>,
    pub sums_url: Option<String>,
}

struct VerifiedApk {
    version: String,
    path: PathBuf,
    size: u64,
    sha256: String,
}

pub struct UpdaterState {
    client: reqwest::Client,
    cache_dir: PathBuf,
    current_version: String,
    snapshot: Mutex<AppUpdateSnapshot>,
    verified: Mutex<Option<VerifiedApk>>,
    cancel_tx: watch::Sender<u64>,
    run_seq: AtomicU64,
}

#[cfg(target_os = "android")]
pub struct AppUpdaterHandle(pub tauri::plugin::PluginHandle<tauri::Wry>);

#[cfg(target_os = "android")]
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri::Manager;
    tauri::plugin::Builder::new("app-updater")
        .setup(|app, api| {
            let handle = api.register_android_plugin("com.lich13.sub2ops", "AppUpdaterPlugin")?;
            app.manage(AppUpdaterHandle(handle));
            Ok(())
        })
        .build()
}

pub(crate) fn allowed_update_url(url: &url::Url, allow_loopback: bool) -> bool {
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return false;
    }
    if allow_loopback
        && url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
    {
        return true;
    }
    if url.scheme() != "https" || url.port().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("github.com")
        || host.eq_ignore_ascii_case("api.github.com")
        || host.eq_ignore_ascii_case("githubusercontent.com")
        || host
            .to_ascii_lowercase()
            .ends_with(".githubusercontent.com")
}

pub(crate) fn build_update_client(allow_loopback: bool) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .tcp_nodelay(true)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error("更新跳转次数过多");
            }
            if !allowed_update_url(attempt.url(), allow_loopback) {
                return attempt.error("拒绝跳转到非官方更新源");
            }
            attempt.follow()
        }))
        .retry(reqwest::retry::never())
        .build()
}

fn valid_sha256_hex(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let hex = trimmed.strip_prefix("sha256:").unwrap_or(trimmed).trim();
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

pub(crate) fn parse_sha256_sums(text: &str, apk_name: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(first) = parts.next() else {
            continue;
        };
        let Some(hash) = valid_sha256_hex(first) else {
            continue;
        };
        if let Some(rest) = parts.next() {
            let file = rest.trim_start_matches('*');
            let base = file.rsplit('/').next().unwrap_or(file);
            if base == apk_name {
                return Some(hash);
            }
        } else if text.lines().filter(|l| !l.trim().is_empty()).count() == 1 {
            return Some(hash);
        }
    }
    None
}

pub(crate) fn select_android_release(
    releases: &Value,
    current: &str,
    download_prefix: &str,
) -> Result<Option<AndroidReleaseCandidate>, &'static str> {
    let parts = current
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "当前版本号无效")?;
    if parts.len() != 3 {
        return Err("当前版本号无效");
    }
    let current_tuple = (parts[0], parts[1], parts[2]);
    let items = releases.as_array().ok_or("版本列表格式无效")?;
    let newest = items
        .iter()
        .filter_map(|release| {
            if release["draft"].as_bool().unwrap_or(false)
                || release["prerelease"].as_bool().unwrap_or(false)
            {
                return None;
            }
            let tag = release["tag_name"].as_str()?;
            let ver_tuple = parsed_release_version(tag)?;
            if ver_tuple <= current_tuple {
                return None;
            }
            Some((ver_tuple, tag, release))
        })
        .max_by_key(|item| item.0);
    let Some((ver_tuple, tag, release)) = newest else {
        return Ok(None);
    };
    let ver_str = format!("{}.{}.{}", ver_tuple.0, ver_tuple.1, ver_tuple.2);
    let expected_apk = format!("Sub2Ops_{ver_str}_arm64-v8a.apk");
    let expected_base = format!("{download_prefix}/download/{tag}/");
    let assets = release["assets"]
        .as_array()
        .ok_or("新版本尚未发布可用安装包")?;
    let apk_asset = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(expected_apk.as_str()))
        .ok_or("新版本尚未生成 Android ARM64 安装包")?;
    let apk_url = apk_asset["browser_download_url"]
        .as_str()
        .filter(|u| u.starts_with(&expected_base))
        .ok_or("安装包下载地址不受信任")?
        .to_string();
    let apk_size = apk_asset["size"]
        .as_u64()
        .filter(|&sz| (1..=MAX_APK_BYTES).contains(&sz))
        .ok_or("安装包大小超出安全范围")?;
    let sha256 = apk_asset["digest"].as_str().and_then(valid_sha256_hex);
    let sums_name_candidates = [
        format!("{expected_apk}.sha256"),
        "SHA256SUMS".to_string(),
        "SHA256SUMS.txt".to_string(),
    ];
    let sums_url = if sha256.is_none() {
        assets.iter().find_map(|a| {
            let name = a["name"].as_str()?;
            if !sums_name_candidates.iter().any(|c| c == name) {
                return None;
            }
            let size = a["size"].as_u64()?;
            if size == 0 || size > MAX_SUMS_BYTES as u64 {
                return None;
            }
            let url = a["browser_download_url"].as_str()?;
            url.starts_with(&expected_base).then(|| url.to_string())
        })
    } else {
        None
    };
    if sha256.is_none() && sums_url.is_none() {
        return Err("发布包缺少 SHA-256 校验值，已拒绝下载");
    }
    Ok(Some(AndroidReleaseCandidate {
        version: ver_str,
        tag: tag.to_string(),
        apk_name: expected_apk,
        apk_url,
        apk_size,
        sha256,
        sums_url,
    }))
}

pub(crate) fn verify_apk_file(path: &Path, expected_size: u64, expected_sha256: &str) -> bool {
    use std::io::Read;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() != expected_size {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let Ok(n) = file.read(&mut buf) else {
            return false;
        };
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    format!("{:x}", hasher.finalize()) == expected_sha256
}

fn cleanup_updates_dir(updates_dir: &Path, keep_file: Option<&str>) {
    let Ok(entries) = std::fs::read_dir(updates_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let keep = keep_file.is_some_and(|k| path.file_name().and_then(|n| n.to_str()) == Some(k));
        if !keep && path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) async fn resolve_candidate_sha256(
    client: &reqwest::Client,
    candidate: &AndroidReleaseCandidate,
) -> Result<String, String> {
    if let Some(digest) = &candidate.sha256 {
        return Ok(digest.clone());
    }
    let sums_url = candidate
        .sums_url
        .as_deref()
        .ok_or_else(|| "发布包缺少 SHA-256 校验值".to_string())?;
    let mut resp = tokio::time::timeout(
        CHUNK_TIMEOUT,
        client.get(sums_url).header("User-Agent", "Sub2Ops").send(),
    )
    .await
    .map_err(|_| "读取校验文件超时".to_string())?
    .map_err(|_| "无法读取校验文件".to_string())?;
    if !resp.status().is_success() {
        return Err(format!("校验文件服务异常 [{}]", resp.status().as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = tokio::time::timeout(CHUNK_TIMEOUT, resp.chunk())
        .await
        .map_err(|_| "读取校验文件超时".to_string())?
        .map_err(|_| "校验文件传输中断".to_string())?
    {
        if bytes.len() + chunk.len() > MAX_SUMS_BYTES {
            return Err("校验文件超出大小限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "校验文件格式无效".to_string())?;
    parse_sha256_sums(text, &candidate.apk_name)
        .ok_or_else(|| "校验文件未包含当前 APK 摘要".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DownloadError {
    Cancelled,
    Failed(String),
}

pub(crate) async fn ensure_verified_apk<F>(
    client: &reqwest::Client,
    updates_dir: &Path,
    candidate: &AndroidReleaseCandidate,
    expected_sha256: &str,
    mut cancel_rx: watch::Receiver<u64>,
    expected_seq: u64,
    mut on_progress: F,
) -> Result<PathBuf, DownloadError>
where
    F: FnMut(&'static str, u64, u64),
{
    std::fs::create_dir_all(updates_dir)
        .map_err(|_| DownloadError::Failed("无法创建更新缓存目录".into()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(updates_dir, std::fs::Permissions::from_mode(0o700));
    }
    let final_path = updates_dir.join(&candidate.apk_name);
    if final_path.exists() {
        on_progress("verifying", candidate.apk_size, candidate.apk_size);
        let path_clone = final_path.clone();
        let expected_size = candidate.apk_size;
        let expected_hash = expected_sha256.to_string();
        let valid = tokio::task::spawn_blocking(move || {
            verify_apk_file(&path_clone, expected_size, &expected_hash)
        })
        .await
        .unwrap_or(false);
        if valid {
            cleanup_updates_dir(updates_dir, Some(&candidate.apk_name));
            return Ok(final_path);
        }
        let _ = std::fs::remove_file(&final_path);
    }
    cleanup_updates_dir(updates_dir, None);

    let part_path = updates_dir.join(format!("{}.part", candidate.apk_name));
    on_progress("downloading", 0, candidate.apk_size);

    let send_fut = client
        .get(&candidate.apk_url)
        .header("User-Agent", "Sub2Ops")
        .send();
    let mut response = tokio::select! {
        biased;
        _ = cancel_rx.changed() => return Err(DownloadError::Cancelled),
        res = tokio::time::timeout(CHUNK_TIMEOUT, send_fut) => {
            res.map_err(|_| DownloadError::Failed("连接更新源超时".into()))?
                .map_err(|_| DownloadError::Failed("无法连接更新下载源".into()))?
        }
    };
    if *cancel_rx.borrow() != expected_seq {
        return Err(DownloadError::Cancelled);
    }
    if !response.status().is_success() {
        return Err(DownloadError::Failed(format!(
            "下载安装包失败 [{}]",
            response.status().as_u16()
        )));
    }
    if let Some(content_len) = response.content_length() {
        if content_len != candidate.apk_size {
            return Err(DownloadError::Failed(
                "远端安装包大小与发布记录不一致".into(),
            ));
        }
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&part_path)
        .map_err(|_| DownloadError::Failed("无法写入临时安装包".into()))?;
    let mut writer = std::io::BufWriter::with_capacity(256 * 1024, file);
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut last_emit = Instant::now();

    let stream_res: Result<(), DownloadError> = async {
        loop {
            let chunk_opt = tokio::select! {
                biased;
                _ = cancel_rx.changed() => return Err(DownloadError::Cancelled),
                chunk_res = tokio::time::timeout(CHUNK_TIMEOUT, response.chunk()) => {
                    chunk_res.map_err(|_| DownloadError::Failed("下载数据块超时，请重试".into()))?
                        .map_err(|_| DownloadError::Failed("下载连接中断，请重试".into()))?
                }
            };
            if *cancel_rx.borrow() != expected_seq {
                return Err(DownloadError::Cancelled);
            }
            let Some(chunk) = chunk_opt else {
                break;
            };
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            if downloaded > candidate.apk_size || downloaded > MAX_APK_BYTES {
                return Err(DownloadError::Failed(
                    "安装包体积超出预期，已中止下载".into(),
                ));
            }
            writer
                .write_all(&chunk)
                .map_err(|_| DownloadError::Failed("写入安装包失败，请检查存储空间".into()))?;
            hasher.update(&chunk);
            if last_emit.elapsed() >= Duration::from_millis(80) || downloaded == candidate.apk_size
            {
                on_progress("downloading", downloaded, candidate.apk_size);
                last_emit = Instant::now();
            }
        }
        Ok(())
    }
    .await;

    if let Err(err) = stream_res {
        drop(writer);
        let _ = std::fs::remove_file(&part_path);
        return Err(err);
    }

    if downloaded != candidate.apk_size {
        drop(writer);
        let _ = std::fs::remove_file(&part_path);
        return Err(DownloadError::Failed("安装包下载不完整，请重试".into()));
    }
    on_progress("verifying", downloaded, candidate.apk_size);
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if actual_sha256 != expected_sha256 {
        drop(writer);
        let _ = std::fs::remove_file(&part_path);
        return Err(DownloadError::Failed(
            "安装包 SHA-256 校验未通过，已拒绝安装".into(),
        ));
    }

    let updates_dir_owned = updates_dir.to_path_buf();
    let apk_name = candidate.apk_name.clone();
    let final_path_clone = final_path.clone();
    tokio::task::spawn_blocking(move || -> Result<(), DownloadError> {
        if writer.flush().is_err() {
            drop(writer);
            let _ = std::fs::remove_file(&part_path);
            return Err(DownloadError::Failed(
                "写入安装包失败，请检查存储空间".into(),
            ));
        }
        let file = match writer.into_inner() {
            Ok(f) => f,
            Err(_) => {
                let _ = std::fs::remove_file(&part_path);
                return Err(DownloadError::Failed(
                    "写入安装包失败，请检查存储空间".into(),
                ));
            }
        };
        if file.sync_all().is_err() {
            drop(file);
            let _ = std::fs::remove_file(&part_path);
            return Err(DownloadError::Failed("同步安装包文件失败".into()));
        }
        drop(file);
        if std::fs::rename(&part_path, &final_path_clone).is_err() {
            let _ = std::fs::remove_file(&part_path);
            return Err(DownloadError::Failed("保存安装包失败".into()));
        }
        cleanup_updates_dir(&updates_dir_owned, Some(&apk_name));
        Ok(())
    })
    .await
    .map_err(|_| DownloadError::Failed("保存安装包失败".into()))??;

    Ok(final_path)
}

impl UpdaterState {
    pub fn new(cache_dir: PathBuf, current_version: String) -> Result<Arc<Self>, reqwest::Error> {
        let (cancel_tx, _) = watch::channel(0u64);
        Ok(Arc::new(Self {
            client: build_update_client(false)?,
            cache_dir,
            snapshot: Mutex::new(AppUpdateSnapshot::idle(&current_version)),
            current_version,
            verified: Mutex::new(None),
            cancel_tx,
            run_seq: AtomicU64::new(0),
        }))
    }

    #[cfg(test)]
    fn new_for_test(cache_dir: PathBuf, current_version: &str) -> Arc<Self> {
        let (cancel_tx, _) = watch::channel(0u64);
        Arc::new(Self {
            client: build_update_client(true).unwrap(),
            cache_dir,
            snapshot: Mutex::new(AppUpdateSnapshot::idle(current_version)),
            current_version: current_version.to_string(),
            verified: Mutex::new(None),
            cancel_tx,
            run_seq: AtomicU64::new(0),
        })
    }

    pub async fn get_snapshot(&self) -> AppUpdateSnapshot {
        self.snapshot.lock().await.clone()
    }

    async fn set_and_emit(
        &self,
        app: Option<&tauri::AppHandle>,
        seq: Option<u64>,
        next: AppUpdateSnapshot,
    ) -> Option<AppUpdateSnapshot> {
        let mut guard = self.snapshot.lock().await;
        if let Some(expected) = seq {
            if self.run_seq.load(Ordering::SeqCst) != expected {
                return None;
            }
        }
        *guard = next.clone();
        drop(guard);
        if let Some(_handle) = app {
            #[cfg(mobile)]
            {
                use tauri::Emitter;
                let _ = _handle.emit("app-update", &next);
            }
        }
        Some(next)
    }

    pub async fn cancel(&self, app: Option<&tauri::AppHandle>) -> AppUpdateSnapshot {
        let next_seq = self.run_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.cancel_tx.send(next_seq);
        *self.verified.lock().await = None;
        clear_android_pending(app.cloned()).await;
        let updates_dir = self.cache_dir.join("updates");
        if let Ok(entries) = std::fs::read_dir(&updates_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("part") {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        let next = AppUpdateSnapshot {
            phase: "idle",
            current_version: self.current_version.clone(),
            latest_version: None,
            downloaded_bytes: 0,
            total_bytes: None,
            message: "已取消更新下载".into(),
        };
        self.set_and_emit(app, None, next.clone())
            .await
            .unwrap_or(next)
    }

    pub async fn start(self: &Arc<Self>, app: tauri::AppHandle) -> AppUpdateSnapshot {
        {
            let guard = self.snapshot.lock().await;
            if matches!(guard.phase, "checking" | "downloading" | "verifying") {
                return guard.clone();
            }
        }
        let seq = self.run_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.cancel_tx.send(seq);
        let checking = AppUpdateSnapshot {
            phase: "checking",
            current_version: self.current_version.clone(),
            latest_version: None,
            downloaded_bytes: 0,
            total_bytes: None,
            message: "正在检查新版本…".into(),
        };
        let _ = self
            .set_and_emit(Some(&app), Some(seq), checking.clone())
            .await;

        let this = self.clone();
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            this.run_workflow(handle, seq, RELEASES_API, RELEASES).await;
        });
        checking
    }

    async fn run_workflow(
        self: &Arc<Self>,
        app: tauri::AppHandle,
        seq: u64,
        releases_api: &str,
        download_prefix: &str,
    ) {
        let mut cancel_rx = self.cancel_tx.subscribe();
        let fetch_fut = async {
            let resp = tokio::time::timeout(
                Duration::from_secs(15),
                self.client
                    .get(releases_api)
                    .header("User-Agent", "Sub2Ops")
                    .send(),
            )
            .await
            .map_err(|_| "检查更新超时，请检查网络连接".to_string())?
            .map_err(|_| "无法连接 GitHub 版本服务".to_string())?;
            if !resp.status().is_success() {
                return Err(format!("版本服务暂不可用 [{}]", resp.status().as_u16()));
            }
            tokio::time::timeout(Duration::from_secs(15), resp.json::<Value>())
                .await
                .map_err(|_| "读取版本信息超时".to_string())?
                .map_err(|_| "版本信息格式无效".to_string())
        };

        let releases = tokio::select! {
            biased;
            _ = cancel_rx.changed() => return,
            res = fetch_fut => match res {
                Ok(val) => val,
                Err(msg) => {
                    self.fail(Some(&app), seq, None, msg).await;
                    return;
                }
            }
        };
        if self.run_seq.load(Ordering::SeqCst) != seq {
            return;
        }

        let candidate =
            match select_android_release(&releases, &self.current_version, download_prefix) {
                Ok(Some(c)) => c,
                Ok(None) => {
                    let up_to_date = AppUpdateSnapshot {
                        phase: "up_to_date",
                        current_version: self.current_version.clone(),
                        latest_version: Some(self.current_version.clone()),
                        downloaded_bytes: 0,
                        total_bytes: None,
                        message: format!("当前版本 {} 已是最新", self.current_version),
                    };
                    let _ = self.set_and_emit(Some(&app), Some(seq), up_to_date).await;
                    return;
                }
                Err(err) => {
                    self.fail(Some(&app), seq, None, err.to_string()).await;
                    return;
                }
            };

        let expected_sha256 = match resolve_candidate_sha256(&self.client, &candidate).await {
            Ok(hash) => hash,
            Err(err) => {
                self.fail(Some(&app), seq, Some(candidate.version), err)
                    .await;
                return;
            }
        };
        if self.run_seq.load(Ordering::SeqCst) != seq {
            return;
        }

        let updates_dir = self.cache_dir.join("updates");
        let version = candidate.version.clone();
        let total = candidate.apk_size;
        let (prog_tx, mut prog_rx) = watch::channel::<Option<(&'static str, u64, u64)>>(None);
        let progress_state = self.clone();
        let progress_app = app.clone();
        let progress_ver = version.clone();
        let forwarder = tauri::async_runtime::spawn(async move {
            while prog_rx.changed().await.is_ok() {
                let Some((phase, dl, tot)) = *prog_rx.borrow_and_update() else {
                    continue;
                };
                let msg = if phase == "verifying" {
                    format!("正在校验 v{progress_ver} 安装包完整性…")
                } else {
                    format!("正在下载 v{progress_ver}…")
                };
                let _ = progress_state
                    .set_and_emit(
                        Some(&progress_app),
                        Some(seq),
                        AppUpdateSnapshot {
                            phase,
                            current_version: progress_state.current_version.clone(),
                            latest_version: Some(progress_ver.clone()),
                            downloaded_bytes: dl,
                            total_bytes: Some(tot),
                            message: msg,
                        },
                    )
                    .await;
            }
        });

        let download_res = ensure_verified_apk(
            &self.client,
            &updates_dir,
            &candidate,
            &expected_sha256,
            cancel_rx,
            seq,
            move |phase, dl, tot| {
                let _ = prog_tx.send(Some((phase, dl, tot)));
            },
        )
        .await;
        let _ = forwarder.await;

        let apk_path = match download_res {
            Ok(path) => path,
            Err(DownloadError::Cancelled) => return,
            Err(DownloadError::Failed(err)) => {
                self.fail(Some(&app), seq, Some(version), err).await;
                return;
            }
        };
        if self.run_seq.load(Ordering::SeqCst) != seq {
            return;
        }

        *self.verified.lock().await = Some(VerifiedApk {
            version: version.clone(),
            path: apk_path.clone(),
            size: total,
            sha256: expected_sha256,
        });
        let _ = self
            .trigger_install(&app, Some(seq), &version, &apk_path, total)
            .await;
    }

    pub async fn install_verified(
        &self,
        app: &tauri::AppHandle,
    ) -> Result<AppUpdateSnapshot, HttpFailure> {
        let (version, path, size, sha256) = {
            let guard = self.verified.lock().await;
            let item = guard
                .as_ref()
                .ok_or_else(|| HttpFailure::from("尚未准备好可安装的更新包，请重新检查更新"))?;
            (
                item.version.clone(),
                item.path.clone(),
                item.size,
                item.sha256.clone(),
            )
        };
        let path_check = path.clone();
        let valid =
            tokio::task::spawn_blocking(move || verify_apk_file(&path_check, size, &sha256))
                .await
                .unwrap_or(false);
        if !valid {
            *self.verified.lock().await = None;
            return Err("本地更新包已失效，请重新下载更新".into());
        }
        self.trigger_install(app, None, &version, &path, size).await
    }

    async fn trigger_install(
        &self,
        app: &tauri::AppHandle,
        seq: Option<u64>,
        version: &str,
        apk_path: &Path,
        total_bytes: u64,
    ) -> Result<AppUpdateSnapshot, HttpFailure> {
        match invoke_android_installer(app.clone(), apk_path.to_path_buf()).await {
            Ok(status) if status == "permission_required" => {
                let snap = AppUpdateSnapshot {
                    phase: "permission_required",
                    current_version: self.current_version.clone(),
                    latest_version: Some(version.to_string()),
                    downloaded_bytes: total_bytes,
                    total_bytes: Some(total_bytes),
                    message: "请开启「允许安装未知应用」权限，返回后将自动继续安装".into(),
                };
                let _ = self.set_and_emit(Some(app), seq, snap.clone()).await;
                Ok(snap)
            }
            Ok(_) => {
                let snap = AppUpdateSnapshot {
                    phase: "installing",
                    current_version: self.current_version.clone(),
                    latest_version: Some(version.to_string()),
                    downloaded_bytes: total_bytes,
                    total_bytes: Some(total_bytes),
                    message: format!("已校验 v{version}，正在调起系统安装器…"),
                };
                let _ = self.set_and_emit(Some(app), seq, snap.clone()).await;
                Ok(snap)
            }
            Err(err) => {
                let snap = AppUpdateSnapshot {
                    phase: "error",
                    current_version: self.current_version.clone(),
                    latest_version: Some(version.to_string()),
                    downloaded_bytes: total_bytes,
                    total_bytes: Some(total_bytes),
                    message: err.clone(),
                };
                let _ = self.set_and_emit(Some(app), seq, snap).await;
                Err(err.into())
            }
        }
    }

    async fn fail(
        &self,
        app: Option<&tauri::AppHandle>,
        seq: u64,
        latest_version: Option<String>,
        message: String,
    ) {
        let snap = AppUpdateSnapshot {
            phase: "error",
            current_version: self.current_version.clone(),
            latest_version,
            downloaded_bytes: 0,
            total_bytes: None,
            message,
        };
        let _ = self.set_and_emit(app, Some(seq), snap).await;
    }
}

#[cfg(target_os = "android")]
async fn invoke_android_installer(app: tauri::AppHandle, path: PathBuf) -> Result<String, String> {
    use tauri::Manager;
    let raw_path = path
        .to_str()
        .ok_or_else(|| "安装包路径编码无效".to_string())?
        .to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let result: Value = app
            .state::<AppUpdaterHandle>()
            .0
            .run_mobile_plugin("installApk", serde_json::json!({ "path": raw_path }))
            .map_err(|e| format!("调起安装器失败：{e}"))?;
        Ok(result["status"]
            .as_str()
            .unwrap_or("installing")
            .to_string())
    })
    .await
    .map_err(|_| "安装器调用任务异常".to_string())?
}

#[cfg(not(target_os = "android"))]
async fn invoke_android_installer(
    _app: tauri::AppHandle,
    _path: PathBuf,
) -> Result<String, String> {
    Err("仅 Android 客户端支持内置 APK 安装".into())
}

#[cfg(target_os = "android")]
async fn clear_android_pending(app: Option<tauri::AppHandle>) {
    use tauri::Manager;
    let Some(handle) = app else {
        return;
    };
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let _ = handle
            .state::<AppUpdaterHandle>()
            .0
            .run_mobile_plugin::<Value>("clearPending", serde_json::json!({}));
    })
    .await;
}

#[cfg(not(target_os = "android"))]
async fn clear_android_pending(_app: Option<tauri::AppHandle>) {}

#[cfg(mobile)]
pub fn on_resumed(app: tauri::AppHandle) {
    use tauri::Manager;
    tauri::async_runtime::spawn(async move {
        let updater = app.state::<Arc<UpdaterState>>().inner().clone();
        let current = updater.get_snapshot().await;
        if current.phase != "permission_required" {
            return;
        }
        #[cfg(target_os = "android")]
        {
            let handle_app = app.clone();
            let resumed = tauri::async_runtime::spawn_blocking(move || {
                handle_app
                    .state::<AppUpdaterHandle>()
                    .0
                    .run_mobile_plugin::<Value>("resumePending", serde_json::json!({}))
            })
            .await;
            if let Ok(Ok(val)) = resumed {
                match val["status"].as_str() {
                    Some("installing") => {
                        let ver = current
                            .latest_version
                            .clone()
                            .unwrap_or_else(|| updater.current_version.clone());
                        let next = AppUpdateSnapshot {
                            phase: "installing",
                            message: format!("权限已授予，正在调起系统安装器（v{ver}）…"),
                            ..current
                        };
                        let _ = updater.set_and_emit(Some(&app), None, next).await;
                    }
                    Some("error") => {
                        let msg = val["error"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("无法调起系统安装器")
                            .to_string();
                        let next = AppUpdateSnapshot {
                            phase: "error",
                            message: msg,
                            ..current
                        };
                        let _ = updater.set_and_emit(Some(&app), None, next).await;
                    }
                    _ => {}
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    #[test]
    fn redirect_url_whitelist_accepts_github_cdn_and_rejects_untrusted_origins() {
        for valid in [
            "https://github.com/lich13/sub2api-ops-companion/releases/download/desktop-v0.1.106/Sub2Ops_0.1.106_arm64-v8a.apk",
            "https://api.github.com/repos/lich13/sub2api-ops-companion/releases?per_page=20",
            "https://objects.githubusercontent.com/github-production-release-asset/123?X-Amz-Signature=abc",
            "https://release-assets.githubusercontent.com/github-production-release-asset/456",
        ] {
            let parsed = url::Url::parse(valid).unwrap();
            assert!(allowed_update_url(&parsed, false), "should allow {valid}");
        }
        for invalid in [
            "http://github.com/lich13/sub2api-ops-companion/releases",
            "https://github.com:8443/lich13/sub2api-ops-companion",
            "https://evilgithubusercontent.com/payload.apk",
            "https://github.com.evil.example/payload.apk",
            "https://token@github.com/lich13/sub2api-ops-companion",
            "https://github.com/lich13/sub2api-ops-companion#frag",
            "http://127.0.0.1:8080/test.apk",
        ] {
            let parsed = url::Url::parse(invalid).unwrap();
            assert!(
                !allowed_update_url(&parsed, false),
                "should reject {invalid}"
            );
        }
        assert!(allowed_update_url(
            &url::Url::parse("http://127.0.0.1:18080/test.apk").unwrap(),
            true
        ));
    }

    #[test]
    fn release_selection_verifies_tag_asset_size_and_sha256_sources() {
        let hash = "a".repeat(64);
        let releases = serde_json::json!([
            {
                "tag_name": "desktop-v0.2.0",
                "draft": true,
                "assets": []
            },
            {
                "tag_name": "desktop-v0.1.107",
                "prerelease": true,
                "assets": []
            },
            {
                "tag_name": "desktop-v0.1.106",
                "assets": [
                    {
                        "name": "Sub2Ops_0.1.106_arm64-v8a.apk",
                        "size": 4096,
                        "digest": format!("sha256:{hash}"),
                        "browser_download_url": format!("{RELEASES}/download/desktop-v0.1.106/Sub2Ops_0.1.106_arm64-v8a.apk")
                    }
                ]
            }
        ]);
        assert_eq!(
            select_android_release(&releases, "0.1.106", RELEASES).unwrap(),
            None
        );
        assert_eq!(
            select_android_release(&releases, "0.1.107", RELEASES).unwrap(),
            None
        );
        let selected = select_android_release(&releases, "0.1.105", RELEASES)
            .unwrap()
            .unwrap();
        assert_eq!(selected.version, "0.1.106");
        assert_eq!(selected.sha256.as_deref(), Some(hash.as_str()));

        // Fallback to SHA256SUMS when digest field is absent
        let sums_releases = serde_json::json!([
            {
                "tag_name": "desktop-v0.1.106",
                "assets": [
                    {
                        "name": "Sub2Ops_0.1.106_arm64-v8a.apk",
                        "size": 4096,
                        "browser_download_url": format!("{RELEASES}/download/desktop-v0.1.106/Sub2Ops_0.1.106_arm64-v8a.apk")
                    },
                    {
                        "name": "SHA256SUMS",
                        "size": 128,
                        "browser_download_url": format!("{RELEASES}/download/desktop-v0.1.106/SHA256SUMS")
                    }
                ]
            }
        ]);
        let sums_candidate = select_android_release(&sums_releases, "0.1.105", RELEASES)
            .unwrap()
            .unwrap();
        assert!(sums_candidate.sha256.is_none());
        assert!(sums_candidate.sums_url.is_some());
        assert_eq!(
            parse_sha256_sums(
                &format!("{} *dist/Sub2Ops_0.1.106_arm64-v8a.apk\n", "B".repeat(64)),
                "Sub2Ops_0.1.106_arm64-v8a.apk"
            )
            .as_deref(),
            Some("b".repeat(64).as_str())
        );

        // Reject release missing both digest and SHA256SUMS
        let no_hash = serde_json::json!([
            {
                "tag_name": "desktop-v0.1.106",
                "assets": [
                    {
                        "name": "Sub2Ops_0.1.106_arm64-v8a.apk",
                        "size": 4096,
                        "browser_download_url": format!("{RELEASES}/download/desktop-v0.1.106/Sub2Ops_0.1.106_arm64-v8a.apk")
                    }
                ]
            }
        ]);
        assert!(select_android_release(&no_hash, "0.1.105", RELEASES).is_err());
    }

    #[tokio::test]
    async fn streaming_download_verifies_sha256_reuses_cache_rejects_bad_hash_and_cancels() {
        let payload = b"synthetic-signed-arm64-apk-payload-for-updater-test";
        let valid_hash = format!("{:x}", Sha256::digest(payload));
        let dir = tempfile::tempdir().unwrap();
        let updates_dir = dir.path().join("updates");
        std::fs::create_dir_all(&updates_dir).unwrap();
        std::fs::write(updates_dir.join("Sub2Ops_0.1.104_arm64-v8a.apk"), b"old").unwrap();

        // 1. Valid streaming download + stale cleanup + cache reuse
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let _ = sock.read(&mut req);
            write!(
                sock,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            )
            .unwrap();
            sock.write_all(payload).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert!(
                listener.accept().is_err(),
                "cache hit must not make a second HTTP request"
            );
        });

        let state = UpdaterState::new_for_test(dir.path().to_path_buf(), "0.1.105");
        let candidate = AndroidReleaseCandidate {
            version: "0.1.106".into(),
            tag: "desktop-v0.1.106".into(),
            apk_name: "Sub2Ops_0.1.106_arm64-v8a.apk".into(),
            apk_url: format!("http://{addr}/Sub2Ops_0.1.106_arm64-v8a.apk"),
            apk_size: payload.len() as u64,
            sha256: Some(valid_hash.clone()),
            sums_url: None,
        };
        let (tx, rx) = watch::channel(1u64);
        let apk_path = ensure_verified_apk(
            &state.client,
            &updates_dir,
            &candidate,
            &valid_hash,
            rx.clone(),
            1,
            |_, _, _| {},
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&apk_path).unwrap(), payload);
        assert!(!updates_dir.join("Sub2Ops_0.1.104_arm64-v8a.apk").exists());
        assert!(!updates_dir
            .join("Sub2Ops_0.1.106_arm64-v8a.apk.part")
            .exists());

        // Second invocation reuses verified cached APK without touching network
        let cached_path = ensure_verified_apk(
            &state.client,
            &updates_dir,
            &candidate,
            &valid_hash,
            rx,
            1,
            |_, _, _| {},
        )
        .await
        .unwrap();
        assert_eq!(cached_path, apk_path);
        server.join().unwrap();

        // 2. Tampered payload rejected and .part removed
        std::fs::remove_file(&apk_path).unwrap();
        let bad_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let bad_addr = bad_listener.local_addr().unwrap();
        let bad_server = std::thread::spawn(move || {
            let (mut sock, _) = bad_listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let _ = sock.read(&mut req);
            let tampered = vec![b'X'; payload.len()];
            write!(
                sock,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                tampered.len()
            )
            .unwrap();
            sock.write_all(&tampered).unwrap();
        });
        let mut bad_candidate = candidate.clone();
        bad_candidate.apk_url = format!("http://{bad_addr}/bad.apk");
        let err = ensure_verified_apk(
            &state.client,
            &updates_dir,
            &bad_candidate,
            &valid_hash,
            tx.subscribe(),
            1,
            |_, _, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            DownloadError::Failed(ref msg) if msg.contains("SHA-256")
        ));
        assert!(!apk_path.exists());
        assert!(!updates_dir
            .join("Sub2Ops_0.1.106_arm64-v8a.apk.part")
            .exists());
        bad_server.join().unwrap();

        // 3. Mid-stream cancellation cleans up .part
        let slow_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let slow_addr = slow_listener.local_addr().unwrap();
        let slow_server = std::thread::spawn(move || {
            let (mut sock, _) = slow_listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let _ = sock.read(&mut req);
            let _ = write!(sock, "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\npart1");
            std::thread::sleep(Duration::from_millis(150));
        });
        let mut slow_candidate = candidate;
        slow_candidate.apk_url = format!("http://{slow_addr}/slow.apk");
        slow_candidate.apk_size = 1000;
        let cancel_rx = tx.subscribe();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = tx.send(2);
        });
        let cancel_err = ensure_verified_apk(
            &state.client,
            &updates_dir,
            &slow_candidate,
            &valid_hash,
            cancel_rx,
            1,
            |_, _, _| {},
        )
        .await
        .unwrap_err();
        let _ = canceller.await;
        assert_eq!(cancel_err, DownloadError::Cancelled);
        assert!(!updates_dir
            .join("Sub2Ops_0.1.106_arm64-v8a.apk.part")
            .exists());
        let _ = slow_server.join();
    }

    #[tokio::test]
    async fn cancel_clears_verified_apk_and_resets_permission_required_phase() {
        let dir = tempfile::tempdir().unwrap();
        let updates_dir = dir.path().join("updates");
        std::fs::create_dir_all(&updates_dir).unwrap();
        let part_file = updates_dir.join("Sub2Ops_0.1.106_arm64-v8a.apk.part");
        std::fs::write(&part_file, b"partial").unwrap();

        let state = UpdaterState::new_for_test(dir.path().to_path_buf(), "0.1.105");
        *state.verified.lock().await = Some(VerifiedApk {
            version: "0.1.106".into(),
            path: updates_dir.join("Sub2Ops_0.1.106_arm64-v8a.apk"),
            size: 16,
            sha256: "a".repeat(64),
        });
        *state.snapshot.lock().await = AppUpdateSnapshot {
            phase: "permission_required",
            current_version: "0.1.105".into(),
            latest_version: Some("0.1.106".into()),
            downloaded_bytes: 16,
            total_bytes: Some(16),
            message: "请开启未知来源权限".into(),
        };

        let next = state.cancel(None).await;
        assert_eq!(next.phase, "idle");
        assert_eq!(next.latest_version, None);
        assert!(state.verified.lock().await.is_none());
        assert!(!part_file.exists());
    }
}
