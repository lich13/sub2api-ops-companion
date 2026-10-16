use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{json, Value};
use sqlx::{types::Json as SqlJson, PgPool};
use std::{
    fs,
    ops::Deref,
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration as StdDuration, Instant},
};
use sub2ops_server::{
    compat::{account_version, datetime, integer, parse_datetime},
    daily_test::{self, Schedule},
    db::Database,
    oauth_native,
    oauth_queries::{Coordinator, QueryOptions, Source},
    oauth_state::OAuthState,
    quota,
    runtime::Runtime,
    settings::{daily_time, Settings},
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

const ADMIN_KEY: &str = "fixture-admin-oauth-scheduler";
const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(9_600_000);

#[cfg(target_os = "linux")]
mod state_read_watch {
    use std::{
        ffi::CString,
        io,
        os::raw::{c_char, c_int, c_void},
        os::unix::ffi::OsStrExt,
        path::Path,
    };

    const IN_NONBLOCK: c_int = 0x800;
    const IN_CLOEXEC: c_int = 0x80000;
    const IN_ACCESS: u32 = 0x0000_0001;
    const IN_OPEN: u32 = 0x0000_0020;

    #[link(name = "c")]
    unsafe extern "C" {
        fn inotify_init1(flags: c_int) -> c_int;
        fn inotify_add_watch(fd: c_int, pathname: *const c_char, mask: u32) -> c_int;
        fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
        fn close(fd: c_int) -> c_int;
    }

    pub struct Watcher {
        fd: c_int,
    }

    impl Watcher {
        pub fn new(path: &Path) -> io::Result<Self> {
            let path = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "state path has NUL"))?;
            let fd = unsafe { inotify_init1(IN_NONBLOCK | IN_CLOEXEC) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { inotify_add_watch(fd, path.as_ptr(), IN_OPEN | IN_ACCESS) } < 0 {
                let error = io::Error::last_os_error();
                unsafe {
                    close(fd);
                }
                return Err(error);
            }
            Ok(Self { fd })
        }

        pub fn assert_no_access(&self) -> io::Result<()> {
            let mut events = [0_u8; 4096];
            let count = unsafe { read(self.fd, events.as_mut_ptr().cast(), events.len()) };
            if count > 0 {
                return Err(io::Error::other("OAuth state file was opened or read"));
            }
            if count == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                Ok(())
            } else {
                Err(error)
            }
        }
    }

    impl Drop for Watcher {
        fn drop(&mut self) {
            unsafe {
                close(self.fd);
            }
        }
    }
}

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/contracts/oauth-scheduler/fixtures.json"
    ))
    .expect("pinned OAuth scheduler fixture must parse")
}

fn reference_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 29, 20, 0, 0).unwrap()
}

fn method_name(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::DELETE => "DELETE",
        _ => "OTHER",
    }
}

#[derive(Clone)]
struct Gate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }
}

#[derive(Clone)]
struct Call {
    method: String,
    path: String,
    api_key: Option<String>,
    authorization: Option<String>,
    body: Value,
}

#[derive(Clone)]
struct MockUpstream {
    pool: PgPool,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    calls: Arc<StdMutex<Vec<Call>>>,
    unexpected: Arc<StdMutex<Vec<String>>>,
    usage_status: Arc<StdMutex<u16>>,
    usage_data: Arc<StdMutex<Option<Value>>>,
    usage_raw: Arc<StdMutex<Option<Vec<u8>>>>,
    usage_gate: Arc<StdMutex<Option<Gate>>>,
    test_sse: Arc<StdMutex<String>>,
    test_gate: Arc<StdMutex<Option<Gate>>>,
    mark_unschedulable_on_test: Arc<StdMutex<Option<i64>>>,
    clear_block_on_test: Arc<StdMutex<Option<i64>>>,
    schedulable_gate: Arc<StdMutex<Option<Gate>>>,
    recovery_status: Arc<StdMutex<u16>>,
    clear_status: Arc<StdMutex<u16>>,
    temp_status: Arc<StdMutex<u16>>,
    readonly_after_usage: Arc<StdMutex<Option<PathBuf>>>,
}

impl MockUpstream {
    fn new(pool: PgPool, clock: Arc<StdMutex<DateTime<Utc>>>) -> Self {
        Self {
            pool,
            clock,
            calls: Arc::new(StdMutex::new(Vec::new())),
            unexpected: Arc::new(StdMutex::new(Vec::new())),
            usage_status: Arc::new(StdMutex::new(200)),
            usage_data: Arc::new(StdMutex::new(None)),
            usage_raw: Arc::new(StdMutex::new(None)),
            usage_gate: Arc::new(StdMutex::new(None)),
            test_sse: Arc::new(StdMutex::new(
                "data: {\"type\":\"test_start\",\"model\":\"gpt-5.6-luna\"}\n\ndata: {\"type\":\"test_complete\",\"success\":true}\n\n".into(),
            )),
            test_gate: Arc::new(StdMutex::new(None)),
            mark_unschedulable_on_test: Arc::new(StdMutex::new(None)),
            clear_block_on_test: Arc::new(StdMutex::new(None)),
            schedulable_gate: Arc::new(StdMutex::new(None)),
            recovery_status: Arc::new(StdMutex::new(200)),
            clear_status: Arc::new(StdMutex::new(200)),
            temp_status: Arc::new(StdMutex::new(200)),
            readonly_after_usage: Arc::new(StdMutex::new(None)),
        }
    }

    fn count(&self, method: &str, suffix: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| {
                call.method == method
                    && call
                        .path
                        .split('?')
                        .next()
                        .unwrap_or(&call.path)
                        .ends_with(suffix)
            })
            .count()
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn unexpected(&self) -> Vec<String> {
        self.unexpected.lock().unwrap().clone()
    }

    fn set_usage(&self, status: u16, data: Option<Value>) {
        *self.usage_status.lock().unwrap() = status;
        *self.usage_data.lock().unwrap() = data;
        *self.usage_raw.lock().unwrap() = None;
    }

    fn set_usage_raw(&self, status: u16, body: Vec<u8>) {
        *self.usage_status.lock().unwrap() = status;
        *self.usage_raw.lock().unwrap() = Some(body);
    }

    fn set_test_sse(&self, sse: impl Into<String>) {
        *self.test_sse.lock().unwrap() = sse.into();
    }

    fn mark_unschedulable_on_test(&self, id: i64) {
        *self.mark_unschedulable_on_test.lock().unwrap() = Some(id);
    }

    fn clear_block_on_test(&self, id: i64) {
        *self.clear_block_on_test.lock().unwrap() = Some(id);
    }

    async fn wait_for(&self, method: &str, suffix: &str, expected: usize) {
        tokio::time::timeout(StdDuration::from_secs(8), async {
            loop {
                if self.count(method, suffix) >= expected {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("loopback scheduler mock did not receive the expected request");
    }
}

fn default_usage(at: DateTime<Utc>) -> Value {
    json!({
        "five_hour": {
            "utilization": 15,
            "resets_at": datetime(at + Duration::hours(5)),
            "remaining_seconds": 18000,
            "window_minutes": 300
        },
        "seven_day": {
            "utilization": 25,
            "resets_at": datetime(at + Duration::days(7)),
            "remaining_seconds": 604800,
            "window_minutes": 10080
        }
    })
}

fn json_response(status: u16, value: Value) -> Response {
    (
        StatusCode::from_u16(status).expect("fixture HTTP status"),
        Json(value),
    )
        .into_response()
}

fn account_id(path: &str, suffix: &str) -> Option<i64> {
    path.strip_prefix("/api/v1/admin/accounts/")?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

async fn mock_upstream(State(state): State<MockUpstream>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let method = parts.method;
    let path = parts.uri.path().to_owned();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str().to_owned())
        .unwrap_or_else(|| path.clone());
    let headers = parts.headers;
    let bytes = to_bytes(body, 2_000_000).await.unwrap_or_default();
    let payload = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    state.calls.lock().unwrap().push(Call {
        method: method_name(&method).into(),
        path: path_and_query,
        api_key: headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body: payload.clone(),
    });

    if method == Method::GET && path.ends_with("/usage") {
        let readonly_directory = { state.readonly_after_usage.lock().unwrap().take() };
        if let Some(directory) = readonly_directory {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = fs::metadata(&directory)
                    .expect("synthetic state directory exists")
                    .permissions();
                permissions.set_mode(0o500);
                fs::set_permissions(directory, permissions)
                    .expect("make only the synthetic state directory read only");
            }
        }
        let gate = { state.usage_gate.lock().unwrap().clone() };
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let status = *state.usage_status.lock().unwrap();
        if status != 200 {
            return json_response(status, json!({"code":1,"message":"fixture usage failure"}));
        }
        if let Some(body) = state.usage_raw.lock().unwrap().clone() {
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("fixture raw usage response");
        }
        let data = state
            .usage_data
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| default_usage(*state.clock.lock().unwrap()));
        return json_response(200, json!({"code":0,"data":data}));
    }

    if method == Method::POST && path.ends_with("/test") {
        let gate = { state.test_gate.lock().unwrap().clone() };
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if account_id(&path, "/test")
            .is_some_and(|id| *state.mark_unschedulable_on_test.lock().unwrap() == Some(id))
        {
            let id = account_id(&path, "/test").unwrap();
            let now = *state.clock.lock().unwrap();
            sqlx::query("UPDATE accounts SET schedulable=false,updated_at=$1 WHERE id=$2")
                .bind(now)
                .bind(id)
                .execute(&state.pool)
                .await
                .expect("change only the synthetic account during the loopback test response");
            *state.mark_unschedulable_on_test.lock().unwrap() = None;
        }
        if account_id(&path, "/test")
            .is_some_and(|id| *state.clear_block_on_test.lock().unwrap() == Some(id))
        {
            let id = account_id(&path, "/test").unwrap();
            let now = *state.clock.lock().unwrap();
            sqlx::query("UPDATE accounts SET rate_limited_at=NULL,rate_limit_reset_at=NULL,overload_until=NULL,temp_unschedulable_until=NULL,temp_unschedulable_reason=NULL,updated_at=$1 WHERE id=$2")
                .bind(now)
                .bind(id)
                .execute(&state.pool)
                .await
                .expect("self-heal only the synthetic account after a successful loopback test");
            *state.clear_block_on_test.lock().unwrap() = None;
        }
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(state.test_sse.lock().unwrap().clone()))
            .expect("fixture test SSE response");
    }

    if method == Method::POST && path.ends_with("/recover-state") {
        let status = *state.recovery_status.lock().unwrap();
        if status == 200 {
            if let Some(id) = account_id(&path, "/recover-state") {
                let now = *state.clock.lock().unwrap();
                sqlx::query("UPDATE accounts SET rate_limited_at=NULL,rate_limit_reset_at=NULL,overload_until=NULL,temp_unschedulable_until=NULL,temp_unschedulable_reason=NULL,updated_at=$1 WHERE id=$2")
                    .bind(now)
                    .bind(id)
                    .execute(&state.pool)
                    .await
                    .expect("apply synthetic recovery only to temporary PostgreSQL");
            }
        }
        return json_response(status, json!({"code":if status == 200 {0} else {1}}));
    }

    if method == Method::POST && path.ends_with("/clear-rate-limit") {
        return json_response(*state.clear_status.lock().unwrap(), json!({"code":0}));
    }

    if method == Method::DELETE && path.ends_with("/temp-unschedulable") {
        return json_response(*state.temp_status.lock().unwrap(), json!({"code":0}));
    }

    if method == Method::POST && path.ends_with("/schedulable") {
        let gate = { state.schedulable_gate.lock().unwrap().clone() };
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let Some(id) = account_id(&path, "/schedulable") else {
            return json_response(404, json!({"code":1}));
        };
        let schedulable = payload["schedulable"].as_bool().unwrap_or(false);
        let now = *state.clock.lock().unwrap();
        sqlx::query("UPDATE accounts SET schedulable=$1,updated_at=$2 WHERE id=$3")
            .bind(schedulable)
            .bind(now)
            .bind(id)
            .execute(&state.pool)
            .await
            .expect("apply synthetic key fallback only to temporary PostgreSQL");
        return json_response(
            200,
            json!({"code":0,"data":{"id":id,"schedulable":schedulable}}),
        );
    }

    if method == Method::POST && path == "/push" {
        return json_response(200, json!({"code":200,"data":"fixture-delivered"}));
    }

    state
        .unexpected
        .lock()
        .unwrap()
        .push(format!("{} {}", method_name(&method), path));
    json_response(404, json!({"code":404,"message":"fixture route missing"}))
}

struct Harness {
    _temp: tempfile::TempDir,
    runtime: RuntimeHandle,
    mock: MockUpstream,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    account_ids: Arc<StdMutex<Vec<i64>>>,
    restore_http_fixture_account_4001: bool,
    server: Option<JoinHandle<()>>,
}

struct RuntimeHandle(Option<Arc<Runtime>>);

impl Deref for RuntimeHandle {
    type Target = Runtime;

    fn deref(&self) -> &Self::Target {
        self.0.as_deref().expect("harness runtime is available")
    }
}

impl Harness {
    async fn new() -> Self {
        let database_url = std::env::var("DATABASE_URL")
            .expect("scheduler contract runner must supply temporary PostgreSQL");
        let database = Database::open(&database_url, false)
            .await
            .expect("open runner owned PostgreSQL for loopback fixtures");
        let restore_http_fixture_account_4001: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=4001 AND name='fixture-account-040' AND platform='openai' AND type='oauth' AND deleted_at IS NULL)",
        )
        .fetch_one(&database.pool)
        .await
        .expect("inspect only the HTTP runner's synthetic account fixture");
        if restore_http_fixture_account_4001 {
            sqlx::query(
                "UPDATE accounts SET deleted_at=now() WHERE id=4001 AND name='fixture-account-040' AND platform='openai' AND type='oauth' AND deleted_at IS NULL",
            )
            .execute(&database.pool)
            .await
            .expect("hide the unrelated HTTP fixture from OAuth scheduler inventories");
        }
        sqlx::query("DELETE FROM accounts WHERE id >= 9600000 AND id < 10000000")
            .execute(&database.pool)
            .await
            .expect("clear only stale synthetic accounts from an earlier panicked test");
        let temp = tempfile::tempdir().expect("create synthetic OAuth scheduler state directory");
        let clock = Arc::new(StdMutex::new(reference_time()));
        let account_ids = Arc::new(StdMutex::new(Vec::new()));
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback only Sub2API fixture");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let mock = MockUpstream::new(database.pool.clone(), clock.clone());
        let server_state = mock.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .fallback(mock_upstream)
                    .with_state(server_state),
            )
            .await
            .expect("serve loopback OAuth scheduler fixture");
        });

        let connection_path = temp.path().join("sub2api-config.json");
        fs::write(
            &connection_path,
            serde_json::to_vec(&json!({"base_url":base_url,"verify_base_url":""})).unwrap(),
        )
        .expect("write synthetic loopback upstream config");
        let oauth = json!({
            "oauth_auto_reset_credit_enabled": false,
            "oauth_daily_test_enabled": false,
            "oauth_daily_test_time": "05:00",
            "oauth_early_probe_batch_size": 1,
            "oauth_recovery_connection_account_ids": [],
            "oauth_recovery_model_account_ids": [],
            "oauth_recovery_monitor_enabled": true,
            "oauth_recovery_test_concurrency": 1,
            "oauth_recovery_test_model_id": "gpt-5.6-luna",
            "oauth_usage_refresh_concurrency": 1
        });
        let settings = Settings {
            database_url,
            bind: "127.0.0.1:0".into(),
            base_path: String::new(),
            audit_path: temp.path().join("audit.jsonl"),
            usage_path: temp.path().join("oauth-state.json"),
            oauth_path: temp.path().join("oauth-config.json"),
            bark_path: temp.path().join("bark-config.json"),
            fallback_path: temp.path().join("key-fallback.json"),
            connection_path,
            model_path: temp.path().join("group-models.json"),
            keepalive_path: temp.path().join("keepalive-state.json"),
            oauth,
            bark: json!({"enabled":false,"device_key":"fixture-bark-only","server_url":base_url}),
            bark_valid: true,
            base_url: base_url.clone(),
            verify_url: String::new(),
            read_only: false,
        };
        let runtime_clock = clock.clone();
        let runtime =
            Runtime::new_with_clock(settings, Arc::new(move || *runtime_clock.lock().unwrap()))
                .await
                .expect("construct real Runtime with synthetic schema and deterministic clock");
        runtime
            .monitor
            .store
            .save_admin_token(ADMIN_KEY)
            .expect("save synthetic Sub2API admin key");
        drop(database);
        Self {
            _temp: temp,
            runtime: RuntimeHandle(Some(runtime)),
            mock,
            clock,
            account_ids,
            restore_http_fixture_account_4001,
            server: Some(server),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }

    fn set_now(&self, at: DateTime<Utc>) {
        *self.clock.lock().unwrap() = at;
    }

    async fn seed_account(&self, plan: &str, token: &str, extra: Value) -> Value {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        self.track_account_id(id);
        let now = self.now();
        sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,updated_at) VALUES ($1,$2,'openai','oauth','active',true,$3,$4,$5)")
            .bind(id)
            .bind(format!("fixture-oauth-scheduler-{id}"))
            .bind(SqlJson(json!({"plan_type":plan,"access_token":token,"refresh_token":"fixture-refresh-token"})))
            .bind(SqlJson(extra))
            .bind(now)
            .execute(&self.runtime.db.pool)
            .await
            .expect("insert synthetic OAuth account into temporary PostgreSQL");
        self.runtime
            .monitor
            .row(id)
            .await
            .expect("read synthetic account through Runtime query projection")
    }

    async fn seed_key(&self, schedulable: bool) -> Value {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        self.track_account_id(id);
        let now = self.now();
        sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,updated_at) VALUES ($1,$2,'openai','apikey','active',$3,$4,'{}'::jsonb,$5)")
            .bind(id)
            .bind(format!("fixture-oauth-scheduler-key-{id}"))
            .bind(schedulable)
            .bind(SqlJson(json!({"api_key":"fixture-key-material"})))
            .bind(now)
            .execute(&self.runtime.db.pool)
            .await
            .expect("insert synthetic API key into temporary PostgreSQL");
        self.runtime
            .monitor
            .row(id)
            .await
            .expect("read synthetic API key through Runtime query projection")
    }

    async fn enable_openai_fallback(&self, key_id: i64) {
        self.runtime
            .config
            .fallback
            .save(
                &json!({
                    "openai_enabled":true,
                    "grok_enabled":false,
                    "managed_account_ids":[key_id]
                }),
                "fixture-oauth-scheduler",
            )
            .await
            .expect("enable fallback only for the synthetic API key");
    }

    async fn stop_upstream(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
    }

    fn track_account_id(&self, id: i64) {
        self.account_ids.lock().unwrap().push(id);
    }

    async fn restart_runtime(&mut self) {
        let settings = (*self.runtime.settings).clone();
        let old = self.runtime.0.take().expect("old harness Runtime exists");
        old.close().await;
        drop(old);
        let clock = self.clock.clone();
        let runtime = Runtime::new_with_clock(settings, Arc::new(move || *clock.lock().unwrap()))
            .await
            .expect("restart Runtime against the same synthetic PostgreSQL and persisted state");
        self.runtime.0 = Some(runtime);
    }

    async fn close(mut self) {
        self.runtime.monitor.drain(5).await;
        let ids = { std::mem::take(&mut *self.account_ids.lock().unwrap()) };
        if !ids.is_empty() {
            sqlx::query("DELETE FROM accounts WHERE id = ANY($1)")
                .bind(ids)
                .execute(&self.runtime.db.pool)
                .await
                .expect("remove only this harness's synthetic PostgreSQL accounts");
        }
        if self.restore_http_fixture_account_4001 {
            sqlx::query(
                "UPDATE accounts SET deleted_at=NULL WHERE id=4001 AND name='fixture-account-040' AND platform='openai' AND type='oauth'",
            )
            .execute(&self.runtime.db.pool)
            .await
            .expect("restore the HTTP runner fixture after this isolated OAuth test");
        }
        self.runtime.close().await;
        self.stop_upstream().await;
    }

    async fn cleanup_after_runtime_close(mut self) {
        let database_url = std::env::var("DATABASE_URL")
            .expect("scheduler contract runner must supply temporary PostgreSQL");
        let database = Database::open(&database_url, false)
            .await
            .expect("reopen only runner-owned PostgreSQL after Runtime shutdown");
        let ids = { std::mem::take(&mut *self.account_ids.lock().unwrap()) };
        if !ids.is_empty() {
            sqlx::query("DELETE FROM accounts WHERE id = ANY($1)")
                .bind(ids)
                .execute(&database.pool)
                .await
                .expect("remove only this harness's synthetic PostgreSQL accounts");
        }
        if self.restore_http_fixture_account_4001 {
            sqlx::query(
                "UPDATE accounts SET deleted_at=NULL WHERE id=4001 AND name='fixture-account-040' AND platform='openai' AND type='oauth'",
            )
            .execute(&database.pool)
            .await
            .expect("restore the HTTP runner fixture after this isolated shutdown test");
        }
        database.close().await;
        self.stop_upstream().await;
    }

    fn after_restart_coordinator(&self) -> Coordinator {
        let mut actions = self.runtime.monitor.actions.clone();
        actions.queries.oauth = OAuthState::new(self.runtime.settings.usage_path.clone(), false);
        let clock = self.clock.clone();
        let capacity_clock = self.runtime.monitor.config.clone();
        let mut coordinator = Coordinator::new(
            actions,
            Arc::new(move || {
                capacity_clock
                    .current()
                    .ok()
                    .and_then(|value| value["oauth_usage_refresh_concurrency"].as_u64())
                    .unwrap_or(4) as usize
            }),
        );
        coordinator.clock = Arc::new(move || *clock.lock().unwrap());
        coordinator
    }
}

