use crate::{ModelTestPreference, Runtime, ViewState};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
};
use tauri::Manager;

pub(crate) const EVENT_OPS_STATE: u32 = 1;
pub(crate) const EVENT_UPDATE_RESULT: u32 = 2;
pub(crate) const EVENT_TRAY_TOOLTIP: u32 = 3;
pub(crate) const EVENT_REOPEN: u32 = 4;
pub(crate) const EVENT_OBSERVED_AT: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Sub2OpsHostCallbacks {
    pub invoke: extern "C" fn(
        request_id: u64,
        cmd_ptr: *const u8,
        cmd_len: usize,
        payload_ptr: *const u8,
        payload_len: usize,
    ),
    pub set_visibility: extern "C" fn(
        main_visible: bool,
        main_minimized: bool,
        quick_visible: bool,
        app_active: bool,
    ),
    pub panel_trace: extern "C" fn(
        stage_ptr: *const u8,
        stage_len: usize,
        detail_ptr: *const u8,
        detail_len: usize,
    ),
    pub quit: extern "C" fn(),
}

extern "C" {
    fn sub2ops_macos_init(
        invoke: extern "C" fn(
            request_id: u64,
            cmd_ptr: *const u8,
            cmd_len: usize,
            payload_ptr: *const u8,
            payload_len: usize,
        ),
        set_visibility: extern "C" fn(
            main_visible: bool,
            main_minimized: bool,
            quick_visible: bool,
            app_active: bool,
        ),
        panel_trace: extern "C" fn(
            stage_ptr: *const u8,
            stage_len: usize,
            detail_ptr: *const u8,
            detail_len: usize,
        ),
        quit: extern "C" fn(),
    );
    fn sub2ops_macos_on_response(request_id: u64, ok: bool, ptr: *const u8, len: usize);
    fn sub2ops_macos_on_event(kind: u32, ptr: *const u8, len: usize);
    fn sub2ops_macos_app_active() -> bool;
}

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();
static MAIN_VISIBLE: AtomicBool = AtomicBool::new(false);
static MAIN_MINIMIZED: AtomicBool = AtomicBool::new(false);
static QUICK_VISIBLE: AtomicBool = AtomicBool::new(false);

pub(crate) fn init(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
    let callbacks = Sub2OpsHostCallbacks {
        invoke: host_invoke,
        set_visibility: host_set_visibility,
        panel_trace: host_panel_trace,
        quit: host_quit,
    };
    // SAFETY: All callbacks have the declared C ABI and live for the process lifetime.
    unsafe {
        sub2ops_macos_init(
            callbacks.invoke,
            callbacks.set_visibility,
            callbacks.panel_trace,
            callbacks.quit,
        );
    }
}

pub(crate) fn application_active() -> bool {
    // SAFETY: The Swift entry point accepts no arguments and synchronously returns a Bool.
    unsafe { sub2ops_macos_app_active() }
}

pub(crate) fn window_visibility() -> (bool, bool, bool) {
    (
        MAIN_VISIBLE.load(Ordering::Acquire),
        MAIN_MINIMIZED.load(Ordering::Acquire),
        QUICK_VISIBLE.load(Ordering::Acquire),
    )
}

pub(crate) fn any_window_visible() -> bool {
    MAIN_VISIBLE.load(Ordering::Acquire) || QUICK_VISIBLE.load(Ordering::Acquire)
}

unsafe fn copy_utf8(ptr: *const u8, len: usize) -> Result<String, String> {
    if len == 0 {
        return Ok(String::new());
    }
    if ptr.is_null() {
        return Err("参数指针无效".into());
    }
    // SAFETY: Swift owns these len bytes for the duration of this callback; copy before returning.
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(slice)
        .map(str::to_owned)
        .map_err(|_| "UTF-8 参数编码无效".into())
}

fn send_failure(request_id: u64, error: &crate::HttpFailure) {
    let json = serde_json::to_string(error).expect("bridge failure is serializable");
    send_response(request_id, false, &json);
}

