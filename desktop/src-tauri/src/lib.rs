use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{Mutex, Notify};

const RELEASES: &str = "https://github.com/lich13/sub2api-ops-companion/releases";

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    base_url: String,
    favorites: Vec<i64>,
    launch_at_login: bool,
    pinned: bool,
}

#[derive(Clone, Serialize, Default)]
struct ViewState {
    connected: bool,
    online: bool,
    error: String,
    snapshot: Option<Value>,
    preferences: Preferences,
}

struct Runtime {
    client: reqwest::Client,
    view: Mutex<ViewState>,
    key: Mutex<Option<String>>,
    network: Mutex<()>,
    wake: Notify,
    path: PathBuf,
    pinned: AtomicBool,
    quitting: AtomicBool,
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
    match method {
        "GET" => {
            matches!(plain, "/config" | "/errors" | "/capabilities")
                || plain
                    .strip_prefix("/errors/")
                    .is_some_and(|s| s.parse::<u64>().is_ok())
        }
        "PUT" => {
            ["oauth", "bark", "telegram", "key_fallback", "model_guard"]
                .iter()
                .any(|s| plain == format!("/config/{s}"))
                && !path.contains('?')
        }
        "POST" => {
            ["bark-test", "telegram-test", "telegram-pairing"]
                .iter()
                .any(|s| path == format!("/actions/{s}"))
                || path
                    .strip_prefix("/accounts/")
                    .and_then(|s| s.strip_suffix("/schedulable"))
                    .is_some_and(|s| s.parse::<u64>().is_ok())
        }
        _ => false,
    }
}

