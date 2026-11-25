use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
#[cfg(mobile)]
use tauri::{Emitter, WindowEvent};
use tauri::{Manager, State};
#[cfg(desktop)]
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{watch, Mutex, Notify};

mod api_http;
mod credentials;
mod updater;
use api_http::{http_detailed, http_snapshot, HttpFailure, SnapshotFetch};

pub(crate) const RELEASES: &str = "https://github.com/lich13/sub2api-ops-companion/releases";

#[cfg(target_os = "macos")]
mod macos_bridge;
#[cfg(target_os = "macos")]
mod record_activity;

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
struct SavedConnection {
    name: String,
    base_url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    base_url: String,
    connections: Vec<SavedConnection>,
    launch_at_login: bool,
    pinned: bool,
    record_columns: Option<Vec<String>>,
    model_test_concurrency: Option<u8>,
    model_test_preferences: std::collections::BTreeMap<String, ModelTestPreference>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ModelTestPreference {
    engine: String,
    sample_count: u8,
}

fn normalize_record_columns(columns: Vec<String>) -> Vec<String> {
    sub2ops_protocol::dto::RECORD_COLUMNS
        .iter()
        .filter(|c| columns.iter().any(|v| v == **c))
        .map(|c| c.to_string())
        .collect()
}

#[derive(Clone, Serialize, Default)]
struct ViewState {
    platform: &'static str,
    foreground: bool,
    initializing: bool,
    connection_revision: u64,
    snapshot_revision: u64,
    connected: bool,
    online: bool,
    error: String,
    upstream_connection: Option<Value>,
    snapshot: Option<Arc<Box<serde_json::value::RawValue>>>,
    preferences: Preferences,
}

struct Runtime {
    client: reqwest::Client,
    view: Mutex<ViewState>,
    key: Mutex<Option<String>>,
    snapshot_etag: Mutex<Option<String>>,
    connecting_url: Mutex<Option<String>>,
    network: Mutex<()>,
    snapshot_gate: Mutex<()>,
    generation: AtomicU64,
    wake: Notify,
    path: PathBuf,
    foreground: AtomicBool,
    read_epoch: watch::Sender<u64>,
    #[cfg(target_os = "macos")]
    record_activity: record_activity::RecordActivity,
}

fn normalize_base(raw: &str) -> Result<String, String> {
    let mut url = url::Url::parse(raw.trim()).map_err(|_| "请输入完整的服务地址")?;
    let local = url.host_str().is_some_and(|h| {
        h == "localhost"
            || h == "[::1]"
            || h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if (url.scheme() != "https" && !(url.scheme() == "http" && local))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("服务必须使用 HTTPS；HTTP 仅允许本机。地址不能包含凭据或查询参数。".into());
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn default_connection_name(base_url: &str) -> String {
    url::Url::parse(base_url)
        .ok()
        .and_then(|u| match (u.host_str(), u.port()) {
            (Some(host), Some(port)) => Some(format!("{host}:{port}")),
            (Some(host), None) => Some(host.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| base_url.to_string())
}

fn normalize_connection_name(name: Option<&str>, base_url: &str) -> String {
    let trimmed = name
        .map(str::trim)
        .unwrap_or("")
        .chars()
        .take(64)
        .collect::<String>();
    if trimmed.is_empty() {
        default_connection_name(base_url)
    } else {
        trimmed
    }
}

fn upsert_connection(prefs: &mut Preferences, base_url: &str, name: Option<&str>) {
    if let Some(existing) = prefs
        .connections
        .iter_mut()
        .find(|c| c.base_url == base_url)
    {
        if let Some(n) = name.map(str::trim).filter(|s| !s.is_empty()) {
            existing.name = normalize_connection_name(Some(n), base_url);
        } else if existing.name.trim().is_empty() {
            existing.name = default_connection_name(base_url);
        }
        return;
    }
    if prefs.connections.len() >= 50 {
        return;
    }
    prefs.connections.push(SavedConnection {
        name: normalize_connection_name(name, base_url),
        base_url: base_url.to_string(),
    });
}

fn normalize_preferences(mut prefs: Preferences) -> Preferences {
    let mut deduped: Vec<SavedConnection> = Vec::new();
    for item in prefs.connections.drain(..) {
        if let Ok(normalized) = normalize_base(&item.base_url) {
            if let Some(existing) = deduped.iter_mut().find(|c| c.base_url == normalized) {
                if !item.name.trim().is_empty() {
                    existing.name = normalize_connection_name(Some(&item.name), &normalized);
                }
            } else if deduped.len() < 50 {
                deduped.push(SavedConnection {
                    name: normalize_connection_name(Some(&item.name), &normalized),
                    base_url: normalized,
                });
            }
        }
    }
    prefs.connections = deduped;
    if !prefs.base_url.trim().is_empty() {
        if let Ok(normalized) = normalize_base(&prefs.base_url) {
            prefs.base_url = normalized.clone();
            upsert_connection(&mut prefs, &normalized, None);
        }
    }
    prefs
}

fn save_preferences(path: &Path, value: &Preferences) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or("设置路径无效")?;
    std::fs::create_dir_all(parent).map_err(|_| "无法创建设置目录")?;
    let temp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|_| "无法保存设置")?;
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|_| "无法保护设置文件权限")?;
    file.write_all(&serde_json::to_vec(value).map_err(|_| "设置格式无效")?)
        .map_err(|_| "无法保存设置")?;
    file.sync_all().map_err(|_| "无法同步设置")?;
    std::fs::rename(&temp, path).map_err(|_| "无法替换设置")?;
    Ok(())
}

async fn save_preferences_async(path: &Path, value: &Preferences) -> Result<(), String> {
    let owned_path = path.to_path_buf();
    let owned_value = value.clone();
    tauri::async_runtime::spawn_blocking(move || save_preferences(&owned_path, &owned_value))
        .await
        .map_err(|_| "无法保存设置".to_string())?
}

struct Fnv1aHasher(u64);

impl Default for Fnv1aHasher {
    fn default() -> Self {
        Self(0xcbf29ce484222325)
    }
}

impl std::io::Write for Fnv1aHasher {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for &byte in buf {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct HashBuffer {
    hasher: Fnv1aHasher,
    buf: Vec<u8>,
}

impl std::io::Write for HashBuffer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let _ = self.hasher.write(data)?;
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode_snapshot(mut data: Value) -> Option<(u64, Arc<Box<serde_json::value::RawValue>>)> {
    let observed = data.as_object_mut().and_then(|m| m.remove("observed_at"));
    let mut writer = HashBuffer {
        hasher: Fnv1aHasher::default(),
        buf: Vec::with_capacity(1024),
    };
    serde_json::to_writer(&mut writer, &data).ok()?;
    let revision = if writer.hasher.0 == 0 {
        1
    } else {
        writer.hasher.0
    };
    let body = String::from_utf8(writer.buf).ok()?;
    let raw_str = match observed.as_ref().and_then(Value::as_str) {
        Some(obs) => {
            let prefix = format!("{{\"observed_at\":{}", serde_json::to_string(obs).ok()?);
            if body.len() > 2 {
                format!("{prefix},{}", &body[1..])
            } else {
                format!("{prefix}}}")
            }
        }
        None => body,
    };
    let raw = serde_json::value::RawValue::from_string(raw_str).ok()?;
    Some((revision, Arc::new(raw)))
}

fn update_raw_observed_at(
    raw: &serde_json::value::RawValue,
    next_obs: &str,
) -> Option<Arc<Box<serde_json::value::RawValue>>> {
    let text = raw.get();
    let rest = text.strip_prefix("{\"observed_at\":\"")?;
    let mut escaped = false;
    let mut end_quote = None;
    for (idx, b) in rest.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if b == b'\\' {
            escaped = true;
        } else if b == b'"' {
            end_quote = Some(idx);
            break;
        }
    }
    let suffix = &rest[end_quote? + 1..];
    let updated = format!(
        "{{\"observed_at\":{}{}",
        serde_json::to_string(next_obs).ok()?,
        suffix
    );
    serde_json::value::RawValue::from_string(updated)
        .ok()
        .map(Arc::new)
}

fn publish(_app: &tauri::AppHandle, value: &ViewState) {
    // Android's window visibility getter always returns false. Its suspended /
    // resumed events already control polling and carry foreground in ViewState.
    #[cfg(mobile)]
    let _ = _app.emit("ops-state", value);
    #[cfg(target_os = "macos")]
    macos_bridge::publish_state(value);
}

fn publish_observed(_app: &tauri::AppHandle, _observed_at: &str) {
    #[cfg(mobile)]
    let _ = _app.emit("ops-observed", _observed_at);
    #[cfg(target_os = "macos")]
    macos_bridge::publish_observed_at(_observed_at);
}

fn apply_refresh(view: &mut ViewState, result: Result<Value, HttpFailure>) {
    match result {
        Ok(data) => {
            if let Some((rev, raw)) = encode_snapshot(data) {
                view.snapshot_revision = rev;
                view.snapshot = Some(raw);
                view.online = true;
                view.error.clear();
                view.upstream_connection = None;
            } else {
                view.online = false;
                view.error = "快照序列化失败".into();
            }
        }
        Err(error) => {
            view.online = false;
            view.error = error.failure.message;
            view.upstream_connection = error.connection;
        }
    }
}

async fn refresh_inner(app: &tauri::AppHandle, state: &Runtime) {
    if !state.foreground.load(Ordering::SeqCst) {
        return;
    }
    let Ok(_gate) = state.snapshot_gate.try_lock() else {
        return;
    };
    let (base, key, generation, etag) = {
        let _connection = state.network.lock().await;
        let view = state.view.lock().await;
        let cached_etag = if view.snapshot.is_some() {
            state.snapshot_etag.lock().await.clone()
        } else {
            None
        };
        (
            view.preferences.base_url.clone(),
            state.key.lock().await.clone(),
            state.generation.load(Ordering::SeqCst),
            cached_etag,
        )
    };
    if base.is_empty() || key.is_none() {
        return;
    }
    let result = active_read(
        state,
        http_snapshot(
            &state.client,
            &base,
            key.as_deref().unwrap_or(""),
            etag.as_deref(),
            &state.path.with_file_name("network-diagnostics.log"),
        ),
    )
    .await;
    let _connection = state.network.lock().await;
    if generation != state.generation.load(Ordering::SeqCst)
        || !state.foreground.load(Ordering::SeqCst)
    {
        return;
    }
    let mut view = state.view.lock().await;
    match result {
        Ok(SnapshotFetch::NotModified {
            etag: next_etag,
            observed_at,
        }) => {
            if let Some(tag) = next_etag {
                *state.snapshot_etag.lock().await = Some(tag);
            }
            let was_healthy =
                view.online && view.error.is_empty() && view.upstream_connection.is_none();
            view.online = true;
            view.error.clear();
            view.upstream_connection = None;
            if let (Some(obs), Some(existing)) = (observed_at.as_deref(), view.snapshot.as_deref())
            {
                if let Some(updated) = update_raw_observed_at(existing, obs) {
                    view.snapshot = Some(updated);
                }
            }
            #[cfg(target_os = "macos")]
            macos_bridge::publish_tooltip(&view);
            if was_healthy {
                if let Some(obs) = observed_at.as_deref() {
                    publish_observed(app, obs);
                }
            } else {
                publish(app, &view);
            }
        }
        Ok(SnapshotFetch::Full {
            data,
            etag: next_etag,
        }) => {
            *state.snapshot_etag.lock().await = next_etag;
            apply_refresh(&mut view, Ok(data));
            #[cfg(target_os = "macos")]
            macos_bridge::publish_tooltip(&view);
            publish(app, &view);
        }
        Err(error) => {
            apply_refresh(&mut view, Err(error));
            #[cfg(target_os = "macos")]
            macos_bridge::publish_tooltip(&view);
            publish(app, &view);
        }
    }
}

async fn active_read<T>(
    state: &Runtime,
    request: impl std::future::Future<Output = Result<T, HttpFailure>>,
) -> Result<T, HttpFailure> {
    let mut changed = state.read_epoch.subscribe();
    if !state.foreground.load(Ordering::SeqCst) {
        return Err("应用已转入后台".into());
    }
    tokio::select! {
        biased;
        _ = changed.changed() => Err(HttpFailure::background()),
        result = request => result,
    }
}

#[cfg(target_os = "macos")]
fn sync_record_activity_with(app: &tauri::AppHandle, state: &Arc<Runtime>) -> bool {
    // A key window can outlive application activation on macOS. Use AppKit's
    // application state, while preserving background snapshots for the tray.
    let active = record_activity::application_active();
    if state.record_activity.set_active(active) {
        if active {
            state.wake.notify_one();
        }
        let state = state.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut view = state.view.lock().await;
            view.foreground = state.record_activity.active();
            publish(&app, &view);
        });
    }
    active
}

#[cfg_attr(mobile, tauri::command)]
async fn get_state(state: State<'_, Arc<Runtime>>) -> Result<ViewState, HttpFailure> {
    Ok(state.view.lock().await.clone())
}

#[cfg_attr(mobile, tauri::command)]
async fn connect(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
    base_url: String,
    api_key: String,
    name: Option<String>,
) -> Result<(), HttpFailure> {
    let base = normalize_base(&base_url)?;
    let trimmed = api_key.trim();
    if trimmed.len() > 4096 {
        return Err(HttpFailure::new(
            "missing_credentials",
            "请输入已有的管理员 API Key",
        ));
    }
    *state.connecting_url.lock().await = Some(base.clone());
    let connect_result: Result<(), HttpFailure> = async {
        let (resolved_key, should_write) = if trimmed.is_empty() {
            let saved = credentials::read(app.clone(), base.clone())
                .await?
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    HttpFailure::new(
                        "missing_credentials",
                        "未找到已保存的管理员 API Key，请重新输入",
                    )
                })?;
            (saved, false)
        } else {
            (trimmed.to_string(), true)
        };
        let _gate = state.network.lock().await;
        let log_path = state.path.with_file_name("network-diagnostics.log");
        let (cap_res, snap_res) = tokio::join!(
            active_read(
                &state,
                http_detailed(
                    &state.client,
                    &base,
                    &resolved_key,
                    "GET",
                    "/capabilities",
                    None,
                    &log_path,
                ),
            ),
            active_read(
                &state,
                http_snapshot(&state.client, &base, &resolved_key, None, &log_path),
            ),
        );
        let data = cap_res?;
        if should_write {
            credentials::write(app.clone(), base.clone(), resolved_key.clone()).await?;
        }
        let mut prefs = state.view.lock().await.preferences.clone();
        prefs.base_url = base.clone();
        upsert_connection(&mut prefs, &base, name.as_deref());
        save_preferences_async(&state.path, &prefs).await?;
        let mut view = state.view.lock().await;
        view.preferences = prefs;
        view.connection_revision = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
        view.connected = true;
        view.snapshot = None;
        view.snapshot_revision = 0;
        view.error.clear();
        let cap_upstream = data.get("upstream_connection").cloned();
        let (snap_val, next_etag) = match snap_res {
            Ok(SnapshotFetch::Full { data, etag }) => (Ok(data), etag),
            Ok(SnapshotFetch::NotModified { .. }) => {
                (Err(HttpFailure::new("http", "快照响应异常")), None)
            }
            Err(err) => (Err(err), None),
        };
        *state.snapshot_etag.lock().await = next_etag;
        apply_refresh(&mut view, snap_val);
        if !view.online && view.upstream_connection.is_none() {
            view.upstream_connection = cap_upstream;
        }
        *state.key.lock().await = Some(resolved_key);
        #[cfg(target_os = "macos")]
        macos_bridge::publish_tooltip(&view);
        publish(&app, &view);
        Ok(())
    }
    .await;
    *state.connecting_url.lock().await = None;
    connect_result?;
    Ok(())
}