fn query_options(source: Source, at: DateTime<Utc>) -> QueryOptions {
    QueryOptions {
        source,
        reason: "oauth_scheduler_parity".into(),
        at,
        requested_at: None,
        timeout: 10,
        generation: None,
    }
}

fn has_no_automatic_attempts(quota_query: &Value) -> bool {
    match quota_query.get("automatic_attempts") {
        None | Some(Value::Null) => true,
        Some(attempts) => attempts.as_array().is_some_and(|items| items.is_empty()),
    }
}

fn force_refresh_diagnostic(report: &Value, state: &Value, id: i64) -> Value {
    let key = id.to_string();
    json!({
        "report": {
            "success": report["success"],
            "timed_out": report["timed_out"],
            "error_code": report["error_code"],
            "total_count": report["total_count"],
            "queried_count": report["queried_count"],
            "skipped_count": report["skipped_count"],
            "reused_count": report["reused_count"],
            "success_count": report["success_count"],
            "failure_count": report["failure_count"],
            "coalesced": report["coalesced"]
        },
        "quota_query": {
            "last_query_at": state["scheduler"][&key]["quota_query"]["last_query_at"],
            "automatic_attempts": state["scheduler"][&key]["quota_query"]["automatic_attempts"],
            "failure_count": state["scheduler"][&key]["quota_query"]["failure_count"],
            "retry_at": state["scheduler"][&key]["quota_query"]["retry_at"],
            "last_error_code": state["scheduler"][&key]["last_error_code"]
        },
        "saved_usage": {
            "success": state["oauth_results"][&key]["success"],
            "error_code": state["oauth_results"][&key]["error_code"],
            "queried_at": state["oauth_results"][&key]["queried_at"]
        }
    })
}

fn cached_result(row: &Value, at: DateTime<Utc>, data: Value) -> Value {
    let summary = quota::from_usage_data(&data, row, at);
    json!({
        "account_id":row["id"],
        "template_type":"oauth",
        "success":true,
        "queried_at":datetime(at),
        "data":data,
        "oauth_quota":summary,
        "source":"sub2api_admin_usage"
    })
}

async fn save_cached(harness: &Harness, id: i64, value: Value) {
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["oauth_results"][id.to_string()] = value;
            Ok(())
        })
        .expect("save synthetic quota result to runner owned state");
}

async fn manual_usage_payload(runtime: &Runtime, id: i64) -> Value {
    let row = runtime
        .actions
        .account(id, None)
        .await
        .expect("read the synthetic account before a Runtime usage action");
    json!({
        "action":"query_usage",
        "expected_version":account_version(&row)
    })
}

async fn wait_for_quota_batch_completion(harness: &Harness) -> Value {
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let progress = harness.runtime.quota_progress();
            if progress["status"] == "completed" {
                return progress;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("Runtime quota-refresh batch must reach a terminal progress state")
}

async fn prepare_expired_recovery(harness: &Harness, row: &Value, due: DateTime<Utc>) {
    let id = integer(&row["id"]).unwrap();
    let block_at = due - Duration::hours(3);
    let reset_at = due - Duration::seconds(60);
    sqlx::query(
        "UPDATE accounts SET rate_limited_at=$1,rate_limit_reset_at=$2,updated_at=$3 WHERE id=$4",
    )
    .bind(block_at)
    .bind(reset_at)
    .bind(harness.now())
    .bind(id)
    .execute(&harness.runtime.db.pool)
    .await
    .expect("seed an expired rate-limit block only in temporary PostgreSQL");

    let observed_at = due - Duration::hours(2);
    let mut depleted = default_usage(observed_at);
    depleted["five_hour"]["utilization"] = json!(100);
    depleted["five_hour"]["resets_at"] = json!(datetime(reset_at));
    save_cached(harness, id, cached_result(row, observed_at, depleted)).await;

    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_recovery_monitor_enabled"] = true.into();
            config["oauth_recovery_connection_account_ids"] = json!([id]);
            config["oauth_recovery_model_account_ids"] = json!([]);
            Ok(())
        })
        .expect("enable recovery only for the synthetic OAuth account");
}

async fn prepare_due_recovery_daily(harness: &Harness, row: &Value, due: DateTime<Utc>) {
    prepare_expired_recovery(harness, row, due).await;
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_daily_test_enabled"] = true.into();
            config["oauth_daily_test_time"] = "05:00".into();
            Ok(())
        })
        .expect("enable the daily schedule only for synthetic accounts");
    harness.set_now(due - Duration::hours(1));
    harness
        .runtime
        .monitor
        .startup()
        .await
        .expect("initialize daily schedule on the synthetic runtime clock");
    harness.set_now(due);
}

#[test]
fn fixed_reference_scope_and_daily_time_contract_are_pinned() {
    let reference = fixture();
    assert_eq!(reference["reference_sha"], REFERENCE_SHA);
    assert_eq!(reference["scope"]["oauth_queries"], 39);
    assert_eq!(reference["scope"]["daily_test"], 13);
    assert_eq!(reference["scope"]["oauth_monitor_runtime_http_state"], 32);
    assert_eq!(reference["scope"]["usage_query"], 1);
    assert_eq!(reference["scope"]["excluded_monitor"], 8);
    assert_eq!(reference["scope"]["key_fallback_snapshot_extra"], 3);
    assert_eq!(reference["scope"]["key_fallback_latest_attempt_extra"], 5);
    assert_eq!(reference["scope"]["key_fallback_grok_isolation_extra"], 1);
    assert_eq!(reference["scope"]["recovery_selection_runtime_extra"], 1);
    assert_eq!(reference["scope"]["previously_mapped_oauth_queries"], 2);
    assert_eq!(reference["scope"]["base_total"], 85);
    assert_eq!(reference["scope"]["total"], 95);
    assert_eq!(
        daily_test::next_time(reference_time(), "05:00").unwrap(),
        parse_datetime(&reference["daily"]["next_at_0500_beijing"]).unwrap()
    );
    assert_eq!(
        daily_test::next_time(
            parse_datetime(&reference["daily"]["before_0500_beijing"]).unwrap(),
            "05:00",
        )
        .unwrap(),
        parse_datetime(&reference["daily"]["due_0500_beijing"]).unwrap()
    );
    assert_eq!(
        daily_test::next_time(
            parse_datetime(&reference["daily"]["due_0500_beijing"]).unwrap(),
            "00:00",
        )
        .unwrap(),
        parse_datetime(&reference["daily"]["next_midnight_beijing"]).unwrap()
    );
    for invalid in ["5:00", "24:00", "00:60", "12:30:00", "", "abc"] {
        assert!(
            daily_time(&json!(invalid)).is_err(),
            "invalid time {invalid}"
        );
        assert!(daily_test::next_time(reference_time(), invalid).is_err());
    }
}

#[test]
fn legacy_oauth_state_migration_preserves_only_live_snapshots_and_retries_idempotently() {
    let temporary = tempfile::tempdir().expect("create a synthetic legacy OAuth state directory");
    let path = temporary.path().join("oauth-state.json");
    let legacy = json!({
        "version":2,
        "settings":{
            "sub2api_admin_token":" fixture-admin-migration ",
            "usage_query_enabled":true,
            "auto_query_interval_seconds":60
        },
        "results":{
            "7":{"template_type":"oauth","success":true,"queried_at":"2026-09-29T20:00:00+00:00"},
            "8":{"template_type":"apikey","success":true}
        },
        "oauth_results":{
            "7":{"template_type":"oauth","success":true,"queried_at":"2026-09-29T20:00:00+00:00"}
        },
        "scheduler":{
            "7":{
                "last_regular_at":"2026-09-29T19:00:00+00:00",
                "last_reason":"regular_refresh",
                "recovery_intent":{"status":"testing","tested_at":"2026-09-29T19:30:00+00:00","attempt_count":2,"fingerprint":"fixture-recovery"}
            },
            "8":{
                "last_7d_probe_at":"2026-09-29T19:00:00+00:00",
                "last_reason":"seven_day_probe",
                "recovery_intent":{"status":"deferred","deferred_until":"2026-09-30T19:00:00+00:00","next_retry_at":"2026-09-30T19:00:00+00:00","fingerprint":"fixture-deferred"}
            }
        },
        "pending_events":{"keep":{"account_id":7,"status":"failed"}},
        "configs":{"legacy_only":true}
    });
    fs::write(&path, serde_json::to_vec(&legacy).unwrap())
        .expect("write only the synthetic v2 OAuth state");

    let state = OAuthState::new(path.clone(), false);
    state
        .transaction(|_| Ok(()))
        .expect("persist the current OAuth state normalization");
    let migrated = state.snapshot().unwrap();
    assert_eq!(migrated["version"], 5);
    assert_eq!(state.admin_token().unwrap(), "fixture-admin-migration");
    assert_eq!(
        migrated["oauth_results"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["7"]
    );
    assert_eq!(
        migrated["scheduler"]["7"]["recovery_intent"]["status"],
        "retry"
    );
    assert_eq!(
        migrated["scheduler"]["7"]["recovery_intent"]["next_retry_at"],
        "2026-09-29T19:30:00+00:00"
    );
    assert!(migrated["scheduler"]["7"].get("last_regular_at").is_none());
    assert!(migrated["scheduler"]["7"].get("last_reason").is_none());
    assert_eq!(
        migrated["scheduler"]["8"]["recovery_intent"]["status"],
        "ready"
    );
    assert_eq!(
        migrated["scheduler"]["8"]["recovery_intent"]["deferred_until"],
        ""
    );
    assert_eq!(
        migrated["scheduler"]["8"]["recovery_intent"]["next_retry_at"],
        ""
    );
    assert!(migrated["scheduler"]["8"].get("last_7d_probe_at").is_none());
    assert!(migrated["scheduler"]["8"].get("last_reason").is_none());
    assert_eq!(migrated["pending_events"]["keep"]["status"], "failed");
    assert!(migrated.get("configs").is_none());
    assert!(migrated["settings"].get("usage_query_enabled").is_none());
    assert!(migrated["settings"]
        .get("auto_query_interval_seconds")
        .is_none());

    let first_write = fs::read(&path).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&first_write).unwrap(),
        migrated
    );
    state
        .transaction(|_| Ok(()))
        .expect("a second normalization remains safe");
    assert_eq!(fs::read(&path).unwrap(), first_write);
}

#[tokio::test]
async fn legacy_recovery_cleanup_failure_keeps_marker_unset_and_can_retry() {
    let harness = Harness::new().await;
    let guard_path = harness.runtime.settings.data_dir().join("guard-state.json");
    fs::create_dir(&guard_path).expect("only the temporary legacy guard path is obstructed");

    let failed = harness.runtime.migrate_legacy_recovery().await;
    assert!(failed.is_err());
    assert!(guard_path.is_dir());
    assert_ne!(
        harness.runtime.monitor.state().unwrap()["settings"]["legacy_recovery_cleanup_completed"],
        true,
        "failed cleanup must not commit its completion marker"
    );

    fs::remove_dir(&guard_path).expect("remove only the synthetic cleanup obstruction");
    harness
        .runtime
        .migrate_legacy_recovery()
        .await
        .expect("a later cleanup attempt can finish safely");
    harness
        .runtime
        .migrate_legacy_recovery()
        .await
        .expect("completed cleanup is idempotent");
    assert!(!guard_path.exists());
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["settings"]["legacy_recovery_cleanup_completed"],
        true
    );
    harness.close().await;
}

#[tokio::test]
async fn active_usage_endpoint_budget_cooldown_manual_priority_and_free_plan_match_reference() {
    let harness = Harness::new().await;
    let plus = harness
        .seed_account("plus", "fixture-access-v1", json!({}))
        .await;
    let id = integer(&plus["id"]).unwrap();
    let start = harness.now();
    let first = harness
        .runtime
        .monitor
        .queries
        .query(id, ADMIN_KEY, query_options(Source::Automatic, start))
        .await;
    assert_eq!(first["success"], true);
    let call = harness.mock.calls().remove(0);
    assert_eq!(call.method, "GET");
    assert_eq!(
        call.path,
        format!("/api/v1/admin/accounts/{id}/usage?source=active&force=true")
    );
    assert_eq!(call.api_key.as_deref(), Some(ADMIN_KEY));
    assert!(call.authorization.is_none());

    harness.set_now(start + Duration::seconds(3599));
    let blocked = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(blocked["error_code"], "query_cooldown");
    assert_eq!(harness.mock.count("GET", "/usage"), 1);

    harness.set_now(start + Duration::minutes(10));
    let manual = harness
        .runtime
        .monitor
        .queries
        .query(id, ADMIN_KEY, query_options(Source::Manual, harness.now()))
        .await;
    assert_eq!(manual["success"], true);
    let state = harness.runtime.monitor.state().unwrap();
    assert_eq!(
        state["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"],
        json!([datetime(start)])
    );
    assert_eq!(
        state["scheduler"][id.to_string()]["quota_query"]["last_source"],
        "manual"
    );
    harness.set_now(start + Duration::hours(1));
    let cooldown = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(cooldown["error_code"], "query_cooldown");
    assert_eq!(
        cooldown["next_query_at"],
        datetime(start + Duration::minutes(70))
    );
    harness.set_now(start + Duration::minutes(70));
    assert_eq!(
        harness
            .runtime
            .monitor
            .queries
            .query(
                id,
                ADMIN_KEY,
                query_options(Source::Automatic, harness.now()),
            )
            .await["success"],
        true
    );

    let free = harness
        .seed_account("free", "fixture-free-access", json!({}))
        .await;
    let free_id = integer(&free["id"]).unwrap();
    harness.mock.set_usage(
        200,
        Some(json!({
            "seven_day": {
                "utilization": 30,
                "resets_at": datetime(harness.now() + Duration::days(7)),
                "remaining_seconds": 604800,
                "window_minutes": 10080
            }
        })),
    );
    let result = harness
        .runtime
        .monitor
        .queries
        .query(
            free_id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(result["success"], true);
    assert_eq!(result["oauth_quota"]["plan_type"], "free");
    assert_eq!(
        result["oauth_quota"]["ui_windows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 4);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn rolling_automatic_budget_and_backoff_survive_state_reopen() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-budget-token", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let start = harness.now();
    for hour in 0..6 {
        let at = start + Duration::hours(hour);
        harness.set_now(at);
        let result = harness
            .runtime
            .monitor
            .queries
            .query(id, ADMIN_KEY, query_options(Source::Automatic, at))
            .await;
        assert_eq!(result["success"], true, "automatic request {hour}");
    }
    let reopened = harness.after_restart_coordinator();
    harness.set_now(start + Duration::hours(6));
    let blocked = reopened
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(blocked["error_code"], "query_budget");
    assert_eq!(
        blocked["next_query_at"],
        datetime(start + Duration::hours(24))
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 6);
    let automatic_attempts = harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]
        ["quota_query"]["automatic_attempts"]
        .clone();
    harness.set_now(start + Duration::hours(6) + Duration::minutes(1));
    let manual = reopened
        .query(id, ADMIN_KEY, query_options(Source::Manual, harness.now()))
        .await;
    assert_eq!(manual["success"], true);
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["quota_query"]
            ["automatic_attempts"],
        automatic_attempts
    );
    assert_eq!(
        reopened
            .query(
                id,
                ADMIN_KEY,
                query_options(Source::Automatic, harness.now()),
            )
            .await["error_code"],
        "query_budget"
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 7);
    harness.set_now(start + Duration::hours(24));
    assert_eq!(
        reopened
            .query(
                id,
                ADMIN_KEY,
                query_options(Source::Automatic, harness.now()),
            )
            .await["success"],
        true
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 8);
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["quota_query"]
            ["automatic_attempts"],
        json!([
            datetime(start + Duration::hours(1)),
            datetime(start + Duration::hours(2)),
            datetime(start + Duration::hours(3)),
            datetime(start + Duration::hours(4)),
            datetime(start + Duration::hours(5)),
            datetime(start + Duration::hours(24))
        ])
    );

    let failures = harness
        .seed_account("plus", "fixture-backoff-token", json!({}))
        .await;
    let failure_id = integer(&failures["id"]).unwrap();
    let failure_start = harness.now() + Duration::days(2);
    harness.mock.set_usage(503, None);
    let mut failure_coordinator = harness.after_restart_coordinator();
    for (index, hours) in [1_i64, 3, 6, 12, 12].into_iter().enumerate() {
        let at = if index == 0 {
            failure_start
        } else {
            harness.now()
        };
        harness.set_now(at);
        let failed = failure_coordinator
            .query(failure_id, ADMIN_KEY, query_options(Source::Automatic, at))
            .await;
        assert_eq!(failed["error_code"], "http_503");
        let retry_at = at + Duration::hours(hours);
        let state = failure_coordinator
            .actions
            .queries
            .oauth
            .snapshot()
            .unwrap();
        assert_eq!(
            state["scheduler"][failure_id.to_string()]["quota_query"]["retry_at"],
            datetime(retry_at)
        );
        harness.set_now(retry_at - Duration::seconds(1));
        let blocked = failure_coordinator
            .query(
                failure_id,
                ADMIN_KEY,
                query_options(Source::Automatic, harness.now()),
            )
            .await;
        assert_eq!(blocked["skipped"], true);
        assert_eq!(blocked["next_query_at"], datetime(retry_at));
        harness.set_now(retry_at);
        if index == 0 {
            // Re-open the state file after the first durable backoff receipt.
            failure_coordinator = harness.after_restart_coordinator();
        }
    }
    assert_eq!(harness.mock.count("GET", "/usage"), 13);
    harness.close().await;
}

