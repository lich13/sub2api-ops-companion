use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tauri::{Emitter, Manager, State, WindowEvent};
#[cfg(desktop)]
use std::sync::Mutex as StdMutex;
#[cfg(desktop)]
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    WebviewUrl, WebviewWindowBuilder,
};
#[cfg(desktop)]
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{oneshot, watch, Mutex, Notify};

mod api_http;
mod credentials;
use api_http::{http, http_detailed, HttpFailure};

const RELEASES: &str = "https://github.com/lich13/sub2api-ops-companion/releases";

#[cfg(target_os = "macos")]
mod tray_macos;
#[cfg(target_os = "macos")]
mod record_activity;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    base_url: String,
    favorites: Vec<i64>,
    launch_at_login: bool,
    pinned: bool,
    #[serde(deserialize_with = "read_record_columns")]
    record_columns: Option<Vec<String>>,
    model_test_concurrency: Option<u8>,
}

fn read_record_columns<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Vec<String>>, D::Error> {
    let columns = Option::<Vec<String>>::deserialize(deserializer)?;
    Ok(columns.map(|values| values.into_iter().filter(|value| value != "type").collect()))
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
    snapshot: Option<Value>,
    preferences: Preferences,
}

struct Runtime {
    client: reqwest::Client,
    view: Mutex<ViewState>,
    key: Mutex<Option<String>>,
    network: Mutex<()>,
    snapshot_gate: Mutex<()>,
    generation: AtomicU64,
    tests: Mutex<HashMap<String, oneshot::Sender<()>>>,
    wake: Notify,
    path: PathBuf,
    foreground: AtomicBool,
    read_epoch: watch::Sender<u64>,
    #[cfg(target_os = "macos")]
    record_activity: record_activity::RecordActivity,
    #[cfg(desktop)]
    pinned: AtomicBool,
    #[cfg(desktop)]
    quitting: AtomicBool,
    #[cfg(desktop)]
    panel: StdMutex<PanelLifecycle>,
}

#[cfg(desktop)]
struct PanelLifecycle {
    generation: u64,
    focused: bool,
    showing: bool,
    height: f64,
    anchor: Option<tauri::Rect>,
}

#[cfg(desktop)]
impl Default for PanelLifecycle {
    fn default() -> Self {
        Self { generation: 0, focused: false, showing: false, height: 520., anchor: None }
    }
}

#[cfg(desktop)]
impl PanelLifecycle {
    fn show(&mut self, focused: bool) {
        self.generation += 1;
        self.focused = focused;
        self.showing = false;
    }
    fn focus(&mut self) {
        self.generation += 1;
        self.focused = true;
    }
    fn blur(&mut self) -> Option<u64> {
        if !self.focused || self.showing {
            return None;
        }
        self.focused = false;
        Some(self.generation)
    }
    fn can_hide(&self, generation: u64) -> bool {
        generation == self.generation && !self.focused && !self.showing
    }
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
    if path.contains(['#', '\\']) || path.contains("..") {
        return false;
    }
    let plain = path.split('?').next().unwrap_or("");
    if plain == "/errors" || plain.starts_with("/errors/") {
        if method != "GET" { return false; }
        let list = plain == "/errors";
        if !list && !plain.strip_prefix("/errors/").is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) && s.parse::<u64>().is_ok_and(|id| id > 0)) { return false; }
        return path.split_once('?').is_none_or(|(_, query)| {
            if query.is_empty() { return false; }
            let mut seen = std::collections::HashSet::new();
            query.split('&').all(|pair| {
                let Some((key, value)) = pair.split_once('=') else { return false; };
                if !seen.insert(key) { return false; }
                if key == "category" { return matches!(value, "degradation" | "other"); }
                list && matches!(key, "account_id" | "before_id" | "limit")
                    && !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit())
                    && value.parse::<u64>().is_ok_and(|id| id > 0 && (key != "limit" || id <= 200))
            })
        });
    }
    if plain == "/connection-status" { return method == "GET" && path == plain; }
    let job_id = |s: &str| s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit());
    if plain.starts_with("/account-operations") {
        let query_ok = path.split_once('?').is_none_or(|(_, q)| q.strip_prefix("after_event=")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) && v.parse::<u64>().is_ok()));
        return match method {
            "GET" => path == "/account-operations" || query_ok && plain.strip_prefix("/account-operations/").is_some_and(job_id),
            "POST" => !path.contains('?') && plain.strip_prefix("/account-operations/").and_then(|s| s.strip_suffix("/cancel")).is_some_and(job_id),
            _ => false,
        };
    }
    if account_path(plain, "/operations") { return method == "POST" && !path.contains('?'); }
    if plain.starts_with("/model-tests/") {
        if path.contains('?') { return false; }
        let suffix = plain.strip_prefix("/model-tests/").unwrap_or("");
        return match method {
            "GET" => job_id(suffix),
            "POST" => suffix.strip_suffix("/cancel").or_else(|| suffix.strip_suffix("/retry")).is_some_and(job_id),
            _ => false,
        };
    }
    if plain == "/account-templates" {
        return match method {
            "GET" => path == "/account-templates" || path.strip_prefix("/account-templates?account_id=").is_some_and(|s| s.parse::<u64>().is_ok_and(|id| id > 0)),
            "PUT" => path == "/account-templates",
            "POST" => path == "/account-templates" || path == "/account-templates/custom",
            _ => false,
        };
    }
    let template_id = |s: &str| s.strip_prefix("custom-").is_some_and(|id| id.len() == 24 && id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
    if let Some(suffix) = plain.strip_prefix("/account-templates/") {
        if suffix == "custom" { return method == "POST" && !path.contains('?'); }
        if let Some(id) = suffix.strip_prefix("custom/") {
            return !path.contains('?') && template_id(id) && matches!(method, "PUT" | "DELETE");
        }
        return !path.contains('?') && template_id(suffix) && matches!(method, "PUT" | "DELETE");
    }
    if account_path(plain, "/model-detection") {
        return match method { "GET" | "PUT" => !path.contains('?'), _ => false };
    }
    if plain == "/modeltrace/fingerprint-bank" { return method == "GET" && !path.contains('?'); }
    if plain == "/modeltrace/fingerprint-bank/sync" { return method == "POST" && !path.contains('?'); }
    let group_path = |suffix: &str| plain.strip_prefix("/model-groups/").and_then(|s| s.strip_suffix(suffix))
        .is_some_and(|s| s.parse::<u64>().is_ok_and(|id| id > 0));
    if !path.contains('?') && match method {
        "GET" => plain == "/model-groups" || group_path("/reasoning"),
        "PUT" | "DELETE" => group_path("/reasoning"),
        "POST" => group_path("/reasoning/resolve"),
        _ => false,
    } { return true; }
    match method {
        "GET" => {
            matches!(plain, "/config" | "/errors" | "/recoveries" | "/quota-refresh" | "/capabilities" | "/connection-status" | "/usage-records" | "/usage-record-options")
                || plain.strip_prefix("/usage-records/")
                    .is_some_and(|s| s.parse::<u64>().is_ok_and(|id| id > 0))
                || plain
                    .strip_prefix("/errors/")
                    .is_some_and(|s| s.parse::<u64>().is_ok())
                || model_options_path(path)
                || account_path(plain, "/quality")
                || plain.strip_prefix("/accounts/")
                    .and_then(|s| s.strip_suffix("/model-tests/latest"))
                    .is_some_and(|s| s.parse::<u64>().is_ok_and(|id| id > 0))
                || plain.strip_prefix("/model-tests/")
                    .is_some_and(|s| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()))
        }
        "PUT" => {
            (["oauth", "bark", "key_fallback"]
                .iter()
                .any(|s| plain == format!("/config/{s}"))
                || account_path(plain, "/groups") || account_path(plain, "/degradation-mark"))
                && !path.contains('?')
        }
        "POST" => {
            path == "/quota-refresh" || ["bark-test"]
                .iter()
                .any(|s| path == format!("/actions/{s}"))
                || path
                    .strip_prefix("/accounts/")
                    .and_then(|s| {
                        s.strip_suffix("/schedulable")
                            .or_else(|| s.strip_suffix("/usage-action"))
                            .or_else(|| s.strip_suffix("/priority"))
                            .or_else(|| s.strip_suffix("/recover-state"))
                            .or_else(|| s.strip_suffix("/model-tests"))
                    })
                    .is_some_and(|s| s.parse::<u64>().is_ok_and(|id| id > 0))
                || plain.strip_prefix("/model-tests/")
                    .and_then(|s| s.strip_suffix("/cancel"))
                    .is_some_and(|s| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()))
        }
        "DELETE" => !path.contains('?') && account_path(path, ""),
        _ => false,
    }
}