fn send_response(request_id: u64, ok: bool, body: &str) {
    let bytes = body.as_bytes();
    let ptr = if bytes.is_empty() {
        NonNull::<u8>::dangling().as_ptr()
    } else {
        bytes.as_ptr()
    };
    // SAFETY: Swift consumes the buffer synchronously while bytes remains borrowed.
    unsafe {
        sub2ops_macos_on_response(request_id, ok, ptr, bytes.len());
    }
}

pub(crate) fn send_event(kind: u32, payload: &str) {
    let bytes = payload.as_bytes();
    let ptr = if bytes.is_empty() {
        NonNull::<u8>::dangling().as_ptr()
    } else {
        bytes.as_ptr()
    };
    // SAFETY: Swift consumes the buffer synchronously while bytes remains borrowed.
    unsafe {
        sub2ops_macos_on_event(kind, ptr, bytes.len());
    }
}

pub(crate) fn publish_state(value: &ViewState) {
    if !any_window_visible() && !value.initializing {
        return;
    }
    if let Ok(json) = serde_json::to_string(value) {
        send_event(EVENT_OPS_STATE, &json);
    }
}

pub(crate) fn publish_observed_at(observed_at: &str) {
    if !any_window_visible() || observed_at.is_empty() {
        return;
    }
    send_event(EVENT_OBSERVED_AT, observed_at);
}

pub(crate) fn emit_update_result(message: &str) {
    send_event(EVENT_UPDATE_RESULT, message);
}

pub(crate) fn emit_reopen() {
    send_event(EVENT_REOPEN, "");
}

pub(crate) fn tray_tooltip_text(view: &ViewState) -> &'static str {
    if view.online {
        "Sub2Ops · 已连接"
    } else {
        match view
            .upstream_connection
            .as_ref()
            .and_then(|c| c["state"].as_str())
        {
            Some("auth_rejected") => "Sub2Ops · 管理员 Key 被拒绝",
            Some("network_unreachable") => "Sub2Ops · 上游不可达",
            Some("route_not_found") => "Sub2Ops · 管理接口路径不兼容",
            Some("protocol_mismatch") => "Sub2Ops · 管理接口响应不兼容",
            _ => "Sub2Ops · 离线",
        }
    }
}

static LAST_TOOLTIP: std::sync::Mutex<&'static str> = std::sync::Mutex::new("");

pub(crate) fn publish_tooltip(view: &ViewState) {
    let next = tray_tooltip_text(view);
    let mut last = LAST_TOOLTIP.lock().unwrap_or_else(|e| e.into_inner());
    if *last == next {
        return;
    }
    *last = next;
    drop(last);
    send_event(EVENT_TRAY_TOOLTIP, next);
}

fn trace_safe(stage: &str, detail: &str) {
    if let Some(app) = APP_HANDLE.get() {
        crate::panel_trace(app, stage, detail);
    }
}