#[tokio::test]
async fn future_full_quota_keeps_existing_backoff_until_an_available_response() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-full-backoff", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let start = harness.now();
    harness.mock.set_usage(503, None);
    for hour in [0_i64, 1, 4] {
        let at = start + Duration::hours(hour);
        harness.set_now(at);
        let failed = harness
            .runtime
            .monitor
            .queries
            .query(id, ADMIN_KEY, query_options(Source::Automatic, at))
            .await;
        assert_eq!(failed["error_code"], "http_503");
    }
    let retry_at = start + Duration::hours(10);
    let failure_state = harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]
        ["quota_query"]
        .clone();
    assert_eq!(failure_state["failure_count"], 3);
    assert_eq!(failure_state["retry_at"], datetime(retry_at));

    let reset = start + Duration::hours(7);
    harness.set_now(start + Duration::hours(5));
    harness.mock.set_usage(
        200,
        Some(json!({
            "five_hour":{"utilization":100,"resets_at":datetime(reset)},
            "seven_day":{"utilization":30,"resets_at":datetime(start + Duration::days(7))}
        })),
    );
    let manual = harness
        .runtime
        .monitor
        .queries
        .query(id, ADMIN_KEY, query_options(Source::Manual, harness.now()))
        .await;
    assert_eq!(manual["success"], true);
    let preserved =
        &harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["quota_query"];
    assert_eq!(preserved["failure_count"], 3);
    assert_eq!(preserved["retry_at"], datetime(retry_at));
    harness.set_now(reset + Duration::minutes(1));
    let blocked = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(blocked["error_code"], "query_backoff");
    assert_eq!(blocked["next_query_at"], datetime(retry_at));
    assert_eq!(harness.mock.count("GET", "/usage"), 4);

    harness.set_now(retry_at);
    harness.mock.set_usage(200, None);
    let available = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(available["success"], true);
    let cleared =
        &harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["quota_query"];
    assert_eq!(cleared["failure_count"], 0);
    assert_eq!(cleared["retry_at"], "");
    assert_eq!(harness.mock.count("GET", "/usage"), 5);
    harness.close().await;
}

#[tokio::test]
async fn depleted_windows_wait_for_latest_exact_reset_and_missing_data_keeps_good_cache() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-reset-token", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let now = harness.now();
    let early = now + Duration::hours(1);
    let latest = now + Duration::hours(2);
    let depleted = json!({
        "five_hour":{"utilization":100,"resets_at":datetime(early)},
        "seven_day":{"utilization":100,"resets_at":datetime(latest)}
    });
    let good = cached_result(&row, now - Duration::hours(2), depleted.clone());
    save_cached(&harness, id, good.clone()).await;
    let blocked = harness
        .runtime
        .monitor
        .queries
        .query(id, ADMIN_KEY, query_options(Source::Automatic, now))
        .await;
    assert_eq!(blocked["error_code"], "waiting_reset");
    assert_eq!(blocked["next_query_at"], datetime(latest));
    harness.set_now(latest - Duration::seconds(1));
    assert_eq!(
        harness
            .runtime
            .monitor
            .queries
            .query(
                id,
                ADMIN_KEY,
                query_options(Source::Automatic, harness.now()),
            )
            .await["error_code"],
        "waiting_reset"
    );
    harness.set_now(latest);
    let available = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(available["success"], true);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);

    let previous = cached_result(
        &row,
        harness.now() - Duration::hours(2),
        default_usage(harness.now()),
    );
    save_cached(&harness, id, previous.clone()).await;
    harness.mock.set_usage(
        200,
        Some(json!({"seven_day":{"utilization":30,"resets_at":datetime(harness.now() + Duration::days(7))}})),
    );
    harness.set_now(harness.now() + Duration::hours(1));
    let incomplete = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(incomplete["error_code"], "incomplete_quota");
    let state = harness.runtime.monitor.state().unwrap();
    assert_eq!(state["oauth_results"][id.to_string()], previous);
    assert_eq!(
        state["scheduler"][id.to_string()]["quota_query"]["failure_count"],
        1
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    harness.close().await;
}

#[tokio::test]
async fn auth_pause_rotates_with_credentials_and_corrupt_state_fails_closed() {
    for status in [401_u16, 402] {
        let harness = Harness::new().await;
        let row = harness
            .seed_account("plus", "fixture-auth-token-v1", json!({}))
            .await;
        let id = integer(&row["id"]).unwrap();
        let start = harness.now();
        harness.mock.set_usage(status, None);
        let failed = harness
            .runtime
            .monitor
            .queries
            .query(id, ADMIN_KEY, query_options(Source::Automatic, start))
            .await;
        assert_eq!(failed["error_code"], format!("http_{status}"));
        let reopened = harness.after_restart_coordinator();
        harness.set_now(start + Duration::days(2));
        let paused = reopened
            .query(
                id,
                ADMIN_KEY,
                query_options(Source::Automatic, harness.now()),
            )
            .await;
        assert_eq!(paused["error_code"], "auth_paused");
        assert_eq!(harness.mock.count("GET", "/usage"), 1);
        sqlx::query("UPDATE accounts SET credentials=$1 WHERE id=$2")
            .bind(SqlJson(json!({"plan_type":"plus","access_token":"fixture-auth-token-v2","refresh_token":"fixture-refresh-token"})))
            .bind(id)
            .execute(&harness.runtime.db.pool)
            .await
            .unwrap();
        harness.mock.set_usage(200, None);
        let rotated = reopened
            .query(
                id,
                ADMIN_KEY,
                query_options(Source::Automatic, harness.now()),
            )
            .await;
        assert_eq!(rotated["success"], true);
        assert!(
            harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["quota_query"]
                .get("auth_fingerprint")
                .is_none()
        );
        assert_eq!(harness.mock.count("GET", "/usage"), 2);
        harness.close().await;
    }

    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-manual-auth-unpause", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let start = harness.now();
    harness.mock.set_usage(401, None);
    let failed = harness
        .runtime
        .monitor
        .queries
        .query(id, ADMIN_KEY, query_options(Source::Automatic, start))
        .await;
    assert_eq!(failed["error_code"], "http_401");
    let attempts = harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]
        ["quota_query"]["automatic_attempts"]
        .clone();
    harness.set_now(start + Duration::minutes(10));
    harness.mock.set_usage(200, None);
    let manual = harness
        .runtime
        .monitor
        .queries
        .query(id, ADMIN_KEY, query_options(Source::Manual, harness.now()))
        .await;
    assert_eq!(manual["success"], true);
    let state = harness.runtime.monitor.state().unwrap();
    assert_eq!(
        state["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"],
        attempts
    );
    assert!(state["scheduler"][id.to_string()]["quota_query"]
        .get("auth_fingerprint")
        .is_none());
    harness.set_now(start + Duration::hours(1));
    assert_eq!(
        harness
            .runtime
            .monitor
            .queries
            .query(
                id,
                ADMIN_KEY,
                query_options(Source::Automatic, harness.now())
            )
            .await["error_code"],
        "query_cooldown"
    );
    harness.set_now(start + Duration::minutes(70));
    assert_eq!(
        harness
            .runtime
            .monitor
            .queries
            .query(
                id,
                ADMIN_KEY,
                query_options(Source::Automatic, harness.now())
            )
            .await["success"],
        true
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 3);
    harness.close().await;

    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-corrupt-token", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let state_path = harness.runtime.settings.usage_path.clone();
    for invalid in [
        br#"{not-json"#.to_vec(),
        serde_json::to_vec(&json!({"scheduler":[]})).unwrap(),
        serde_json::to_vec(&json!({"scheduler":{(id.to_string()):{"quota_query":{"automatic_attempts":"not-list"}}}})).unwrap(),
    ] {
        fs::write(&state_path, invalid).unwrap();
        for source in [Source::Automatic, Source::Manual] {
            let blocked = harness
                .runtime
                .monitor
                .queries
                .query(id, ADMIN_KEY, query_options(source, harness.now()))
                .await;
            assert_eq!(blocked["error_code"], "query_state_unavailable");
            assert!(blocked["skipped"].as_bool().unwrap_or(false));
        }
    }
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    fs::remove_file(&state_path).unwrap();
    for source in [Source::Automatic, Source::Manual] {
        let blocked = harness
            .runtime
            .monitor
            .queries
            .query(id, ADMIN_KEY, query_options(source, harness.now()))
            .await;
        assert_eq!(blocked["error_code"], "query_state_unavailable");
    }
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    harness.close().await;
}

#[tokio::test]
async fn invalid_usage_json_consumes_automatic_budget_without_replay() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-invalid-json", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    harness.mock.set_usage_raw(200, b"{not-json".to_vec());
    let first = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(first["error_code"], "usage_query_error");
    let metadata =
        &harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["quota_query"];
    assert_eq!(metadata["automatic_attempts"].as_array().unwrap().len(), 1);
    assert_eq!(metadata["failure_count"], 1);
    let second = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(second["error_code"], "query_cooldown");
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    harness.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn reservation_write_failure_prevents_automatic_and_manual_http() {
    use std::os::unix::fs::PermissionsExt;
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-reservation-failure", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let parent = harness.runtime.settings.usage_path.parent().unwrap();
    let before = fs::metadata(parent).unwrap().permissions().mode() & 0o777;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o500)).unwrap();
    for source in [Source::Automatic, Source::Manual] {
        let result = harness
            .runtime
            .monitor
            .queries
            .query(id, ADMIN_KEY, query_options(source, harness.now()))
            .await;
        assert_eq!(result["error_code"], "query_state_unavailable");
        assert_eq!(result["skipped"], true);
    }
    fs::set_permissions(parent, fs::Permissions::from_mode(before)).unwrap();
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert!(harness.runtime.monitor.state().is_ok());
    harness.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn reservation_precedes_http_and_failed_result_write_keeps_the_reservation() {
    use std::os::unix::fs::PermissionsExt;
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-write-token", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let path = harness.runtime.settings.usage_path.clone();
    let raw_before_request = Arc::new(StdMutex::new(None::<Value>));
    let gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());
    let now = harness.now();
    let coordinator = harness.runtime.monitor.queries.clone();
    let captured = raw_before_request.clone();
    let query = tokio::spawn(async move {
        coordinator
            .query(
                id,
                ADMIN_KEY,
                QueryOptions {
                    timeout: 2,
                    ..query_options(Source::Automatic, now)
                },
            )
            .await
    });
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("query must write the reservation and then dispatch HTTP");
    *captured.lock().unwrap() = Some(
        serde_json::from_slice(&fs::read(&path).unwrap()).expect("reserved state must be JSON"),
    );
    let query_result = tokio::time::timeout(StdDuration::from_secs(6), query).await;
    gate.release.notify_one();
    *harness.mock.usage_gate.lock().unwrap() = None;
    let query_result = query_result
        .expect("HTTP request timeout must return without replay")
        .unwrap();
    assert_eq!(query_result["error_code"], "timeout");
    let reserved = raw_before_request.lock().unwrap().clone().unwrap();
    assert_eq!(
        reserved["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"],
        json!([datetime(now)])
    );
    assert_eq!(
        reserved["scheduler"][id.to_string()]["quota_query"]["last_source"],
        "automatic"
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 1);

    let root = harness._temp.path().to_path_buf();
    *harness.mock.readonly_after_usage.lock().unwrap() = Some(root.clone());
    harness.mock.set_usage(200, None);
    harness.set_now(now + Duration::hours(2));
    let write_failed = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    let mut permissions = fs::metadata(&root).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&root, permissions).unwrap();
    assert_eq!(write_failed["error_code"], "query_state_unavailable");
    assert_eq!(write_failed["skipped"], false);
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    let reopened = harness.after_restart_coordinator();
    let blocked = reopened
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(blocked["error_code"], "query_cooldown");
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    harness.close().await;
}

#[tokio::test]
async fn concurrent_automatic_and_manual_coordinator_queries_coalesce_once() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-coordinator-coalesce", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let requested_at = Instant::now();
    let start = harness.now();
    let gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());

    let coordinator = harness.runtime.monitor.queries.clone();
    let owner = tokio::spawn(async move {
        coordinator
            .query(
                id,
                ADMIN_KEY,
                QueryOptions {
                    source: Source::Automatic,
                    requested_at: Some(requested_at),
                    ..query_options(Source::Automatic, start)
                },
            )
            .await
    });
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("automatic coordinator owner entered the loopback usage endpoint");

    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut followers = Vec::new();
    for index in 0..7 {
        let barrier = barrier.clone();
        let coordinator = harness.runtime.monitor.queries.clone();
        followers.push(tokio::spawn(async move {
            barrier.wait().await;
            coordinator
                .query(
                    id,
                    ADMIN_KEY,
                    QueryOptions {
                        source: if index % 2 == 0 {
                            Source::Automatic
                        } else {
                            Source::Manual
                        },
                        requested_at: Some(requested_at),
                        ..query_options(Source::Automatic, start)
                    },
                )
                .await
        }));
    }
    barrier.wait().await;
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    *harness.mock.usage_gate.lock().unwrap() = None;
    gate.release.notify_one();

    let owner_result = owner.await.unwrap();
    let mut results = vec![owner_result];
    for follower in followers {
        results.push(follower.await.unwrap());
    }
    assert!(results.iter().all(|result| result["success"] == true));
    let coalesced_count = results
        .iter()
        .filter(|result| result["coalesced"] == true)
        .count();
    assert_eq!(coalesced_count, 7);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["quota_query"]
            ["automatic_attempts"],
        json!([datetime(start)])
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn single_and_batch_runtime_queries_coalesce_and_keep_request_locks() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-coalesce-token", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());
    let start = harness.now();

    // Exercise the public batch entry point. The direct user action joins the
    // batch-owned usage request without reserving automatic query budget.
    let first_batch = harness
        .runtime
        .0
        .as_ref()
        .unwrap()
        .start_quota_refresh(ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(first_batch["total"], 1);
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("Runtime batch entered the loopback usage endpoint");
    let runtime = harness.runtime.0.as_ref().unwrap().clone();
    let payload = manual_usage_payload(&runtime, id).await;
    let requested = Instant::now();
    let single = tokio::spawn(async move {
        runtime
            .usage_action(id, &payload, ADMIN_KEY, Some(requested))
            .await
    });
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    *harness.mock.usage_gate.lock().unwrap() = None;
    gate.release.notify_one();
    let single_result = single.await.unwrap().unwrap();
    let batch_result = wait_for_quota_batch_completion(&harness).await;
    assert_eq!(single_result["account"]["id"], id);
    assert_eq!(batch_result["total"], 1);
    assert_eq!(batch_result["items"][0]["status"], "success");
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    let state = harness.runtime.monitor.state().unwrap();
    let quota_query = &state["scheduler"][id.to_string()]["quota_query"];
    assert!(
        has_no_automatic_attempts(quota_query),
        "manual batch and user action must not consume automatic budget: {quota_query}"
    );

    // A later action is a new query, and a later batch is a new batch cycle.
    harness.set_now(start + Duration::hours(1));
    let runtime = harness.runtime.0.as_ref().unwrap().clone();
    let payload = manual_usage_payload(&runtime, id).await;
    runtime
        .usage_action(id, &payload, ADMIN_KEY, Some(Instant::now()))
        .await
        .unwrap();
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    harness
        .runtime
        .0
        .as_ref()
        .unwrap()
        .start_quota_refresh(ADMIN_KEY)
        .await
        .unwrap();
    let completed = wait_for_quota_batch_completion(&harness).await;
    assert_eq!(completed["total"], 1);
    assert_eq!(harness.mock.count("GET", "/usage"), 3);

    // The UI waiter can be cancelled while the Runtime-owned request keeps its
    // account lease until the already-dispatched loopback request returns.
    let cancel_gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(cancel_gate.clone());
    harness.set_now(start + Duration::hours(2));
    let runtime = harness.runtime.0.as_ref().unwrap().clone();
    let payload = manual_usage_payload(&runtime, id).await;
    let cancelled_waiter = tokio::spawn(async move {
        runtime
            .usage_action(id, &payload, ADMIN_KEY, Some(Instant::now()))
            .await
    });
    tokio::time::timeout(StdDuration::from_secs(5), cancel_gate.entered.notified())
        .await
        .expect("cancelled user-action waiter entered loopback HTTP");
    cancelled_waiter.abort();
    let _ = cancelled_waiter.await;
    let held = match harness.runtime.monitor.lease(&row) {
        Ok(lease) => {
            drop(lease);
            panic!("cancelled UI waiter must leave the dispatched request owner holding its lease")
        }
        Err(error) => error,
    };
    assert_eq!(held.status.as_u16(), 409);
    *harness.mock.usage_gate.lock().unwrap() = None;
    cancel_gate.release.notify_one();
    tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            if let Ok(lease) = harness.runtime.monitor.lease(&row) {
                drop(lease);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owner must release the account lock after its dispatched request ends");
    assert_eq!(harness.mock.count("GET", "/usage"), 4);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn runtime_shutdown_waits_for_batch_request_before_releasing_account_lock() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-shutdown-batch-inflight", json!({}))
        .await;
    let gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());
    let runtime = harness.runtime.0.as_ref().unwrap().clone();
    let batch = runtime.start_quota_refresh(ADMIN_KEY).await.unwrap();
    assert_eq!(batch["total"], 1);
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("Runtime-owned batch request must enter loopback HTTP before shutdown");

    let runtime = harness.runtime.0.as_ref().unwrap().clone();
    let shutdown = tokio::spawn(async move { runtime.close().await });
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    let shutdown_waits_for_request = !shutdown.is_finished();
    let account_lock_is_held = match harness.runtime.monitor.lease(&row) {
        Ok(lease) => {
            drop(lease);
            false
        }
        Err(error) => error.status.as_u16() == 409,
    };
    let dispatched_before_release = harness.mock.count("GET", "/usage");

    *harness.mock.usage_gate.lock().unwrap() = None;
    gate.release.notify_one();
    let shutdown_completed = tokio::time::timeout(StdDuration::from_secs(8), shutdown)
        .await
        .is_ok_and(|result| result.is_ok());
    let account_lock_released = match harness.runtime.monitor.lease(&row) {
        Ok(lease) => {
            drop(lease);
            true
        }
        Err(_) => false,
    };
    let progress = harness.runtime.quota_progress();
    let request_count_after_shutdown = harness.mock.count("GET", "/usage");
    let unexpected = harness.mock.unexpected();

    harness.cleanup_after_runtime_close().await;

    assert!(
        shutdown_waits_for_request,
        "shutdown must wait for the owner request"
    );
    assert!(
        account_lock_is_held,
        "the dispatched batch must retain its account lease"
    );
    assert_eq!(dispatched_before_release, 1);
    assert!(
        shutdown_completed,
        "Runtime shutdown must finish after the request returns"
    );
    assert!(
        account_lock_released,
        "shutdown must release the owner lease after completion"
    );
    assert_eq!(progress["status"], "completed");
    assert_eq!(progress["items"][0]["status"], "success");
    assert_eq!(
        request_count_after_shutdown, 1,
        "shutdown must not replay the query"
    );
    assert!(unexpected.is_empty());
}