fn account_path(path: &str, suffix: &str) -> bool {
    path.strip_prefix("/accounts/").and_then(|s| s.strip_suffix(suffix))
        .is_some_and(|s| s.parse::<u64>().is_ok_and(|id| id > 0))
}

fn model_options_path(path: &str) -> bool {
    if path.contains('?') {
        path.strip_suffix("?purpose=model_test")
            .is_some_and(|plain| account_path(plain, "/models"))
    } else {
        account_path(path, "/models")
    }
}

fn publish(app: &tauri::AppHandle, value: &ViewState) {
    let _ = app.emit("ops-state", value);
}

fn apply_refresh(view: &mut ViewState, result: Result<Value, HttpFailure>) {
    match result {
        Ok(data) => { view.snapshot = Some(data); view.online = true; view.error.clear(); view.upstream_connection = None; }
        Err(error) => { view.online = false; view.error = error.message; view.upstream_connection = error.connection; }
    }
}

async fn refresh_inner(app: &tauri::AppHandle, state: &Runtime) {
    if !state.foreground.load(Ordering::SeqCst) { return; }
    let Ok(_gate) = state.snapshot_gate.try_lock() else { return; };
    let (base, key, generation) = {
        let _connection = state.network.lock().await;
        (state.view.lock().await.preferences.base_url.clone(), state.key.lock().await.clone(),
         state.generation.load(Ordering::SeqCst))
    };
    if base.is_empty() || key.is_none() {
        return;
    }
    let result = active_read(state, http_detailed(
        &state.client,
        &base,
        key.as_deref().unwrap_or(""),
        "GET",
        "/snapshot",
        None,
        &state.path.with_file_name("network-diagnostics.log"),
    ))
    .await;
    let _connection = state.network.lock().await;
    if generation != state.generation.load(Ordering::SeqCst) || !state.foreground.load(Ordering::SeqCst) { return; }
    let mut view = state.view.lock().await;
    apply_refresh(&mut view, result);
    #[cfg(desktop)]
    if let Some(tray) = app.tray_by_id("ops") {
        let text = if view.online {
            "Sub2Ops · 已连接"
        } else {
            match view.upstream_connection.as_ref().and_then(|c| c["state"].as_str()) {
                Some("auth_rejected") => "Sub2Ops · 管理员 Key 被拒绝",
                Some("network_unreachable") => "Sub2Ops · 上游不可达",
                Some("route_not_found") => "Sub2Ops · 管理接口路径不兼容",
                Some("protocol_mismatch") => "Sub2Ops · 管理接口响应不兼容",
                _ => "Sub2Ops · 离线",
            }
        };
        let _ = tray.set_tooltip(Some(text));
    }
    publish(app, &view);
}

async fn active_read<T, E: From<&'static str>>(state: &Runtime, request: impl std::future::Future<Output = Result<T, E>>) -> Result<T, E> {
    let mut changed = state.read_epoch.subscribe();
    if !state.foreground.load(Ordering::SeqCst) { return Err("应用已转入后台".into()); }
    tokio::select! {
        biased;
        _ = changed.changed() => Err("应用已转入后台，已停止读取".into()),
        result = request => result,
    }
}

#[cfg(target_os = "macos")]
fn sync_record_activity(app: &tauri::AppHandle) -> bool {
    // A key window can outlive application activation on macOS. Use AppKit's
    // application state, while preserving background snapshots for the tray.
    let active = record_activity::application_active();
    let state = app.state::<Arc<Runtime>>();
    if state.record_activity.set_active(active) {
        let state = state.inner().clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut view = state.view.lock().await;
            view.foreground = state.record_activity.active();
            publish(&app, &view);
        });
    }
    active
}

#[cfg(target_os = "macos")]
async fn check_record_activity(app: &tauri::AppHandle) -> Result<(), String> {
    let (send, receive) = oneshot::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || { let _ = send.send(sync_record_activity(&handle)); })
        .map_err(|_| "无法读取应用状态".to_string())?;
    match receive.await {
        Ok(true) => Ok(()),
        Ok(false) => Err("应用已转入后台，已停止读取".into()),
        Err(_) => Err("无法读取应用状态".into()),
    }
}

#[tauri::command]
async fn get_state(state: State<'_, Arc<Runtime>>) -> Result<ViewState, String> {
    Ok(state.view.lock().await.clone())
}

#[tauri::command]
async fn connect(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
    base_url: String,
    api_key: String,
) -> Result<(), String> {
    let base = normalize_base(&base_url)?;
    if api_key.trim().is_empty() || api_key.len() > 4096 {
        return Err("请输入已有的管理员 API Key".into());
    }
    {
        let _gate = state.network.lock().await;
        let data = active_read(&state, http(
            &state.client,
            &base,
            api_key.trim(),
            "GET",
            "/capabilities",
            None,
            &state.path.with_file_name("network-diagnostics.log"),
        ))
        .await?;
        if data.get("api_version").and_then(Value::as_u64) != Some(1) {
            return Err("云端接口版本不兼容".into());
        }
        credentials::write(app.clone(), base.clone(), api_key.trim().to_string()).await?;
        let mut view = state.view.lock().await;
        let mut prefs = view.preferences.clone();
        prefs.base_url = base;
        save_preferences(&state.path, &prefs)?;
        view.preferences = prefs;
        view.connection_revision = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
        state.tests.lock().await.clear();
        view.connected = true;
        view.snapshot = None;
        view.error.clear();
        view.upstream_connection = data.get("upstream_connection").cloned();
        *state.key.lock().await = Some(api_key.trim().to_string());
        publish(&app, &view);
    }
    refresh_inner(&app, &state).await;
    Ok(())
}

