mod account_creation;
mod key_quota;
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
mod priority_api;
mod proxy_management;
use api_http::{http_detailed, HttpFailure};

const RELEASES: &str = "https://github.com/lich13/sub2api-ops-companion/releases";

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
    let allowed = [
        "api_key",
        "account",
        "model",
        "type",
        "reasoning",
        "tokens",
        "cost",
        "latency",
        "user_agent",
        "ip",
        "endpoint",
        "group",
        "billing",
        "request_id",
        "upstream_id",
    ];
    allowed
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
    connected: bool,
    online: bool,
    error: String,
    upstream_connection: Option<Value>,
    snapshot: Option<Arc<Value>>,
    preferences: Preferences,
}

struct Runtime {
    client: reqwest::Client,
    view: Mutex<ViewState>,
    key: Mutex<Option<String>>,
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

fn allowed_request(method: &str, path: &str) -> bool {
    sub2ops_protocol::routes::client_endpoint(method, path).is_some()
}

fn publish(_app: &tauri::AppHandle, value: &ViewState) {
    // Android's window visibility getter always returns false. Its suspended /
    // resumed events already control polling and carry foreground in ViewState.
    #[cfg(mobile)]
    let _ = _app.emit("ops-state", value);
    #[cfg(target_os = "macos")]
    macos_bridge::publish_state(value);
}

fn apply_refresh(view: &mut ViewState, result: Result<Value, HttpFailure>) {
    match result {
        Ok(data) => {
            view.snapshot = Some(Arc::new(data));
            view.online = true;
            view.error.clear();
            view.upstream_connection = None;
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
    let (base, key, generation) = {
        let _connection = state.network.lock().await;
        (
            state.view.lock().await.preferences.base_url.clone(),
            state.key.lock().await.clone(),
            state.generation.load(Ordering::SeqCst),
        )
    };
    if base.is_empty() || key.is_none() {
        return;
    }
    let result = active_read(
        state,
        http_detailed(
            &state.client,
            &base,
            key.as_deref().unwrap_or(""),
            "GET",
            "/snapshot",
            None,
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
    apply_refresh(&mut view, result);
    #[cfg(target_os = "macos")]
    macos_bridge::publish_tooltip(&view);
    publish(app, &view);
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
        let data = active_read(
            &state,
            http_detailed(
                &state.client,
                &base,
                &resolved_key,
                "GET",
                "/capabilities",
                None,
                &state.path.with_file_name("network-diagnostics.log"),
            ),
        )
        .await?;
        if should_write {
            credentials::write(app.clone(), base.clone(), resolved_key.clone()).await?;
        }
        let mut view = state.view.lock().await;
        let mut prefs = view.preferences.clone();
        prefs.base_url = base.clone();
        upsert_connection(&mut prefs, &base, name.as_deref());
        save_preferences(&state.path, &prefs)?;
        view.preferences = prefs;
        view.connection_revision = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
        view.connected = true;
        view.snapshot = None;
        view.error.clear();
        view.upstream_connection = data.get("upstream_connection").cloned();
        *state.key.lock().await = Some(resolved_key);
        publish(&app, &view);
        Ok(())
    }
    .await;
    *state.connecting_url.lock().await = None;
    connect_result?;
    refresh_inner(&app, &state).await;
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
    let mut view = state.view.lock().await;
    if !view
        .preferences
        .connections
        .iter()
        .any(|item| item.base_url == target)
        && view.preferences.base_url != target
    {
        return Err("未找到要更新的实例配置".into());
    }
    let mut prefs = view.preferences.clone();
    upsert_connection(&mut prefs, &target, Some(&name));
    save_preferences(&state.path, &prefs)?;
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
    save_preferences(&state.path, &prefs)?;
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
    view.connection_revision = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    view.connected = false;
    view.online = false;
    view.snapshot = None;
    view.error.clear();
    view.upstream_connection = None;
    view.preferences.base_url.clear();
    save_preferences(&state.path, &view.preferences)?;
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
    if !allowed_request(&method, &path) {
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
    pinned: bool,
    launch_at_login: bool,
    record_columns: Option<Vec<String>>,
    model_test_concurrency: Option<u8>,
    model_test_preferences: Option<std::collections::BTreeMap<String, ModelTestPreference>>,
) -> Result<(), HttpFailure> {
    let mut view = state.view.lock().await;
    #[cfg(desktop)]
    if launch_at_login != view.preferences.launch_at_login {
        let launch = app.autolaunch();
        if launch_at_login {
            launch.enable()
        } else {
            launch.disable()
        }
        .map_err(|_| "无法更改开机启动")?;
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
    prefs.pinned = cfg!(desktop) && pinned;
    prefs.launch_at_login = cfg!(desktop) && launch_at_login;
    if let Some(columns) = record_columns {
        prefs.record_columns = Some(normalize_record_columns(columns));
    }
    save_preferences(&state.path, &prefs)?;
    view.preferences = prefs;
    publish(&app, &view);
    Ok(())
}

#[cfg_attr(mobile, tauri::command)]
async fn open_authorization_url(app: tauri::AppHandle, url: String) -> Result<(), HttpFailure> {
    if !account_creation::authorization_url(&url) {
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

#[cfg_attr(mobile, tauri::command)]
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
    if let Some((tag, url)) = release_update(&releases, env!("CARGO_PKG_VERSION"), cfg!(mobile)) {
        app.opener()
            .open_url(&url, None::<&str>)
            .map_err(|_| "无法打开发布页")?;
        return Ok(if cfg!(mobile) {
            format!("发现 {tag}，已打开发布页")
        } else {
            format!("发现 {tag}，已打开安装包")
        });
    }
    if cfg!(mobile) {
        let page = latest_release_page(&releases).unwrap_or_else(|| format!("{RELEASES}/latest"));
        app.opener()
            .open_url(&page, None::<&str>)
            .map_err(|_| "无法打开发布页")?;
        return Ok(format!(
            "当前版本 {}，已打开发布页",
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(format!("当前版本 {}，已是最新", env!("CARGO_PKG_VERSION")))
}

fn parsed_release_version(tag: &str) -> Option<(u64, u64, u64)> {
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

fn latest_release_page(releases: &Value) -> Option<String> {
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
            parsed_release_version(tag)?;
            Some((parsed_release_version(tag)?, tag))
        })
        .max_by_key(|r| r.0)
        .map(|(_, tag)| format!("{RELEASES}/tag/{tag}"))
}

fn release_update(releases: &Value, current: &str, android: bool) -> Option<(String, String)> {
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
            if android {
                return Some((newer, tag.to_string(), format!("{RELEASES}/tag/{tag}")));
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
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<credentials::SecureStore>()
            .0
            .run_mobile_plugin::<Value>("background", serde_json::json!({}))
            .map(|_| ())
            .map_err(|_| "无法返回后台".to_string())
    })
    .await
    .map_err(|_| "无法返回后台".to_string())?
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
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init());
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
    let builder = builder.plugin(credentials::plugin());
    let builder = builder.setup(|app| {
        let path = app.path().app_config_dir()?.join("preferences.json");
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
            connecting_url: Mutex::new(None),
            network: Mutex::new(()),
            snapshot_gate: Mutex::new(()),
            generation: AtomicU64::new(0),
            wake: Notify::new(),
            path,
        });
        app.manage(state.clone());
        #[cfg(desktop)]
        {
            // Re-register an existing LaunchAgent with the new arguments without
            // changing the user's preference or enabling a disabled login item.
            if app.autolaunch().is_enabled()? {
                app.autolaunch().enable()?;
            }
            #[cfg(target_os = "macos")]
            macos_bridge::init(app.handle().clone());
        }
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
        check_updates,
        open_authorization_url,
        open_sub2api_release
    ]);
    #[cfg(target_os = "macos")]
    let mut cached_state: Option<Arc<Runtime>> = None;
    #[cfg(target_os = "macos")]
    let mut activity_checked = false;
    builder
        .build(tauri::generate_context!())
        .expect("Sub2Ops failed to start")
        .run(move |_app, _event| {
            #[cfg(target_os = "macos")]
            {
                let state = cached_state
                    .get_or_insert_with(|| _app.state::<Arc<Runtime>>().inner().clone());
                match &_event {
                    tauri::RunEvent::Ready
                    | tauri::RunEvent::Resumed
                    | tauri::RunEvent::Reopen { .. } => {
                        sync_record_activity_with(_app, state);
                        activity_checked = true;
                    }
                    tauri::RunEvent::MainEventsCleared => {
                        if !activity_checked {
                            sync_record_activity_with(_app, state);
                        }
                        activity_checked = false;
                    }
                    _ => {}
                }
                if let tauri::RunEvent::Reopen { .. } = _event {
                    macos_bridge::emit_reopen();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    #[test]
    fn configuration_routes_are_explicit_and_reject_arbitrary_targets() {
        for (method, path) in [
            ("GET", "/sub2api-config"),
            ("GET", "/sub2api-config/operation"),
            ("POST", "/sub2api-config/validate"),
            ("POST", "/sub2api-config/apply"),
        ] {
            assert!(super::allowed_request(method, path));
        }
        for (method, path) in [
            ("DELETE", "/sub2api-config"),
            ("POST", "/sub2api-config/operation"),
            ("POST", "/sub2api-config/restart"),
        ] {
            assert!(!super::allowed_request(method, path));
        }
    }
    use super::*;
    fn runtime() -> Runtime {
        Runtime {
            client: reqwest::Client::new(),
            view: Mutex::new(ViewState::default()),
            key: Mutex::new(None),
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
    fn account_settings_routes_are_exact_and_read_only() {
        for path in ["/accounts/7/settings", "/proxies"] {
            assert!(allowed_request("GET", path), "{path}");
            for method in ["POST", "PUT", "DELETE"] {
                assert!(!allowed_request(method, path), "{method} {path}");
            }
        }
        for path in [
            "/accounts/0/settings",
            "/accounts/-1/settings",
            "/accounts/a/settings",
            "/accounts/7/settings/",
            "/accounts/7/settings/../credentials",
            "/accounts/7%2fsettings",
            "/proxies/all",
            "/accounts/18446744073709551616/settings",
        ] {
            assert!(!allowed_request("GET", path), "{path}");
        }
    }

    #[test]
    fn account_keepalive_routes_allow_only_exact_get_and_put() {
        let path = "/accounts/7/keepalive";
        for method in ["GET", "PUT"] {
            assert!(allowed_request(method, path), "{method} {path}");
        }
        for method in ["POST", "PATCH", "DELETE"] {
            assert!(!allowed_request(method, path), "{method} {path}");
        }
        for malformed in [
            "/accounts/0/keepalive",
            "/accounts/-1/keepalive",
            "/accounts/+7/keepalive",
            "/accounts/a/keepalive",
            "/accounts/7/keepalive/",
            "/accounts/7/keepalive/../credentials",
            "/accounts/7%2fkeepalive",
            "/accounts/18446744073709551616/keepalive",
        ] {
            assert!(!allowed_request("GET", malformed), "GET {malformed}");
            assert!(!allowed_request("PUT", malformed), "PUT {malformed}");
        }
    }

    #[test]
    fn proxy_diagnostic_routes_are_exact_post_only() {
        for suffix in ["test", "quality-check"] {
            let path = format!("/proxies/7/{suffix}");
            assert!(allowed_request("POST", &path), "{path}");
            for method in ["GET", "PUT", "DELETE"] {
                assert!(!allowed_request(method, &path), "{method} {path}");
            }
        }
        for path in [
            "/proxies/0/test",
            "/proxies/-1/test",
            "/proxies/7/unknown",
            "/proxies/7/test/extra",
            "/proxies/7/test/../credentials",
            "/proxies/18446744073709551616/quality-check",
        ] {
            assert!(!allowed_request("POST", path), "{path}");
        }
    }

    #[test]
    fn error_categories_have_exact_read_only_routes() {
        for path in [
            "/errors",
            "/errors?category=degradation",
            "/errors?category=other&before_id=25&limit=50",
            "/errors/25?category=other",
        ] {
            assert!(allowed_request("GET", path), "{path}");
            assert!(!allowed_request("POST", path), "{path}");
        }
        for path in [
            "/errors/0?category=other",
            "/errors/25?category=other#fragment",
        ] {
            assert!(!allowed_request("GET", path), "{path}");
        }
    }

    #[test]
    fn updates_select_newer_signed_platform_package_only() {
        let data = serde_json::json!([
            {"tag_name":"desktop-v0.1.10","assets":[
                {"name":"Sub2Ops_0.1.10_aarch64.dmg","browser_download_url":format!("{RELEASES}/download/desktop-v0.1.10/Sub2Ops_0.1.10_aarch64.dmg")},
                {"name":"Sub2Ops_0.1.10_arm64-v8a.apk","browser_download_url":format!("{RELEASES}/download/desktop-v0.1.10/Sub2Ops_0.1.10_arm64-v8a.apk")}]},
            {"tag_name":"desktop-v0.2.0","draft":true,"assets":[]},
            {"tag_name":"desktop-v9.0.0","assets":[{"name":"unsafe_arm64-v8a.apk","browser_download_url":"https://other.example/file.apk"}]}
        ]);
        assert_eq!(
            release_update(&data, "0.1.9", true).unwrap().1,
            format!("{RELEASES}/tag/desktop-v9.0.0")
        );
        assert!(release_update(&data, "0.1.9", false)
            .unwrap()
            .1
            .ends_with("_aarch64.dmg"));
        assert!(release_update(&data, "9.0.0", true).is_none());
        assert_eq!(
            latest_release_page(&data).as_deref(),
            Some("https://github.com/lich13/sub2api-ops-companion/releases/tag/desktop-v9.0.0")
        );
    }
    #[test]
    fn release_pages_reject_untrusted_tags_and_nonstable_releases() {
        let data = serde_json::json!([
            {"tag_name":"desktop-v9.9.9/../../other"},
            {"tag_name":"desktop-v9.9.9?redirect=bad"},
            {"tag_name":"desktop-v99.1.1","prerelease":true},
            {"tag_name":"desktop-v99.1.2","draft":true},
            {"tag_name":"desktop-v1.2.3","html_url":"https://other.invalid"}
        ]);
        assert_eq!(
            latest_release_page(&data),
            Some(format!("{RELEASES}/tag/desktop-v1.2.3"))
        );
        assert_eq!(
            release_update(&data, "1.2.2", true).unwrap().1,
            format!("{RELEASES}/tag/desktop-v1.2.3")
        );
        assert!(latest_release_page(&serde_json::json!([])).is_none());
    }
    #[test]
    fn templates_allow_legacy_and_generated_ids_with_explicit_methods() {
        let id = "custom-0123456789abcdef01234567";
        assert!(allowed_request("POST", "/account-templates"));
        assert!(!allowed_request("POST", "/account-templates/custom"));
        for method in ["PUT", "DELETE"] {
            assert!(allowed_request(method, &format!("/account-templates/{id}")));
            assert!(!allowed_request(
                method,
                &format!("/account-templates/custom/{id}")
            ));
            for legacy in ["full", "degraded", "takeover"] {
                assert!(allowed_request(
                    method,
                    &format!("/account-templates/{legacy}")
                ));
            }
            for suffix in [
                "unknown",
                "custom-not-an-id",
                "custom-0123456789ABCDEF01234567",
            ] {
                assert!(!allowed_request(
                    method,
                    &format!("/account-templates/{suffix}")
                ));
            }
        }
        assert!(!allowed_request("GET", &format!("/account-templates/{id}")));
    }
    #[test]
    fn template_batch_routes_require_the_declared_method() {
        let id = "0123456789abcdef0123456789abcdef";
        assert!(allowed_request("POST", "/account-templates/apply"));
        assert!(allowed_request(
            "GET",
            &format!("/account-operations?batch_id={id}")
        ));
        assert!(!allowed_request("PUT", "/account-templates/apply"));
        assert!(!allowed_request(
            "POST",
            &format!("/account-operations?batch_id={id}")
        ));
    }
    #[test]
    fn proxy_batch_path_is_explicit() {
        assert!(allowed_request("POST", "/account-operations/batch"));
        for method in ["GET", "PUT", "DELETE"] {
            assert!(!allowed_request(method, "/account-operations/batch"));
        }
        assert!(!allowed_request("POST", "/account-operations/batch/"));
    }
    #[test]
    fn connection_batch_cancel_requires_post_and_one_lowercase_batch_id() {
        let id = "0123456789abcdef0123456789abcdef";
        let path = format!("/account-operations/batches/{id}/cancel");
        assert!(allowed_request("POST", &path));
        for method in ["GET", "PUT", "PATCH", "DELETE", "HEAD"] {
            assert!(!allowed_request(method, &path), "{method}");
        }
        for invalid_id in [
            "",
            "invalid",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert!(!allowed_request(
                "POST",
                &format!("/account-operations/batches/{invalid_id}/cancel")
            ));
        }
        for invalid_path in [
            format!("{path}/"),
            format!("/account-operations/batches/{id}"),
            format!("/account-operations/batches/{id}/cancel/extra"),
            format!("/account-operations/batches/{id}/{id}/cancel"),
        ] {
            assert!(!allowed_request("POST", &invalid_path), "{invalid_path}");
        }
    }

    #[test]
    fn oauth_reauthorization_only_allows_post_for_one_account() {
        let path = "/accounts/7/apply-oauth-credentials";
        assert!(allowed_request("POST", path));
        for method in ["GET", "PUT", "PATCH", "DELETE", "HEAD"] {
            assert!(!allowed_request(method, path));
        }
        for path in [
            "/accounts/0/apply-oauth-credentials",
            "/accounts/-1/apply-oauth-credentials",
            "/accounts/7/apply-oauth-credentials/",
            "/accounts/7/8/apply-oauth-credentials",
        ] {
            assert!(!allowed_request("POST", path));
        }
    }

    #[test]
    fn transient_failure_keeps_last_data_and_recovers() {
        let mut view = ViewState::default();
        let first = serde_json::json!({"observed_at":"2026-09-25T10:00:00Z","accounts":[{"id":1}]});
        apply_refresh(&mut view, Ok(first.clone()));
        apply_refresh(&mut view, Err("网关暂不可用 [502]".into()));
        assert!(!view.online);
        assert_eq!(view.snapshot.as_deref(), Some(&first));
        let cloned = view.clone();
        assert!(Arc::ptr_eq(
            view.snapshot.as_ref().unwrap(),
            cloned.snapshot.as_ref().unwrap()
        ));
        assert_eq!(serde_json::to_value(&cloned).unwrap()["snapshot"], first);
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
        let next = serde_json::json!({"observed_at":"2026-09-25T10:00:02Z","accounts":[{"id":2}]});
        apply_refresh(&mut view, Ok(next.clone()));
        assert!(view.online && view.error.is_empty());
        assert_eq!(view.snapshot.as_deref(), Some(&next));
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
    #[test]
    fn client_routes_delegate_queries_and_keep_requests_inside_the_desktop_api() {
        for path in [
            "/accounts/7/models",
            "/accounts/7/models?purpose=model_test&refresh=true",
            "/accounts/7/models?refresh=true&purpose=model_test",
            "/accounts/7/models?refresh=false",
            "/accounts/7/key-quota-settings/access-token",
            "/config/key-quota",
            "/accounts/7/usage",
            "/usage-records?limit=50&after_id=12",
            "/usage-record-options?kind=api_keys&user_id=7",
            "/usage-records/12",
            "/connection-status",
            "/detection-logs?before=YWJj&limit=100",
        ] {
            assert!(allowed_request("GET", path), "{path}");
        }
        for (method, path) in [
            ("GET", "/snapshot"),
            ("GET", "/capabilities"),
            ("GET", "//example.invalid/config"),
            ("GET", "https://example.invalid/config"),
            ("GET", "/config/../snapshot"),
            ("GET", "/accounts/7/%2e%2e/settings"),
            ("GET", "/config#fragment"),
            ("GET", "/accounts/0/key-quota-settings/access-token"),
            ("POST", "/accounts/7/key-quota-settings/access-token"),
            ("GET", "/usage-records/12/credentials"),
            ("POST", "/accounts/7/recover-state"),
            ("PUT", "/accounts/7/groups"),
            ("DELETE", "/accounts/7"),
            ("POST", "/detection-logs"),
            ("GET", "/detection-logs/1"),
            ("GET", "/model-tests/invalid"),
            ("POST", "/model-tests/invalid/cancel"),
        ] {
            assert!(!allowed_request(method, path), "{method} {path}");
        }
    }
    #[test]
    fn queued_operations_and_retry_have_explicit_paths() {
        let id = "0123456789abcdef0123456789abcdef";
        for (method, path) in [
            ("POST", "/accounts/421/operations".to_owned()),
            ("GET", "/account-operations".to_owned()),
            ("GET", format!("/account-operations/{id}")),
            ("GET", format!("/account-operations/{id}?after_event=100")),
            ("POST", format!("/account-operations/{id}/cancel")),
            ("POST", format!("/model-tests/{id}/retry")),
        ] {
            assert!(allowed_request(method, &path), "{method} {path}");
        }
        for (method, path) in [
            ("DELETE", format!("/account-operations/{id}")),
            ("GET", "/account-operations/invalid".to_owned()),
        ] {
            assert!(!allowed_request(method, &path), "{method} {path}");
        }
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
    fn account_templates_and_detection_have_explicit_method_and_path_allowlist() {
        for (method, path) in [
            ("GET", "/account-templates"),
            ("GET", "/account-templates?account_id=421"),
            ("GET", "/accounts/421/model-detection"),
            ("PUT", "/accounts/421/model-detection"),
            ("GET", "/modeltrace/fingerprint-bank"),
            ("POST", "/modeltrace/fingerprint-bank/sync"),
        ] {
            assert!(allowed_request(method, path));
        }
        for method in ["PUT", "DELETE"] {
            assert!(!allowed_request(method, "/account-templates"));
        }
        assert!(!allowed_request("PUT", "/modeltrace/fingerprint-bank"));
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
        assert!(allowed_request("GET", "/lm-detector/fingerprint-bank"));
        assert!(allowed_request(
            "POST",
            "/lm-detector/fingerprint-bank/sync"
        ));
        assert!(!allowed_request("PUT", "/lm-detector/fingerprint-bank"));
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