#[tokio::test]
async fn runtime_batch_joins_manual_single_query_that_started_first() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-single-first-batch-join", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());
    let runtime = harness.runtime.0.as_ref().unwrap().clone();
    let payload = manual_usage_payload(&runtime, id).await;
    let single = tokio::spawn(async move {
        runtime
            .usage_action(id, &payload, ADMIN_KEY, Some(Instant::now()))
            .await
    });
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("single Runtime usage action must own the loopback HTTP request");

    // Accept the real Runtime batch before releasing the single-action gate,
    // matching the legacy ordering where start_batch ran while the owner was
    // blocked in usage HTTP.
    let batch = harness
        .runtime
        .0
        .as_ref()
        .unwrap()
        .start_quota_refresh(ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(batch["total"], 1);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    *harness.mock.usage_gate.lock().unwrap() = None;
    gate.release.notify_one();

    assert_eq!(single.await.unwrap().unwrap()["account"]["id"], id);
    let completed = wait_for_quota_batch_completion(&harness).await;
    assert_eq!(completed["completed"], 1);
    assert_eq!(completed["items"][0]["status"], "success");
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    let state = harness.runtime.monitor.state().unwrap();
    let quota_query = &state["scheduler"][id.to_string()]["quota_query"];
    assert!(
        has_no_automatic_attempts(quota_query),
        "joining a manual single request must not reserve automatic budget: {quota_query}"
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn monitor_and_direct_query_share_one_inflight_usage_request() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    for monitor_first in [true, false] {
        let harness = Harness::new().await;
        let row = harness
            .seed_account(
                "plus",
                &format!("fixture-monitor-query-join-{monitor_first}"),
                json!({}),
            )
            .await;
        let id = integer(&row["id"]).unwrap();
        harness.set_now(due - Duration::hours(1));
        harness.runtime.monitor.startup().await.unwrap();
        let baseline_attempts = harness.runtime.monitor.state().unwrap()["scheduler"]
            [id.to_string()]["quota_query"]["automatic_attempts"]
            .clone();
        let baseline = baseline_attempts.as_array().cloned().unwrap_or_default();
        assert!(
            !baseline.contains(&json!(datetime(due))),
            "the injected due time must be a new automatic reservation: {baseline_attempts}"
        );
        harness.set_now(due);
        let gate = Gate::new();
        *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());

        let tick = |harness: &Harness| {
            let monitor = harness.runtime.monitor.clone();
            tokio::spawn(async move { monitor.tick().await })
        };

        let direct;
        let background;
        if monitor_first {
            background = tick(&harness);
            tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
                .await
                .expect("monitor worker must enter the loopback usage request");
            let runtime = harness.runtime.0.as_ref().unwrap().clone();
            let payload = manual_usage_payload(&runtime, id).await;
            let requested = Instant::now();
            direct = tokio::spawn(async move {
                runtime
                    .usage_action(id, &payload, ADMIN_KEY, Some(requested))
                    .await
            });
        } else {
            let runtime = harness.runtime.0.as_ref().unwrap().clone();
            let payload = manual_usage_payload(&runtime, id).await;
            let requested = Instant::now();
            direct = tokio::spawn(async move {
                runtime
                    .usage_action(id, &payload, ADMIN_KEY, Some(requested))
                    .await
            });
            tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
                .await
                .expect("direct Runtime usage action must enter the loopback usage request");
            background = tick(&harness);
        }
        tokio::time::sleep(StdDuration::from_millis(25)).await;
        *harness.mock.usage_gate.lock().unwrap() = None;
        gate.release.notify_one();
        background.await.unwrap().unwrap();
        let direct_result = direct.await.unwrap().unwrap();
        assert_eq!(direct_result["account"]["id"], id);
        let expected_usage_calls;
        if monitor_first {
            tokio::time::timeout(StdDuration::from_secs(8), async {
                loop {
                    let attempts = harness.runtime.monitor.state().unwrap()["scheduler"]
                        [id.to_string()]["quota_query"]["automatic_attempts"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    let new_due_reservations = attempts
                        .iter()
                        .filter(|value| **value == json!(datetime(due)))
                        .count();
                    if attempts.len() == baseline.len() + 1 && new_due_reservations == 1 {
                        break;
                    }
                    tokio::time::sleep(StdDuration::from_millis(10)).await;
                }
            })
            .await
            .expect("monitor-owned query must consume one automatic budget reservation");
            let runtime = harness.runtime.0.as_ref().unwrap().clone();
            let payload = manual_usage_payload(&runtime, id).await;
            runtime
                .usage_action(id, &payload, ADMIN_KEY, Some(Instant::now()))
                .await
                .unwrap();
            expected_usage_calls = 2;
            let mut expected_attempts = baseline.clone();
            expected_attempts.push(json!(datetime(due)));
            assert_eq!(
                harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]
                    ["quota_query"]["automatic_attempts"],
                json!(expected_attempts),
                "the joined manual request must preserve the preexisting budget history"
            );
        } else {
            let state = harness.runtime.monitor.state().unwrap();
            let quota_query = &state["scheduler"][id.to_string()]["quota_query"];
            assert!(
                quota_query["automatic_attempts"] == baseline_attempts,
                "a running manual action must not spend automatic budget: {quota_query}"
            );
            expected_usage_calls = 1;
        }
        assert_eq!(harness.mock.count("GET", "/usage"), expected_usage_calls);
        assert_eq!(
            harness.mock.count("POST", "/test"),
            0,
            "joining an ordinary quota query must not dispatch recovery model tests"
        );
        assert!(harness.mock.unexpected().is_empty());
        harness.close().await;
    }
}

#[tokio::test]
async fn concurrent_force_refresh_callers_share_one_runtime_cycle() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-force-coalesce", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());
    let monitor = harness.runtime.monitor.clone();
    let first = tokio::spawn(async move { monitor.force_refresh().await.unwrap() });
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("first force-refresh cycle entered the loopback usage endpoint");

    let monitor = harness.runtime.monitor.clone();
    let second = tokio::spawn(async move { monitor.force_refresh().await.unwrap() });
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    gate.release.notify_one();
    let first = first.await.unwrap();
    let second = second.await.unwrap();
    let state = harness.runtime.monitor.state().unwrap();
    let first_diagnostic = force_refresh_diagnostic(&first, &state, id);
    let second_diagnostic = force_refresh_diagnostic(&second, &state, id);
    assert_eq!(first["success"], true, "first report: {first_diagnostic}");
    assert_eq!(
        second["success"], true,
        "coalesced report: {second_diagnostic}"
    );
    assert_eq!(first["coalesced"], false);
    assert_eq!(second["coalesced"], true);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    let quota_query = &state["scheduler"][id.to_string()]["quota_query"];
    assert!(
        has_no_automatic_attempts(quota_query),
        "manual refresh must not consume automatic budget: {quota_query}"
    );
    harness.close().await;
}

#[tokio::test]
async fn force_refresh_waiter_reports_explicit_timeout_without_replaying_owner_request() {
    let harness = Harness::new().await;
    harness
        .seed_account("plus", "fixture-force-timeout", json!({}))
        .await;
    let gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());
    let monitor = harness.runtime.monitor.clone();
    let owner = tokio::spawn(async move { monitor.force_refresh().await.unwrap() });
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("owner request remains in flight at the loopback usage endpoint");

    let timed_out = harness
        .runtime
        .monitor
        .force_refresh_with_timeout(StdDuration::from_millis(50))
        .await
        .unwrap();
    assert_eq!(timed_out["success"], false);
    assert_eq!(timed_out["timed_out"], true);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    gate.release.notify_one();
    assert_eq!(owner.await.unwrap()["success"], true);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    harness.close().await;
}

#[tokio::test]
async fn force_refresh_cycle_lock_timeout_does_not_dispatch_after_unlock() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-force-cycle-lock-timeout", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_recovery_monitor_enabled"] = false.into();
            config["oauth_recovery_connection_account_ids"] = json!([id]);
            config["oauth_recovery_model_account_ids"] = json!([]);
            Ok(())
        })
        .unwrap();

    let mut blocker = harness.runtime.db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE accounts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let monitor = harness.runtime.monitor.clone();
    let tick = tokio::spawn(async move { monitor.tick().await });
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND wait_event_type='Lock' AND query ILIKE '%FROM accounts%'",
            )
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("monitor tick must hold the real cycle lock while waiting on inventory");

    let report = harness
        .runtime
        .monitor
        .force_refresh_with_timeout(StdDuration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(report["success"], false);
    assert_eq!(report["timed_out"], true);
    assert_eq!(report["error_code"], "force_refresh_timeout");
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    blocker.commit().await.unwrap();
    tick.await.unwrap().unwrap();
    tokio::time::sleep(StdDuration::from_millis(150)).await;
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn force_refresh_without_admin_key_returns_explicit_error_without_upstream_calls() {
    let harness = Harness::new().await;
    harness
        .seed_account("plus", "fixture-missing-admin", json!({}))
        .await;
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["settings"]
                .as_object_mut()
                .unwrap()
                .remove("sub2api_admin_token");
            Ok(())
        })
        .expect("remove only the synthetic admin credential from the temporary state");
    let report = harness.runtime.monitor.force_refresh().await.unwrap();
    assert_eq!(report["success"], false);
    assert_eq!(report["error_code"], "missing_sub2api_admin_credentials");
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    harness.close().await;
}