#[cfg_attr(mobile, tauri::command)]
async fn save_connection_profile(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
    base_url: String,
    name: String,
) -> Result<(), HttpFailure> {
    let target = normalize_base(&base_url)?;
    let mut prefs = {
        let view = state.view.lock().await;
        if !view
            .preferences
            .connections
            .iter()
            .any(|item| item.base_url == target)
            && view.preferences.base_url != target
        {
            return Err("未找到要更新的实例配置".into());
        }
        view.preferences.clone()
    };
    upsert_connection(&mut prefs, &target, Some(&name));
    save_preferences_async(&state.path, &prefs).await?;
    let mut view = state.view.lock().await;
    view.preferences = prefs;
    publish(&app, &view);
    Ok(())
}

async fn remove_connection_with<F, Fut>(
    state: &Runtime,
    base_url: &str,
    delete_credential: F,
) -> Result<Preferences, String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let target = normalize_base(base_url)?;
    let _gate = state.network.lock().await;
    {
        let connecting = state.connecting_url.lock().await;
        let view = state.view.lock().await;
        if !view
            .preferences
            .connections
            .iter()
            .any(|item| item.base_url == target)
        {
            return Err("未找到要删除的实例配置".into());
        }
        if (view.connected && view.preferences.base_url == target)
            || connecting.as_deref() == Some(target.as_str())
        {
            return Err("当前实例正在连接或使用中，请先切换到其他实例或断开连接后再删除".into());
        }
    }
    delete_credential(target.clone()).await?;
    let mut view = state.view.lock().await;
    let connecting = state.connecting_url.lock().await;
    if (view.connected && view.preferences.base_url == target)
        || connecting.as_deref() == Some(target.as_str())
    {
        return Err("当前实例正在连接或使用中，请先切换到其他实例或断开连接后再删除".into());
    }
    if !view
        .preferences
        .connections
        .iter()
        .any(|item| item.base_url == target)
    {
        return Err("未找到要删除的实例配置".into());
    }
    let mut prefs = view.preferences.clone();
    prefs.connections.retain(|item| item.base_url != target);
    if prefs.base_url == target {
        prefs.base_url.clear();
    }
    save_preferences_async(&state.path, &prefs).await?;
    view.preferences = prefs.clone();
    Ok(prefs)
}