#[tauri::command]
async fn disconnect(app: tauri::AppHandle, state: State<'_, Arc<Runtime>>) -> Result<(), String> {
    let _gate = state.network.lock().await;
    let mut view = state.view.lock().await;
    if !view.preferences.base_url.is_empty() {
        credentials::delete(app.clone(), view.preferences.base_url.clone()).await?;
    }
    *state.key.lock().await = None;
    view.connection_revision = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    state.tests.lock().await.clear();
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

#[tauri::command]
async fn refresh(state: State<'_, Arc<Runtime>>) -> Result<(), String> {
    state.wake.notify_one();
    Ok(())
}

#[tauri::command]
async fn api_request(
    _app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
    method: String,
    path: String,
    body: Option<Value>,
) -> Result<Value, String> {
    if !allowed_request(&method, &path) {
        return Err("客户端不允许此操作".into());
    }
    #[cfg(target_os = "macos")]
    let record_read = method == "GET" && record_activity::record_path(&path);
    #[cfg(target_os = "macos")]
    if record_read { check_record_activity(&_app).await?; }
    let (view, key, generation) = {
        let _gate = state.network.lock().await;
        let view = state.view.lock().await.clone();
        let diagnostic = method == "GET" && path == "/connection-status";
        if (!view.online && !diagnostic) || !state.foreground.load(Ordering::SeqCst) {
            return Err("当前离线，请恢复连接后操作".into());
        }
        let key = state.key.lock().await.clone().ok_or("尚未连接")?;
        (view, key, state.generation.load(Ordering::SeqCst))
    };
    let log = state.path.with_file_name("network-diagnostics.log");
    let request = http(
            &state.client,
            &view.preferences.base_url,
            &key,
            &method,
            &path,
            body,
            &log,
        );
    let request = async { if method == "GET" { active_read(&state, request).await } else { request.await } };
    #[cfg(target_os = "macos")]
    let result = if record_read { state.record_activity.read(request).await } else { request.await };
    #[cfg(not(target_os = "macos"))]
    let result = request.await;
    if generation != state.generation.load(Ordering::SeqCst) {
        return Err("连接已切换，旧操作结果已忽略".into());
    }
    if method != "GET" {
        state.wake.notify_one();
    }
    result
}

#[tauri::command]
async fn cancel_test(window: tauri::WebviewWindow, state: State<'_, Arc<Runtime>>) -> Result<(), String> {
    if let Some(cancel) = state.tests.lock().await.remove(window.label()) { let _ = cancel.send(()); }
    Ok(())
}

async fn media_data(client: &reqwest::Client, value: &str, kind: &str) -> Result<String, String> {
    use base64::Engine;
    if value.starts_with(&format!("data:{kind}/")) { return Ok(value.into()); }
    let url = url::Url::parse(value).map_err(|_| "媒体地址无效")?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err("媒体地址必须使用 HTTPS".into());
    }
    // This request intentionally carries no admin headers or cookies.
    let mut response = client.get(url).timeout(Duration::from_secs(90)).send().await.map_err(|_| "无法读取测试媒体")?;
    if !response.status().is_success() { return Err("测试媒体请求失败".into()); }
    let mime = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|h| h.to_str().ok())
        .unwrap_or("").split(';').next().unwrap_or("").to_owned();
    if !mime.starts_with(&format!("{kind}/")) { return Err("测试媒体类型无效".into()); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "测试媒体连接中断")? {
        if bytes.len() + chunk.len() > 64 * 1024 * 1024 { return Err("测试媒体超过 64 MiB".into()); }
        bytes.extend_from_slice(&chunk);
    }
    Ok(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

#[tauri::command]
async fn run_test(app: tauri::AppHandle, window: tauri::WebviewWindow, state: State<'_, Arc<Runtime>>,
    account_id: u64, body: Value, on_event: tauri::ipc::Channel<Value>) -> Result<(), String> {
    if account_id == 0 { return Err("账号无效".into()); }
    let (base, key, generation) = {
        let _gate = state.network.lock().await;
        let view = state.view.lock().await;
        if !view.online || !state.foreground.load(Ordering::SeqCst) { return Err("当前离线".into()); }
        (view.preferences.base_url.clone(), state.key.lock().await.clone().ok_or("尚未连接")?, state.generation.load(Ordering::SeqCst))
    };
    let (tx, rx) = oneshot::channel();
    {
        let mut tests = state.tests.lock().await;
        if tests.contains_key(window.label()) { return Err("测试正在进行".into()); }
        tests.insert(window.label().to_string(), tx);
    }
    let work = async {
        let mut response = state.client.post(format!("{base}/api/desktop/v1/accounts/{account_id}/test"))
            .header("x-api-key", key).header("Accept", "text/event-stream")
            .timeout(Duration::from_secs(310)).json(&body).send().await.map_err(|_| "测试连接失败")?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let data: Value = response.json().await.unwrap_or(Value::Null);
            return Err(format!("{} [{status}]", data.get("detail").and_then(Value::as_str).unwrap_or("测试请求被拒绝")));
        }
        let mut buffer = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "测试连接中断")? {
            if generation != state.generation.load(Ordering::SeqCst) { return Err("连接已切换".into()); }
            buffer.extend_from_slice(&chunk);
            if buffer.len() > 128 * 1024 * 1024 { return Err("测试输出过大".into()); }
            while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=end).collect();
                if !line.starts_with(b"data:") { continue; }
                if let Ok(mut event) = serde_json::from_slice::<Value>(&line[5..]) {
                    let kind = event.get("type").and_then(Value::as_str).unwrap_or("").to_string();
                    if ["image", "audio", "video"].contains(&kind.as_str()) {
                        let field = format!("{kind}_url");
                        if let Some(raw) = event.get(&field).and_then(Value::as_str) {
                            event[&field] = Value::String(media_data(&state.client, raw, &kind).await?);
                        }
                    }
                    on_event.send(event).map_err(|_| "测试窗口已关闭")?;
                }
            }
        }
        Ok(())
    };
    let result = tokio::select! { result=active_read(&state, work)=>result, _=rx=>Err("测试已取消，未重放请求".into()) };
    let mut tests = state.tests.lock().await;
    if tests.get(window.label()).is_some_and(|sender| sender.is_closed()) {
        tests.remove(window.label());
    }
    drop(tests);
    state.wake.notify_one();
    let _ = app;
    result
}

#[tauri::command]
async fn preferences(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
    favorites: Vec<i64>,
    pinned: bool,
    launch_at_login: bool,
    record_columns: Option<Vec<String>>,
    model_test_concurrency: Option<u8>,
) -> Result<(), String> {
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
        if !(1..=3).contains(&concurrency) { return Err("测试并发必须为 1、2 或 3".into()); }
        prefs.model_test_concurrency = Some(concurrency);
    }
    prefs.favorites = favorites
        .into_iter()
        .filter(|id| *id > 0)
        .take(100)
        .collect();
    prefs.favorites.sort_unstable();
    prefs.favorites.dedup();
    prefs.pinned = cfg!(desktop) && pinned;
    prefs.launch_at_login = cfg!(desktop) && launch_at_login;
    if let Some(columns) = record_columns {
        let allowed = ["api_key", "account", "model", "reasoning", "tokens", "cost", "latency", "user_agent", "ip", "endpoint", "group", "billing", "request_id", "upstream_id"];
        prefs.record_columns = Some(allowed.iter().filter(|c| columns.iter().any(|v| v == **c)).map(|c| c.to_string()).collect());
    }
    save_preferences(&state.path, &prefs)?;
    #[cfg(desktop)]
    state.pinned.store(pinned, Ordering::Relaxed);
    view.preferences = prefs;
    publish(&app, &view);
    Ok(())
}