#[tokio::test]
async fn force_refresh_ignores_probe_batch_cap_and_queries_all_accounts() {
    let harness = Harness::new().await;
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_early_probe_batch_size"] = 1.into();
            Ok(())
        })
        .unwrap();
    let mut ids = Vec::new();
    for index in 0..3 {
        let row = harness
            .seed_account("plus", &format!("fixture-force-all-{index}"), json!({}))
            .await;
        ids.push(integer(&row["id"]).unwrap());
    }

    let report = harness.runtime.monitor.force_refresh().await.unwrap();
    assert_eq!(report["success"], true);
    assert_eq!(report["total_count"], 3);
    assert_eq!(report["queried_count"], 3);
    assert_eq!(report["success_count"], 3);
    assert_eq!(report["failure_count"], 0);
    assert_eq!(harness.mock.count("GET", "/usage"), 3);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    let state = harness.runtime.monitor.state().unwrap();
    for id in ids {
        assert_eq!(
            state["oauth_results"][id.to_string()]["queried_at"],
            report["refresh_at"]
        );
    }
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn force_refresh_recovers_fresh_available_quota_once_at_0459_beijing() {
    let due = Utc.with_ymd_and_hms(2026, 9, 29, 20, 59, 0).unwrap();
    assert!(due
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .to_rfc3339()
        .contains("T04:59:00+08:00"));
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-force-recovery-0459", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    prepare_expired_recovery(&harness, &row, due).await;
    harness.set_now(due);
    harness.mock.clear_block_on_test(id);

    let first = harness.runtime.monitor.force_refresh().await.unwrap();
    assert_eq!(first["recovered_count"], 1);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert_eq!(
        harness.runtime.monitor.row(id).await.unwrap()["rate_limited_at"],
        Value::Null
    );
    let state = harness.runtime.monitor.state().unwrap();
    let event = state["pending_events"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(event["status"], "recovered");
    assert_eq!(event["model_id"], "gpt-5.6-luna");

    harness.set_now(due + Duration::seconds(1));
    let second = harness.runtime.monitor.force_refresh().await.unwrap();
    assert_eq!(second["recovered_count"], 0);
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"]
            ["status"],
        "recovered"
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn runtime_batch_waits_for_busy_account_without_spending_budget() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-batch-busy-account", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let held = harness.runtime.monitor.lease(&row).unwrap();
    let waiting = harness
        .runtime
        .0
        .as_ref()
        .unwrap()
        .start_quota_refresh(ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(waiting["total"], 1);

    let progress = tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            let progress = harness.runtime.quota_progress();
            if progress["items"]
                .as_array()
                .is_some_and(|items| items.len() == 1 && items[0]["status"] == "queued")
            {
                break progress;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "single-account batch did not queue behind the held account lease: {}",
            harness.runtime.quota_progress()
        )
    });
    assert_eq!(progress["items"][0]["status"], "queued");
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    let state = harness.runtime.monitor.state().unwrap();
    let quota_query = &state["scheduler"][id.to_string()]["quota_query"];
    assert!(
        has_no_automatic_attempts(quota_query),
        "a queued manual batch must not reserve automatic budget: {quota_query}"
    );

    drop(held);
    let completed = wait_for_quota_batch_completion(&harness).await;
    assert_eq!(completed["total"], 1);
    assert_eq!(completed["completed"], 1);
    assert_eq!(completed["items"][0]["status"], "success");
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn linked_credentials_share_locks_and_runtime_batch_ignores_probe_cap() {
    let harness = Harness::new().await;
    let parent = harness
        .seed_account("plus", "fixture-shared-credential", json!({}))
        .await;
    let child = harness
        .seed_account("plus", "fixture-shared-credential", json!({}))
        .await;
    let parent_id = integer(&parent["id"]).unwrap();
    let child_id = integer(&child["id"]).unwrap();
    sqlx::query("UPDATE accounts SET parent_account_id=$1 WHERE id=$2")
        .bind(parent_id)
        .bind(child_id)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("link only synthetic OAuth rows to a shared credential owner");
    let parent_row = harness.runtime.monitor.row(parent_id).await.unwrap();
    let held = harness.runtime.monitor.lease(&parent_row).unwrap();
    let waiting = harness
        .runtime
        .0
        .as_ref()
        .unwrap()
        .start_quota_refresh(ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(waiting["total"], 2);
    let queued = tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            let progress = harness.runtime.quota_progress();
            let queued = progress["items"].as_array().is_some_and(|items| {
                items.len() == 2
                    && items.iter().any(|item| item["status"] == "queued")
                    && items
                        .iter()
                        .all(|item| item["status"] == "queued" || item["status"] == "pending")
            });
            if queued {
                break progress;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await;
    let queued = if let Ok(queued) = queued {
        queued
    } else {
        let state = harness.runtime.monitor.state().unwrap();
        let progress = harness.runtime.quota_progress();
        panic!(
            "linked batch did not reach a queued/pending state behind the shared credential lock: {}",
            json!({
                "progress":progress,
                "usage_get":harness.mock.count("GET", "/usage"),
                "parent_attempts":state["scheduler"][parent_id.to_string()]["quota_query"]["automatic_attempts"],
                "child_attempts":state["scheduler"][child_id.to_string()]["quota_query"]["automatic_attempts"]
            })
        );
    };
    assert_eq!(queued["total"], 2);
    assert_eq!(queued["completed"], 0);
    assert!(queued["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["status"] == "queued"));
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    let state = harness.runtime.monitor.state().unwrap();
    for id in [parent_id, child_id] {
        let quota_query = &state["scheduler"][id.to_string()]["quota_query"];
        assert!(
            has_no_automatic_attempts(quota_query),
            "queued manual batch must not reserve automatic budget for {id}: {quota_query}"
        );
    }
    drop(held);

    let refreshed = wait_for_quota_batch_completion(&harness).await;
    assert_eq!(refreshed["total"], 2);
    assert_eq!(refreshed["completed"], 2);
    assert!(refreshed["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["status"] == "success"));
    assert_eq!(harness.mock.count("GET", "/usage"), 2);

    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_early_probe_batch_size"] = 1.into();
            Ok(())
        })
        .unwrap();
    let third = harness
        .seed_account("plus", "fixture-third-credential", json!({}))
        .await;
    let third_id = integer(&third["id"]).unwrap();
    let all = harness
        .runtime
        .0
        .as_ref()
        .unwrap()
        .start_quota_refresh(ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(all["total"], 3);
    let all = wait_for_quota_batch_completion(&harness).await;
    assert_eq!(all["completed"], 3);
    assert_eq!(harness.mock.count("GET", "/usage"), 5);
    assert_eq!(
        harness
            .mock
            .calls()
            .iter()
            .filter(|call| call.method == "GET")
            .count(),
        5
    );
    assert_eq!(
        harness.runtime.monitor.row(third_id).await.unwrap()["type"],
        "oauth"
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn manual_runtime_batch_refreshes_cooldown_without_automatic_budget() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-manual-batch-cooldown", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let start = Utc::now();
    harness.set_now(start);
    let automatic = harness
        .runtime
        .monitor
        .queries
        .query(id, ADMIN_KEY, query_options(Source::Automatic, start))
        .await;
    assert_eq!(automatic["success"], true);
    let initial = harness.runtime.monitor.state().unwrap();
    let initial_attempts =
        initial["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"].clone();
    assert_eq!(initial_attempts, json!([datetime(start)]));

    harness
        .runtime
        .0
        .as_ref()
        .unwrap()
        .start_quota_refresh(ADMIN_KEY)
        .await
        .unwrap();
    let batch = wait_for_quota_batch_completion(&harness).await;
    assert_eq!(batch["total"], 1);
    assert_eq!(batch["items"][0]["status"], "success");
    let after_batch = harness.runtime.monitor.state().unwrap();
    let query = &after_batch["scheduler"][id.to_string()]["quota_query"];
    assert_eq!(query["last_source"], "manual");
    assert_eq!(query["automatic_attempts"], initial_attempts);
    assert_eq!(harness.mock.count("GET", "/usage"), 2);

    let batch_query_at = parse_datetime(&query["last_query_at"]).unwrap();
    harness.set_now(batch_query_at + Duration::minutes(59));
    let blocked = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(blocked["error_code"], "query_cooldown");
    assert_eq!(
        blocked["next_query_at"],
        datetime(batch_query_at + Duration::hours(1))
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 2);

    harness.set_now(batch_query_at + Duration::hours(1));
    let available = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(available["success"], true);
    assert_eq!(harness.mock.count("GET", "/usage"), 3);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn daily_schedule_hot_update_eligibility_and_missed_restart_never_catch_up() {
    let harness = Harness::new().await;
    let reference = fixture();
    let before = parse_datetime(&reference["daily"]["before_0500_beijing"]).unwrap();
    let due = parse_datetime(&reference["daily"]["due_0500_beijing"]).unwrap();
    assert_eq!(daily_test::next_time(before, "05:00").unwrap(), due);
    assert_eq!(
        daily_test::next_time(due, "05:00").unwrap(),
        due + Duration::days(1)
    );
    assert_eq!(
        daily_test::next_time(due, "00:00").unwrap(),
        parse_datetime(&reference["daily"]["next_midnight_beijing"]).unwrap()
    );

    let now = due - Duration::seconds(1);
    harness.set_now(now);
    let config = json!({"oauth_daily_test_enabled":true,"oauth_daily_test_time":"05:00"});
    let schedule = Schedule::new(harness.runtime.monitor.store.clone());
    schedule.startup(&config, now).unwrap();
    harness.set_now(due + Duration::minutes(2));
    schedule.tick(&config, harness.now()).unwrap();
    assert!(schedule.claim(&config, harness.now()).unwrap().is_none());
    let missed =
        harness.runtime.monitor.state().unwrap()["daily_test"]["batches"]["2026-09-22"].clone();
    assert_eq!(missed["status"], "missed");
    let updated = json!({"oauth_daily_test_enabled":true,"oauth_daily_test_time":"06:00"});
    schedule.tick(&updated, harness.now()).unwrap();
    let after_update = harness.runtime.monitor.state().unwrap()["daily_test"].clone();
    assert_eq!(
        after_update["next_run_at"],
        datetime(due + Duration::hours(1))
    );
    schedule
        .tick(
            &json!({"oauth_daily_test_enabled":false,"oauth_daily_test_time":"06:00"}),
            harness.now(),
        )
        .unwrap();
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["daily_test"]["next_run_at"],
        ""
    );

    let row = harness
        .seed_account("plus", "fixture-daily-eligibility", json!({}))
        .await;
    for changes in [
        json!({"status":"paused"}),
        json!({"schedulable":false}),
        json!({"platform":"grok"}),
        json!({"type":"apikey"}),
        json!({"deleted_at":datetime(due)}),
        json!({"temp_unschedulable_until":datetime(due + Duration::minutes(1))}),
        json!({"rate_limit_reset_at":datetime(due + Duration::minutes(1))}),
        json!({"overload_until":datetime(due + Duration::minutes(1))}),
        json!({"temp_unschedulable_reason":"telegram pause by user"}),
        json!({"expires_at":datetime(due - Duration::seconds(1)),"auto_pause_on_expired":true}),
        json!({"overload_until":"invalid"}),
    ] {
        let mut candidate = row.clone();
        candidate.as_object_mut().unwrap().extend(
            changes
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        assert!(
            !daily_test::eligible(&candidate, due),
            "ineligible delta {changes}"
        );
    }
    harness.close().await;
}

#[tokio::test]
async fn daily_runtime_reuses_fresh_passive_quota_tests_once_and_queues_failure_without_bark() {
    let harness = Harness::new().await;
    let now = harness.now();
    let extra = json!({
        "codex_usage_updated_at":datetime(now - Duration::minutes(10)),
        "codex_5h_used_percent":20,
        "codex_5h_reset_at":datetime(now + Duration::hours(5)),
        "codex_7d_used_percent":30,
        "codex_7d_reset_at":datetime(now + Duration::days(7))
    });
    let row = harness
        .seed_account("plus", "fixture-daily-passive", extra)
        .await;
    let id = integer(&row["id"]).unwrap();
    let batch =
        json!({"date":"2026-09-30","scheduled_at":datetime(now),"status":"running","accounts":{}});
    harness
        .runtime
        .monitor
        .run_daily(batch.clone(), vec![row.clone()])
        .await
        .unwrap();
    let saved =
        harness.runtime.monitor.state().unwrap()["daily_test"]["batches"]["2026-09-30"].clone();
    assert_eq!(saved["status"], "completed");
    assert_eq!(saved["accounts"][id.to_string()]["status"], "success");
    assert_eq!(
        saved["accounts"][id.to_string()]["model_id"],
        "gpt-5.6-luna"
    );
    let model_call = harness
        .mock
        .calls()
        .into_iter()
        .find(|call| call.method == "POST" && call.path.ends_with("/test"))
        .unwrap();
    assert_eq!(model_call.body["model_id"], "gpt-5.6-luna");
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert!(harness.runtime.monitor.state().unwrap()["pending_events"]
        .as_object()
        .unwrap()
        .is_empty());

    let failure = harness
        .seed_account("plus", "fixture-daily-failure", json!({}))
        .await;
    let failure_id = integer(&failure["id"]).unwrap();
    harness.mock.set_test_sse(
        "data: {\"type\":\"error\",\"code\":\"http_401\",\"message\":\"Authorization: Bearer fixture-secret-do-not-log\"}\n\n",
    );
    let failed_batch =
        json!({"date":"2026-10-01","scheduled_at":datetime(now),"status":"running","accounts":{}});
    harness
        .runtime
        .monitor
        .run_daily(failed_batch, vec![failure.clone()])
        .await
        .unwrap();
    let after = harness.runtime.monitor.state().unwrap();
    assert_eq!(
        after["daily_test"]["batches"]["2026-10-01"]["accounts"][failure_id.to_string()]["status"],
        "failed"
    );
    assert_eq!(
        after["daily_test"]["batches"]["2026-10-01"]["accounts"][failure_id.to_string()]
            ["error_code"],
        "http_401"
    );
    assert_eq!(after["pending_events"].as_object().unwrap().len(), 1);
    let serialized = after["pending_events"].to_string();
    assert!(!serialized.contains("fixture-secret-do-not-log"));
    harness.runtime.monitor.deliver().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 2);
    let paused = harness
        .runtime
        .monitor
        .queries
        .query(
            failure_id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(paused["error_code"], "auth_paused");
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    harness.close().await;
}

#[tokio::test]
async fn daily_model_402_pauses_later_automatic_queries_without_retrying_or_barking() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-daily-model-402", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    harness.mock.set_test_sse(
        "data: {\"type\":\"error\",\"code\":\"http_402\",\"message\":\"fixture auth failure\"}\n\n",
    );
    harness
        .runtime
        .monitor
        .run_daily(
            json!({"date":"2026-10-02","scheduled_at":datetime(harness.now()),"status":"running","accounts":{}}),
            vec![row],
        )
        .await
        .unwrap();

    let state = harness.runtime.monitor.state().unwrap();
    assert_eq!(
        state["daily_test"]["batches"]["2026-10-02"]["accounts"][id.to_string()]["error_code"],
        "http_402"
    );
    assert_eq!(state["pending_events"].as_object().unwrap().len(), 1);
    assert_eq!(
        state["pending_events"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()["status"],
        "auth_failed"
    );
    let blocked = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            query_options(Source::Automatic, harness.now()),
        )
        .await;
    assert_eq!(blocked["error_code"], "auth_paused");
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    harness.close().await;
}

#[tokio::test]
async fn monitor_tick_auth_usage_failures_create_events_without_model_or_recovery_calls() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    for status in [401, 402] {
        let harness = Harness::new().await;
        let row = harness
            .seed_account("plus", &format!("fixture-monitor-auth-{status}"), json!({}))
            .await;
        let id = integer(&row["id"]).unwrap();
        prepare_expired_recovery(&harness, &row, due).await;
        harness.set_now(due - Duration::hours(1));
        harness.runtime.monitor.startup().await.unwrap();
        harness.set_now(due);
        harness.mock.set_usage(status, None);
        harness.runtime.monitor.tick().await.unwrap();
        harness.mock.wait_for("GET", "/usage", 1).await;

        tokio::time::timeout(StdDuration::from_secs(8), async {
            loop {
                let state = harness.runtime.monitor.state().unwrap();
                let event = state["pending_events"]
                    .as_object()
                    .and_then(|items| items.values().next());
                if event.is_some_and(|event| event["error_code"] == format!("http_{status}")) {
                    break;
                }
                tokio::time::sleep(StdDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("monitor worker must persist the loopback authorization failure");

        let state = harness.runtime.monitor.state().unwrap();
        assert_eq!(
            state["scheduler"][id.to_string()]["recovery_intent"]["status"],
            "auth_failed"
        );
        assert_eq!(
            state["pending_events"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap()["status"],
            "auth_failed"
        );
        assert_eq!(harness.mock.count("GET", "/usage"), 1);
        assert_eq!(harness.mock.count("POST", "/test"), 0);
        assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
        assert_eq!(harness.mock.count("POST", "/push"), 0);
        harness.close().await;
    }
}

#[tokio::test]
async fn monitor_tick_self_heals_a_released_block_without_recovery_request() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-monitor-self-heal", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    prepare_expired_recovery(&harness, &row, due).await;
    harness.set_now(due - Duration::hours(1));
    harness.runtime.monitor.startup().await.unwrap();
    harness.set_now(due);
    harness.mock.set_usage(200, Some(default_usage(due)));
    harness.mock.clear_block_on_test(id);

    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("GET", "/usage", 1).await;
    harness.mock.wait_for("POST", "/test", 1).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]
                ["recovery_intent"]["status"]
                == "recovered"
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("a successful model test and cleared block must persist recovery once");

    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert_eq!(
        harness.runtime.monitor.row(id).await.unwrap()["rate_limited_at"],
        Value::Null
    );
    let state = harness.runtime.monitor.state().unwrap();
    let event = state["pending_events"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(event["status"], "recovered");
    assert_eq!(event["model_id"], "gpt-5.6-luna");
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn recovery_model_failure_is_persisted_once_and_auth_failure_does_not_retry() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    for (code, expected_status) in [("http_500", "retry"), ("http_401", "auth_failed")] {
        let harness = Harness::new().await;
        let row = harness
            .seed_account("plus", &format!("fixture-recovery-test-{code}"), json!({}))
            .await;
        let id = integer(&row["id"]).unwrap();
        prepare_expired_recovery(&harness, &row, due).await;
        harness.set_now(due - Duration::hours(1));
        harness.runtime.monitor.startup().await.unwrap();
        harness.set_now(due);
        harness.mock.set_test_sse(format!(
            "data: {{\"type\":\"error\",\"code\":\"{code}\",\"message\":\"fixture test failure\"}}\n\n"
        ));

        harness.runtime.monitor.tick().await.unwrap();
        harness.mock.wait_for("GET", "/usage", 1).await;
        harness.mock.wait_for("POST", "/test", 1).await;
        tokio::time::timeout(StdDuration::from_secs(8), async {
            loop {
                let state = harness.runtime.monitor.state().unwrap();
                let intent = &state["scheduler"][id.to_string()];
                let event_status = if expected_status == "auth_failed" {
                    "auth_failed"
                } else {
                    "test_failed"
                };
                let event_committed = state["pending_events"].as_object().is_some_and(|events| {
                    events.values().any(|event| event["status"] == event_status)
                });
                if intent["recovery_intent"]["status"] == expected_status && event_committed {
                    break;
                }
                tokio::time::sleep(StdDuration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            let state = harness.runtime.monitor.state().unwrap();
            let account = &state["scheduler"][id.to_string()];
            let event_status = state["pending_events"]
                .as_object()
                .and_then(|events| events.values().next())
                .map(|event| event["status"].clone())
                .unwrap_or(Value::Null);
            panic!(
                "model failure state or notification was not committed: {}",
                json!({
                    "intent_status":account["recovery_intent"]["status"],
                    "intent_error":account["recovery_intent"]["last_error_code"],
                    "next_retry_at":account["recovery_intent"]["next_retry_at"],
                    "pending_event_status":event_status,
                    "post_test":harness.mock.count("POST", "/test"),
                    "post_recover_state":harness.mock.count("POST", "/recover-state"),
                    "post_push":harness.mock.count("POST", "/push")
                })
            );
        });

        let state = harness.runtime.monitor.state().unwrap();
        let intent = &state["scheduler"][id.to_string()]["recovery_intent"];
        assert_eq!(intent["status"], expected_status);
        if expected_status == "auth_failed" {
            assert_eq!(intent["last_error_code"], "http_401");
            assert_eq!(intent["next_retry_at"], "");
            let event = state["pending_events"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            assert_eq!(event["status"], "auth_failed");
            assert_eq!(event["error_code"], "http_401");
            assert_eq!(event["test_success"], false);
        } else {
            assert_eq!(intent["last_error_code"], "http_500");
            assert_eq!(
                intent["next_retry_at"],
                datetime(due + Duration::seconds(60))
            );
            let event = state["pending_events"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            assert_eq!(event["status"], "test_failed");
            assert_eq!(event["error_code"], "http_500");
            assert_eq!(event["test_success"], false);
        }
        harness.runtime.monitor.deliver().await.unwrap();
        harness.set_now(due + Duration::seconds(1));
        harness.runtime.monitor.tick().await.unwrap();
        assert_eq!(harness.mock.count("GET", "/usage"), 1);
        assert_eq!(harness.mock.count("POST", "/test"), 1);
        assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
        assert_eq!(harness.mock.count("POST", "/push"), 0);
        assert!(harness.mock.unexpected().is_empty());
        harness.close().await;
    }
}

#[tokio::test]
async fn account_read_failure_marks_retry_without_model_or_recovery_and_recovers() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-account-read-retry", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    prepare_expired_recovery(&harness, &row, due).await;
    harness.set_now(due - Duration::hours(1));
    harness.runtime.monitor.startup().await.unwrap();
    harness.set_now(due);
    let mut available = default_usage(due);
    available["five_hour"]["utilization"] = json!(0);
    available["five_hour"]["resets_at"] = json!(datetime(due + Duration::hours(5)));
    available["seven_day"]["utilization"] = json!(30);
    available["seven_day"]["resets_at"] = json!(datetime(due + Duration::days(7)));
    harness.mock.set_usage(200, Some(available));

    let gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());
    let monitor = harness.runtime.monitor.clone();
    let tick = tokio::spawn(async move { monitor.tick().await });
    tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
        .await
        .expect("the first account lookup must reach the loopback quota request");
    tick.await
        .expect("monitor tick task must return after dispatching its worker")
        .expect("monitor tick should dispatch the scheduled recovery worker");

    // The initial scheduler lookup has completed. Hold the table only while
    // verify_recovery performs its fresh account read after the usage response.
    let mut blocker = harness.runtime.db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE accounts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    *harness.mock.usage_gate.lock().unwrap() = None;
    gate.release.notify_one();

    let blocked_pid = tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND wait_event_type='Lock' AND query ILIKE '%FROM accounts WHERE deleted_at IS NULL AND id=%' ORDER BY query_start DESC LIMIT 1",
            )
            .fetch_optional(&harness.runtime.db.pool)
            .await
            .unwrap();
            if let Some(pid) = pid {
                break pid;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .ok();
    let read_was_blocked = blocked_pid.is_some();
    let read_was_terminated = if let Some(pid) = blocked_pid {
        sqlx::query_scalar::<_, bool>("SELECT pg_terminate_backend($1)")
            .bind(pid)
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap_or(false)
    } else {
        false
    };
    blocker.commit().await.unwrap();

    let lease_released = tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if let Ok(lease) = harness.runtime.monitor.lease(&row) {
                drop(lease);
                break true;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or(false);
    let state_after_failure = harness.runtime.monitor.state().unwrap();
    let intent = &state_after_failure["scheduler"][id.to_string()]["recovery_intent"];
    let retry_state = intent.clone();
    let retry_committed = intent["status"] == "retry"
        && intent["last_error_code"] == "account_read_failed"
        && intent["next_retry_at"] == datetime(due + Duration::seconds(60));
    let events_empty = state_after_failure["pending_events"]
        .as_object()
        .is_some_and(|events| events.is_empty());
    let first_usage_calls = harness.mock.count("GET", "/usage");
    let first_test_calls = harness.mock.count("POST", "/test");
    let first_recovery_calls = harness.mock.count("POST", "/recover-state");

    harness.set_now(due + Duration::hours(1));
    harness.runtime.monitor.tick().await.unwrap();
    let retry_test_started = tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if harness.mock.count("POST", "/test") >= 1 {
                break true;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or(false);
    let recovered = if retry_test_started {
        tokio::time::timeout(StdDuration::from_secs(8), async {
            loop {
                if harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]
                    ["recovery_intent"]["status"]
                    == "recovered"
                {
                    break true;
                }
                tokio::time::sleep(StdDuration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or(false)
    } else {
        false
    };
    let retry_test_calls = harness.mock.count("POST", "/test");
    let unexpected = harness.mock.unexpected();

    harness.close().await;

    assert!(
        read_was_blocked,
        "fresh recovery account lookup must reach the injected PG lock"
    );
    assert!(
        read_was_terminated,
        "terminate only the blocked account-reader backend"
    );
    assert!(
        lease_released,
        "a failed account lookup must release the account lease"
    );
    assert_eq!(first_usage_calls, 1);
    assert_eq!(first_test_calls, 0);
    assert_eq!(first_recovery_calls, 0);
    assert!(
        retry_committed,
        "account-read failure must persist the legacy +60s retry: {retry_state}"
    );
    assert!(
        events_empty,
        "account-read failure must not create a notification event"
    );
    assert!(
        retry_test_started,
        "the due retry must continue to model verification"
    );
    assert_eq!(retry_test_calls, 1);
    assert!(
        recovered,
        "the account must recover once its read path is restored"
    );
    assert!(unexpected.is_empty());
}

#[tokio::test]
async fn monitor_test_success_falls_back_and_unknown_recovery_write_is_not_replayed() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    for recovery_status in [200_u16, 500] {
        let mut harness = Harness::new().await;
        let row = harness
            .seed_account(
                "plus",
                &format!("fixture-recover-state-result-{recovery_status}"),
                json!({}),
            )
            .await;
        let id = integer(&row["id"]).unwrap();
        prepare_expired_recovery(&harness, &row, due).await;
        harness.set_now(due - Duration::hours(1));
        harness.runtime.monitor.startup().await.unwrap();
        let blocked_before_write = harness.runtime.monitor.row(id).await.unwrap();
        harness.set_now(due);
        *harness.mock.recovery_status.lock().unwrap() = recovery_status;

        harness.runtime.monitor.tick().await.unwrap();
        harness.mock.wait_for("GET", "/usage", 1).await;
        harness.mock.wait_for("POST", "/test", 1).await;
        harness.mock.wait_for("POST", "/recover-state", 1).await;
        let expected_status = if recovery_status == 200 {
            "recovered"
        } else {
            "blocked"
        };
        tokio::time::timeout(StdDuration::from_secs(8), async {
            loop {
                if harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]
                    ["recovery_intent"]["status"]
                    == expected_status
                {
                    break;
                }
                tokio::time::sleep(StdDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("the recovery-state outcome must be committed after the model test");

        let state = harness.runtime.monitor.state().unwrap();
        let intent = &state["scheduler"][id.to_string()]["recovery_intent"];
        assert_eq!(intent["status"], expected_status);
        assert_eq!(harness.mock.count("GET", "/usage"), 1);
        assert_eq!(harness.mock.count("POST", "/test"), 1);
        assert_eq!(harness.mock.count("POST", "/recover-state"), 1);
        assert_eq!(harness.mock.count("POST", "/clear-rate-limit"), 0);
        assert_eq!(harness.mock.count("DELETE", "/temp-unschedulable"), 0);
        if recovery_status == 500 {
            assert_eq!(intent["last_error_code"], "recovery_uncertain");
            assert_eq!(intent["next_retry_at"], "");
            assert_eq!(intent["recovery_write_dispatched"], true);
            let after_write = harness.runtime.monitor.row(id).await.unwrap();
            assert_eq!(
                after_write["rate_limited_at"],
                blocked_before_write["rate_limited_at"]
            );
            assert_eq!(
                after_write["rate_limit_reset_at"],
                blocked_before_write["rate_limit_reset_at"]
            );

            harness.set_now(due + Duration::seconds(61));
            harness.runtime.monitor.tick().await.unwrap();
            assert_eq!(harness.mock.count("GET", "/usage"), 1);
            assert_eq!(harness.mock.count("POST", "/test"), 1);
            assert_eq!(harness.mock.count("POST", "/recover-state"), 1);

            harness.restart_runtime().await;
            harness.runtime.monitor.startup().await.unwrap();
            harness.runtime.monitor.tick().await.unwrap();
            let restarted = harness.runtime.monitor.state().unwrap();
            let restarted_intent = &restarted["scheduler"][id.to_string()]["recovery_intent"];
            assert_eq!(restarted_intent["status"], "blocked");
            assert_eq!(restarted_intent["last_error_code"], "recovery_uncertain");
            let after_restart = harness.runtime.monitor.row(id).await.unwrap();
            assert_eq!(
                after_restart["rate_limited_at"],
                after_write["rate_limited_at"]
            );
            assert_eq!(harness.mock.count("GET", "/usage"), 1);
            assert_eq!(harness.mock.count("POST", "/test"), 1);
            assert_eq!(harness.mock.count("POST", "/recover-state"), 1);
        } else {
            assert_eq!(
                harness.runtime.monitor.row(id).await.unwrap()["rate_limited_at"],
                Value::Null
            );
        }
        assert!(harness.mock.unexpected().is_empty());
        harness.close().await;
    }
}

#[tokio::test]
async fn monitor_recovery_stops_if_account_concurrency_changes_during_test() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-recovery-concurrency-change", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    sqlx::query("UPDATE accounts SET concurrency=2,updated_at=$1 WHERE id=$2")
        .bind(harness.now())
        .bind(id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let initial_concurrency: Option<i32> =
        sqlx::query_scalar("SELECT concurrency FROM accounts WHERE id=$1")
            .bind(id)
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap();
    assert_eq!(initial_concurrency, Some(2));
    prepare_expired_recovery(&harness, &row, due).await;
    harness.set_now(due - Duration::hours(1));
    harness.runtime.monitor.startup().await.unwrap();
    harness.set_now(due);
    let gate = Gate::new();
    *harness.mock.test_gate.lock().unwrap() = Some(gate.clone());

    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("GET", "/usage", 1).await;
    let test_entered =
        tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified()).await;
    if test_entered.is_err() {
        let state = harness.runtime.monitor.state().unwrap();
        let account = &state["scheduler"][id.to_string()];
        let usage = &state["oauth_results"][id.to_string()];
        panic!(
            "recovery did not dispatch model verification: {}",
            json!({
                "intent_status":account["recovery_intent"]["status"],
                "intent_error":account["recovery_intent"]["last_error_code"],
                "next_retry_at":account["recovery_intent"]["next_retry_at"],
                "usage_success":usage["success"],
                "usage_error":usage["error_code"],
                "get_usage":harness.mock.count("GET", "/usage"),
                "post_test":harness.mock.count("POST", "/test"),
                "post_recover_state":harness.mock.count("POST", "/recover-state")
            })
        );
    }
    sqlx::query("UPDATE accounts SET concurrency=concurrency+1,updated_at=$1 WHERE id=$2")
        .bind(harness.now())
        .bind(id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let changed_concurrency: Option<i32> =
        sqlx::query_scalar("SELECT concurrency FROM accounts WHERE id=$1")
            .bind(id)
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap();
    assert_eq!(changed_concurrency, Some(3));
    gate.release.notify_one();

    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let state = harness.runtime.monitor.state().unwrap();
            let intent = &state["scheduler"][id.to_string()]["recovery_intent"];
            let event_committed = state["pending_events"].as_object().is_some_and(|events| {
                events.values().any(|event| {
                    event["status"] == "recovery_failed"
                        && event["error_code"] == "recovery_state_changed"
                        && event["test_success"] == true
                })
            });
            if intent["status"] == "retry"
                && intent["last_error_code"] == "recovery_state_changed"
                && intent["next_retry_at"] == datetime(due + Duration::seconds(60))
                && event_committed
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("a changed account concurrency signature must stop stale recovery mutation");

    let intent =
        &harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"];
    assert_eq!(intent["status"], "retry");
    assert_eq!(intent["last_error_code"], "recovery_state_changed");
    assert_eq!(
        intent["next_retry_at"],
        datetime(due + Duration::seconds(60))
    );
    let state = harness.runtime.monitor.state().unwrap();
    let event = state["pending_events"]
        .as_object()
        .unwrap()
        .values()
        .find(|event| {
            event["error_code"] == "recovery_state_changed" && event["status"] == "recovery_failed"
        })
        .expect("changed recovery signature must publish its known failure event");
    assert_eq!(event["test_success"], true);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert_eq!(harness.mock.count("POST", "/clear-rate-limit"), 0);
    assert_eq!(harness.mock.count("DELETE", "/temp-unschedulable"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn daily_active_refresh_obeys_budget_and_known_reset_without_model_or_bark() {
    let harness = Harness::new().await;
    let account = harness
        .seed_account("plus", "fixture-daily-budget", json!({}))
        .await;
    let id = integer(&account["id"]).unwrap();
    let start = harness.now();
    for hour in 0..6 {
        let at = start + Duration::hours(hour);
        harness.set_now(at);
        let result = harness
            .runtime
            .monitor
            .queries
            .query(id, ADMIN_KEY, query_options(Source::Automatic, at))
            .await;
        assert_eq!(result["success"], true);
    }
    harness.set_now(start + Duration::hours(6) + Duration::seconds(1));
    harness
        .runtime
        .monitor
        .run_daily(
            json!({"date":"2026-10-01","scheduled_at":datetime(harness.now()),"status":"running","accounts":{}}),
            vec![account],
        )
        .await
        .unwrap();
    let state = harness.runtime.monitor.state().unwrap();
    assert_eq!(
        state["daily_test"]["batches"]["2026-10-01"]["accounts"][id.to_string()]["status"],
        "skipped"
    );
    assert_eq!(
        state["daily_test"]["batches"]["2026-10-01"]["accounts"][id.to_string()]["error_code"],
        "query_budget"
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 6);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    harness.runtime.monitor.deliver().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 0);

    let depleted = harness
        .seed_account("plus", "fixture-daily-reset-wait", json!({}))
        .await;
    let depleted_id = integer(&depleted["id"]).unwrap();
    let reset_at = harness.now() + Duration::hours(2);
    let old = cached_result(
        &depleted,
        harness.now() - Duration::hours(2),
        json!({
            "five_hour":{"utilization":100,"resets_at":datetime(reset_at)},
            "seven_day":{"utilization":100,"resets_at":datetime(reset_at + Duration::days(1))}
        }),
    );
    save_cached(&harness, depleted_id, old).await;
    harness
        .runtime
        .monitor
        .run_daily(
            json!({"date":"2026-10-02","scheduled_at":datetime(harness.now()),"status":"running","accounts":{}}),
            vec![depleted],
        )
        .await
        .unwrap();
    let state = harness.runtime.monitor.state().unwrap();
    assert_eq!(
        state["daily_test"]["batches"]["2026-10-02"]["accounts"][depleted_id.to_string()]["status"],
        "skipped"
    );
    assert_eq!(
        state["daily_test"]["batches"]["2026-10-02"]["accounts"][depleted_id.to_string()]
            ["error_code"],
        "waiting_reset"
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 6);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    harness.runtime.monitor.deliver().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    harness.close().await;
}

#[tokio::test]
async fn runtime_tick_runs_a_due_daily_batch_once_without_catching_up() {
    let harness = Harness::new().await;
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    let passive = json!({
        "codex_usage_updated_at":datetime(due - Duration::minutes(10)),
        "codex_5h_used_percent":20,
        "codex_5h_reset_at":datetime(due + Duration::hours(5)),
        "codex_7d_used_percent":30,
        "codex_7d_reset_at":datetime(due + Duration::days(7))
    });
    let row = harness
        .seed_account("plus", "fixture-daily-runtime-tick", passive)
        .await;
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_daily_test_enabled"] = true.into();
            config["oauth_daily_test_time"] = "05:00".into();
            Ok(())
        })
        .unwrap();
    harness.set_now(due - Duration::hours(1));
    harness.runtime.monitor.startup().await.unwrap();
    harness.set_now(due);
    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("POST", "/test", 1).await;
    for _ in 0..100 {
        if harness.runtime.monitor.state().unwrap()["daily_test"]["batches"]["2026-09-30"]["status"]
            == "completed"
        {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    harness.set_now(due + Duration::seconds(1));
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["daily_test"]["batches"]["2026-09-30"]["status"],
        "completed"
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(integer(&row["id"]).unwrap() > 0);
    harness.close().await;
}

#[tokio::test]
async fn daily_cycle_lock_stays_queued_and_runs_after_due_minute() {
    let harness = Harness::new().await;
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    let local_date = due
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive()
        .to_string();
    let passive = json!({
        "codex_usage_updated_at":datetime(due - Duration::minutes(10)),
        "codex_5h_used_percent":20,
        "codex_5h_reset_at":datetime(due + Duration::hours(5)),
        "codex_7d_used_percent":30,
        "codex_7d_reset_at":datetime(due + Duration::days(7))
    });
    let row = harness
        .seed_account("plus", "fixture-daily-busy-lock", passive)
        .await;
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_daily_test_enabled"] = true.into();
            config["oauth_daily_test_time"] = "05:00".into();
            config["oauth_recovery_monitor_enabled"] = false.into();
            Ok(())
        })
        .unwrap();
    harness.set_now(due - Duration::seconds(1));

    let mut blocker = harness.runtime.db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE accounts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let monitor = harness.runtime.monitor.clone();
    let blocked_tick = tokio::spawn(async move { monitor.tick().await });
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND wait_event_type='Lock' AND query ILIKE '%FROM accounts%'",
            )
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first monitor cycle must hold its cycle lock while inventory waits on PostgreSQL");

    harness.set_now(due);
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["daily_test"]["batches"][&local_date]["status"],
        "queued",
        "a due batch must be registered even while another whole monitor cycle owns the lock"
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);

    blocker.commit().await.unwrap();
    blocked_tick.await.unwrap().unwrap();
    harness.mock.wait_for("POST", "/test", 1).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if harness.runtime.monitor.state().unwrap()["daily_test"]["batches"][&local_date]
                ["status"]
                == "completed"
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("the queued daily account must run once after the active cycle releases its lock");
    harness.set_now(due + Duration::minutes(3));
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(integer(&row["id"]).unwrap() > 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn daily_tick_reuses_only_same_cycle_recovery_test_for_a_still_eligible_account() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    let local_date = due
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive()
        .to_string();

    for becomes_unschedulable in [false, true] {
        let harness = Harness::new().await;
        let row = harness
            .seed_account(
                "plus",
                if becomes_unschedulable {
                    "fixture-daily-recovery-ineligible"
                } else {
                    "fixture-daily-recovery-reuse"
                },
                json!({}),
            )
            .await;
        let id = integer(&row["id"]).unwrap();
        prepare_due_recovery_daily(&harness, &row, due).await;
        if becomes_unschedulable {
            harness.mock.mark_unschedulable_on_test(id);
        }

        let gate = Gate::new();
        *harness.mock.test_gate.lock().unwrap() = Some(gate.clone());
        harness.runtime.monitor.tick().await.unwrap();
        tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
            .await
            .expect("same-cycle recovery verification reached the loopback model test");
        assert_eq!(harness.mock.count("GET", "/usage"), 1);
        assert_eq!(harness.mock.count("POST", "/test"), 1);
        gate.release.notify_one();

        tokio::time::timeout(StdDuration::from_secs(8), async {
            loop {
                if harness.runtime.monitor.state().unwrap()["daily_test"]["batches"][&local_date]
                    ["status"]
                    == "completed"
                {
                    break;
                }
                tokio::time::sleep(StdDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("daily record must commit after the same-cycle recovery worker finishes");

        let state = harness.runtime.monitor.state().unwrap();
        let record = &state["daily_test"]["batches"][&local_date]["accounts"][id.to_string()];
        if becomes_unschedulable {
            assert_eq!(record["status"], "skipped");
            assert_eq!(record["reused_recovery_test"], false);
            assert_eq!(
                harness.runtime.monitor.row(id).await.unwrap()["schedulable"],
                false
            );
            assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
        } else {
            assert_eq!(record["status"], "success");
            assert_eq!(record["reused_recovery_test"], true);
            assert_eq!(harness.mock.count("POST", "/recover-state"), 1);
            assert_eq!(
                state["scheduler"][id.to_string()]["recovery_intent"]["status"],
                "recovered"
            );
        }
        assert_eq!(record["model_id"], "gpt-5.6-luna");
        assert_eq!(harness.mock.count("GET", "/usage"), 1);
        assert_eq!(harness.mock.count("POST", "/test"), 1);
        assert_eq!(harness.mock.count("POST", "/push"), 0);
        assert!(harness.mock.unexpected().is_empty());
        harness.close().await;
    }
}

#[tokio::test]
async fn retired_probe_interval_is_accepted_but_never_saved_or_exposed() {
    let harness = Harness::new().await;
    let before = harness.runtime.config.snapshot(Some("oauth")).unwrap();
    let result = harness
        .runtime
        .config
        .save(
            "oauth",
            &json!({
                "oauth_7d_probe_interval_seconds":60,
                "oauth_daily_test_time":"06:15"
            }),
            "fixture-old-client",
            before["oauth"]["revision"].as_str(),
        )
        .await
        .unwrap();
    assert_eq!(result["oauth_daily_test_time"], "06:15");
    assert!(result.get("oauth_7d_probe_interval_seconds").is_none());
    let saved: Value =
        serde_json::from_slice(&fs::read(&harness.runtime.settings.oauth_path).unwrap()).unwrap();
    assert!(saved.get("oauth_7d_probe_interval_seconds").is_none());
    assert_eq!(
        harness
            .runtime
            .config
            .current()
            .unwrap()
            .get("oauth_7d_probe_interval_seconds"),
        None
    );
    assert_eq!(
        harness.runtime.config.current().unwrap()["oauth_daily_test_time"],
        "06:15"
    );
    assert!(harness.mock.calls().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn monitor_tick_reuses_passive_state_and_dispatches_one_bootstrap_query() {
    let harness = Harness::new().await;
    let now = harness.now();
    let passive = json!({
        "codex_usage_updated_at":datetime(now - Duration::minutes(10)),
        "codex_5h_used_percent":20,
        "codex_5h_reset_at":datetime(now + Duration::hours(5)),
        "codex_7d_used_percent":30,
        "codex_7d_reset_at":datetime(now + Duration::days(7))
    });
    let passive_row = harness
        .seed_account("plus", "fixture-monitor-passive", passive)
        .await;
    let bootstrap = harness
        .seed_account("plus", "fixture-monitor-bootstrap", json!({}))
        .await;
    let passive_id = integer(&passive_row["id"]).unwrap();
    let bootstrap_id = integer(&bootstrap["id"]).unwrap();
    harness.runtime.monitor.startup().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("GET", "/usage", 1).await;
    for _ in 0..100 {
        if harness.runtime.monitor.state().unwrap()["oauth_results"][bootstrap_id.to_string()]
            ["success"]
            == true
        {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    harness.set_now(now + Duration::seconds(1));
    harness.runtime.monitor.tick().await.unwrap();
    let calls = harness.mock.calls();
    let usage_calls = calls
        .iter()
        .filter(|call| {
            call.method == "GET" && call.path.ends_with("/usage?source=active&force=true")
        })
        .collect::<Vec<_>>();
    assert_eq!(usage_calls.len(), 1);
    assert!(usage_calls[0]
        .path
        .contains(&format!("/accounts/{bootstrap_id}/usage")));
    assert_eq!(usage_calls[0].api_key.as_deref(), Some(ADMIN_KEY));
    assert!(usage_calls[0].authorization.is_none());
    assert!(
        harness.runtime.monitor.state().unwrap()["oauth_results"][passive_id.to_string()].is_null()
    );
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn monitor_bootstrap_queue_is_bounded_and_advances_between_ticks() {
    let harness = Harness::new().await;
    let mut ids = Vec::new();
    for index in 0..3 {
        let row = harness
            .seed_account(
                "plus",
                &format!("fixture-bootstrap-queue-{index}"),
                json!({}),
            )
            .await;
        ids.push(integer(&row["id"]).unwrap());
    }
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_usage_refresh_concurrency"] = 2.into();
            config["oauth_recovery_test_concurrency"] = 0.into();
            config["oauth_recovery_connection_account_ids"] = json!(ids);
            config["oauth_recovery_model_account_ids"] = json!([]);
            Ok(())
        })
        .unwrap();
    harness.runtime.monitor.startup().await.unwrap();

    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("GET", "/usage", 2).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let state = harness.runtime.monitor.state().unwrap();
            if ids[..2]
                .iter()
                .all(|id| state["oauth_results"][id.to_string()]["success"] == true)
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("the bounded first wave must commit both loopback results");
    assert_eq!(harness.mock.count("GET", "/usage"), 2);

    harness.set_now(harness.now() + Duration::seconds(1));
    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("GET", "/usage", 3).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if harness.runtime.monitor.state().unwrap()["oauth_results"][ids[2].to_string()]
                ["success"]
                == true
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("the next tick must advance the final queued bootstrap account");
    let usage_ids = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| {
            call.method == "GET" && call.path.ends_with("/usage?source=active&force=true")
        })
        .filter_map(|call| {
            call.path
                .split("/accounts/")
                .nth(1)
                .and_then(|value| value.split('/').next())
                .and_then(|value| value.parse::<i64>().ok())
        })
        .collect::<Vec<_>>();
    assert_eq!(usage_ids, ids);
    assert_eq!(harness.mock.count("GET", "/usage"), 3);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn fresh_seven_day_exhaustion_blocks_model_verification_until_reset() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-monitor-seven-day-wait", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    prepare_expired_recovery(&harness, &row, due).await;
    harness.set_now(due - Duration::hours(1));
    harness.runtime.monitor.startup().await.unwrap();
    harness.set_now(due);
    let mut depleted = default_usage(due);
    depleted["seven_day"]["utilization"] = json!(100);
    depleted["seven_day"]["resets_at"] = json!(datetime(due + Duration::days(7)));
    harness.mock.set_usage(200, Some(depleted));

    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("GET", "/usage", 1).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let state = harness.runtime.monitor.state().unwrap();
            let usage = &state["oauth_results"][id.to_string()];
            if usage["success"] == true && usage["queried_at"] == datetime(due) {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("the fresh usage result must persist before checking the legacy no-test behavior");
    let state = harness.runtime.monitor.state().unwrap();
    assert_eq!(
        state["scheduler"][id.to_string()]["recovery_intent"]["status"],
        "pending",
        "a fresh future seven-day reset may leave an intent pending"
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert!(harness.runtime.monitor.state().unwrap()["pending_events"]
        .as_object()
        .unwrap()
        .is_empty());
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn cached_seven_day_exhaustion_does_not_probe_before_future_reset() {
    let harness = Harness::new().await;
    let now = harness.now();
    let run_at = now + Duration::seconds(60);
    let row = harness
        .seed_account("plus", "fixture-monitor-cached-seven-day", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    sqlx::query(
        "UPDATE accounts SET rate_limited_at=$1,rate_limit_reset_at=$2,updated_at=$3 WHERE id=$4",
    )
    .bind(now - Duration::minutes(5))
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(&harness.runtime.db.pool)
    .await
    .unwrap();
    let mut cached = default_usage(now - Duration::hours(2));
    cached["five_hour"]["utilization"] = json!(20);
    cached["five_hour"]["resets_at"] = json!(datetime(now + Duration::hours(1)));
    cached["seven_day"]["utilization"] = json!(100);
    cached["seven_day"]["resets_at"] = json!(datetime(now + Duration::days(4)));
    let prior = cached_result(&row, now - Duration::hours(2), cached);
    save_cached(&harness, id, prior.clone()).await;
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_recovery_monitor_enabled"] = true.into();
            config["oauth_recovery_connection_account_ids"] = json!([id]);
            config["oauth_recovery_model_account_ids"] = json!([]);
            Ok(())
        })
        .unwrap();

    harness.runtime.monitor.startup().await.unwrap();
    harness.set_now(run_at);
    harness.runtime.monitor.tick().await.unwrap();
    tokio::time::sleep(StdDuration::from_millis(150)).await;

    let state = harness.runtime.monitor.state().unwrap();
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert_eq!(state["oauth_results"][id.to_string()], prior);
    assert!(state["pending_events"].as_object().unwrap().is_empty());
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn threshold_block_still_above_threshold_waits_one_hour() {
    let harness = Harness::new().await;
    let now = harness.now();
    let due = now + Duration::seconds(60);
    let row = harness
        .seed_account("plus", "fixture-monitor-threshold-still-blocked", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    let reason = json!({
        "source":"account_scheduling_threshold",
        "platform":"openai",
        "window":"5h",
        "threshold_percent":90,
        "used_percent":95,
        "until_unix":now.timestamp(),
        "error_message":"fixture threshold reached"
    });
    sqlx::query("UPDATE accounts SET temp_unschedulable_until=$1,temp_unschedulable_reason=$2,updated_at=$3 WHERE id=$4")
        .bind(now)
        .bind(serde_json::to_string(&reason).unwrap())
        .bind(now)
        .bind(id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let mut cached = default_usage(now - Duration::hours(2));
    cached["five_hour"]["utilization"] = json!(95);
    cached["five_hour"]["resets_at"] = json!(datetime(now + Duration::hours(5)));
    cached["seven_day"]["utilization"] = json!(30);
    cached["seven_day"]["resets_at"] = json!(datetime(now + Duration::days(7)));
    save_cached(
        &harness,
        id,
        cached_result(&row, now - Duration::hours(2), cached.clone()),
    )
    .await;
    harness.mock.set_usage(200, Some(cached));
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_recovery_monitor_enabled"] = true.into();
            config["oauth_recovery_connection_account_ids"] = json!([id]);
            config["oauth_recovery_model_account_ids"] = json!([]);
            Ok(())
        })
        .unwrap();
    harness.runtime.monitor.startup().await.unwrap();
    harness.set_now(due);

    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("GET", "/usage", 1).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let state = harness.runtime.monitor.state().unwrap();
            if state["scheduler"][id.to_string()]["recovery_intent"]["status"] == "waiting_quota" {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("an unreleased scheduling threshold must enter quota wait after the active query");

    harness.set_now(due + Duration::seconds(3599));
    harness.runtime.monitor.tick().await.unwrap();
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 0);

    harness.set_now(due + Duration::seconds(3600));
    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("GET", "/usage", 2).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let state = harness.runtime.monitor.state().unwrap();
            let intent = &state["scheduler"][id.to_string()]["recovery_intent"];
            if intent["status"] == "waiting_quota"
                && intent["next_retry_at"] == datetime(due + Duration::seconds(7200))
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("second threshold refresh must preserve the one-hour retry boundary");
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"]
            ["next_retry_at"],
        datetime(due + Duration::seconds(7200))
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn due_quota_still_depleted_waits_one_hour() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    let harness = Harness::new().await;
    let row = harness
        .seed_account(
            "plus",
            "fixture-monitor-due-quota-still-depleted",
            json!({}),
        )
        .await;
    let id = integer(&row["id"]).unwrap();
    prepare_expired_recovery(&harness, &row, due).await;
    let mut depleted = default_usage(due);
    depleted["five_hour"]["utilization"] = json!(100);
    depleted["five_hour"]["resets_at"] = json!(datetime(due));
    harness.mock.set_usage(200, Some(depleted));
    harness.set_now(due - Duration::hours(1));
    harness.runtime.monitor.startup().await.unwrap();
    harness.set_now(due);

    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("GET", "/usage", 1).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let state = harness.runtime.monitor.state().unwrap();
            let intent = &state["scheduler"][id.to_string()]["recovery_intent"];
            if intent["status"] == "waiting_quota"
                && intent["next_retry_at"] == datetime(due + Duration::seconds(3600))
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("still depleted 5h quota must retry at the one-hour deadline");

    harness.set_now(due + Duration::seconds(3599));
    harness.runtime.monitor.tick().await.unwrap();
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 0);

    harness.set_now(due + Duration::seconds(3600));
    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for("GET", "/usage", 2).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let state = harness.runtime.monitor.state().unwrap();
            let intent = &state["scheduler"][id.to_string()]["recovery_intent"];
            if intent["status"] == "waiting_quota"
                && intent["next_retry_at"] == datetime(due + Duration::seconds(7200))
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("still depleted retry must move forward by one hour without a model test");
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"]
            ["next_retry_at"],
        datetime(due + Duration::seconds(7200))
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn recovery_disabled_tick_refreshes_inventory_without_upstream_requests() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-recovery-disabled-inventory", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_recovery_monitor_enabled"] = false.into();
            config["oauth_recovery_connection_account_ids"] = json!([id]);
            config["oauth_recovery_model_account_ids"] = json!([]);
            Ok(())
        })
        .unwrap();

    let mut blocker = harness.runtime.db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE accounts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let monitor = harness.runtime.monitor.clone();
    let tick = tokio::spawn(async move { monitor.tick().await });
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND wait_event_type='Lock' AND query ILIKE '%FROM accounts%'",
            )
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("recovery-disabled tick must read the live OAuth inventory");
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    blocker.commit().await.unwrap();
    tick.await.unwrap().unwrap();
    let inventory = harness.runtime.monitor.inventory(false).await.unwrap();
    assert!(inventory
        .iter()
        .any(|account| integer(&account["id"]) == Some(id)));
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn idle_monitor_ticks_do_not_repeat_upstream_requests() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-idle-monitor-ticks", json!({}))
        .await;
    harness.runtime.monitor.startup().await.unwrap();
    harness.set_now(reference_time());
    harness.runtime.monitor.tick().await.unwrap();
    #[cfg(target_os = "linux")]
    let state_reads = state_read_watch::Watcher::new(&harness.runtime.settings.usage_path)
        .expect("watch only the synthetic OAuth state file on Linux");

    // The legacy idle-tick contract avoids a second inventory query inside the
    // 60-second cache window. Locking the real PostgreSQL table turns a hidden
    // projection reread into a bounded, observable stall.
    let mut blocker = harness.runtime.db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE accounts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    harness.set_now(reference_time() + Duration::seconds(2));
    let monitor = harness.runtime.monitor.clone();
    let mut tick = tokio::spawn(async move { monitor.tick().await });
    let tick_before_unlock = tokio::time::timeout(StdDuration::from_millis(500), &mut tick).await;
    let completed_while_locked = tick_before_unlock.is_ok();
    blocker.commit().await.unwrap();
    match tick_before_unlock {
        Ok(result) => result.expect("idle monitor tick task must finish").unwrap(),
        Err(_) => tick
            .await
            .expect("idle monitor tick task must finish after releasing the test lock")
            .unwrap(),
    }
    assert!(
        completed_while_locked,
        "an idle tick inside the inventory cache window must not query accounts"
    );
    #[cfg(target_os = "linux")]
    state_reads
        .assert_no_access()
        .expect("an idle tick inside the state cache window must not open or read OAuth state");

    harness.set_now(reference_time() + Duration::hours(2));
    harness.runtime.monitor.tick().await.unwrap();

    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert!(integer(&row["id"]).unwrap() > 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn live_monitor_reserves_request_time_after_blocked_inventory_read() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-inventory-request-time", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_recovery_connection_account_ids"] = json!([id]);
            config["oauth_recovery_model_account_ids"] = json!([]);
            Ok(())
        })
        .unwrap();

    let mut blocker = harness.runtime.db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE accounts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let monitor = harness.runtime.monitor.clone();
    let tick = tokio::spawn(async move { monitor.tick().await });
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND wait_event_type='Lock' AND query ILIKE '%FROM accounts%'",
            )
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("monitor must be blocked in the real PostgreSQL account inventory query");

    let requested = harness.now() + Duration::minutes(10);
    harness.set_now(requested);
    blocker.commit().await.unwrap();
    tick.await.unwrap().unwrap();
    harness.mock.wait_for("GET", "/usage", 1).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if harness.runtime.monitor.state().unwrap()["oauth_results"][id.to_string()]["success"]
                == true
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("the monitor's admitted request must finish against loopback");

    let state = harness.runtime.monitor.state().unwrap();
    let query = &state["scheduler"][id.to_string()]["quota_query"];
    assert_eq!(query["last_query_at"], datetime(requested));
    assert_eq!(query["automatic_attempts"], json!([datetime(requested)]));
    assert_eq!(
        state["oauth_results"][id.to_string()]["queried_at"],
        datetime(requested)
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn cached_inventory_rechecks_live_eligibility_before_any_scheduler_request() {
    let due = parse_datetime(&fixture()["daily"]["next_at_0500_beijing"]).unwrap();
    for change in ["unschedulable", "paused", "deleted", "apikey"] {
        let harness = Harness::new().await;
        let row = harness
            .seed_account(
                "plus",
                &format!("fixture-live-ineligible-{change}"),
                json!({}),
            )
            .await;
        let id = integer(&row["id"]).unwrap();
        prepare_expired_recovery(&harness, &row, due).await;
        harness.set_now(due - Duration::hours(1));
        harness.runtime.monitor.startup().await.unwrap();

        let updated_at = harness.now();
        let query = match change {
            "unschedulable" => "UPDATE accounts SET schedulable=false,updated_at=$1 WHERE id=$2",
            "paused" => "UPDATE accounts SET status='paused',updated_at=$1 WHERE id=$2",
            "deleted" => "UPDATE accounts SET deleted_at=$1,updated_at=$1 WHERE id=$2",
            "apikey" => "UPDATE accounts SET type='apikey',updated_at=$1 WHERE id=$2",
            _ => unreachable!(),
        };
        sqlx::query(query)
            .bind(updated_at)
            .bind(id)
            .execute(&harness.runtime.db.pool)
            .await
            .unwrap();

        harness.set_now(due);
        harness.runtime.monitor.tick().await.unwrap();
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        assert_eq!(harness.mock.count("GET", "/usage"), 0, "{change}");
        assert_eq!(harness.mock.count("POST", "/test"), 0, "{change}");
        assert_eq!(harness.mock.count("POST", "/recover-state"), 0, "{change}");
        assert!(harness.mock.unexpected().is_empty(), "{change}");
        harness.close().await;
    }
}

#[tokio::test]
async fn fallback_skips_an_uncommitted_force_refresh_snapshot_and_monitor_tick() {
    let harness = Harness::new().await;
    let oauth = harness
        .seed_account("plus", "fixture-snapshot-cycle", json!({}))
        .await;
    let key = harness.seed_key(false).await;
    let oauth_id = integer(&oauth["id"]).unwrap();
    let key_id = integer(&key["id"]).unwrap();
    harness.enable_openai_fallback(key_id).await;
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["oauth_results"][oauth_id.to_string()] = json!({
                "account_id":oauth_id,
                "template_type":"oauth",
                "success":false,
                "queried_at":datetime(harness.now()),
                "error_code":"http_401"
            });
            Ok(())
        })
        .unwrap();
    let gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());
    let monitor = harness.runtime.monitor.clone();
    let refresh = tokio::spawn(async move { monitor.force_refresh().await.unwrap() });
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("force refresh must hold its cycle while the loopback request is pending");
    let report = harness.runtime.monitor.fallback().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(report["platform_reasons"]["openai"], "refresh_in_progress");
    assert_eq!(report["desired_by_platform"]["openai"], Value::Null);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 0);
    gate.release.notify_one();
    let refresh = tokio::time::timeout(StdDuration::from_secs(8), refresh)
        .await
        .expect("finish gated loopback force refresh")
        .unwrap();
    assert_eq!(refresh["success"], true);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    harness.close().await;
}

#[tokio::test]
async fn fallback_write_excludes_monitor_tick_until_schedulable_response() {
    let harness = Harness::new().await;
    let oauth = harness
        .seed_account("plus", "fixture-fallback-write", json!({}))
        .await;
    let key = harness.seed_key(false).await;
    let oauth_id = integer(&oauth["id"]).unwrap();
    let key_id = integer(&key["id"]).unwrap();
    harness.enable_openai_fallback(key_id).await;
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["oauth_results"][oauth_id.to_string()] = json!({
                "account_id":oauth_id,
                "template_type":"oauth",
                "success":false,
                "queried_at":datetime(harness.now()),
                "error_code":"http_401"
            });
            Ok(())
        })
        .unwrap();
    harness.runtime.monitor.startup().await.unwrap();
    let gate = Gate::new();
    *harness.mock.schedulable_gate.lock().unwrap() = Some(gate.clone());
    let monitor = harness.runtime.monitor.clone();
    let fallback = tokio::spawn(async move { monitor.fallback().await.unwrap() });
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("fallback must reach the synthetic schedulable endpoint");
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 1);
    gate.release.notify_one();
    let report = tokio::time::timeout(StdDuration::from_secs(5), fallback)
        .await
        .expect("fallback write returns after its loopback response")
        .unwrap();
    assert_eq!(report["changed_ids"], json!([key_id]));
    assert_eq!(
        harness.runtime.monitor.row(key_id).await.unwrap()["schedulable"],
        true
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 1);
    harness.close().await;
}

#[tokio::test]
async fn fallback_ignores_uncommitted_snapshot_while_monitor_worker_is_active() {
    let harness = Harness::new().await;
    let oauth = harness
        .seed_account("plus", "fixture-fallback-worker", json!({}))
        .await;
    let key = harness.seed_key(false).await;
    let oauth_id = integer(&oauth["id"]).unwrap();
    let key_id = integer(&key["id"]).unwrap();
    harness.enable_openai_fallback(key_id).await;
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_recovery_monitor_enabled"] = true.into();
            config["oauth_recovery_connection_account_ids"] = json!([oauth_id]);
            config["oauth_recovery_model_account_ids"] = json!([]);
            Ok(())
        })
        .unwrap();
    harness.runtime.monitor.startup().await.unwrap();
    let gate = Gate::new();
    *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());
    harness.runtime.monitor.tick().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("monitor worker must dispatch exactly one loopback quota request");
    let report = harness.runtime.monitor.fallback().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(report["platform_reasons"]["openai"], "refresh_in_progress");
    assert_eq!(report["desired_by_platform"]["openai"], Value::Null);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 0);
    gate.release.notify_one();
    *harness.mock.usage_gate.lock().unwrap() = None;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let current = harness.runtime.monitor.state().unwrap()["oauth_results"]
                [oauth_id.to_string()]["success"]
                .clone();
            if current == true {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker completes the reserved quota request");
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 0);
    harness.close().await;
}

#[tokio::test]
async fn grok_fallback_runs_without_openai_monitor_snapshot_when_oauth_state_is_valid() {
    let harness = Harness::new().await;
    let _openai_oauth = harness
        .seed_account("plus", "fixture-grok-isolation-openai", json!({}))
        .await;
    let openai_key = harness.seed_key(true).await;
    let openai_key_id = integer(&openai_key["id"]).unwrap();

    let grok_oauth_id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
    let grok_key_id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
    harness.track_account_id(grok_oauth_id);
    harness.track_account_id(grok_key_id);
    let now = harness.now();
    sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,updated_at) VALUES ($1,'fixture-grok-oauth','grok','oauth','active',true,$2,$3,$4)")
        .bind(grok_oauth_id)
        .bind(SqlJson(json!({"plan_type":"plus","access_token":"fixture-grok-oauth-token","refresh_token":"fixture-grok-refresh"})))
        .bind(SqlJson(json!({"grok_needs_reauth":true})))
        .bind(now)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("insert a synthetic Grok OAuth row in temporary PostgreSQL");
    sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,updated_at) VALUES ($1,'fixture-grok-key','grok','apikey','active',false,$2,'{}'::jsonb,$3)")
        .bind(grok_key_id)
        .bind(SqlJson(json!({"api_key":"fixture-grok-key-material"})))
        .bind(now)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("insert a synthetic Grok API key in temporary PostgreSQL");

    harness
        .runtime
        .config
        .fallback
        .save(
            &json!({
                "openai_enabled":true,
                "grok_enabled":true,
                "managed_account_ids":[openai_key_id,grok_key_id]
            }),
            "fixture-oauth-scheduler",
        )
        .await
        .unwrap();

    assert!(harness.runtime.config.state.admin_token().is_ok());
    let oauth_state_before = fs::read(&harness.runtime.settings.usage_path).unwrap();
    let report = harness
        .runtime
        .config
        .fallback
        .run(harness.now(), None, |_| Ok(false))
        .await
        .unwrap();

    assert_eq!(report["platform_reasons"]["openai"], "refresh_in_progress");
    assert_eq!(report["desired_by_platform"]["openai"], Value::Null);
    assert_eq!(
        report["grok_oauth_states"][grok_oauth_id.to_string()],
        "unavailable"
    );
    assert_eq!(report["desired_by_platform"]["grok"], true);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 1);
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(
        harness.runtime.monitor.row(openai_key_id).await.unwrap()["schedulable"],
        true
    );
    assert_eq!(
        harness.runtime.monitor.row(grok_key_id).await.unwrap()["schedulable"],
        true
    );
    let writes = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| call.method == "POST" && call.path.ends_with("/schedulable"))
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 1);
    assert!(writes[0]
        .path
        .ends_with(&format!("/api/v1/admin/accounts/{grok_key_id}/schedulable")));
    assert_eq!(writes[0].body["schedulable"], true);
    assert_eq!(writes[0].api_key.as_deref(), Some(ADMIN_KEY));
    assert_eq!(writes[0].authorization, None);
    assert_eq!(
        fs::read(&harness.runtime.settings.usage_path).unwrap(),
        oauth_state_before
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn corrupt_oauth_state_prevents_fallback_write_without_cached_admin_key() {
    let harness = Harness::new().await;
    let openai_key = harness.seed_key(true).await;
    let openai_key_id = integer(&openai_key["id"]).unwrap();
    let grok_oauth_id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
    let grok_key_id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
    harness.track_account_id(grok_oauth_id);
    harness.track_account_id(grok_key_id);
    let now = harness.now();
    sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,updated_at) VALUES ($1,'fixture-grok-oauth','grok','oauth','active',true,$2,$3,$4)")
        .bind(grok_oauth_id)
        .bind(SqlJson(json!({"plan_type":"plus","access_token":"fixture-grok-oauth-token","refresh_token":"fixture-grok-refresh"})))
        .bind(SqlJson(json!({"grok_needs_reauth":true})))
        .bind(now)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,updated_at) VALUES ($1,'fixture-grok-key','grok','apikey','active',false,$2,'{}'::jsonb,$3)")
        .bind(grok_key_id)
        .bind(SqlJson(json!({"api_key":"fixture-grok-key-material"})))
        .bind(now)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    harness
        .runtime
        .config
        .fallback
        .save(
            &json!({
                "openai_enabled":true,
                "grok_enabled":true,
                "managed_account_ids":[openai_key_id,grok_key_id]
            }),
            "fixture-oauth-scheduler",
        )
        .await
        .unwrap();

    assert!(harness.runtime.config.state.admin_token().is_ok());
    let corrupt = b"{synthetic-invalid-oauth-state";
    fs::write(&harness.runtime.settings.usage_path, corrupt).unwrap();
    assert!(harness.runtime.config.state.admin_token().is_err());
    let result = harness
        .runtime
        .config
        .fallback
        .run(harness.now(), None, |_| Ok(false))
        .await;
    let error = result.expect_err("fallback writes must fail closed without readable admin state");
    assert_eq!(error.status.as_u16(), 503);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 0);
    assert_eq!(
        harness.runtime.monitor.row(grok_key_id).await.unwrap()["schedulable"],
        false
    );
    assert_eq!(
        fs::read(&harness.runtime.settings.usage_path).unwrap(),
        corrupt
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn force_refresh_latest_failures_drive_fallback_only_when_fresh_and_known() {
    let harness = Harness::new().await;
    let oauth = harness
        .seed_account("plus", "fixture-fallback-401-prior", json!({}))
        .await;
    let key = harness.seed_key(true).await;
    let oauth_id = integer(&oauth["id"]).unwrap();
    let key_id = integer(&key["id"]).unwrap();
    harness.enable_openai_fallback(key_id).await;
    let initial = harness
        .runtime
        .monitor
        .queries
        .query(
            oauth_id,
            ADMIN_KEY,
            query_options(Source::Manual, harness.now()),
        )
        .await;
    assert_eq!(initial["success"], true);
    harness.set_now(harness.now() + Duration::minutes(1));
    harness.mock.set_usage(401, None);
    let refresh = harness.runtime.monitor.force_refresh().await.unwrap();
    let diagnostic = force_refresh_diagnostic(
        &refresh,
        &harness.runtime.monitor.state().unwrap(),
        oauth_id,
    );
    assert_eq!(refresh["success"], false, "HTTP 401 report: {diagnostic}");
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    let report = harness.runtime.monitor.fallback().await.unwrap();
    assert_eq!(report["oauth_states"][oauth_id.to_string()], "unavailable");
    assert_eq!(report["desired"], true);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    harness.close().await;

    let harness = Harness::new().await;
    let oauth = harness
        .seed_account("plus", "fixture-fallback-402-empty", json!({}))
        .await;
    let key = harness.seed_key(false).await;
    let oauth_id = integer(&oauth["id"]).unwrap();
    let key_id = integer(&key["id"]).unwrap();
    harness.enable_openai_fallback(key_id).await;
    harness.mock.set_usage(402, None);
    let refresh = harness.runtime.monitor.force_refresh().await.unwrap();
    let diagnostic = force_refresh_diagnostic(
        &refresh,
        &harness.runtime.monitor.state().unwrap(),
        oauth_id,
    );
    assert_eq!(refresh["success"], false, "HTTP 402 report: {diagnostic}");
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    let report = harness.runtime.monitor.fallback().await.unwrap();
    assert_eq!(report["oauth_states"][oauth_id.to_string()], "unavailable");
    assert_eq!(report["desired"], true);
    let writes = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| call.method == "POST" && call.path.ends_with("/schedulable"))
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].body["schedulable"], true);
    assert_eq!(
        harness.runtime.monitor.row(key_id).await.unwrap()["schedulable"],
        true
    );
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    harness.close().await;

    for failure_kind in ["timeout", "network_error", "http_500"] {
        let mut harness = Harness::new().await;
        let oauth = harness
            .seed_account(
                "plus",
                &format!("fixture-fallback-{failure_kind}"),
                json!({}),
            )
            .await;
        let key = harness.seed_key(true).await;
        let oauth_id = integer(&oauth["id"]).unwrap();
        let key_id = integer(&key["id"]).unwrap();
        harness.enable_openai_fallback(key_id).await;
        let initial = harness
            .runtime
            .monitor
            .queries
            .query(
                oauth_id,
                ADMIN_KEY,
                query_options(Source::Manual, harness.now()),
            )
            .await;
        assert_eq!(initial["success"], true);
        harness.set_now(harness.now() + Duration::minutes(1));
        let refresh = match failure_kind {
            "timeout" => {
                let gate = Gate::new();
                *harness.mock.usage_gate.lock().unwrap() = Some(gate.clone());
                let monitor = harness.runtime.monitor.clone();
                let refresh = tokio::spawn(async move { monitor.force_refresh().await.unwrap() });
                tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
                    .await
                    .expect("timed request reaches the loopback server");
                let refresh = tokio::time::timeout(StdDuration::from_secs(16), refresh)
                    .await
                    .expect("real upstream timeout terminates the force-refresh request")
                    .unwrap();
                gate.release.notify_one();
                *harness.mock.usage_gate.lock().unwrap() = None;
                assert_eq!(
                    harness.runtime.monitor.state().unwrap()["scheduler"][oauth_id.to_string()]
                        ["last_error_code"],
                    "timeout"
                );
                refresh
            }
            "network_error" => {
                harness.stop_upstream().await;
                let refresh = harness.runtime.monitor.force_refresh().await.unwrap();
                assert_eq!(
                    harness.runtime.monitor.state().unwrap()["scheduler"][oauth_id.to_string()]
                        ["last_error_code"],
                    "network_error"
                );
                refresh
            }
            "http_500" => {
                harness.mock.set_usage(500, None);
                let refresh = harness.runtime.monitor.force_refresh().await.unwrap();
                assert_eq!(
                    harness.runtime.monitor.state().unwrap()["scheduler"][oauth_id.to_string()]
                        ["last_error_code"],
                    "http_500"
                );
                refresh
            }
            _ => unreachable!(),
        };
        let diagnostic = force_refresh_diagnostic(
            &refresh,
            &harness.runtime.monitor.state().unwrap(),
            oauth_id,
        );
        assert_eq!(
            refresh["success"], false,
            "{failure_kind} is an unsuccessful forced refresh: {diagnostic}"
        );
        let report = harness.runtime.monitor.fallback().await.unwrap();
        assert_eq!(
            report["oauth_states"][oauth_id.to_string()],
            "unknown",
            "{failure_kind}"
        );
        assert_eq!(report["desired"], Value::Null, "{failure_kind}");
        assert_eq!(
            harness.mock.count("POST", "/schedulable"),
            0,
            "{failure_kind}"
        );
        assert_eq!(harness.mock.count("POST", "/test"), 0, "{failure_kind}");
        assert_eq!(
            harness.runtime.monitor.row(key_id).await.unwrap()["schedulable"],
            true,
            "{failure_kind} must leave the selected fallback key untouched"
        );
        if matches!(failure_kind, "timeout" | "http_500") {
            assert_eq!(harness.mock.count("GET", "/usage"), 2);
        } else {
            assert_eq!(harness.mock.count("GET", "/usage"), 1);
        }
        harness.close().await;
    }

    let harness = Harness::new().await;
    let oauth = harness
        .seed_account("plus", "fixture-fallback-success-after-failure", json!({}))
        .await;
    let key = harness.seed_key(true).await;
    let oauth_id = integer(&oauth["id"]).unwrap();
    let key_id = integer(&key["id"]).unwrap();
    harness.enable_openai_fallback(key_id).await;
    harness.mock.set_usage(401, None);
    harness.runtime.monitor.force_refresh().await.unwrap();
    harness.set_now(harness.now() + Duration::minutes(1));
    harness.mock.set_usage(200, None);
    harness.runtime.monitor.force_refresh().await.unwrap();
    let state = harness.runtime.monitor.state().unwrap();
    assert!(state["scheduler"][oauth_id.to_string()]["last_error_at"].is_string());
    let report = harness.runtime.monitor.fallback().await.unwrap();
    assert_eq!(report["oauth_states"][oauth_id.to_string()], "available");
    assert_eq!(report["desired"], false);
    let writes = harness
        .mock
        .calls()
        .into_iter()
        .filter(|call| call.method == "POST" && call.path.ends_with("/schedulable"))
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].body["schedulable"], false);
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    harness.close().await;

    let harness = Harness::new().await;
    let oauth = harness
        .seed_account("plus", "fixture-fallback-stale-failure", json!({}))
        .await;
    let key = harness.seed_key(false).await;
    let oauth_id = integer(&oauth["id"]).unwrap();
    let key_id = integer(&key["id"]).unwrap();
    harness.enable_openai_fallback(key_id).await;
    harness.mock.set_usage(401, None);
    harness.runtime.monitor.force_refresh().await.unwrap();
    harness.set_now(harness.now() + Duration::hours(2));
    let report = harness.runtime.monitor.fallback().await.unwrap();
    assert_eq!(report["oauth_states"][oauth_id.to_string()], "unknown");
    assert_eq!(report["desired"], Value::Null);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(
        harness.runtime.monitor.row(key_id).await.unwrap()["schedulable"],
        false
    );
    harness.close().await;
}