#[cfg_attr(mobile, tauri::command)]
async fn remove_connection(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
    base_url: String,
) -> Result<(), HttpFailure> {
    remove_connection_with(&state, &base_url, |target| {
        credentials::delete(app.clone(), target)
    })
    .await?;
    let view = state.view.lock().await;
    publish(&app, &view);
    Ok(())
}

#[cfg_attr(mobile, tauri::command)]
async fn disconnect(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
) -> Result<(), HttpFailure> {
    let _gate = state.network.lock().await;
    let mut view = state.view.lock().await;
    if !view.preferences.base_url.is_empty() {
        credentials::delete(app.clone(), view.preferences.base_url.clone()).await?;
    }
    *state.key.lock().await = None;
    *state.snapshot_etag.lock().await = None;
    view.connection_revision = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    view.snapshot_revision = 0;
    view.connected = false;
    view.online = false;
    view.snapshot = None;
    view.error.clear();
    view.upstream_connection = None;
    let mut prefs = view.preferences.clone();
    prefs.base_url.clear();
    save_preferences_async(&state.path, &prefs).await?;
    view.preferences = prefs;
    #[cfg(target_os = "macos")]
    macos_bridge::publish_tooltip(&view);
    publish(&app, &view);
    Ok(())
}

#[cfg_attr(mobile, tauri::command)]
async fn refresh(state: State<'_, Arc<Runtime>>) -> Result<(), HttpFailure> {
    state.wake.notify_one();
    Ok(())
}