#[tauri::command]
#[cfg(desktop)]
fn show_main(app: tauri::AppHandle) -> Result<(), String> {
    queue_main(app, true)
}

#[cfg(desktop)]
fn main_visibility(app: &tauri::AppHandle, visible: bool) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("主窗口不存在")?;
    if !visible { window.hide().map_err(|_| "无法关闭主窗口")?; }
    #[cfg(target_os = "macos")]
    {
        let policy = if visible { tauri::ActivationPolicy::Regular } else { tauri::ActivationPolicy::Accessory };
        app.set_activation_policy(policy).map_err(|_| "无法切换 Dock 状态")?;
        app.set_dock_visibility(visible).map_err(|_| "无法更新 Dock 图标")?;
        if visible { app.show().map_err(|_| "无法显示客户端")?; }
    }
    if visible {
        window.show().map_err(|_| "无法打开窗口")?;
        window.unminimize().map_err(|_| "无法恢复主窗口")?;
        window.set_focus().map_err(|_| "无法聚焦主窗口")?;
    }
    app.state::<Arc<Runtime>>().wake.notify_one();
    panel_trace(app, "main_visibility", if visible { "open_dock_visible" } else { "closed_dock_hidden" });
    Ok(())
}

#[cfg(desktop)]
fn queue_main(app: tauri::AppHandle, visible: bool) -> Result<(), String> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        if let Err(error) = main_visibility(&handle, visible) { panel_trace(&handle, "main_failed", &error); }
    }).map_err(|_| "无法调度主窗口操作".to_string())
}

#[tauri::command]
async fn check_updates(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
) -> Result<String, String> {
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
    response.json::<Value>().await.map_err(|_| "版本信息无效".to_string())
    }).await?;
    if let Some((tag, url)) = release_update(&releases, env!("CARGO_PKG_VERSION"), cfg!(mobile)) {
            app.opener()
                .open_url(&url, None::<&str>)
                .map_err(|_| "无法打开发布页")?;
            return Ok(if cfg!(mobile) { format!("发现 {tag}，已打开发布页") }
                else { format!("发现 {tag}，已打开安装包") });
    }
    if cfg!(mobile) {
        let page = latest_release_page(&releases).unwrap_or_else(|| format!("{RELEASES}/latest"));
        app.opener().open_url(&page, None::<&str>).map_err(|_| "无法打开发布页")?;
        return Ok(format!("当前版本 {}，已打开发布页", env!("CARGO_PKG_VERSION")));
    }
    Ok(format!("当前版本 {}，已是最新", env!("CARGO_PKG_VERSION")))
}

fn parsed_release_version(tag: &str) -> Option<(u64, u64, u64)> {
    let raw = tag.strip_prefix("desktop-v")?;
    if !raw.bytes().all(|c| c.is_ascii_digit() || c == b'.') { return None; }
    let parts = raw.split('.').map(str::parse::<u64>).collect::<Result<Vec<_>, _>>().ok()?;
    if parts.len() != 3 { return None; }
    Some((parts[0], parts[1], parts[2]))
}

fn latest_release_page(releases: &Value) -> Option<String> {
    releases.as_array()?.iter().filter_map(|release| {
        if release["draft"].as_bool().unwrap_or(false) || release["prerelease"].as_bool().unwrap_or(false) { return None; }
        let tag = release["tag_name"].as_str()?;
        parsed_release_version(tag)?;
        Some((parsed_release_version(tag)?, tag))
    }).max_by_key(|r| r.0).map(|(_, tag)| format!("{RELEASES}/tag/{tag}"))
}

fn release_update(releases: &Value, current: &str, android: bool) -> Option<(String, String)> {
    let version = |value: &str| -> Option<(u64, u64, u64)> {
        let parts = value.split('.').map(str::parse::<u64>).collect::<Result<Vec<_>, _>>().ok()?;
        if parts.len() != 3 { return None; }
        Some((parts[0], parts[1], parts[2]))
    };
    let current = version(current)?;
    releases.as_array()?.iter().filter_map(|release| {
        if release["draft"].as_bool().unwrap_or(false) || release["prerelease"].as_bool().unwrap_or(false) { return None; }
        let tag = release["tag_name"].as_str()?;
        let newer = parsed_release_version(tag)?;
        if newer <= current { return None; }
        if android { return Some((newer, tag.to_string(), format!("{RELEASES}/tag/{tag}"))); }
        let asset = release["assets"].as_array()?.iter().find(|asset| asset["name"].as_str().is_some_and(|name| {
            name.ends_with("_aarch64.dmg")
        }))?;
        let url = asset["browser_download_url"].as_str()?;
        if !url.starts_with(&format!("{RELEASES}/download/{tag}/")) { return None; }
        Some((newer, tag.to_string(), url.to_string()))
    }).max_by_key(|r| r.0).map(|(_, tag, url)| (tag, url))
}

#[cfg(desktop)]
fn panel_bounds(
    anchor: tauri::PhysicalPosition<f64>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scale: f64,
    content_height: f64,
) -> (tauri::PhysicalPosition<i32>, tauri::PhysicalSize<u32>) {
    let w = (420. * scale).min((width as f64 - 16.).max(1.));
    let h = (content_height.clamp(128., 520.) * scale).min((height as f64 - 16.).max(1.));
    let px = (anchor.x - w + 20.).clamp(
        x as f64 + 8.,
        (x as f64 + width as f64 - w - 8.).max(x as f64 + 8.),
    );
    let py = (anchor.y + 10.).clamp(
        y as f64 + 8.,
        (y as f64 + height as f64 - h - 8.).max(y as f64 + 8.),
    );
    (
        tauri::PhysicalPosition::new(px as i32, py as i32),
        tauri::PhysicalSize::new(w as u32, h as u32),
    )
}