#[tokio::test]
async fn interrupted_daily_test_with_unknown_response_is_marked_and_not_replayed_after_reopen() {
    let harness = Harness::new().await;
    let now = harness.now();
    let extra = json!({
        "codex_usage_updated_at":datetime(now - Duration::minutes(5)),
        "codex_5h_used_percent":20,
        "codex_5h_reset_at":datetime(now + Duration::hours(5)),
        "codex_7d_used_percent":30,
        "codex_7d_reset_at":datetime(now + Duration::days(7))
    });
    let row = harness
        .seed_account("plus", "fixture-daily-unknown", extra)
        .await;
    let gate = Gate::new();
    *harness.mock.test_gate.lock().unwrap() = Some(gate.clone());
    let schedule = Schedule::new(harness.runtime.monitor.store.clone());
    let due = daily_test::next_time(now, "05:00").unwrap();
    let config = json!({"oauth_daily_test_enabled":true,"oauth_daily_test_time":"05:00"});
    schedule.startup(&config, due - Duration::hours(1)).unwrap();
    schedule.tick(&config, due).unwrap();
    let batch = schedule
        .claim(&config, due)
        .unwrap()
        .expect("daily batch due on time");
    let monitor = harness.runtime.monitor.clone();
    let run = tokio::spawn(async move { monitor.run_daily(batch, vec![row]).await });
    tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
        .await
        .expect("model test request must reach loopback server before interruption");
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    run.abort();
    let _ = run.await;
    gate.release.notify_one();
    *harness.mock.test_gate.lock().unwrap() = None;

    let reopened = Schedule::new(OAuthState::new(
        harness.runtime.settings.usage_path.clone(),
        false,
    ));
    reopened
        .startup(&config, due + Duration::seconds(2))
        .unwrap();
    let state = harness.runtime.monitor.state().unwrap();
    let date = due
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive()
        .to_string();
    assert_eq!(
        state["daily_test"]["batches"][date]["status"],
        "interrupted"
    );
    reopened.tick(&config, due + Duration::seconds(3)).unwrap();
    assert!(reopened
        .claim(&config, due + Duration::seconds(3))
        .unwrap()
        .is_none());
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    harness.close().await;
}