#[cfg_attr(mobile, tauri::command)]
async fn api_request(
    state: State<'_, Arc<Runtime>>,
    method: String,
    path: String,
    body: Option<Value>,
) -> Result<Value, HttpFailure> {
    if sub2ops_protocol::routes::client_endpoint(&method, &path).is_none() {
        return Err("客户端不允许此操作".into());
    }
    #[cfg(target_os = "macos")]
    let record_read = method == "GET" && record_activity::record_path(&path);
    let (base, key, generation) = {
        let _gate = state.network.lock().await;
        let view = state.view.lock().await;
        let diagnostic = method == "GET"
            && matches!(
                path.as_str(),
                "/connection-status" | "/sub2api-config/operation" | "/sub2api-system/version"
            );
        if (!view.online && !diagnostic) || !state.foreground.load(Ordering::SeqCst) {
            return Err(HttpFailure::new("offline", "当前离线，请恢复连接后操作"));
        }
        let key = state
            .key
            .lock()
            .await
            .clone()
            .ok_or_else(|| HttpFailure::new("missing_credentials", "尚未连接"))?;
        (
            view.preferences.base_url.clone(),
            key,
            state.generation.load(Ordering::SeqCst),
        )
    };
    let log = state.path.with_file_name("network-diagnostics.log");
    let request = http_detailed(&state.client, &base, &key, &method, &path, body, &log);
    #[cfg(target_os = "macos")]
    let result = if record_read {
        state.record_activity.read(request).await
    } else {
        request.await
    };
    #[cfg(not(target_os = "macos"))]
    let result = if method == "GET" {
        active_read(&state, request).await
    } else {
        request.await
    };
    if generation != state.generation.load(Ordering::SeqCst) {
        return Err(HttpFailure::new(
            "connection_changed",
            "连接已切换，旧操作结果已忽略",
        ));
    }
    if method != "GET" {
        state.wake.notify_one();
    }
    result
}

#[cfg_attr(mobile, tauri::command)]
async fn preferences(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
    pinned: Option<bool>,
    launch_at_login: Option<bool>,
    record_columns: Option<Vec<String>>,
    model_test_concurrency: Option<u8>,
    model_test_preferences: Option<std::collections::BTreeMap<String, ModelTestPreference>>,
) -> Result<(), HttpFailure> {
    let mut view = state.view.lock().await;
    #[cfg(desktop)]
    if let Some(next_launch) = launch_at_login {
        if next_launch != view.preferences.launch_at_login {
            let launch = app.autolaunch();
            if next_launch {
                launch.enable()
            } else {
                launch.disable()
            }
            .map_err(|_| "无法更改开机启动")?;
        }
    }
    let mut prefs = view.preferences.clone();
    if let Some(concurrency) = model_test_concurrency {
        if !(1..=3).contains(&concurrency) {
            return Err("测试并发必须为 1、2 或 3".into());
        }
        prefs.model_test_concurrency = Some(concurrency);
    }
    if let Some(selections) = model_test_preferences {
        if selections.values().any(|v| {
            !matches!(v.engine.as_str(), "modeltrace" | "lm_detector")
                || !matches!(v.sample_count, 1 | 3)
        }) {
            return Err("检测源或采样组数无效".into());
        }
        prefs.model_test_preferences = selections;
    }
    #[cfg(desktop)]
    {
        if let Some(next_pinned) = pinned {
            prefs.pinned = next_pinned;
        }
        if let Some(next_launch) = launch_at_login {
            prefs.launch_at_login = next_launch;
        }
    }
    #[cfg(not(desktop))]
    {
        let _ = (pinned, launch_at_login);
        prefs.pinned = false;
        prefs.launch_at_login = false;
    }
    if let Some(columns) = record_columns {
        prefs.record_columns = Some(normalize_record_columns(columns));
    }
    save_preferences_async(&state.path, &prefs).await?;
    view.preferences = prefs;
    publish(&app, &view);
    Ok(())
}