#[cfg(desktop)]
fn panel_trace(app: &tauri::AppHandle, stage: &str, detail: &str) {
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
fn create_quick(app: &tauri::AppHandle) -> tauri::Result<()> {
    WebviewWindowBuilder::new(
        app,
        "quick",
        WebviewUrl::App("index.html?panel=quick".into()),
    )
    .title("Sub2Ops 快捷面板")
    .inner_size(420., 520.)
    .decorations(false)
    .resizable(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .visible(false)
    .focused(false)
    .build()?;
    panel_trace(app, "precreated", "hidden");
    Ok(())
}

#[cfg(desktop)]
fn quick_panel(
    app: &tauri::AppHandle,
    rect: tauri::Rect,
) -> Result<(), Box<dyn std::error::Error>> {
    panel_trace(app, "show_begin", "");
    let window = app.get_webview_window("quick").ok_or("快捷窗口未创建")?;
    let height = {
        let state = app.state::<Arc<Runtime>>();
        let mut panel = state.panel.lock().unwrap();
        panel.show(false);
        panel.showing = true;
        panel.anchor = Some(rect);
        panel.height
    };
    position_quick(app, &window, rect, height)?;
    #[cfg(target_os = "macos")]
    app.show()?;
    window.show()?;
    panel_trace(app, "shown", "");
    window.set_focus()?;
    let focused = window.is_focused()?;
    {
        let state = app.state::<Arc<Runtime>>();
        let mut panel = state.panel.lock().unwrap();
        panel.showing = false;
        panel.focused = focused;
    }
    panel_trace(app, "focus_requested", if focused { "focused" } else { "awaiting_event" });
    app.state::<Arc<Runtime>>().wake.notify_one();
    Ok(())
}

#[cfg(desktop)]
fn position_quick(app: &tauri::AppHandle, window: &tauri::WebviewWindow, rect: tauri::Rect, height: f64)
    -> Result<(), Box<dyn std::error::Error>> {
    let position = rect.position.to_physical::<f64>(1.0);
    let size = rect.size.to_physical::<f64>(1.0);
    let anchor = tauri::PhysicalPosition::new(position.x + size.width, position.y + size.height);
    let monitors = window.available_monitors()?;
    let monitor = monitors
        .iter()
        .find(|m| {
            let p = m.position();
            let s = m.size();
            anchor.x >= p.x as f64
                && anchor.x < p.x as f64 + s.width as f64
                && anchor.y >= p.y as f64
                && anchor.y < p.y as f64 + s.height as f64
        })
        .ok_or("菜单栏边界未匹配到屏幕")?;
    {
        let m = monitor;
        let area = m.work_area();
        let (position, size) = panel_bounds(
            anchor,
            area.position.x,
            area.position.y,
            area.size.width,
            area.size.height,
            m.scale_factor(),
            height,
        );
        window.set_size(size)?;
        window.set_position(position)?;
        panel_trace(
            app,
            "positioned",
            &format!(
                "x={} y={} width={} height={}",
                position.x, position.y, size.width, size.height
            ),
        );
    }
    Ok(())
}

#[tauri::command]
#[cfg(desktop)]
fn resize_quick(app: tauri::AppHandle, window: tauri::WebviewWindow, height: f64) -> Result<(), String> {
    if window.label() != "quick" || !height.is_finite() { return Err("快捷窗口尺寸无效".into()); }
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let state = handle.state::<Arc<Runtime>>();
        let height = height.ceil().clamp(128., 520.);
        let anchor = {
            let mut panel = state.panel.lock().unwrap();
            if panel.height == height { return; }
            panel.height = height;
            panel.anchor
        };
        let result = if let Some(rect) = anchor {
            position_quick(&handle, &window, rect, height).map_err(|e| e.to_string())
        } else {
            window.set_size(tauri::LogicalSize::new(420., height)).map_err(|e| e.to_string())
        };
        if let Err(error) = result { panel_trace(&handle, "resize_failed", &error); }
    }).map_err(|_| "无法调整快捷窗口尺寸".into())
}

#[tauri::command]
#[cfg(desktop)]
fn show_quick(app: tauri::AppHandle) -> Result<(), String> {
    queue_quick(app, None)
}

#[cfg(desktop)]
fn queue_quick(app: tauri::AppHandle, event_rect: Option<tauri::Rect>) -> Result<(), String> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        // Tray clicks use the event's geometry. The toolbar alone needs a lookup.
        let rect = event_rect.or_else(|| {
            handle
                .tray_by_id("ops")
                .and_then(|t| t.rect().ok().flatten())
        });
        let result = rect
            .ok_or_else(|| "菜单栏边界不可用".to_string())
            .and_then(|rect| quick_panel(&handle, rect).map_err(|e| e.to_string()));
        if let Err(error) = result {
            handle.state::<Arc<Runtime>>().panel.lock().unwrap().showing = false;
            panel_trace(&handle, "show_failed", &error);
            let _ = handle.emit("update-result", format!("无法打开快捷面板：{error}"));
        }
    })
    .map_err(|_| "无法打开快捷面板".into())
}

#[tauri::command]
#[cfg(desktop)]
fn hide_quick(app: tauri::AppHandle) -> Result<(), String> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        handle
            .state::<Arc<Runtime>>()
            .panel
            .lock()
            .unwrap()
            .show(false);
        if let Some(window) = handle.get_webview_window("quick") {
            match window.hide() {
                Ok(()) => panel_trace(&handle, "hidden", "explicit"),
                Err(error) => panel_trace(&handle, "hide_failed", &error.to_string()),
            }
        }
    })
    .map_err(|_| "无法关闭快捷面板".into())
}

#[cfg(desktop)]
fn defer_panel_blur(app: &tauri::AppHandle) {
    panel_trace(app, "blur", "");
    let generation = app.state::<Arc<Runtime>>().panel.lock().unwrap().blur();
    let Some(generation) = generation else {
        return;
    };
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let app = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            let state = app.state::<Arc<Runtime>>();
            if state.pinned.load(Ordering::Relaxed) {
                return;
            }
            let eligible = state.panel.lock().unwrap().can_hide(generation);
            if eligible {
                if let Some(window) = app.get_webview_window("quick") {
                    if !window.is_focused().unwrap_or(true) {
                        let mut panel = state.panel.lock().unwrap();
                        if !panel.can_hide(generation) {
                            return;
                        }
                        panel.show(false);
                        drop(panel);
                        match window.hide() {
                            Ok(()) => panel_trace(&app, "hidden", "blur"),
                            Err(error) => panel_trace(&app, "hide_failed", &error.to_string()),
                        }
                    }
                }
            }
        });
    });
}

#[cfg(desktop)]
fn silent_launch(args: &[String]) -> bool { args.iter().any(|arg| arg == "--autostart") }

#[cfg(desktop)]
fn fit_main_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    let window = app.get_webview_window("main").expect("main window");
    if let Some(monitor) = window.current_monitor()?.or(window.primary_monitor()?) {
        let area = monitor.work_area();
        let scale = monitor.scale_factor();
        let width = (1440. * scale).min((area.size.width as f64 - 24. * scale).max(1.));
        let height = (860. * scale).min((area.size.height as f64 - 24. * scale).max(1.));
        window.set_min_size(Some(tauri::PhysicalSize::new((900. * scale).min(width) as u32, (620. * scale).min(height) as u32)))?;
        window.set_size(tauri::PhysicalSize::new(width as u32, height as u32))?;
        window.set_position(tauri::PhysicalPosition::new(
            area.position.x + ((area.size.width as f64 - width) / 2.) as i32,
            area.position.y + ((area.size.height as f64 - height) / 2.) as i32,
        ))?;
    }
    Ok(())
}