#[derive(Deserialize)]
struct ConnectArgs {
    #[serde(alias = "baseUrl")]
    base_url: String,
    #[serde(default, alias = "apiKey")]
    api_key: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct SaveProfileArgs {
    #[serde(alias = "baseUrl")]
    base_url: String,
    name: String,
}

#[derive(Deserialize)]
struct RemoveConnectionArgs {
    #[serde(alias = "baseUrl")]
    base_url: String,
}

#[derive(Deserialize)]
struct ApiRequestArgs {
    method: String,
    path: String,
    #[serde(default)]
    body: Option<Value>,
}

#[derive(Deserialize)]
struct PreferencesArgs {
    #[serde(default)]
    pinned: Option<bool>,
    #[serde(default, alias = "launchAtLogin")]
    launch_at_login: Option<bool>,
    #[serde(default, alias = "recordColumns")]
    record_columns: Option<Vec<String>>,
    #[serde(default, alias = "modelTestConcurrency")]
    model_test_concurrency: Option<u8>,
    #[serde(default, alias = "modelTestPreferences")]
    model_test_preferences: Option<BTreeMap<String, ModelTestPreference>>,
}

#[derive(Deserialize)]
struct UrlArgs {
    url: String,
}

fn parse_args<T: for<'de> Deserialize<'de>>(payload: &str) -> Result<T, String> {
    let normalized = if payload.trim().is_empty() {
        "{}"
    } else {
        payload
    };
    serde_json::from_str(normalized).map_err(|_| "请求参数格式无效".into())
}

async fn dispatch_command(
    app: tauri::AppHandle,
    cmd: String,
    payload: String,
) -> Result<String, crate::HttpFailure> {
    match cmd.as_str() {
        "get_state" => {
            let value = crate::get_state(app.state::<Arc<Runtime>>()).await?;
            serde_json::to_string(&value).map_err(|_| "状态序列化失败".into())
        }
        "connect" => {
            let args: ConnectArgs = parse_args(&payload)?;
            crate::connect(
                app.clone(),
                app.state::<Arc<Runtime>>(),
                args.base_url,
                args.api_key,
                args.name,
            )
            .await?;
            Ok("null".into())
        }
        "save_connection_profile" => {
            let args: SaveProfileArgs = parse_args(&payload)?;
            crate::save_connection_profile(
                app.clone(),
                app.state::<Arc<Runtime>>(),
                args.base_url,
                args.name,
            )
            .await?;
            Ok("null".into())
        }
        "remove_connection" => {
            let args: RemoveConnectionArgs = parse_args(&payload)?;
            crate::remove_connection(app.clone(), app.state::<Arc<Runtime>>(), args.base_url)
                .await?;
            Ok("null".into())
        }
        "disconnect" => {
            crate::disconnect(app.clone(), app.state::<Arc<Runtime>>()).await?;
            Ok("null".into())
        }
        "refresh" => {
            crate::refresh(app.state::<Arc<Runtime>>()).await?;
            Ok("null".into())
        }
        "api_request" => {
            let args: ApiRequestArgs = parse_args(&payload)?;
            let value = crate::api_request(
                app.state::<Arc<Runtime>>(),
                args.method,
                args.path,
                args.body,
            )
            .await?;
            serde_json::to_string(&value).map_err(|_| "响应序列化失败".into())
        }
        "preferences" => {
            let args: PreferencesArgs = parse_args(&payload)?;
            crate::preferences(
                app.clone(),
                app.state::<Arc<Runtime>>(),
                args.pinned,
                args.launch_at_login,
                args.record_columns,
                args.model_test_concurrency,
                args.model_test_preferences,
            )
            .await?;
            Ok("null".into())
        }
        "check_updates" => {
            let message = crate::check_updates(app.clone(), app.state::<Arc<Runtime>>()).await?;
            emit_update_result(&message);
            serde_json::to_string(&message).map_err(|_| "结果序列化失败".into())
        }
        "open_authorization_url" => {
            let args: UrlArgs = parse_args(&payload)?;
            crate::open_authorization_url(app, args.url).await?;
            Ok("null".into())
        }
        "open_sub2api_release" => {
            let args: UrlArgs = parse_args(&payload)?;
            crate::open_sub2api_release(app, args.url).await?;
            Ok("null".into())
        }
        _ => Err("未知的客户端命令".into()),
    }
}

struct CatchUnwindFuture<F>(F);

impl<F: std::future::Future> std::future::Future for CatchUnwindFuture<F> {
    type Output = std::thread::Result<F::Output>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // SAFETY: Field projection without moving `F`.
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        match catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
            Ok(std::task::Poll::Ready(val)) => std::task::Poll::Ready(Ok(val)),
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Err(err) => std::task::Poll::Ready(Err(err)),
        }
    }
}