async fn http(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "请求方式无效")?;
    let mut req = client
        .request(method, format!("{base}/api/desktop/v1{path}"))
        .header("x-api-key", key)
        .header("Accept", "application/json");
    if let Some(body) = body {
        req = req.json(&body);
    }
    let response = req.send().await.map_err(|e| {
        if e.is_timeout() {
            "请求超时，未重放操作"
        } else {
            "连接失败，请检查网络和服务地址"
        }
    })?;
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err("管理员 API Key 已失效，请重新连接".into());
    }
    let data: Value = response
        .json()
        .await
        .map_err(|_| "服务返回了无法识别的数据")?;
    if !status.is_success() {
        let detail = data.get("detail");
        let message = detail
            .and_then(Value::as_str)
            .or_else(|| {
                detail
                    .and_then(|d| d.get("message"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("操作失败，请刷新后重试");
        let suffix = if detail
            .and_then(|d| d.get("detached"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            "（该账号已解除托管）"
        } else {
            ""
        };
        return Err(format!(
            "{}{} [{}]",
            message.chars().take(240).collect::<String>(),
            suffix,
            status.as_u16()
        ));
    }
    Ok(data)
}

fn publish(app: &tauri::AppHandle, value: &ViewState) {
    let _ = app.emit("ops-state", value);
}

async fn refresh_inner(app: &tauri::AppHandle, state: &Runtime) {
    let _gate = state.network.lock().await;
    let base = state.view.lock().await.preferences.base_url.clone();
    let key = state.key.lock().await.clone();
    if base.is_empty() || key.is_none() {
        return;
    }
    let result = http(
        &state.client,
        &base,
        key.as_deref().unwrap_or(""),
        "GET",
        "/snapshot",
        None,
    )
    .await;
    let mut view = state.view.lock().await;
    match result {
        Ok(data) => {
            view.snapshot = Some(data);
            view.online = true;
            view.error.clear();
        }
        Err(error) => {
            view.online = false;
            view.error = error;
        }
    }
    if let Some(tray) = app.tray_by_id("ops") {
        let text = if view.online {
            "Sub2Ops · 已连接"
        } else {
            "Sub2Ops · 离线"
        };
        let _ = tray.set_tooltip(Some(text));
    }
    publish(app, &view);
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
        let data = http(
            &state.client,
            &base,
            api_key.trim(),
            "GET",
            "/capabilities",
            None,
        )
        .await?;
        if data.get("api_version").and_then(Value::as_u64) != Some(1) {
            return Err("云端接口版本不兼容".into());
        }
        keyring::Entry::new("com.lich13.sub2ops", &base)
            .map_err(|_| "无法访问钥匙串")?
            .set_password(api_key.trim())
            .map_err(|_| "无法将 Key 保存到钥匙串；连接未保存")?;
        let mut view = state.view.lock().await;
        let mut prefs = view.preferences.clone();
        prefs.base_url = base;
        save_preferences(&state.path, &prefs)?;
        view.preferences = prefs;
        view.connected = true;
        view.snapshot = None;
        view.error.clear();
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
        let entry = keyring::Entry::new("com.lich13.sub2ops", &view.preferences.base_url)
            .map_err(|_| "无法访问钥匙串")?;
        match entry.delete_credential() {
            Ok(_) | Err(keyring::Error::NoEntry) => (),
            Err(_) => return Err("无法删除钥匙串中的 Key".into()),
        }
    }
    *state.key.lock().await = None;
    view.connected = false;
    view.online = false;
    view.snapshot = None;
    view.error.clear();
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
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
    method: String,
    path: String,
    body: Option<Value>,
) -> Result<Value, String> {
    if !allowed_request(&method, &path) {
        return Err("客户端不允许此操作".into());
    }
    let result;
    {
        let _gate = state.network.lock().await;
        let view = state.view.lock().await.clone();
        if !view.online {
            return Err("当前离线，请恢复连接后操作".into());
        }
        let key = state.key.lock().await.clone().ok_or("尚未连接")?;
        result = http(
            &state.client,
            &view.preferences.base_url,
            &key,
            &method,
            &path,
            body,
        )
        .await;
    }
    if method != "GET" {
        refresh_inner(&app, &state).await;
    }
    result
}

#[tauri::command]
async fn preferences(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
    favorites: Vec<i64>,
    pinned: bool,
    launch_at_login: bool,
) -> Result<(), String> {
    let mut view = state.view.lock().await;
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
    prefs.favorites = favorites
        .into_iter()
        .filter(|id| *id > 0)
        .take(100)
        .collect();
    prefs.favorites.sort_unstable();
    prefs.favorites.dedup();
    prefs.pinned = pinned;
    prefs.launch_at_login = launch_at_login;
    save_preferences(&state.path, &prefs)?;
    state.pinned.store(pinned, Ordering::Relaxed);
    view.preferences = prefs;
    publish(&app, &view);
    Ok(())
}

#[tauri::command]
fn show_main(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("main") {
        w.show().map_err(|_| "无法打开窗口")?;
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    app.state::<Arc<Runtime>>().wake.notify_one();
    Ok(())
}

#[tauri::command]
async fn check_updates(
    app: tauri::AppHandle,
    state: State<'_, Arc<Runtime>>,
) -> Result<String, String> {
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
    let releases: Value = response.json().await.map_err(|_| "版本信息无效")?;
    let tag = releases.as_array().and_then(|r| {
        r.iter().find_map(|item| {
            item.get("tag_name")
                .and_then(Value::as_str)
                .filter(|tag| tag.starts_with("desktop-v"))
        })
    });
    if let Some(tag) = tag {
        if tag.trim_start_matches("desktop-v") != env!("CARGO_PKG_VERSION") {
            app.opener()
                .open_url(RELEASES, None::<&str>)
                .map_err(|_| "无法打开发布页")?;
            return Ok(format!("发现 {tag}，已打开发布页"));
        }
    }
    Ok(format!("当前版本 {}，已是最新", env!("CARGO_PKG_VERSION")))
}

fn panel_bounds(
    anchor: tauri::PhysicalPosition<f64>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scale: f64,
) -> (tauri::PhysicalPosition<i32>, tauri::PhysicalSize<u32>) {
    let w = (420. * scale).min((width as f64 - 16.).max(1.));
    let h = (620. * scale).min((height as f64 - 16.).max(1.));
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

fn quick_panel(
    app: &tauri::AppHandle,
    anchor: tauri::PhysicalPosition<f64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let window = if let Some(window) = app.get_webview_window("quick") {
        window
    } else {
        WebviewWindowBuilder::new(
            app,
            "quick",
            WebviewUrl::App("index.html?panel=quick".into()),
        )
        .title("Sub2Ops 快捷面板")
        .inner_size(420., 620.)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .build()?
    };
    if window.is_visible()? {
        window.hide()?;
        return Ok(());
    }
    let monitors = window.available_monitors()?;
    let monitor = monitors.iter().find(|m| {
        let p = m.position();
        let s = m.size();
        anchor.x >= p.x as f64
            && anchor.x < p.x as f64 + s.width as f64
            && anchor.y >= p.y as f64
            && anchor.y < p.y as f64 + s.height as f64
    });
    if let Some(m) = monitor {
        let area = m.work_area();
        let (position, size) = panel_bounds(
            anchor,
            area.position.x,
            area.position.y,
            area.size.width,
            area.size.height,
            m.scale_factor(),
        );
        window.set_size(size)?;
        window.set_position(position)?;
    }
    window.show()?;
    window.set_focus()?;
    app.state::<Arc<Runtime>>().wake.notify_one();
    Ok(())
}

#[tauri::command]
fn show_quick(app: tauri::AppHandle) -> Result<(), String> {
    let rect = app
        .tray_by_id("ops")
        .and_then(|t| t.rect().ok().flatten())
        .ok_or("菜单栏图标暂不可用")?;
    quick_panel(&app, rect.position.to_physical::<f64>(1.0)).map_err(|_| "无法打开快捷面板".into())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| { let _ = show_main(app.clone()); }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::Builder::new().macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent).build())
        .setup(|app| {
            let path = app.path().app_config_dir()?.join("preferences.json");
            let prefs: Preferences = std::fs::read(&path).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default();
            let key = if prefs.base_url.is_empty() { None } else { keyring::Entry::new("com.lich13.sub2ops", &prefs.base_url).ok().and_then(|e| e.get_password().ok()) };
            let state = Arc::new(Runtime { client: reqwest::Client::builder().timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).build()?,
                pinned: AtomicBool::new(prefs.pinned), quitting: AtomicBool::new(false),
                view: Mutex::new(ViewState { connected: key.is_some(), preferences: prefs, ..Default::default() }),
                key: Mutex::new(key), network: Mutex::new(()), wake: Notify::new(), path });
            app.manage(state.clone());
            let menu = Menu::with_items(app, &[
                &MenuItem::with_id(app, "open", "打开 Sub2Ops", true, None::<&str>)?,
                &MenuItem::with_id(app, "updates", "检查更新", true, None::<&str>)?,
                &MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?,
            ])?;
            // Original monochrome pulse mark, drawn in memory for the macOS template icon.
            let mut pixels = vec![0u8; 22*22*4];
            for x in 2..20usize { let y = match x { 6..=8 => 11-(x-5)*2, 9..=12 => 5+(x-8)*3, 13..=15 => 17-(x-12)*2, _ => 11 }; for d in 0..2 { let i=((y+d).min(21)*22+x)*4; pixels[i+3]=255; } }
            TrayIconBuilder::with_id("ops").icon(tauri::image::Image::new_owned(pixels,22,22)).icon_as_template(true).tooltip("Sub2Ops").menu(&menu).show_menu_on_left_click(false)
                .on_tray_icon_event(|tray,event| { if let TrayIconEvent::Click {button:MouseButton::Left,button_state:MouseButtonState::Up,position,..}=event {let _=quick_panel(tray.app_handle(),position);} })
                .on_menu_event(|app,event| match event.id().as_ref() {
                    "open" => {let _=show_main(app.clone());},
                    "updates" => { let handle=app.clone(); tauri::async_runtime::spawn(async move { let result=check_updates(handle.clone(),handle.state()).await; let _=handle.emit("update-result",result.unwrap_or_else(|e|e)); let _=show_main(handle); }); },
                    "quit" => { app.state::<Arc<Runtime>>().quitting.store(true,Ordering::Relaxed); app.exit(0); }, _=>()
                }).build(app)?;
            let handle=app.handle().clone();
            tauri::async_runtime::spawn(async move { loop {
                refresh_inner(&handle,&state).await;
                let visible=["main","quick"].iter().any(|label| handle.get_webview_window(label).is_some_and(|w|w.is_visible().unwrap_or(false)));
                // Wall time advances through macOS sleep. Check locally once a
                // second so wake does not wait out a suspended 15-second timer.
                let due=std::time::SystemTime::now()+Duration::from_secs(if visible {2} else {15});
                loop {
                    tokio::select! { _=tokio::time::sleep(Duration::from_secs(1))=>(), _=state.wake.notified()=>break }
                    if std::time::SystemTime::now()>=due {break;}
                }
            }});
            Ok(())
        })
        .on_window_event(|window,event| {
            let state=window.state::<Arc<Runtime>>();
            match event {
                WindowEvent::CloseRequested {api,..} if !state.quitting.load(Ordering::Relaxed) => {api.prevent_close();let _=window.hide();},
                WindowEvent::Focused(false) if window.label()=="quick" && !state.pinned.load(Ordering::Relaxed) => {let _=window.hide();},
                WindowEvent::Focused(true) => state.wake.notify_one(), _=>()
            }
        })
        .invoke_handler(tauri::generate_handler![get_state,connect,disconnect,refresh,api_request,preferences,show_main,show_quick,check_updates])
        .build(tauri::generate_context!()).expect("Sub2Ops failed to start")
        .run(|app,event| {
            if let tauri::RunEvent::Reopen {..}=event {let _=show_main(app.clone());}
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panel_stays_inside_secondary_and_scaled_displays() {
        for (x, y, w, h, scale, ax, ay) in [
            (-1920, 24, 1920, 1056, 1., -8., 0.),
            (0, 50, 2560, 1614, 2., 2540., 20.),
            (0, -900, 800, 850, 2., 200., -900.),
        ] {
            let (p, s) = panel_bounds(tauri::PhysicalPosition::new(ax, ay), x, y, w, h, scale);
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
        assert!(allowed_request("POST", "/accounts/7/schedulable"));
        assert!(allowed_request("GET", "/errors?account_id=7"));
        for p in ["/accounts/7/test", "https://evil.test", "/config/../token"] {
            assert!(!allowed_request("POST", p));
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
}