#[cfg(mobile)]
#[tauri::command]
async fn background(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<credentials::SecureStore>().0.run_mobile_plugin::<Value>("background", serde_json::json!({}))
            .map(|_| ()).map_err(|_| "无法返回后台".to_string())
    }).await.map_err(|_| "无法返回后台".to_string())?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, args, _| { if !silent_launch(&args) { let _ = show_main(app.clone()); } }))
        .plugin(tauri_plugin_autostart::Builder::new().macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent).args(["--autostart"]).build());
    #[cfg(mobile)]
    let builder = builder.plugin(credentials::plugin());
    let builder = builder
        .setup(|app| {
            let path = app.path().app_config_dir()?.join("preferences.json");
            let prefs: Preferences = std::fs::read(&path).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default();
            let state = Arc::new(Runtime { client: reqwest::Client::builder().timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).retry(reqwest::retry::never()).build()?,
                #[cfg(desktop)] pinned: AtomicBool::new(prefs.pinned),
                #[cfg(desktop)] quitting: AtomicBool::new(false),
                #[cfg(desktop)] panel: StdMutex::new(PanelLifecycle::default()),
                foreground: AtomicBool::new(true), read_epoch: watch::channel(0).0,
                #[cfg(target_os = "macos")] record_activity: record_activity::RecordActivity::default(),
                view: Mutex::new(ViewState { platform: if cfg!(mobile) { "android" } else { "macos" }, foreground: !cfg!(target_os = "macos"), initializing: true, preferences: prefs, ..Default::default() }),
                key: Mutex::new(None), network: Mutex::new(()), snapshot_gate: Mutex::new(()),
                generation: AtomicU64::new(0), tests: Mutex::new(HashMap::new()), wake: Notify::new(), path });
            app.manage(state.clone());
            #[cfg(desktop)]
            {
            fit_main_window(app.handle())?;
            create_quick(app.handle())?;
            // Re-register an existing LaunchAgent with the new arguments without
            // changing the user's preference or enabling a disabled login item.
            if app.autolaunch().is_enabled()? { app.autolaunch().enable()?; }
            main_visibility(app.handle(), !silent_launch(&std::env::args().collect::<Vec<_>>()))?;
            let menu = Menu::with_items(app, &[
                &MenuItem::with_id(app, "open", "打开 Sub2Ops", true, None::<&str>)?,
                &MenuItem::with_id(app, "updates", "检查更新", true, None::<&str>)?,
                &MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?,
            ])?;
            // Original monochrome pulse mark, drawn in memory for the macOS template icon.
            let mut pixels = vec![0u8; 22*22*4];
            for x in 2..20usize { let y = match x { 6..=8 => 11-(x-5)*2, 9..=12 => 5+(x-8)*3, 13..=15 => 17-(x-12)*2, _ => 11 }; for d in 0..2 { let i=((y+d).min(21)*22+x)*4; pixels[i+3]=255; } }
            let tray = TrayIconBuilder::with_id("ops").icon(tauri::image::Image::new_owned(pixels,22,22)).icon_as_template(true).tooltip("Sub2Ops").menu(&menu).show_menu_on_left_click(false)
                .on_tray_icon_event(|tray,event| {
                    if let TrayIconEvent::Click {button,button_state,rect,..}=event {
                        panel_trace(tray.app_handle(), "tray_click", &format!("{button:?} {button_state:?}"));
                        if button == MouseButton::Left && button_state == MouseButtonState::Up {
                            if let Err(error) = queue_quick(tray.app_handle().clone(), Some(rect)) {
                                panel_trace(tray.app_handle(), "dispatch_failed", &error);
                            }
                        }
                        #[cfg(target_os = "macos")]
                        if button == MouseButton::Right && button_state == MouseButtonState::Down {
                            if let Err(error) = tray_macos::show_menu(tray) {
                                panel_trace(tray.app_handle(), "menu_failed", &error.to_string());
                            }
                        }
                    }
                })
                .on_menu_event(|app,event| match event.id().as_ref() {
                    "open" => {let _=show_main(app.clone());},
                    "updates" => { let handle=app.clone(); tauri::async_runtime::spawn(async move { let result=check_updates(handle.clone(),handle.state()).await; let _=handle.emit("update-result",result.unwrap_or_else(|e|e)); let _=show_main(handle); }); },
                    "quit" => { app.state::<Arc<Runtime>>().quitting.store(true,Ordering::Relaxed); app.exit(0); }, _=>()
                }).build(app)?;
            #[cfg(target_os = "macos")]
            { tray_macos::install(&tray)?; panel_trace(app.handle(), "tray_ready", "transient_menu_macos27"); }
            }
            let handle=app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let base = state.view.lock().await.preferences.base_url.clone();
                let stored = credentials::read(handle.clone(), base).await;
                {
                    let _gate = state.network.lock().await;
                    let mut view = state.view.lock().await;
                    if state.generation.load(Ordering::SeqCst) == 0 {
                        match stored {
                            Ok(key) => { view.connected = key.is_some(); *state.key.lock().await = key; }
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
                #[cfg(desktop)]
                let visible=["main","quick"].iter().any(|label| handle.get_webview_window(label).is_some_and(|w|w.is_visible().unwrap_or(false)));
                #[cfg(mobile)]
                let visible = true;
                // Wall time advances through macOS sleep. Check locally once a
                // second so wake does not wait out a suspended 15-second timer.
                let due=std::time::SystemTime::now()+Duration::from_secs(if visible {2} else {15});
                refresh_inner(&handle,&state).await;
                loop {
                    if std::time::SystemTime::now()>=due {break;}
                    tokio::select! { _=tokio::time::sleep(Duration::from_millis(100))=>(), _=state.wake.notified()=>break }
                }
            }});
            Ok(())
        })
        .on_window_event(|window,event| {
            let state=window.state::<Arc<Runtime>>();
            match event {
                #[cfg(desktop)]
                WindowEvent::CloseRequested {api,..} if !state.quitting.load(Ordering::Relaxed) => {
                    api.prevent_close();
                    if window.label() == "main" {
                        if let Err(error) = queue_main(window.app_handle().clone(), false) { panel_trace(window.app_handle(), "main_failed", &error); }
                    } else if let Err(error) = window.hide() { panel_trace(window.app_handle(), "hide_failed", &error.to_string()); }
                },
                #[cfg(desktop)]
                WindowEvent::Focused(false) if window.label()=="quick" => defer_panel_blur(window.app_handle()),
                #[cfg(desktop)]
                WindowEvent::Focused(true) => { if window.label()=="quick" {state.panel.lock().unwrap().focus();panel_trace(window.app_handle(), "focused", "event");} state.wake.notify_one(); },
                #[cfg(mobile)]
                WindowEvent::Suspended | WindowEvent::Resumed => {
                    let active = matches!(event, WindowEvent::Resumed);
                    state.foreground.store(active, Ordering::SeqCst);
                    if !active { state.read_epoch.send_modify(|epoch| *epoch += 1); }
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
                _=>()
            }
        });
    #[cfg(desktop)]
    let builder = builder.invoke_handler(tauri::generate_handler![get_state,connect,disconnect,refresh,api_request,run_test,cancel_test,preferences,show_main,show_quick,hide_quick,resize_quick,check_updates]);
    #[cfg(mobile)]
    let builder = builder.invoke_handler(tauri::generate_handler![get_state,connect,disconnect,refresh,api_request,run_test,cancel_test,preferences,background,check_updates]);
    builder.build(tauri::generate_context!()).expect("Sub2Ops failed to start")
        .run(|_app,_event| {
            #[cfg(target_os = "macos")]
            if matches!(&_event, tauri::RunEvent::Ready | tauri::RunEvent::MainEventsCleared) {
                sync_record_activity(_app);
            }
            #[cfg(desktop)]
            if let tauri::RunEvent::Reopen {..}=_event {let _=show_main(_app.clone());}
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn runtime() -> Runtime {
        Runtime {
            client: reqwest::Client::new(), view: Mutex::new(ViewState::default()),
            key: Mutex::new(None), network: Mutex::new(()), snapshot_gate: Mutex::new(()),
            generation: AtomicU64::new(0), tests: Mutex::new(HashMap::new()), wake: Notify::new(),
            path: PathBuf::new(), foreground: AtomicBool::new(true), read_epoch: watch::channel(0).0,
            #[cfg(target_os = "macos")] record_activity: record_activity::RecordActivity::default(),
            #[cfg(desktop)] pinned: AtomicBool::new(false),
            #[cfg(desktop)] quitting: AtomicBool::new(false),
            #[cfg(desktop)] panel: StdMutex::new(PanelLifecycle::default()),
        }
    }
    #[tokio::test]
    async fn background_cancels_inflight_reads_and_does_not_start_new_reads() {
        let state = runtime();
        let started = AtomicBool::new(false);
        let request = async { started.store(true, Ordering::SeqCst); std::future::pending::<Result<(), String>>().await };
        let (result, ()) = tokio::join!(active_read(&state, request), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            state.foreground.store(false, Ordering::SeqCst);
            state.read_epoch.send_modify(|n| *n += 1);
        });
        assert!(started.load(Ordering::SeqCst));
        assert!(result.unwrap_err().contains("后台"));
        assert!(active_read(&state, async { panic!("background request must not be polled"); #[allow(unreachable_code)] Ok::<(), String>(()) }).await.is_err());
        state.foreground.store(true, Ordering::SeqCst);
        assert_eq!(active_read(&state, async { Ok::<i32, String>(7) }).await.unwrap(), 7);
    }
    #[test]
    fn error_categories_have_exact_read_only_routes() {
        for path in ["/errors", "/errors?category=degradation", "/errors?category=other&before_id=25&limit=50", "/errors/25?category=other"] {
            assert!(allowed_request("GET", path), "{path}");
            assert!(!allowed_request("POST", path), "{path}");
        }
        for path in ["/errors?category=all", "/errors?category=other&category=degradation", "/errors?url=example.invalid", "/errors/25?before_id=2", "/errors/0?category=other", "/errors?limit=201", "/errors?category=other&", "/errors/25?category=other#fragment"] {
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
        assert_eq!(release_update(&data, "0.1.9", true).unwrap().1, format!("{RELEASES}/tag/desktop-v9.0.0"));
        assert!(release_update(&data, "0.1.9", false).unwrap().1.ends_with("_aarch64.dmg"));
        assert!(release_update(&data, "9.0.0", true).is_none());
        assert_eq!(latest_release_page(&data).as_deref(), Some("https://github.com/lich13/sub2api-ops-companion/releases/tag/desktop-v9.0.0"));
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
        assert_eq!(latest_release_page(&data), Some(format!("{RELEASES}/tag/desktop-v1.2.3")));
        assert_eq!(release_update(&data, "1.2.2", true).unwrap().1, format!("{RELEASES}/tag/desktop-v1.2.3"));
        assert!(latest_release_page(&serde_json::json!([])).is_none());
    }
    #[test]
    fn custom_templates_only_allow_generated_ids_and_explicit_methods() {
        let id = "custom-0123456789abcdef01234567";
        assert!(allowed_request("POST", "/account-templates"));
        assert!(allowed_request("POST", "/account-templates/custom"));
        for method in ["PUT", "DELETE"] {
            assert!(allowed_request(method, &format!("/account-templates/{id}")));
            assert!(allowed_request(method, &format!("/account-templates/custom/{id}")));
            for suffix in ["full", "custom-not-an-id", "custom-0123456789ABCDEF01234567"] {
                assert!(!allowed_request(method, &format!("/account-templates/{suffix}")));
            }
            assert!(!allowed_request(method, &format!("/account-templates/{id}?x=1")));
        }
        assert!(!allowed_request("GET", &format!("/account-templates/{id}")));
    }
    #[test]
    fn transient_failure_keeps_last_data_and_recovers() {
        let mut view = ViewState::default();
        let first = serde_json::json!({"observed_at":"2026-09-25T10:00:00Z","accounts":[{"id":1}]});
        apply_refresh(&mut view, Ok(first.clone()));
        apply_refresh(&mut view, Err("网关暂不可用 [502]".into()));
        assert!(!view.online);
        assert_eq!(view.snapshot, Some(first));
        let connection = serde_json::json!({"state":"auth_rejected"});
        apply_refresh(&mut view, Err(HttpFailure { message: "管理员 Key 被拒绝".into(), connection: Some(connection.clone()) }));
        assert_eq!(view.upstream_connection, Some(connection));
        let next = serde_json::json!({"observed_at":"2026-09-25T10:00:02Z","accounts":[{"id":2}]});
        apply_refresh(&mut view, Ok(next.clone()));
        assert!(view.online && view.error.is_empty());
        assert_eq!(view.snapshot, Some(next));
    }
    #[test]
    fn obsolete_blur_cannot_close_a_reopened_panel() {
        let mut panel = PanelLifecycle::default();
        panel.show(false);
        assert_eq!(panel.blur(), None);
        panel.focus();
        let blur = panel.blur().unwrap();
        assert!(panel.can_hide(blur));
        panel.show(false);
        assert!(!panel.can_hide(blur));
        panel.focus();
        let blur = panel.blur().unwrap();
        panel.focus();
        assert!(!panel.can_hide(blur));
    }
    #[test]
    fn panel_stays_inside_secondary_and_scaled_displays() {
        for (x, y, w, h, scale, ax, ay) in [
            (-1920, 24, 1920, 1056, 1., -8., 0.),
            (0, 50, 2560, 1614, 2., 2540., 20.),
            (0, -900, 800, 850, 2., 200., -900.),
        ] {
            let (p, s) = panel_bounds(tauri::PhysicalPosition::new(ax, ay), x, y, w, h, scale, 520.);
            assert!(p.x >= x && p.y >= y);
            assert!(p.x + s.width as i32 <= x + w as i32);
            assert!(p.y + s.height as i32 <= y + h as i32);
        }
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
    fn command_allowlist() {
        for suffix in ["groups", "degradation-mark"] {
            assert!(allowed_request("PUT", &format!("/accounts/7/{suffix}")));
            for method in ["GET", "POST", "DELETE"] {
                assert!(!allowed_request(method, &format!("/accounts/7/{suffix}")));
            }
            for path in [format!("/accounts/0/{suffix}"), format!("/accounts/7/{suffix}?all=true"), format!("/accounts/7/{suffix}/credentials")] {
                assert!(!allowed_request("PUT", &path));
            }
        }
        assert!(allowed_request("GET", "/usage-records?limit=50&after_id=12"));
        assert!(allowed_request("GET", "/connection-status"));
        assert!(!allowed_request("POST", "/connection-status"));
        assert!(!allowed_request("GET", "/connection-status?url=https://other.invalid"));
        assert!(allowed_request("GET", "/usage-records/12"));
        assert!(allowed_request("GET", "/usage-record-options?kind=api_keys&user_id=7"));
        for path in ["/usage-records/0", "/usage-records/12/credentials", "/usage-records/../config", "/usage-record-options/keys", "/usage-record-options/../config"] {
            assert!(!allowed_request("GET", path));
        }
        for method in ["POST", "PUT", "DELETE"] {
            assert!(!allowed_request(method, "/usage-records"));
            assert!(!allowed_request(method, "/usage-records/12"));
            assert!(!allowed_request(method, "/usage-record-options"));
        }
        assert!(allowed_request("GET", "/model-groups"));
        assert!(allowed_request("GET", "/model-groups/7/reasoning"));
        assert!(allowed_request("POST", "/model-groups/7/reasoning/resolve"));
        assert!(allowed_request("PUT", "/model-groups/7/reasoning"));
        assert!(allowed_request("DELETE", "/model-groups/7/reasoning"));
        for (method, path) in [("GET", "/model-catalog"), ("GET", "/model-groups/7"), ("POST", "/model-groups/7/preview"), ("POST", "/model-groups/7/upstream-import"), ("PUT", "/model-groups/7/allowlist"), ("PUT", "/model-groups/7/overrides")] {
            assert!(!allowed_request(method, path));
        }
        for path in ["/model-groups/0", "/model-groups/7/credentials", "/model-groups/7?key=secret", "/model-groups/7/../accounts"] {
            assert!(!allowed_request("GET", path));
        }
        assert!(!allowed_request("DELETE", "/model-groups/7"));
        assert!(allowed_request("POST", "/accounts/7/schedulable"));
        assert!(allowed_request("GET", "/errors?account_id=7"));
        assert!(allowed_request("GET", "/accounts/7/models"));
        assert!(allowed_request("GET", "/accounts/7/models?purpose=model_test"));
        for path in [
            "/accounts/7/models?purpose=legacy",
            "/accounts/7/models?purpose=model_test&x=1",
            "/accounts/7/models?purpose=model_test&purpose=model_test",
            "/accounts/7/models?purpose=other",
            "/accounts/0/models?purpose=model_test",
        ] {
            assert!(!allowed_request("GET", path));
        }
        assert!(allowed_request("DELETE", "/accounts/7"));
        assert!(allowed_request("POST", "/accounts/7/recover-state"));
        assert!(allowed_request("GET", "/accounts/7/quality"));
        assert!(allowed_request("POST", "/accounts/7/model-tests"));
        assert!(allowed_request("GET", "/accounts/7/model-tests/latest"));
        assert!(allowed_request("GET", "/model-tests/0123456789abcdef0123456789abcdef"));
        assert!(allowed_request("POST", "/model-tests/0123456789abcdef0123456789abcdef/cancel"));
        assert!(!allowed_request("POST", "/model-tests/invalid/cancel"));
        assert!(!allowed_request("GET", "/model-tests/0"));
        assert!(!allowed_request("POST", "/accounts/7/quality"));
        assert!(!allowed_request("GET", "/accounts/0/quality"));
        assert!(!allowed_request("GET", "/accounts/7/quality/../credentials"));
        for p in ["/accounts/0", "/accounts/7/", "/accounts/7?all=true", "/accounts"] {
            assert!(!allowed_request("DELETE", p));
        }
        for p in ["/accounts/7/test", "https://evil.test", "/config/../token"] {
            assert!(!allowed_request("POST", p));
        }
    }
    #[test]
    fn queued_operations_and_retry_have_explicit_paths() {
        let id = "0123456789abcdef0123456789abcdef";
        for (method, path) in [("POST", "/accounts/421/operations".to_owned()),
            ("GET", "/account-operations".to_owned()), ("GET", format!("/account-operations/{id}")),
            ("GET", format!("/account-operations/{id}?after_event=100")),
            ("POST", format!("/account-operations/{id}/cancel")), ("POST", format!("/model-tests/{id}/retry"))] {
            assert!(allowed_request(method, &path), "{method} {path}");
        }
        for (method, path) in [("POST", format!("/model-tests/{id}/retry?force=true")),
            ("GET", format!("/account-operations/{id}?after_event=1&after_event=2")),
            ("GET", format!("/account-operations/{id}?other=1")),
            ("POST", "/accounts/421/operations?force=true".to_owned()),
            ("DELETE", format!("/account-operations/{id}")), ("GET", "/account-operations/invalid".to_owned())] {
            assert!(!allowed_request(method, &path), "{method} {path}");
        }
    }
    #[test]
    fn login_launch_is_silent_but_manual_and_second_instances_open() {
        assert!(silent_launch(&["sub2ops".into(), "--autostart".into()]));
        assert!(!silent_launch(&["sub2ops".into()]));
        assert!(!silent_launch(&["sub2ops".into(), "--autostart=false".into()]));
    }
    #[test]
    fn compact_panel_height_is_content_bounded() {
        for (height, expected) in [(100., 128), (386., 386), (800., 520)] {
            let (_, size) = panel_bounds(tauri::PhysicalPosition::new(800., 24.), 0, 24, 1400, 900, 1., height);
            assert_eq!(size.width, 420);
            assert_eq!(size.height, expected);
        }
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
        for (method, path) in [("GET", "/account-templates"), ("PUT", "/account-templates"),
            ("GET", "/account-templates?account_id=421"), ("GET", "/accounts/421/model-detection"),
            ("PUT", "/accounts/421/model-detection"), ("GET", "/modeltrace/fingerprint-bank"),
            ("POST", "/modeltrace/fingerprint-bank/sync")] {
            assert!(allowed_request(method, path));
        }
        for path in ["/account-templates?unknown=1", "/account-templates?account_id=0",
                     "/accounts/421/model-detection?x=1", "/modeltrace/fingerprint-bank?x=1"] {
            assert!(!allowed_request("GET", path));
        }
        assert!(!allowed_request("DELETE", "/account-templates"));
        assert!(!allowed_request("PUT", "/modeltrace/fingerprint-bank"));
        let old: Preferences = serde_json::from_str("{}").unwrap();
        assert!(old.model_test_concurrency.is_none());
        let changed = Preferences { model_test_concurrency: Some(3), ..old };
        let restored: Preferences = serde_json::from_str(&serde_json::to_string(&changed).unwrap()).unwrap();
        assert_eq!(restored.model_test_concurrency, Some(3));
    }

    #[test]
    fn old_preferences_migrate_without_losing_existing_choices() {
        let old: Preferences = serde_json::from_str(r#"{"base_url":"https://example.com","favorites":[7],"pinned":true,"launch_at_login":true}"#).unwrap();
        assert!(old.record_columns.is_none());
        assert_eq!(old.favorites, vec![7]);
        assert!(old.pinned && old.launch_at_login);
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("prefs.json");
        let changed = Preferences { record_columns: Some(vec!["model".into(), "cost".into()]), ..old };
        save_preferences(&p, &changed).unwrap();
        let restored: Preferences = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        assert_eq!(restored.record_columns, changed.record_columns);
        assert_eq!(restored.base_url, "https://example.com");
        assert_eq!(restored.favorites, vec![7]);
        assert!(restored.pinned && restored.launch_at_login);
    }

    #[test]
    fn retired_type_column_is_removed_without_resetting_preferences() {
        let prefs: Preferences = serde_json::from_str(r#"{"base_url":"https://example.com","favorites":[7],"pinned":true,"launch_at_login":true,"record_columns":["ip","type","model"]}"#).unwrap();
        assert_eq!(prefs.record_columns, Some(vec!["ip".into(), "model".into()]));
        assert_eq!(prefs.favorites, vec![7]);
        assert!(prefs.pinned && prefs.launch_at_login);
        let empty: Preferences = serde_json::from_str(r#"{"record_columns":["type"]}"#).unwrap();
        assert_eq!(empty.record_columns, Some(vec![]));
    }
}