#[cfg_attr(mobile, tauri::command)]
async fn open_authorization_url(app: tauri::AppHandle, url: String) -> Result<(), HttpFailure> {
    if !sub2ops_protocol::creation::authorization_url(&url) {
        return Err("授权链接必须是 HTTPS 地址".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|_| "无法打开系统浏览器".into())
}

#[cfg_attr(mobile, tauri::command)]
async fn open_sub2api_release(app: tauri::AppHandle, url: String) -> Result<(), HttpFailure> {
    let parsed = url::Url::parse(&url).map_err(|_| "发布链接无效")?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || !(parsed.path() == "/Wei-Shaw/sub2api/releases"
            || parsed.path().starts_with("/Wei-Shaw/sub2api/releases/"))
    {
        return Err("发布链接无效".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|_| "无法打开系统浏览器".into())
}

#[cfg(desktop)]
async fn check_updates(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
) -> Result<String, HttpFailure> {
    let releases = active_read(&state, async {
        let response = state
            .client
            .get("https://api.github.com/repos/lich13/sub2api-ops-companion/releases?per_page=20")
            .header("User-Agent", "Sub2Ops")
            .send()
            .await
            .map_err(|_| "无法检查版本")?;
        if !response.status().is_success() {
            return Err("版本服务暂不可用".into());
        }
        response
            .json::<Value>()
            .await
            .map_err(|_| HttpFailure::new("invalid_response", "版本信息无效"))
    })
    .await?;
    if let Some((tag, url)) = release_update(&releases, env!("CARGO_PKG_VERSION")) {
        app.opener()
            .open_url(&url, None::<&str>)
            .map_err(|_| "无法打开发布页")?;
        return Ok(format!("发现 {tag}，已打开安装包"));
    }
    Ok(format!("当前版本 {}，已是最新", env!("CARGO_PKG_VERSION")))
}

#[cfg(mobile)]
#[tauri::command]
async fn get_app_update_state(
    updater: State<'_, Arc<updater::UpdaterState>>,
) -> Result<updater::AppUpdateSnapshot, HttpFailure> {
    Ok(updater.get_snapshot().await)
}

#[cfg(mobile)]
#[tauri::command]
async fn start_app_update(
    app: tauri::AppHandle,
    updater: State<'_, Arc<updater::UpdaterState>>,
) -> Result<updater::AppUpdateSnapshot, HttpFailure> {
    Ok(updater.start(app).await)
}

#[cfg(mobile)]
#[tauri::command]
async fn cancel_app_update(
    app: tauri::AppHandle,
    updater: State<'_, Arc<updater::UpdaterState>>,
) -> Result<updater::AppUpdateSnapshot, HttpFailure> {
    Ok(updater.cancel(Some(&app)).await)
}

#[cfg(mobile)]
#[tauri::command]
async fn install_app_update(
    app: tauri::AppHandle,
    updater: State<'_, Arc<updater::UpdaterState>>,
) -> Result<updater::AppUpdateSnapshot, HttpFailure> {
    updater.install_verified(&app).await
}

pub(crate) fn parsed_release_version(tag: &str) -> Option<(u64, u64, u64)> {
    let raw = tag.strip_prefix("desktop-v")?;
    if !raw.bytes().all(|c| c.is_ascii_digit() || c == b'.') {
        return None;
    }
    let parts = raw
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if parts.len() != 3 {
        return None;
    }
    Some((parts[0], parts[1], parts[2]))
}

#[cfg(any(desktop, test))]
fn release_update(releases: &Value, current: &str) -> Option<(String, String)> {
    let version = |value: &str| -> Option<(u64, u64, u64)> {
        let parts = value
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if parts.len() != 3 {
            return None;
        }
        Some((parts[0], parts[1], parts[2]))
    };
    let current = version(current)?;
    releases
        .as_array()?
        .iter()
        .filter_map(|release| {
            if release["draft"].as_bool().unwrap_or(false)
                || release["prerelease"].as_bool().unwrap_or(false)
            {
                return None;
            }
            let tag = release["tag_name"].as_str()?;
            let newer = parsed_release_version(tag)?;
            if newer <= current {
                return None;
            }
            let asset = release["assets"].as_array()?.iter().find(|asset| {
                asset["name"]
                    .as_str()
                    .is_some_and(|name| name.ends_with("_aarch64.dmg"))
            })?;
            let url = asset["browser_download_url"].as_str()?;
            if !url.starts_with(&format!("{RELEASES}/download/{tag}/")) {
                return None;
            }
            Some((newer, tag.to_string(), url.to_string()))
        })
        .max_by_key(|r| r.0)
        .map(|(_, tag, url)| (tag, url))
}

#[cfg(desktop)]
pub(crate) fn panel_trace(app: &tauri::AppHandle, stage: &str, detail: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let path = dir.join("panel-diagnostics.log");
    let truncate = std::fs::metadata(&path).is_ok_and(|m| m.len() > 64 * 1024);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(!truncate)
        .truncate(truncate)
        .mode(0o600)
        .open(path)
    {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        // Window lifecycle only: no server address, account data or credentials.
        let _ = writeln!(file, "{stamp} {stage} {detail}");
    }
}

#[cfg(desktop)]
fn silent_launch(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--autostart")
}

#[cfg(mobile)]
#[tauri::command]
async fn background(app: tauri::AppHandle) -> Result<(), HttpFailure> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        app.state::<credentials::SecureStore>()
            .0
            .run_mobile_plugin::<Value>("background", serde_json::json!({}))
    })
    .await;
    match result {
        Ok(Ok(_)) => Ok(()),
        _ => Err("无法返回后台".into()),
    }
}

#[cfg(any(desktop, test))]
fn should_poll_fast(
    main_visible: bool,
    main_minimized: bool,
    app_active: bool,
    quick_visible: bool,
) -> bool {
    (main_visible && !main_minimized && app_active) || quick_visible
}