#[tokio::test]
async fn recovery_http_uses_exact_404_fallback_sequence_and_never_falls_back_on_500() {
    let harness = Harness::new().await;
    let row = harness
        .seed_account("plus", "fixture-recovery-http", json!({}))
        .await;
    let id = integer(&row["id"]).unwrap();
    *harness.mock.recovery_status.lock().unwrap() = 404;
    let result = oauth_native::recover(&harness.runtime.actions, id, ADMIN_KEY, 10).await;
    assert_eq!(result["success"], true);
    assert_eq!(result["fallback"], true);
    let calls = harness.mock.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].method, "POST");
    assert!(calls[0]
        .path
        .ends_with(&format!("/api/v1/admin/accounts/{id}/recover-state")));
    assert_eq!(calls[1].method, "POST");
    assert!(calls[1]
        .path
        .ends_with(&format!("/api/v1/admin/accounts/{id}/clear-rate-limit")));
    assert_eq!(calls[2].method, "DELETE");
    assert!(calls[2]
        .path
        .ends_with(&format!("/api/v1/admin/accounts/{id}/temp-unschedulable")));
    *harness.mock.recovery_status.lock().unwrap() = 500;
    let failed = oauth_native::recover(&harness.runtime.actions, id, ADMIN_KEY, 10).await;
    assert_eq!(failed["success"], false);
    assert_eq!(failed["error_code"], "http_500");
    assert_eq!(harness.mock.calls().len(), 4);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}