extern "C" fn host_invoke(
    request_id: u64,
    cmd_ptr: *const u8,
    cmd_len: usize,
    payload_ptr: *const u8,
    payload_len: usize,
) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: The Swift caller lends this buffer until host_invoke returns.
        let cmd = match unsafe { copy_utf8(cmd_ptr, cmd_len) } {
            Ok(s) => s,
            Err(err) => {
                send_failure(request_id, &err.into());
                return;
            }
        };
        // SAFETY: The Swift caller lends this buffer until host_invoke returns.
        let payload = match unsafe { copy_utf8(payload_ptr, payload_len) } {
            Ok(s) => s,
            Err(err) => {
                send_failure(request_id, &err.into());
                return;
            }
        };
        let Some(app) = APP_HANDLE.get().cloned() else {
            send_failure(request_id, &"宿主尚未完成初始化".into());
            return;
        };
        tauri::async_runtime::spawn(async move {
            match CatchUnwindFuture(dispatch_command(app, cmd, payload)).await {
                Ok(Ok(json)) => send_response(request_id, true, &json),
                Ok(Err(error)) => send_failure(request_id, &error),
                Err(_) => {
                    trace_safe("bridge_panic", "async_command");
                    send_failure(request_id, &"内部运行异常".into());
                }
            }
        });
    }));
    if outcome.is_err() {
        trace_safe("bridge_panic", "sync_invoke");
        send_failure(request_id, &"内部运行异常".into());
    }
}

extern "C" fn host_set_visibility(
    main_visible: bool,
    main_minimized: bool,
    quick_visible: bool,
    app_active: bool,
) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let was_main = MAIN_VISIBLE.swap(main_visible, Ordering::Release);
        MAIN_MINIMIZED.store(main_minimized, Ordering::Release);
        let was_quick = QUICK_VISIBLE.swap(quick_visible, Ordering::Release);

        let Some(app) = APP_HANDLE.get().cloned() else {
            return;
        };
        let state = app.state::<Arc<Runtime>>().inner().clone();
        let activity_changed = state.record_activity.set_active(app_active);
        let newly_visible = (main_visible && !was_main) || (quick_visible && !was_quick);

        if newly_visible || (activity_changed && app_active) {
            state.wake.notify_one();
        }
        if newly_visible || activity_changed {
            tauri::async_runtime::spawn(async move {
                let mut view = state.view.lock().await;
                view.foreground = state.record_activity.active();
                if let Ok(json) = serde_json::to_string(&*view) {
                    send_event(EVENT_OPS_STATE, &json);
                }
            });
        }
    }));
    if outcome.is_err() {
        trace_safe("bridge_panic", "set_visibility");
    }
}

extern "C" fn host_panel_trace(
    stage_ptr: *const u8,
    stage_len: usize,
    detail_ptr: *const u8,
    detail_len: usize,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: The Swift caller lends this buffer for this synchronous callback.
        let Ok(stage) = (unsafe { copy_utf8(stage_ptr, stage_len) }) else {
            return;
        };
        // SAFETY: The Swift caller lends this buffer for this synchronous callback.
        let Ok(detail) = (unsafe { copy_utf8(detail_ptr, detail_len) }) else {
            return;
        };
        trace_safe(&stage, &detail);
    }));
}

extern "C" fn host_quit() {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        trace_safe("quit", "menu");
        if let Some(app) = APP_HANDLE.get() {
            app.exit(0);
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_length_slice_copies_safely_even_with_null_pointer() {
        assert_eq!(unsafe { copy_utf8(std::ptr::null(), 0) }.unwrap(), "");
        assert!(unsafe { copy_utf8(std::ptr::null(), 4) }.is_err());
    }

    #[test]
    fn tooltip_reflects_online_and_upstream_diagnostic_states() {
        let mut view = ViewState::default();
        assert_eq!(tray_tooltip_text(&view), "Sub2Ops · 离线");
        view.online = true;
        assert_eq!(tray_tooltip_text(&view), "Sub2Ops · 已连接");
        view.online = false;
        view.upstream_connection = Some(serde_json::json!({"state": "auth_rejected"}));
        assert_eq!(tray_tooltip_text(&view), "Sub2Ops · 管理员 Key 被拒绝");
    }
}