#[cfg(desktop)]
fn active_polling_visible(state: &Runtime) -> bool {
    #[cfg(target_os = "macos")]
    let (main_visible, main_minimized, quick_visible) = macos_bridge::window_visibility();
    #[cfg(not(target_os = "macos"))]
    let (main_visible, main_minimized, quick_visible) = (true, false, false);
    #[cfg(target_os = "macos")]
    let app_active = state.record_activity.active();
    #[cfg(not(target_os = "macos"))]
    let app_active = true;
    should_poll_fast(main_visible, main_minimized, app_active, quick_visible)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|_app, args, _| {
            if !silent_launch(&args) {
                #[cfg(target_os = "macos")]
                macos_bridge::emit_reopen();
            }
        }))
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent)
                .args(["--autostart"])
                .build(),
        );
    #[cfg(mobile)]
    let builder = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(credentials::plugin());
    #[cfg(target_os = "android")]
    let builder = builder.plugin(updater::plugin());
    let builder = builder.setup(|app| {
        let path = app.path().app_config_dir()?.join("preferences.json");
        #[cfg(mobile)]
        {
            let cache_dir = app.path().app_cache_dir()?;
            app.manage(updater::UpdaterState::new(
                cache_dir,
                env!("CARGO_PKG_VERSION").to_string(),
            )?);
        }
        let prefs = normalize_preferences(
            std::fs::read(&path)
                .ok()
                .and_then(|data| serde_json::from_slice(&data).ok())
                .unwrap_or_default(),
        );
        #[cfg(target_os = "macos")]
        let initial_active = record_activity::application_active();
        #[cfg(not(target_os = "macos"))]
        let initial_active = true;
        let state = Arc::new(Runtime {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .tcp_nodelay(true)
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
            foreground: AtomicBool::new(true),
            read_epoch: watch::channel(0).0,
            #[cfg(target_os = "macos")]
            record_activity: record_activity::RecordActivity::new(initial_active),
            view: Mutex::new(ViewState {
                platform: if cfg!(mobile) { "android" } else { "macos" },
                foreground: initial_active,
                initializing: true,
                preferences: prefs,
                ..Default::default()
            }),
            key: Mutex::new(None),
            snapshot_etag: Mutex::new(None),
            connecting_url: Mutex::new(None),
            network: Mutex::new(()),
            snapshot_gate: Mutex::new(()),
            generation: AtomicU64::new(0),
            wake: Notify::new(),
            path,
        });
        app.manage(state.clone());
        #[cfg(target_os = "macos")]
        macos_bridge::init(app.handle().clone());
        let handle = app.handle().clone();
        tauri::async_runtime::spawn(async move {
            let base = state.view.lock().await.preferences.base_url.clone();
            let stored = credentials::read(handle.clone(), base).await;
            {
                let _gate = state.network.lock().await;
                let mut view = state.view.lock().await;
                if state.generation.load(Ordering::SeqCst) == 0 {
                    match stored {
                        Ok(key) => {
                            view.connected = key.is_some();
                            *state.key.lock().await = key;
                        }
                        Err(error) => view.error = error,
                    }
                }
                view.initializing = false;
                #[cfg(target_os = "macos")]
                macos_bridge::publish_tooltip(&view);
                publish(&handle, &view);
            }
            loop {
                if !state.foreground.load(Ordering::SeqCst) {
                    state.wake.notified().await;
                    continue;
                }
                refresh_inner(&handle, &state).await;
                #[cfg(desktop)]
                let active_visible = active_polling_visible(&state);
                #[cfg(mobile)]
                let active_visible = true;
                let interval = Duration::from_secs(if active_visible { 2 } else { 15 });
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = state.wake.notified() => {}
                }
            }
        });
        Ok(())
    });
    #[cfg(mobile)]
    let builder = builder.on_window_event(|window, event| {
        let state = window.state::<Arc<Runtime>>();
        if let WindowEvent::Suspended | WindowEvent::Resumed = event {
            let active = matches!(event, WindowEvent::Resumed);
            state.foreground.store(active, Ordering::SeqCst);
            if !active {
                state.read_epoch.send_modify(|epoch| *epoch += 1);
            } else {
                updater::on_resumed(window.app_handle().clone());
            }
            let handle = window.app_handle().clone();
            let state = state.inner().clone();
            tauri::async_runtime::spawn(async move {
                let mut view = state.view.lock().await;
                view.foreground = state.foreground.load(Ordering::SeqCst);
                view.online = false;
                view.error.clear();
                publish(&handle, &view);
                state.wake.notify_one();
            });
        }
    });
    #[cfg(mobile)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        get_state,
        connect,
        save_connection_profile,
        remove_connection,
        disconnect,
        refresh,
        api_request,
        preferences,
        background,
        get_app_update_state,
        start_app_update,
        cancel_app_update,
        install_app_update,
        open_authorization_url,
        open_sub2api_release
    ]);
    #[cfg(target_os = "macos")]
    let mut cached_state: Option<Arc<Runtime>> = None;
    builder
        .build(tauri::generate_context!())
        .expect("Sub2Ops failed to start")
        .run(move |_app, _event| {
            #[cfg(target_os = "macos")]
            {
                let state = cached_state
                    .get_or_insert_with(|| _app.state::<Arc<Runtime>>().inner().clone());
                if matches!(
                    &_event,
                    tauri::RunEvent::Ready
                        | tauri::RunEvent::Resumed
                        | tauri::RunEvent::Reopen { .. }
                ) {
                    sync_record_activity_with(_app, state);
                }
                if let tauri::RunEvent::Reopen { .. } = _event {
                    macos_bridge::emit_reopen();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn runtime() -> Runtime {
        Runtime {
            client: reqwest::Client::new(),
            view: Mutex::new(ViewState::default()),
            key: Mutex::new(None),
            snapshot_etag: Mutex::new(None),
            connecting_url: Mutex::new(None),
            network: Mutex::new(()),
            snapshot_gate: Mutex::new(()),
            generation: AtomicU64::new(0),
            wake: Notify::new(),
            path: PathBuf::new(),
            foreground: AtomicBool::new(true),
            read_epoch: watch::channel(0).0,
            #[cfg(target_os = "macos")]
            record_activity: record_activity::RecordActivity::default(),
        }
    }
    #[tokio::test]
    async fn background_cancels_inflight_reads_and_does_not_start_new_reads() {
        let state = runtime();
        let started = AtomicBool::new(false);
        let request = async {
            started.store(true, Ordering::SeqCst);
            std::future::pending::<Result<(), HttpFailure>>().await
        };
        let (result, ()) = tokio::join!(active_read(&state, request), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            state.foreground.store(false, Ordering::SeqCst);
            state.read_epoch.send_modify(|n| *n += 1);
        });
        assert!(started.load(Ordering::SeqCst));
        assert!(result.unwrap_err().code == "background");
        assert!(active_read(&state, async {
            panic!("background request must not be polled");
            #[allow(unreachable_code)]
            Ok::<(), HttpFailure>(())
        })
        .await
        .is_err());
        state.foreground.store(true, Ordering::SeqCst);
        assert_eq!(
            active_read(&state, async { Ok::<i32, HttpFailure>(7) })
                .await
                .unwrap(),
            7
        );
    }

    #[test]
    fn updates_select_newer_signed_platform_package_only() {
        let digest = format!("sha256:{}", "c".repeat(64));
        let data = serde_json::json!([
            {"tag_name":"desktop-v0.1.10","assets":[
                {"name":"Sub2Ops_0.1.10_aarch64.dmg","browser_download_url":format!("{RELEASES}/download/desktop-v0.1.10/Sub2Ops_0.1.10_aarch64.dmg")},
                {"name":"Sub2Ops_0.1.10_arm64-v8a.apk","size":2048,"digest":digest,"browser_download_url":format!("{RELEASES}/download/desktop-v0.1.10/Sub2Ops_0.1.10_arm64-v8a.apk")}]},
            {"tag_name":"desktop-v0.2.0","draft":true,"assets":[]},
            {"tag_name":"desktop-v9.0.0","assets":[{"name":"unsafe_arm64-v8a.apk","size":2048,"digest":digest,"browser_download_url":"https://other.example/file.apk"}]}
        ]);
        assert!(release_update(&data, "0.1.9")
            .unwrap()
            .1
            .ends_with("_aarch64.dmg"));
        assert!(release_update(&data, "0.1.10").is_none());
        assert!(updater::select_android_release(&data, "0.1.9", RELEASES).is_err());
        let valid_android = serde_json::json!([data[0].clone()]);
        let selected = updater::select_android_release(&valid_android, "0.1.9", RELEASES)
            .unwrap()
            .unwrap();
        assert_eq!(selected.apk_name, "Sub2Ops_0.1.10_arm64-v8a.apk");
        assert!(
            updater::select_android_release(&valid_android, "0.1.10", RELEASES)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn release_pages_reject_untrusted_tags_and_nonstable_releases() {
        let digest = format!("sha256:{}", "d".repeat(64));
        let data = serde_json::json!([
            {"tag_name":"desktop-v9.9.9/../../other","assets":[]},
            {"tag_name":"desktop-v9.9.9?redirect=bad","assets":[]},
            {"tag_name":"desktop-v99.1.1","prerelease":true,"assets":[]},
            {"tag_name":"desktop-v99.1.2","draft":true,"assets":[]},
            {"tag_name":"desktop-v1.2.3","html_url":"https://other.invalid","assets":[
                {"name":"Sub2Ops_1.2.3_aarch64.dmg","browser_download_url":format!("{RELEASES}/download/desktop-v1.2.3/Sub2Ops_1.2.3_aarch64.dmg")},
                {"name":"Sub2Ops_1.2.3_arm64-v8a.apk","size":1024,"digest":digest,"browser_download_url":format!("{RELEASES}/download/desktop-v1.2.3/Sub2Ops_1.2.3_arm64-v8a.apk")}
            ]}
        ]);
        assert_eq!(release_update(&data, "1.2.2").unwrap().0, "desktop-v1.2.3");
        assert_eq!(
            updater::select_android_release(&data, "1.2.2", RELEASES)
                .unwrap()
                .unwrap()
                .tag,
            "desktop-v1.2.3"
        );
        assert!(
            updater::select_android_release(&serde_json::json!([]), "1.2.2", RELEASES)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn raw_observed_at_splice_preserves_payload_and_revision() {
        let orig = serde_json::json!({"observed_at":"2026-09-25T10:00:00Z","accounts":[{"id":1,"name":"A\"B"}]});
        let (rev, raw) = encode_snapshot(orig).unwrap();
        let updated = update_raw_observed_at(&raw, "2026-09-25T10:00:08Z").unwrap();
        let parsed: Value = serde_json::from_str(updated.get()).unwrap();
        assert_eq!(parsed["observed_at"], "2026-09-25T10:00:08Z");
        assert_eq!(parsed["accounts"][0]["name"], "A\"B");
        let (rev2, _) = encode_snapshot(parsed).unwrap();
        assert_eq!(rev, rev2);
    }

    #[test]
    fn transient_failure_keeps_last_data_and_recovers() {
        let mut view = ViewState::default();
        let first = serde_json::json!({"observed_at":"2026-09-25T10:00:00Z","accounts":[{"id":1}]});
        apply_refresh(&mut view, Ok(first.clone()));
        let first_rev = view.snapshot_revision;
        assert_ne!(first_rev, 0);
        apply_refresh(&mut view, Err("网关暂不可用 [502]".into()));
        assert!(!view.online);
        assert_eq!(
            serde_json::from_str::<Value>(view.snapshot.as_ref().unwrap().get()).unwrap(),
            first
        );
        let cloned = view.clone();
        assert!(Arc::ptr_eq(
            view.snapshot.as_ref().unwrap(),
            cloned.snapshot.as_ref().unwrap()
        ));
        assert_eq!(serde_json::to_value(&cloned).unwrap()["snapshot"], first);

        // Changing only observed_at keeps the exact same snapshot_revision
        let same_payload =
            serde_json::json!({"observed_at":"2026-09-25T10:00:02Z","accounts":[{"id":1}]});
        apply_refresh(&mut view, Ok(same_payload));
        assert_eq!(view.snapshot_revision, first_rev);

        let connection = serde_json::json!({"state":"auth_rejected"});
        apply_refresh(
            &mut view,
            Err(HttpFailure {
                failure: sub2ops_protocol::dto::BridgeFailure::new(
                    "auth_rejected",
                    "管理员 Key 被拒绝",
                ),
                connection: Some(connection.clone()),
            }),
        );
        assert_eq!(view.upstream_connection, Some(connection));
        let next = serde_json::json!({"observed_at":"2026-09-25T10:00:04Z","accounts":[{"id":2}]});
        apply_refresh(&mut view, Ok(next.clone()));
        assert!(view.online && view.error.is_empty());
        assert_ne!(view.snapshot_revision, first_rev);
        assert_eq!(
            serde_json::from_str::<Value>(view.snapshot.as_ref().unwrap().get()).unwrap(),
            next
        );
    }

    #[test]
    fn fast_polling_requires_active_unminimized_main_or_visible_quick_panel() {
        assert!(should_poll_fast(true, false, true, false));
        assert!(!should_poll_fast(true, true, true, false));
        assert!(!should_poll_fast(true, false, false, false));
        assert!(!should_poll_fast(false, false, true, false));
        assert!(should_poll_fast(false, false, false, true));
        assert!(should_poll_fast(true, true, false, true));
    }

    #[test]
    fn url_policy() {
        assert_eq!(
            normalize_base("https://example.com/sub2ops/").unwrap(),
            "https://example.com/sub2ops"
        );
        for url in [
            "http://example.com",
            "https://a:b@example.com",
            "https://example.com/?key=x",
            "file:///tmp",
        ] {
            assert!(normalize_base(url).is_err());
        }
        assert!(normalize_base("http://127.0.0.1:18081").is_ok());
    }

    #[cfg(desktop)]
    #[test]
    fn login_launch_is_silent_but_manual_and_second_instances_open() {
        assert!(silent_launch(&["sub2ops".into(), "--autostart".into()]));
        assert!(!silent_launch(&["sub2ops".into()]));
        assert!(!silent_launch(&[
            "sub2ops".into(),
            "--autostart=false".into()
        ]));
    }
    #[test]
    fn preferences_have_no_credentials() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("prefs.json");
        save_preferences(
            &p,
            &Preferences {
                base_url: "https://example.com".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("api_key"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn model_test_preferences_persist_and_default_cleanly() {
        let old: Preferences = serde_json::from_str("{}").unwrap();
        assert!(old.model_test_concurrency.is_none());
        assert!(old.model_test_preferences.is_empty());
        let changed = Preferences {
            model_test_concurrency: Some(3),
            model_test_preferences: std::collections::BTreeMap::from([(
                "https://example.invalid:7".into(),
                ModelTestPreference {
                    engine: "lm_detector".into(),
                    sample_count: 3,
                },
            )]),
            ..old
        };
        let restored: Preferences =
            serde_json::from_str(&serde_json::to_string(&changed).unwrap()).unwrap();
        assert_eq!(restored.model_test_concurrency, Some(3));
        assert_eq!(
            restored.model_test_preferences["https://example.invalid:7"].engine,
            "lm_detector"
        );
        assert_eq!(
            restored.model_test_preferences["https://example.invalid:7"].sample_count,
            3
        );
    }

    #[test]
    fn old_preferences_migrate_without_losing_existing_choices() {
        let old: Preferences = serde_json::from_str(r#"{"base_url":"https://example.com","favorites":[7],"pinned":true,"launch_at_login":true}"#).unwrap();
        assert!(old.record_columns.is_none());
        assert!(old.pinned && old.launch_at_login);
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("prefs.json");
        let changed = Preferences {
            record_columns: Some(normalize_record_columns(vec![
                "model".into(),
                "type".into(),
                "cost".into(),
                "invalid".into(),
            ])),
            ..old
        };
        save_preferences(&p, &changed).unwrap();
        let restored: Preferences = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        assert_eq!(restored.record_columns, changed.record_columns);
        assert_eq!(
            restored.record_columns,
            Some(vec!["model".into(), "type".into(), "cost".into()])
        );
        assert_eq!(restored.base_url, "https://example.com");
        assert!(restored.pinned && restored.launch_at_login);
    }

    #[test]
    fn type_column_and_empty_choices_survive_preference_reload() {
        let prefs: Preferences = serde_json::from_str(r#"{"base_url":"https://example.com","favorites":[7],"pinned":true,"launch_at_login":true,"record_columns":["ip","type","model"]}"#).unwrap();
        assert_eq!(
            prefs.record_columns,
            Some(vec!["ip".into(), "type".into(), "model".into()])
        );
        assert!(prefs.pinned && prefs.launch_at_login);
        for columns in [vec!["type".into()], vec![]] {
            let changed = Preferences {
                record_columns: Some(normalize_record_columns(columns.clone())),
                ..Preferences::default()
            };
            let restored: Preferences =
                serde_json::from_str(&serde_json::to_string(&changed).unwrap()).unwrap();
            assert_eq!(restored.record_columns, Some(columns));
        }
        assert!(normalize_record_columns(vec!["invalid".into()]).is_empty());
    }

    #[test]
    fn connection_profiles_migrate_legacy_base_url_and_deduplicate_without_credentials() {
        let legacy: Preferences =
            serde_json::from_str(r#"{"base_url":"https://alpha.example.invalid/sub2ops/"}"#)
                .unwrap();
        let migrated = normalize_preferences(legacy);
        assert_eq!(migrated.base_url, "https://alpha.example.invalid/sub2ops");
        assert_eq!(
            migrated.connections,
            vec![SavedConnection {
                name: "alpha.example.invalid".into(),
                base_url: "https://alpha.example.invalid/sub2ops".into(),
            }]
        );

        let mut prefs = migrated;
        upsert_connection(
            &mut prefs,
            "https://alpha.example.invalid/sub2ops",
            Some("主力节点"),
        );
        upsert_connection(
            &mut prefs,
            "https://beta.example.invalid",
            Some("  备用节点  "),
        );
        assert_eq!(
            prefs.connections,
            vec![
                SavedConnection {
                    name: "主力节点".into(),
                    base_url: "https://alpha.example.invalid/sub2ops".into(),
                },
                SavedConnection {
                    name: "备用节点".into(),
                    base_url: "https://beta.example.invalid".into(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn remove_connection_validates_target_rejects_active_or_connecting_and_stops_on_credential_error(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut state = runtime();
        state.path = dir.path().join("preferences.json");
        let initial_prefs = Preferences {
            base_url: "https://alpha.example.invalid".into(),
            connections: vec![
                SavedConnection {
                    name: "Alpha".into(),
                    base_url: "https://alpha.example.invalid".into(),
                },
                SavedConnection {
                    name: "Beta".into(),
                    base_url: "https://beta.example.invalid".into(),
                },
            ],
            ..Default::default()
        };
        save_preferences(&state.path, &initial_prefs).unwrap();
        {
            let mut view = state.view.lock().await;
            view.connected = true;
            view.preferences = initial_prefs.clone();
        }

        // 1. Out-of-bounds / unknown target is rejected before credential deletion.
        let unknown =
            remove_connection_with(&state, "https://unknown.example.invalid", |_| async {
                panic!("credential deletion must not run for unknown target")
            })
            .await;
        assert!(unknown.unwrap_err().contains("未找到"));

        // 2. Active connected target is rejected.
        let active = remove_connection_with(&state, "https://alpha.example.invalid/", |_| async {
            panic!("credential deletion must not run for active target")
        })
        .await;
        assert!(active.unwrap_err().contains("正在连接或使用中"));

        // 3. In-flight connecting target is rejected.
        *state.connecting_url.lock().await = Some("https://beta.example.invalid".into());
        let connecting =
            remove_connection_with(&state, "https://beta.example.invalid", |_| async {
                panic!("credential deletion must not run for connecting target")
            })
            .await;
        assert!(connecting.unwrap_err().contains("正在连接或使用中"));
        *state.connecting_url.lock().await = None;

        // 4. Partial failure (credential deletion fails) stops and leaves preferences intact.
        let partial = remove_connection_with(&state, "https://beta.example.invalid", |_| async {
            Err("无法删除钥匙串中的 Key".into())
        })
        .await;
        assert_eq!(partial.unwrap_err(), "无法删除钥匙串中的 Key");
        assert_eq!(state.view.lock().await.preferences.connections.len(), 2);
        let on_disk: Preferences =
            serde_json::from_slice(&std::fs::read(&state.path).unwrap()).unwrap();
        assert_eq!(on_disk.connections.len(), 2);

        // 5. Inactive target succeeds after credential deletion and persists remaining connections.
        let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let deleted_clone = deleted.clone();
        let remaining =
            remove_connection_with(&state, "https://beta.example.invalid/", move |target| {
                deleted_clone.lock().unwrap().push(target);
                async { Ok(()) }
            })
            .await
            .unwrap();
        assert_eq!(
            *deleted.lock().unwrap(),
            vec!["https://beta.example.invalid"]
        );
        assert_eq!(
            remaining.connections,
            vec![SavedConnection {
                name: "Alpha".into(),
                base_url: "https://alpha.example.invalid".into(),
            }]
        );
        let on_disk: Preferences =
            serde_json::from_slice(&std::fs::read(&state.path).unwrap()).unwrap();
        assert_eq!(on_disk.connections, remaining.connections);
    }
}
