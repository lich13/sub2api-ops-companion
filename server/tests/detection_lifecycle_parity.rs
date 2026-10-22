#![cfg(feature = "test-fixtures")]

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
    collections::VecDeque,
    fs,
    sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration as StdDuration,
};
use sub2ops_server::{
    compat::{account_version, datetime, integer, sha256},
    db::Database,
    model_detection, modeltrace,
    oauth_state::Update,
    runtime::Runtime,
    settings::Settings,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";
const ADMIN_KEY: &str = "fixture-only";
const DETECTION_FIXTURE: &str =
    include_str!("../../tests/contracts/detection-lifecycle/fixtures.json");
const MODELTRACE_FIXTURE: &str =
    include_str!("../../tests/contracts/modeltrace-runtime/fixtures.json");
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(9_800_000);

fn fixture() -> Value {
    serde_json::from_str(DETECTION_FIXTURE).expect("pinned detection fixture JSON")
}

fn object_field_is_empty_or_missing(value: &Value, key: &str) -> bool {
    match value.get(key) {
        None => true,
        Some(Value::Object(object)) => object.is_empty(),
        _ => false,
    }
}

fn modeltrace_fixture() -> Value {
    serde_json::from_str(MODELTRACE_FIXTURE).expect("pinned ModelTrace result fixture")
}

fn reference_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2031, 4, 5, 6, 7, 8).unwrap()
}

fn sample_text(case: &str, index: usize) -> String {
    modeltrace_fixture()["modeltrace_runtime"][case]["outputs"][index]
        .as_str()
        .expect("synthetic output from pinned ModelTrace fixture")
        .to_owned()
}

fn response_sse(text: &str) -> String {
    let response = json!({
        "status":"completed",
        "model":"fixture-returned-model",
        "object":"response",
        "output_text":text,
        "output":[{"type":"message","phase":"final","content":[{"type":"output_text","text":text}]}],
        "usage":{"input_tokens":23,"output_tokens":19,"output_tokens_details":{"reasoning_tokens":2}}
    });
    let created = json!({
        "type":"response.created",
        "response":{"id":"fixture-response","status":"in_progress","model":"fixture-returned-model","object":"response"}
    });
    let event = json!({"type":"response.completed","response":response});
    format!("data: {created}\n\ndata: {event}\n\n")
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

#[cfg(unix)]
struct DirectoryReadOnly {
    path: std::path::PathBuf,
    original_mode: u32,
    restored: bool,
}

#[cfg(unix)]
impl DirectoryReadOnly {
    fn new(path: std::path::PathBuf) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let original_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500))
            .expect("make only the fixture-owned state directory temporarily non-writable");
        Self {
            path,
            original_mode,
            restored: false,
        }
    }

    fn restore(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        if !self.restored {
            fs::set_permissions(&self.path, fs::Permissions::from_mode(self.original_mode))
                .expect("restore the fixture-owned state directory permissions");
            self.restored = true;
        }
    }
}

#[cfg(unix)]
impl Drop for DirectoryReadOnly {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(unix)]
struct FileReadOnly {
    path: std::path::PathBuf,
    original_mode: u32,
    restored: bool,
}

#[cfg(unix)]
impl FileReadOnly {
    fn new(path: std::path::PathBuf) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let original_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400))
            .expect("make only the fixture-owned state lock read-only");
        Self {
            path,
            original_mode,
            restored: false,
        }
    }

    fn restore(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        if !self.restored {
            fs::set_permissions(&self.path, fs::Permissions::from_mode(self.original_mode))
                .expect("restore the fixture-owned state lock permissions");
            self.restored = true;
        }
    }
}

#[cfg(unix)]
impl Drop for FileReadOnly {
    fn drop(&mut self) {
        self.restore();
    }
}

#[derive(Clone, Debug)]
struct Call {
    method: String,
    path: String,
    payload: Value,
    admin_authorized: bool,
    admin_header_present: bool,
    bearer_authorized: bool,
    oauth_native_headers: bool,
    native_codex_payload: bool,
}

fn assert_native_oauth_calls(calls: &[Call]) {
    assert_native_oauth_calls_for(calls, "gpt-6-luna");
}

fn assert_native_oauth_calls_for(calls: &[Call], expected_model: &str) {
    assert!(
        !calls.is_empty(),
        "OAuth fixture must issue a Responses request"
    );
    for call in calls {
        assert_eq!(call.path, "/v1/responses");
        assert!(call.bearer_authorized);
        assert!(!call.admin_header_present);
        assert!(call.oauth_native_headers);
        assert!(call.native_codex_payload);
        assert_eq!(call.payload["model"], expected_model);
    }
}

#[derive(Clone)]
struct MockUpstream {
    pool: PgPool,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    calls: Arc<StdMutex<Vec<Call>>>,
    unexpected: Arc<StdMutex<Vec<String>>>,
    output: Arc<StdMutex<String>>,
    model_outputs: Arc<StdMutex<VecDeque<String>>>,
    models: Arc<StdMutex<Vec<Value>>>,
    model_directory_gate: Arc<StdMutex<Option<Gate>>>,
    model_gate: Arc<StdMutex<Option<Gate>>>,
    schedulable_gate: Arc<StdMutex<Option<Gate>>>,
    schedulable_write_before_gate: Arc<StdMutex<bool>>,
    bark_gate: Arc<StdMutex<Option<Gate>>>,
    invalid_schedulable_response: Arc<StdMutex<bool>>,
    model_failures: Arc<AtomicUsize>,
    model_failure_status: Arc<AtomicUsize>,
    bark_failures: Arc<AtomicUsize>,
    bark_response_failures: Arc<AtomicUsize>,
}

impl MockUpstream {
    fn new(pool: PgPool, clock: Arc<StdMutex<DateTime<Utc>>>, output: String) -> Self {
        Self {
            pool,
            clock,
            calls: Arc::new(StdMutex::new(Vec::new())),
            unexpected: Arc::new(StdMutex::new(Vec::new())),
            output: Arc::new(StdMutex::new(output)),
            model_outputs: Arc::new(StdMutex::new(VecDeque::new())),
            models: Arc::new(StdMutex::new(vec![
                json!({"id":"gpt-6-luna","name":"fixture model"}),
                json!({"id":"gpt-5.6-luna","name":"fixture target"}),
                json!({"id":"claude-sonnet-4-6","name":"fixture alternative"}),
            ])),
            model_directory_gate: Arc::new(StdMutex::new(None)),
            model_gate: Arc::new(StdMutex::new(None)),
            schedulable_gate: Arc::new(StdMutex::new(None)),
            schedulable_write_before_gate: Arc::new(StdMutex::new(false)),
            bark_gate: Arc::new(StdMutex::new(None)),
            invalid_schedulable_response: Arc::new(StdMutex::new(false)),
            model_failures: Arc::new(AtomicUsize::new(0)),
            model_failure_status: Arc::new(AtomicUsize::new(
                StatusCode::SERVICE_UNAVAILABLE.as_u16() as usize,
            )),
            bark_failures: Arc::new(AtomicUsize::new(0)),
            bark_response_failures: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }

    fn set_model_gate(&self, gate: Option<Gate>) {
        *self.model_gate.lock().unwrap() = gate;
    }

    fn set_model_directory_gate(&self, gate: Option<Gate>) {
        *self.model_directory_gate.lock().unwrap() = gate;
    }

    fn set_schedulable_gate(&self, gate: Option<Gate>) {
        *self.schedulable_gate.lock().unwrap() = gate;
    }

    fn set_schedulable_write_before_gate(&self, write_before_gate: bool) {
        *self.schedulable_write_before_gate.lock().unwrap() = write_before_gate;
    }

    fn set_bark_gate(&self, gate: Option<Gate>) {
        *self.bark_gate.lock().unwrap() = gate;
    }

    fn set_models(&self, models: Vec<Value>) {
        *self.models.lock().unwrap() = models;
    }

    fn set_model_outputs(&self, outputs: impl IntoIterator<Item = String>) {
        *self.model_outputs.lock().unwrap() = outputs.into_iter().collect();
    }

    fn set_model_failures(&self, count: usize) {
        self.set_model_failure_response(count, StatusCode::SERVICE_UNAVAILABLE.as_u16());
    }

    fn set_model_failure_response(&self, count: usize, status: u16) {
        self.model_failure_status
            .store(status as usize, Ordering::Release);
        self.model_failures.store(count, Ordering::Release);
    }

    fn set_invalid_schedulable_response(&self, invalid: bool) {
        *self.invalid_schedulable_response.lock().unwrap() = invalid;
    }

    fn set_bark_failures(&self, count: usize) {
        self.bark_failures.store(count, Ordering::Release);
    }

    fn set_bark_response_failures(&self, count: usize) {
        self.bark_response_failures.store(count, Ordering::Release);
    }

    fn calls(&self, method: &str, path: &str) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.method == method && call.path == path)
            .cloned()
            .collect()
    }

    fn count(&self, method: &str, path: &str) -> usize {
        self.calls(method, path).len()
    }

    fn unexpected(&self) -> Vec<String> {
        self.unexpected.lock().unwrap().clone()
    }

    async fn wait_for(&self, method: &str, path: &str, expected: usize) {
        tokio::time::timeout(StdDuration::from_secs(12), async {
            loop {
                if self.count(method, path) >= expected {
                    return;
                }
                tokio::time::sleep(StdDuration::from_millis(5)).await;
            }
        })
        .await
        .expect("loopback fixture did not receive the expected request");
    }
}

fn json_response(value: Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

fn method_name(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        _ => "OTHER",
    }
}

fn path_id(path: &str, suffix: &str) -> Option<i64> {
    path.strip_prefix("/api/v1/admin/accounts/")?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

fn take_failure(counter: &AtomicUsize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    while current > 0 {
        match counter.compare_exchange_weak(
            current,
            current - 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(next) => current = next,
        }
    }
    false
}

async fn persist_synthetic_schedulable(state: &MockUpstream, id: i64, schedulable: bool) {
    sqlx::query("UPDATE accounts SET schedulable=$1,updated_at=$2 WHERE id=$3")
        .bind(schedulable)
        .bind(state.now())
        .bind(id)
        .execute(&state.pool)
        .await
        .expect("apply synthetic loopback write to temporary PostgreSQL");
}

async fn mock_upstream(State(state): State<MockUpstream>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let method = parts.method;
    let path = parts.uri.path().to_owned();
    let bytes = to_bytes(body, 2_000_000).await.unwrap_or_default();
    let payload = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    let header_value = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    };
    let admin_header_present = parts.headers.contains_key("x-api-key");
    let admin_authorized = parts
        .headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == ADMIN_KEY);
    let bearer_authorized = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "Bearer fixture-only");
    let originator = header_value("originator");
    let version = header_value("version");
    let user_agent = header_value("user-agent");
    let native_identity = !originator.is_empty()
        && !version.is_empty()
        && user_agent.starts_with(&format!("{originator}/{version}"));
    let oauth_native_headers = bearer_authorized
        && !admin_header_present
        && header_value("openai-beta") == "responses=experimental"
        && header_value("accept") == "text/event-stream"
        && header_value("content-type").starts_with("application/json")
        && native_identity;
    let native_codex_payload = payload["input"][0]["content"][0]["type"] == "input_text"
        && payload["input"][0]["content"][0]["text"].is_string()
        && payload["instructions"] == "You are a helpful assistant."
        && payload["stream"] == true
        && payload["store"] == false
        && payload.get("max_output_tokens").is_none();
    state.calls.lock().unwrap().push(Call {
        method: method_name(&method).into(),
        path: path.clone(),
        payload: payload.clone(),
        admin_authorized,
        admin_header_present,
        bearer_authorized,
        oauth_native_headers,
        native_codex_payload,
    });

    if method == Method::GET && path.starts_with("/fixture-models/") {
        assert!(
            !admin_header_present,
            "provider model reads use account credentials"
        );
        assert!(
            bearer_authorized,
            "provider model reads use account credentials"
        );
        let directory_gate = { state.model_directory_gate.lock().unwrap().clone() };
        if let Some(gate) = directory_gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        return json_response(json!({"data":state.models.lock().unwrap().clone()}));
    }
    if method == Method::GET && path.ends_with("/models") {
        return json_response(json!({"code":0,"data":state.models.lock().unwrap().clone()}));
    }
    if method == Method::GET && path.ends_with("/groups/all") {
        return json_response(json!({"code":0,"data":[]}));
    }
    if method == Method::POST && path.ends_with("/schedulable") {
        let Some(id) = path_id(&path, "/schedulable") else {
            return (StatusCode::NOT_FOUND, Json(json!({"code":1}))).into_response();
        };
        let write_before_gate = *state.schedulable_write_before_gate.lock().unwrap();
        if write_before_gate {
            persist_synthetic_schedulable(
                &state,
                id,
                payload["schedulable"].as_bool().unwrap_or(false),
            )
            .await;
        }
        let schedulable_gate = { state.schedulable_gate.lock().unwrap().clone() };
        if let Some(gate) = schedulable_gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if *state.invalid_schedulable_response.lock().unwrap() {
            return json_response(json!({
                "code":0,
                "data":{"id":id,"schedulable":!payload["schedulable"].as_bool().unwrap_or(false)}
            }));
        }
        if !write_before_gate {
            persist_synthetic_schedulable(
                &state,
                id,
                payload["schedulable"].as_bool().unwrap_or(false),
            )
            .await;
        }
        return json_response(json!({
            "code":0,
            "data":{"id":id,"schedulable":payload["schedulable"]}
        }));
    }
    if method == Method::POST && path == "/v1/responses" {
        if take_failure(&state.model_failures) {
            let status =
                StatusCode::from_u16(state.model_failure_status.load(Ordering::Acquire) as u16)
                    .unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
            let error_type = if status.is_server_error() {
                "server_error"
            } else {
                "invalid_request_error"
            };
            return (
                status,
                Json(json!({"error":{"type":error_type,"message":"fixture synthetic upstream response"}})),
            )
                .into_response();
        }
        let model_gate = { state.model_gate.lock().unwrap().clone() };
        if let Some(gate) = model_gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let output = state
            .model_outputs
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| state.output.lock().unwrap().clone());
        let body = response_sse(&output);
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(body))
            .expect("synthetic Responses SSE envelope");
    }
    if method == Method::POST && path == "/push" {
        let bark_gate = { state.bark_gate.lock().unwrap().clone() };
        if let Some(gate) = bark_gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if take_failure(&state.bark_failures) {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"code":503,"message":"fixture notification retry"})),
            )
                .into_response();
        }
        if take_failure(&state.bark_response_failures) {
            return json_response(json!({
                "code":500,
                "message":"fixture notification response-code retry"
            }));
        }
        return json_response(json!({"code":200,"data":"fixture-delivered"}));
    }

    state
        .unexpected
        .lock()
        .unwrap()
        .push(format!("{} {}", method_name(&method), path));
    (StatusCode::NOT_FOUND, Json(json!({"code":404}))).into_response()
}

struct Harness {
    _temp: tempfile::TempDir,
    settings: Settings,
    fixture_db: Database,
    runtime: Option<Arc<Runtime>>,
    mock: MockUpstream,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    monotonic_origin: std::time::Instant,
    base_url: String,
    server: Option<JoinHandle<()>>,
}

impl Harness {
    async fn new(output: String) -> Self {
        let database_url = std::env::var("DATABASE_URL")
            .expect("detection contract runner must provide temporary PostgreSQL");
        let fixture_db = Database::open(&database_url, false)
            .await
            .expect("open independent pool for synthetic upstream writes");
        let temp = tempfile::tempdir().expect("create private detection state directory");
        let clock = Arc::new(StdMutex::new(reference_time()));
        let monotonic_origin = std::time::Instant::now();
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback-only upstream fixture");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let mock = MockUpstream::new(fixture_db.pool.clone(), clock.clone(), output);
        let router = Router::new()
            .fallback(mock_upstream)
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve detection loopback fixture");
        });

        let connection_path = temp.path().join("sub2api-config.json");
        fs::write(
            &connection_path,
            serde_json::to_vec(&json!({"base_url":base_url,"verify_base_url":""})).unwrap(),
        )
        .expect("write synthetic loopback connection config");
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
            oauth: json!({
                "oauth_auto_reset_credit_enabled":false,
                "oauth_daily_test_enabled":true,
                "oauth_daily_test_time":"14:08",
                "oauth_early_probe_batch_size":1,
                "oauth_recovery_connection_account_ids":[],
                "oauth_recovery_model_account_ids":[],
                "oauth_recovery_monitor_enabled":true,
                "oauth_recovery_test_concurrency":1,
                "oauth_recovery_test_model_id":"gpt-6-luna",
                "oauth_usage_refresh_concurrency":1
            }),
            bark: json!({
                "enabled":true,
                "device_key":"fixture-only",
                "server_url":base_url.clone()
            }),
            bark_valid: true,
            base_url: base_url.clone(),
            verify_url: String::new(),
            read_only: false,
        };
        let runtime =
            Self::open_runtime(settings.clone(), clock.clone(), monotonic_origin, &base_url).await;
        Self {
            _temp: temp,
            settings,
            fixture_db,
            runtime: Some(runtime),
            mock,
            clock,
            monotonic_origin,
            base_url,
            server: Some(server),
        }
    }

    async fn open_runtime(
        settings: Settings,
        clock: Arc<StdMutex<DateTime<Utc>>>,
        monotonic_origin: std::time::Instant,
        base_url: &str,
    ) -> Arc<Runtime> {
        let runtime_clock = clock.clone();
        let monotonic_clock = clock.clone();
        let wall_origin = reference_time();
        let runtime = Runtime::new_with_clocks(
            settings,
            Arc::new(move || *runtime_clock.lock().unwrap()),
            Arc::new(move || {
                let elapsed = (*monotonic_clock.lock().unwrap() - wall_origin)
                    .to_std()
                    .expect("fixture wall clock must not move backwards");
                monotonic_origin + elapsed
            }),
        )
        .await
        .expect("construct real Runtime over temporary PostgreSQL and injected clock");
        runtime
            .tests
            .set_test_endpoint(&format!("{base_url}/v1/responses"))
            .expect("use the loopback-only ModelTrace endpoint for this test Runtime");
        runtime
            .actions
            .set_model_directory_endpoint(&format!("{base_url}/fixture-models"))
            .expect("use the loopback-only provider directory fixture");
        runtime
            .monitor
            .store
            .save_admin_token(ADMIN_KEY)
            .expect("save synthetic Sub2API credential for loopback only");
        runtime
    }

    fn runtime(&self) -> &Arc<Runtime> {
        self.runtime
            .as_ref()
            .expect("live Runtime in detection harness")
    }

    fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }

    fn advance(&self, seconds: i64) {
        *self.clock.lock().unwrap() += Duration::seconds(seconds);
    }

    async fn seed_account(&self, kind: &str, schedulable: bool, extra: Value) -> Value {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        let credentials = if kind == "apikey" {
            json!({
                "api_key":"fixture-only",
                "base_url":format!("{}/v1", self.base_url)
            })
        } else {
            json!({
                "plan_type":"plus",
                "access_token":"fixture-only",
                "refresh_token":"fixture-only"
            })
        };
        sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,updated_at) VALUES ($1,$2,'openai',$3,'active',$4,$5,$6,$7)")
            .bind(id)
            .bind(format!("fixture-detection-account-{id}"))
            .bind(kind)
            .bind(schedulable)
            .bind(SqlJson(credentials))
            .bind(SqlJson(extra))
            .bind(self.now() - Duration::minutes(1))
            .execute(&self.runtime().db.pool)
            .await
            .expect("insert synthetic account into temporary business schema");
        self.runtime()
            .monitor
            .row(id)
            .await
            .expect("read synthetic account through Runtime query projection")
    }

    async fn seed_shared_child_account(&self, parent_id: i64) -> Value {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,parent_account_id,updated_at) VALUES ($1,$2,'openai','apikey','active',true,$3,$4,$5,$6)")
            .bind(id)
            .bind(format!("fixture-detection-child-{id}"))
            .bind(SqlJson(json!({})))
            .bind(SqlJson(json!({})))
            .bind(parent_id)
            .bind(self.now() - Duration::minutes(1))
            .execute(&self.runtime().db.pool)
            .await
            .expect("insert synthetic child sharing only the fixture parent credentials");
        self.runtime()
            .monitor
            .row(id)
            .await
            .expect("read synthetic child through Runtime query projection")
    }

    async fn insert_capacity_error(
        &self,
        error_id: i64,
        account_id: i64,
        created_at: DateTime<Utc>,
    ) {
        sqlx::query(
            "INSERT INTO ops_error_logs (id,account_id,created_at,platform,error_owner,error_phase,error_source,stream,status_code,upstream_status_code,upstream_error_message) \
             VALUES ($1,$2,$3,'openai','provider','upstream','upstream',true,503,503,$4)",
        )
        .bind(error_id)
        .bind(account_id)
        .bind(created_at)
        .bind("Our servers are currently overloaded. Please try again later.")
        .execute(&self.runtime().db.pool)
        .await
        .expect("insert synthetic provider capacity evidence into temporary PostgreSQL");
    }

    async fn insert_slow_usage_window(&self, account_id: i64, first_usage_id: i64) {
        let newest_sample_at = Utc::now() - Duration::milliseconds(1);
        for index in 0..10_i64 {
            let first_token_ms = if index < 8 { 11_001_i64 } else { 9_000_i64 };
            let duration_ms = if index < 8 { 12_001_i64 } else { 12_000_i64 };
            sqlx::query(
                "INSERT INTO usage_logs (id,account_id,created_at,model,upstream_model,stream,first_token_ms,duration_ms,output_tokens,inbound_endpoint) \
                 VALUES ($1,$2,$3,'gpt-6-luna','gpt-6-luna',true,$4,$5,64,'/v1/responses')",
            )
            .bind(first_usage_id + index)
            .bind(account_id)
            .bind(newest_sample_at - Duration::minutes(index))
            .bind(first_token_ms)
            .bind(duration_ms)
            .execute(&self.runtime().db.pool)
            .await
            .expect("insert synthetic slow TTFT sample window into temporary PostgreSQL");
        }
    }

    async fn enable_detection(&self, id: i64, interval_minutes: i64) -> Value {
        self.enable_detection_with_model(id, interval_minutes, "gpt-6-luna")
            .await
    }

    async fn enable_detection_with_model(
        &self,
        id: i64,
        interval_minutes: i64,
        model_id: &str,
    ) -> Value {
        self.set_detection_model(model_id).await;
        let control = self.runtime().detection.view(id).unwrap();
        self.runtime()
            .detection
            .save(
                id,
                &json!({
                    "enabled":true,
                    "interval_minutes":interval_minutes,
                    "model_id":model_id,
                    "expected_version":control["version"]
                }),
            )
            .await
            .expect("save detection schedule through real Runtime control")
    }

    async fn set_detection_model(&self, model_id: &str) {
        self.save_detection_model_value(json!(model_id))
            .await
            .expect("configure the global automatic detection model");
    }

    async fn save_detection_model_value(
        &self,
        model_id: Value,
    ) -> sub2ops_server::error::Result<Value> {
        let snapshot = self.runtime().config.snapshot(Some("oauth")).unwrap();
        self.runtime()
            .config
            .save(
                "oauth",
                &json!({"model_detection_model_id":model_id}),
                "fixture",
                snapshot["oauth"]["revision"].as_str(),
            )
            .await
    }

    async fn wait_job_terminal(&self, id: &str) -> Value {
        tokio::time::timeout(StdDuration::from_secs(20), async {
            loop {
                let job = self
                    .runtime()
                    .tests
                    .get(id)
                    .expect("persisted model test job");
                if !matches!(
                    job["status"].as_str(),
                    Some("queued" | "running" | "retrying")
                ) {
                    return job;
                }
                tokio::time::sleep(StdDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("real model test worker did not reach a terminal state")
    }

    async fn restart_runtime(&mut self) {
        let old = self.runtime.take().expect("Runtime to restart");
        old.close().await;
        drop(old);
        let runtime = Self::open_runtime(
            self.settings.clone(),
            self.clock.clone(),
            self.monotonic_origin,
            &self.base_url,
        )
        .await;
        self.runtime = Some(runtime);
    }

    async fn restart_with_bank_state(&mut self, state: &Value) {
        let old = self
            .runtime
            .take()
            .expect("Runtime to restart with synthetic bank state");
        old.close().await;
        drop(old);
        let path = self.settings.data_dir().join("modeltrace-bank-state.json");
        fs::write(
            path,
            serde_json::to_vec(state).expect("serialize synthetic fingerprint bank state"),
        )
        .expect("write synthetic fingerprint bank state in the fixture-owned directory");
        let runtime = Self::open_runtime(
            self.settings.clone(),
            self.clock.clone(),
            self.monotonic_origin,
            &self.base_url,
        )
        .await;
        self.runtime = Some(runtime);
    }

    async fn close(mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.close().await;
        }
        self.fixture_db.close().await;
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
    }
}

fn bank_state_without_target() -> Value {
    let (mut bank, mut version) =
        modeltrace::bundled().expect("load bundled synthetic bank source");
    let models = bank["models"].as_array_mut().unwrap();
    let target_index = models
        .iter()
        .position(|model| model["id"] == model_detection::TARGET)
        .expect("bundled bank contains its target before the fixture mutation");
    models.remove(target_index);
    let model_ids = models
        .iter()
        .map(|model| model["id"].clone())
        .collect::<Vec<_>>();
    bank["robust"]["model_order"] = json!(model_ids);
    for feature in ["hellinger", "ordered_blocks"] {
        bank["robust"][feature]["centroids"]
            .as_array_mut()
            .unwrap()
            .remove(target_index);
    }
    for environment in bank["robust"]["ordered_blocks"]["environment_centroids"]
        .as_array_mut()
        .unwrap()
    {
        environment.as_array_mut().unwrap().remove(target_index);
    }

    let raw_bank = serde_json::to_string(&bank).expect("serialize target-less synthetic bank");
    let manifest = modeltrace::manifest();
    version["sha256"] = sha256(&raw_bank).into();
    version["core_blob"] = manifest["files"]["core"]["gitBlob"].clone();
    version["challenge_blob"] = manifest["files"]["challenge"]["gitBlob"].clone();
    json!({
        "version":1,
        "active":{"raw_bank":raw_bank,"version":version},
        "source":"fixture",
        "checked_at":0,
        "synced_at":0,
        "cooldown_until":null,
        "last_error":null,
        "result":null
    })
}

fn bank_state_with_swapped_model_labels(left_id: &str, right_id: &str) -> Value {
    assert_ne!(left_id, right_id);
    let (mut bank, mut version) =
        modeltrace::bundled().expect("load bundled synthetic bank source");
    let models = bank["models"].as_array_mut().unwrap();
    let left_index = models
        .iter()
        .position(|model| model["id"] == left_id)
        .expect("synthetic bank contains the first model label");
    let right_index = models
        .iter()
        .position(|model| model["id"] == right_id)
        .expect("synthetic bank contains the second model label");
    let left_name = models[left_index]["display_name"].clone();
    let right_name = models[right_index]["display_name"].clone();
    models[left_index]["id"] = right_id.into();
    models[left_index]["display_name"] = right_name;
    models[right_index]["id"] = left_id.into();
    models[right_index]["display_name"] = left_name;
    let model_order = models
        .iter()
        .map(|model| model["id"].clone())
        .collect::<Vec<_>>();
    bank["robust"]["model_order"] = json!(model_order);

    let raw_bank = serde_json::to_string(&bank).expect("serialize relabeled synthetic bank");
    let manifest = modeltrace::manifest();
    version["sha256"] = sha256(&raw_bank).into();
    version["core_blob"] = manifest["files"]["core"]["gitBlob"].clone();
    version["challenge_blob"] = manifest["files"]["challenge"]["gitBlob"].clone();
    json!({
        "version":1,
        "active":{"raw_bank":raw_bank,"version":version},
        "source":"fixture",
        "checked_at":0,
        "synced_at":0,
        "cooldown_until":null,
        "last_error":null,
        "result":null
    })
}

fn completed_recheck_job(
    id: i64,
    generation: &Value,
    mark_version: &Value,
    account_version: &Value,
    job_id: &str,
    completed_at: DateTime<Utc>,
) -> Value {
    json!({
        "id":job_id,
        "account_id":id,
        "automatic":true,
        "degradation_recheck":true,
        "status":"completed",
        "completed_at":datetime(completed_at),
        "detection_generation":generation,
        "detection_mark_version":mark_version,
        "recheck_account_version":account_version,
        "recheck_schedulable":true,
        "requested_model":"gpt-6-luna",
        "first_group_valid":true,
        "first_group_prediction":"claude-sonnet-4-6",
        "report":{"prediction":"claude-sonnet-4-6","used_outputs":1,"probability":0.97},
        "groups":[{"index":1,"status":"completed","error":""}],
        "error_code":null,
        "can_retry":false
    })
}

#[tokio::test]
async fn global_detection_model_overlays_legacy_fields_and_keeps_put_validation() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    harness.mock.set_models(vec![
        json!({"id":"gpt-6-luna"}),
        json!({"id":"gpt-6-astra"}),
        json!({"id":"gpt-6-sol"}),
    ]);
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    assert_eq!(
        harness.runtime().detection.model_id().unwrap(),
        "gpt-6-luna"
    );

    let persisted_legacy = "fixture-per-account-model";
    harness
        .runtime()
        .detection
        .update(id, json!({"model_id":persisted_legacy}))
        .unwrap();
    let initial = harness.runtime().detection.view(id).unwrap();
    assert_eq!(initial["model_id"], "gpt-6-luna");
    assert_eq!(initial["model_source"], "global");
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["model_id"],
        persisted_legacy
    );

    harness.set_detection_model("gpt-6-astra").await;
    let changed = harness.runtime().detection.view(id).unwrap();
    assert_eq!(changed["model_id"], "gpt-6-astra");
    assert_eq!(changed["model_source"], "global");
    assert_ne!(changed["version"], initial["version"]);

    for (index, legacy_model) in [
        json!("old-string-model"),
        Value::Null,
        json!({"model":"old-object-model"}),
        json!(["old-array-model"]),
        json!(true),
    ]
    .into_iter()
    .enumerate()
    {
        let current = harness.runtime().detection.view(id).unwrap();
        let saved = harness
            .runtime()
            .detection
            .save(
                id,
                &json!({
                    "enabled":true,
                    "interval_minutes":index as i64 + 1,
                    "model_id":legacy_model,
                    "expected_version":current["version"]
                }),
            )
            .await
            .expect("legacy PUT model_id is accepted and ignored regardless of JSON type");
        assert_eq!(saved["model_id"], "gpt-6-astra");
        assert_eq!(saved["model_source"], "global");
        assert_eq!(
            harness.runtime().detection.control(id).unwrap()["model_id"],
            persisted_legacy,
            "legacy per-account model is preserved while PUT ignores it"
        );
    }

    let current = harness.runtime().detection.view(id).unwrap();
    for payload in [
        json!({
            "enabled":"true",
            "interval_minutes":30,
            "model_id":null,
            "expected_version":current["version"]
        }),
        json!({
            "enabled":true,
            "interval_minutes":0,
            "model_id":{},
            "expected_version":current["version"]
        }),
        json!({
            "enabled":true,
            "interval_minutes":30,
            "model_id":null,
            "expected_version":"not-a-version"
        }),
    ] {
        assert_eq!(
            harness
                .runtime()
                .detection
                .save(id, &payload)
                .await
                .unwrap_err()
                .status
                .as_u16(),
            422,
            "enabled, interval and version inputs retain their validation"
        );
    }
    let stale = harness
        .runtime()
        .detection
        .save(
            id,
            &json!({
                "enabled":false,
                "interval_minutes":30,
                "model_id":{"ignored":true},
                "expected_version":"0".repeat(64)
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.status.as_u16(), 409);
    assert_eq!(
        harness.runtime().detection.view(id).unwrap()["model_id"],
        "gpt-6-astra"
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn key_detection_override_uses_shared_candidates_and_survives_global_changes() {
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    harness
        .mock
        .set_models(vec![json!({"id":"gpt-6-sol"}), json!({"id":"gpt-6-luna"})]);
    let id = integer(&harness.seed_account("apikey", true, json!({})).await["id"]).unwrap();
    let initial = harness.runtime().detection.view(id).unwrap();
    assert!(initial["model_override"].is_null());
    let saved = harness
        .runtime()
        .detection
        .save(
            id,
            &json!({
                "expected_version":initial["version"], "enabled":true, "interval_minutes":1,
                "model_override":"  gpt-6-sol  "
            }),
        )
        .await
        .unwrap();
    assert_eq!(saved["model_override"], "gpt-6-sol");
    assert_eq!(saved["model_id"], "gpt-6-sol");
    assert_eq!(saved["model_source"], "account");
    assert_eq!(saved["global_model_id"], "gpt-6-luna");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    harness.set_detection_model("gpt-6-astra").await;
    let current = harness.runtime().detection.view(id).unwrap();
    assert_eq!(current["version"], saved["version"]);
    assert_eq!(current["model_id"], "gpt-6-sol");
    assert_eq!(current["global_model_id"], "gpt-6-astra");
    let missing = harness
        .runtime()
        .detection
        .save(
            id,
            &json!({
                "expected_version":current["version"], "enabled":true, "interval_minutes":1,
                "model_override":"missing-model"
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(missing.status.as_u16(), 422);
    assert_eq!(
        harness.runtime().detection.view(id).unwrap()["version"],
        current["version"]
    );
    let stale = harness
        .runtime()
        .detection
        .save(
            id,
            &json!({
                "expected_version":initial["version"], "enabled":false, "interval_minutes":1,
                "model_override":null
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.status.as_u16(), 409);
    harness.restart_runtime().await;
    assert_eq!(
        harness.runtime().detection.view(id).unwrap()["model_override"],
        "gpt-6-sol"
    );
    let budget = harness
        .runtime()
        .budget
        .clone()
        .acquire_many_owned(24)
        .await
        .unwrap();
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({
                "candidate_blocked":true, "candidate_recheck_at":null,
                "next_at":datetime(harness.now()-Duration::seconds(1))
            }),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let control = harness.runtime().detection.control(id).unwrap();
    let job_id = control["job_id"].as_str().unwrap().to_owned();
    let queued = harness.runtime().tests.get(&job_id).unwrap();
    assert_eq!(queued["requested_model"], "gpt-6-sol");
    assert_eq!(queued["forwarded_model"], "gpt-6-sol");
    drop(budget);
    let done = harness.wait_job_terminal(&job_id).await;
    assert_eq!(done["status"], "completed");
    assert_eq!(done["requested_model"], "gpt-6-sol");
    let current = harness.runtime().detection.view(id).unwrap();
    let following = harness
        .runtime()
        .detection
        .save(
            id,
            &json!({
                "expected_version":current["version"], "enabled":false, "interval_minutes":1,
                "model_override":null
            }),
        )
        .await
        .unwrap();
    assert!(following["model_override"].is_null());
    assert_eq!(following["model_id"], "gpt-6-astra");
    assert_eq!(following["model_source"], "global");
    let oauth = integer(&harness.seed_account("oauth", true, json!({})).await["id"]).unwrap();
    let current = harness.runtime().detection.view(oauth).unwrap();
    let error = harness
        .runtime()
        .detection
        .save(
            oauth,
            &json!({
                "expected_version":current["version"], "enabled":false, "interval_minutes":1,
                "model_override":"gpt-6-sol"
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 422);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn global_detection_model_normalizes_and_changed_model_rejects_pending_candidate_save() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let id = integer(&harness.seed_account("apikey", true, json!({})).await["id"]).unwrap();
    let initial = harness.runtime().config.snapshot(Some("oauth")).unwrap();
    assert_eq!(initial["oauth"]["model_detection_model_id"], "gpt-6-luna");
    let recovery_model = initial["oauth"]["oauth_recovery_test_model_id"].clone();

    for empty in [json!(""), json!(" \t "), Value::Null] {
        let saved = harness
            .save_detection_model_value(empty)
            .await
            .expect("empty global model values normalize to the default");
        assert_eq!(saved["model_detection_model_id"], "gpt-6-luna");
    }
    let trimmed = harness
        .save_detection_model_value(json!("  gpt-6-astra \t"))
        .await
        .expect("valid global model names are trimmed");
    assert_eq!(trimmed["model_detection_model_id"], "gpt-6-astra");
    for invalid in [
        json!("model id with spaces"),
        json!({"id":"gpt-6-luna"}),
        json!(42),
        json!(false),
    ] {
        let before = harness.runtime().config.snapshot(Some("oauth")).unwrap();
        let error = harness
            .save_detection_model_value(invalid)
            .await
            .unwrap_err();
        assert_eq!(error.status.as_u16(), 422);
        let after = harness.runtime().config.snapshot(Some("oauth")).unwrap();
        assert_eq!(
            after["oauth"]["model_detection_model_id"], before["oauth"]["model_detection_model_id"],
            "invalid global model values leave configuration unchanged"
        );
    }
    assert_eq!(
        harness.runtime().config.snapshot(Some("oauth")).unwrap()["oauth"]
            ["oauth_recovery_test_model_id"],
        recovery_model,
        "changing the detection model never rewrites the OAuth recovery model"
    );

    harness.set_detection_model("gpt-6-luna").await;
    let before_control = harness.runtime().detection.control(id).unwrap();
    let before_view = harness.runtime().detection.view(id).unwrap();
    let gate = Gate::new();
    harness.mock.set_model_directory_gate(Some(gate.clone()));
    let runtime = Arc::clone(harness.runtime());
    let payload = json!({
        "enabled":true,
        "interval_minutes":30,
        "model_id":"ignored-legacy-model",
        "expected_version":before_view["version"]
    });
    let pending = tokio::spawn(async move { runtime.detection.save(id, &payload).await });
    tokio::time::timeout(StdDuration::from_secs(10), gate.entered.notified())
        .await
        .expect("candidate model directory request reaches the held fixture");
    let directory_path = format!("/fixture-models/{id}");
    assert_eq!(harness.mock.count("GET", &directory_path), 1);

    harness.set_detection_model("gpt-6-astra").await;
    gate.release.notify_one();
    let error = tokio::time::timeout(StdDuration::from_secs(10), pending)
        .await
        .expect("pending detection save resumes after provider directory fixture release")
        .unwrap()
        .unwrap_err();
    harness.mock.set_model_directory_gate(None);
    assert_eq!(error.status.as_u16(), 409);
    assert_eq!(
        harness.runtime().detection.control(id).unwrap(),
        before_control,
        "a stale candidate response cannot persist the prior model selection"
    );
    let after_view = harness.runtime().detection.view(id).unwrap();
    assert_eq!(after_view["model_id"], "gpt-6-astra");
    assert_eq!(after_view["model_source"], "global");
    assert_eq!(after_view["enabled"], false);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(harness.mock.unexpected(), Vec::<String>::new());
    harness.close().await;
}

#[tokio::test]
async fn detection_scheduler_consumes_duplicate_clues_and_honors_exact_persisted_cooldown() {
    let expected = fixture();
    assert_eq!(expected["reference_sha"], REFERENCE_SHA);
    let cooldown = expected["oracle"]["automatic_dispatch_cooldown_seconds"]
        .as_i64()
        .unwrap();
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();

    let default = harness.runtime().detection.view(id).unwrap();
    assert_eq!(default["enabled"], false);
    assert_eq!(
        default["interval_minutes"],
        expected["oracle"]["default_interval_minutes"]
    );
    assert_eq!(default["status"], "disabled");
    assert!(default["next_at"].is_null());

    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    let first_clue = format!("error:fixture-{id}");
    let duplicate_clue = format!("slow:{id}:fixture");
    harness
        .runtime()
        .detection
        .capacity
        .store
        .transaction(|state| {
            state["detection_events"][&first_clue] =
                json!({"account_id":id,"created_at":datetime(harness.now())});
            Ok(())
        })
        .unwrap();
    let gate = Gate::new();
    harness.mock.set_model_gate(Some(gate.clone()));
    harness.runtime().detection.tick().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(10), gate.entered.notified())
        .await
        .expect("automatic worker must reach the synthetic model endpoint");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);

    harness
        .runtime()
        .detection
        .capacity
        .store
        .transaction(|state| {
            state["detection_events"][&duplicate_clue] =
                json!({"account_id":id,"created_at":datetime(harness.now())});
            Ok(())
        })
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let in_flight = harness
        .runtime()
        .tests
        .get(
            harness.runtime().detection.control(id).unwrap()["job_id"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(in_flight["automatic"], true);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    gate.release.notify_one();
    harness.mock.set_model_gate(None);

    let first_job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_job = harness.wait_job_terminal(&first_job_id).await;
    assert_eq!(first_job["status"], "completed");
    assert_eq!(first_job["first_group_prediction"], "claude-sonnet-4-6");
    assert_eq!(first_job["automatic_disposition"], Value::Null);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );
    harness.runtime().detection.tick().await.unwrap();
    let first_control = harness.runtime().detection.control(id).unwrap();
    assert_eq!(first_control["status"], "waiting");
    assert_eq!(
        first_control["next_at"],
        datetime(harness.now() + Duration::seconds(cooldown))
    );
    let detection_state = harness.runtime().detection.store.read().unwrap();
    assert!(detection_state["consumed"].get(&first_clue).is_some());
    assert!(detection_state["consumed"].get(&duplicate_clue).is_some());
    assert_eq!(
        detection_state["accounts"][id.to_string()]["triggers"],
        json!([first_clue])
    );
    assert_eq!(
        harness
            .runtime()
            .detection
            .capacity
            .store
            .snapshot()
            .unwrap()["detection_events"],
        json!({})
    );
    let first_requests = harness.mock.calls("POST", "/v1/responses");
    assert_eq!(first_requests.len(), 1);
    assert_eq!(first_requests[0].method, "POST");
    assert_eq!(first_requests[0].payload["model"], "gpt-6-luna");
    assert_eq!(first_requests[0].payload["stream"], true);
    assert_eq!(first_requests[0].payload["store"], false);
    assert_eq!(first_requests[0].payload["max_output_tokens"], 4096);
    assert_eq!(
        first_requests[0].payload["input"][0]["content"][0]["type"],
        "input_text"
    );

    let consumed_before_restart =
        harness.runtime().detection.store.read().unwrap()["consumed"].clone();
    harness.restart_runtime().await;
    harness
        .runtime()
        .detection
        .capacity
        .store
        .transaction(|state| {
            state["detection_events"][&first_clue] =
                json!({"account_id":id,"created_at":datetime(harness.now())});
            state["detection_events"][&duplicate_clue] =
                json!({"account_id":id,"created_at":datetime(harness.now())});
            Ok(())
        })
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    let after_reinjection = harness.runtime().detection.store.read().unwrap();
    assert_eq!(after_reinjection["consumed"], consumed_before_restart);
    assert_eq!(
        harness
            .runtime()
            .detection
            .capacity
            .store
            .snapshot()
            .unwrap()["detection_events"],
        json!({})
    );

    harness.advance(cooldown - 1);
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()]
            ["started_at"],
        datetime(reference_time())
    );

    harness.advance(1);
    let new_clue = format!("error:fixture-new-{id}");
    harness
        .runtime()
        .detection
        .capacity
        .store
        .transaction(|state| {
            state["detection_events"][&new_clue] =
                json!({"account_id":id,"created_at":datetime(harness.now())});
            Ok(())
        })
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    harness.mock.wait_for("POST", "/v1/responses", 2).await;
    let second_job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_job = harness.wait_job_terminal(&second_job_id).await;
    assert_eq!(second_job["status"], "completed");
    assert_eq!(second_job["triggers"], json!([new_clue]));
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 2);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()]
            ["started_at"],
        datetime(reference_time() + Duration::seconds(cooldown))
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn cooling_clues_are_consumed_without_later_replay() {
    let cooldown = fixture()["oracle"]["automatic_dispatch_cooldown_seconds"]
        .as_i64()
        .unwrap();
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 10).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["automatic_disposition"]["status"], "completed");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    let model_calls = harness.mock.calls("POST", "/v1/responses");
    assert!(model_calls[0].bearer_authorized);
    assert!(!model_calls[0].admin_authorized);
    assert_eq!(model_calls[0].payload["model"], "gpt-6-luna");
    assert_eq!(model_calls[0].payload["stream"], true);

    let error_clue = format!("error:fixture-cooling-{id}");
    let slow_clue = format!("slow:{id}:fixture-cooling");
    harness
        .runtime()
        .detection
        .capacity
        .store
        .transaction(|state| {
            state["detection_events"][&error_clue] =
                json!({"account_id":id,"created_at":datetime(harness.now())});
            state["detection_events"][&slow_clue] =
                json!({"account_id":id,"created_at":datetime(harness.now())});
            Ok(())
        })
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let after_cooling_tick = harness.runtime().detection.store.read().unwrap();
    assert!(after_cooling_tick["consumed"].get(&error_clue).is_some());
    assert!(after_cooling_tick["consumed"].get(&slow_clue).is_some());
    let capacity_state = harness
        .runtime()
        .detection
        .capacity
        .store
        .snapshot()
        .unwrap();
    assert!(capacity_state["detection_events"]
        .get(&error_clue)
        .is_none());
    assert!(capacity_state["detection_events"].get(&slow_clue).is_none());
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);

    harness.advance(cooldown);
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()]
            ["job_id"],
        job_id
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn unavailable_candidate_rechecks_after_one_minute_and_resumes_a_full_interval() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .mock
        .set_models(vec![json!({"id":"claude-sonnet-4-6"})]);
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();

    let models_path = format!("/fixture-models/{id}");
    let models_before_tick = harness.mock.count("GET", &models_path);
    harness.advance(300);
    harness.runtime().detection.tick().await.unwrap();
    let blocked = harness.runtime().detection.control(id).unwrap();
    assert_eq!(blocked["status"], "paused");
    assert_eq!(blocked["candidate_blocked"], true);
    assert_eq!(blocked["reason"], "所选模型不在当前账号上游模型目录中");
    assert_eq!(
        blocked["candidate_recheck_at"],
        datetime(harness.now() + Duration::minutes(1))
    );
    assert_eq!(
        harness.mock.count("GET", &models_path),
        models_before_tick + 1
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"]
            .get(id.to_string())
            .is_none()
    );

    harness.mock.set_models(vec![
        json!({"id":"gpt-6-luna"}),
        json!({"id":"gpt-5.6-luna"}),
    ]);
    harness.advance(59);
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(
        harness.mock.count("GET", &models_path),
        models_before_tick + 1
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);

    harness.advance(1);
    harness.runtime().detection.tick().await.unwrap();
    let resumed = harness.runtime().detection.control(id).unwrap();
    assert_eq!(resumed["candidate_blocked"], false);
    assert!(resumed["candidate_recheck_at"].is_null());
    assert_eq!(resumed["status"], "waiting");
    assert_eq!(
        resumed["next_at"],
        datetime(harness.now() + Duration::minutes(1))
    );
    assert_eq!(
        harness.mock.count("GET", &models_path),
        models_before_tick + 2
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);

    harness.advance(60);
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["status"], "completed");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    harness
        .mock
        .wait_for("GET", &models_path, models_before_tick + 3)
        .await;
    // The final scheduled tick and its worker share the same controlled refresh.
    assert_eq!(
        harness.mock.count("GET", &models_path),
        models_before_tick + 3
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn missing_fingerprint_target_pauses_candidate_without_dispatch_or_recheck_loop() {
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .restart_with_bank_state(&bank_state_without_target())
        .await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();

    let models_path = format!("/fixture-models/{id}");
    let models_count_before_tick = harness.mock.count("GET", &models_path);
    harness.runtime().detection.tick().await.unwrap();
    let blocked = harness.runtime().detection.control(id).unwrap();
    assert_eq!(blocked["status"], "paused");
    assert_eq!(blocked["candidate_blocked"], true);
    assert_eq!(blocked["reason"], "当前指纹库缺少降智判定模型");
    assert_eq!(
        blocked["candidate_recheck_at"],
        datetime(harness.now() + Duration::minutes(1))
    );
    assert!(!harness.runtime().tests.bank.snapshot().0["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["id"] == model_detection::TARGET));
    assert_eq!(
        harness.mock.count("GET", &models_path),
        models_count_before_tick + 1
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(
        harness.mock.count("GET", &models_path),
        models_count_before_tick + 1
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"]
            .get(id.to_string())
            .is_none()
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn all_automatic_sources_and_models_share_the_same_exact_cooldown_boundary() {
    let cooldown = fixture()["oracle"]["automatic_dispatch_cooldown_seconds"]
        .as_i64()
        .unwrap();
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness.mock.set_models(vec![
        json!({"id":"gpt-6-luna"}),
        json!({"id":"fixture-other-model"}),
        json!({"id":"gpt-5.6-luna"}),
    ]);
    let first = harness
        .runtime()
        .detection
        .trigger(id, &["automatic-api".into()])
        .await
        .unwrap();
    let first_job = harness
        .wait_job_terminal(first["id"].as_str().unwrap())
        .await;
    assert_eq!(first_job["status"], "completed");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    let started = harness.runtime().detection.store.read().unwrap()["automatic_starts"]
        [id.to_string()]
    .clone();
    assert_eq!(started["job_id"], first_job["id"]);
    assert_eq!(started["started_at"], datetime(reference_time()));
    assert_eq!(
        harness.runtime().detection.next_allowed_at(id).unwrap(),
        datetime(reference_time() + Duration::seconds(cooldown))
    );

    harness.advance(cooldown - 1);
    for cause in [
        "scheduled",
        "error:fixture-source",
        "slow:fixture-source",
        "recovery",
        "credit",
        "automatic-api",
    ] {
        harness
            .runtime()
            .detection
            .update(id, json!({"model_id":"fixture-other-model"}))
            .unwrap();
        let outcome = harness
            .runtime()
            .detection
            .trigger(id, &[cause.to_owned()])
            .await
            .unwrap();
        assert!(
            outcome.is_null(),
            "trigger {cause} must be deferred during cooldown"
        );
        assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
        assert_eq!(
            harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()],
            started
        );
    }

    let live = harness.runtime().actions.account(id, None).await.unwrap();
    let operation_version =
        sub2ops_server::compat::operation_versions(&live, false)["model_test"].clone();
    let queued_automatic = harness
        .runtime()
        .tests
        .start(
            id,
            &json!({
                "model_id":"gpt-6-luna",
                "expected_version":"0".repeat(64),
                "expected_operation_version":operation_version,
                "request_id":uuid::Uuid::new_v4().simple().to_string(),
                "concurrency":1
            }),
            Some(&json!({
                "triggers":["scheduled"],
                "planned_groups":1,
                "detection_generation":harness.runtime().detection.control(id).unwrap()["generation"]
            })),
        )
        .await
        .unwrap();
    let queued_automatic_id = queued_automatic["id"].as_str().unwrap().to_owned();
    let waiting = tokio::time::timeout(StdDuration::from_secs(10), async {
        loop {
            let job = harness.runtime().tests.get(&queued_automatic_id).unwrap();
            if job["waiting_until"] == datetime(reference_time() + Duration::seconds(cooldown)) {
                break job;
            }
            tokio::time::sleep(StdDuration::from_millis(5)).await;
        }
    })
    .await
    .expect("accepted automatic ModelTests job must wait for the persisted cooldown");
    assert_eq!(waiting["status"], "queued");
    assert_eq!(
        waiting["waiting_until"],
        datetime(reference_time() + Duration::seconds(cooldown))
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    let lease = harness
        .runtime()
        .actions
        .locks
        .try_account(&live)
        .expect("cooldown-waiting automatic job must not hold an AccountLease");
    drop(lease);

    let gate = Gate::new();
    harness.mock.set_model_gate(Some(gate.clone()));
    let manual_rows = [
        harness.seed_account("apikey", true, json!({})).await,
        harness.seed_account("apikey", true, json!({})).await,
    ];
    let mut manual_jobs = Vec::new();
    for (index, manual_row) in manual_rows.iter().enumerate() {
        let manual_id = integer(&manual_row["id"]).unwrap();
        let manual_live = harness
            .runtime()
            .actions
            .account(manual_id, None)
            .await
            .unwrap();
        let manual = harness
            .runtime()
            .tests
            .start(
                manual_id,
                &json!({
                    "model_id":"gpt-6-luna",
                    "expected_version":account_version(&manual_live),
                    "request_id":format!("fixture-cooldown-manual-{index}"),
                    "concurrency":1
                }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(manual["status"], "queued");
        manual_jobs.push(manual["id"].as_str().unwrap().to_owned());
    }
    harness.mock.wait_for("POST", "/v1/responses", 3).await;
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 3);
    assert_eq!(
        harness.runtime().tests.get(&queued_automatic_id).unwrap()["waiting_until"],
        datetime(reference_time() + Duration::seconds(cooldown))
    );
    gate.release.notify_waiters();
    harness.mock.set_model_gate(None);
    for manual_job_id in &manual_jobs {
        assert_eq!(
            harness.wait_job_terminal(manual_job_id).await["status"],
            "completed"
        );
    }
    let manual_request_count = harness.mock.count("POST", "/v1/responses");
    assert!(manual_request_count >= 3);
    let queued_cancelled = harness
        .runtime()
        .tests
        .cancel(&queued_automatic_id)
        .await
        .unwrap();
    assert_eq!(queued_cancelled["status"], "cancelled");
    assert_eq!(
        harness.runtime().detection.next_allowed_at(id).unwrap(),
        datetime(reference_time() + Duration::seconds(cooldown))
    );

    harness
        .runtime()
        .detection
        .update(id, json!({"model_id":"gpt-6-luna"}))
        .unwrap();
    harness.advance(1);
    let second = harness
        .runtime()
        .detection
        .trigger(id, &["automatic-api".into()])
        .await
        .unwrap();
    let second_job = harness
        .wait_job_terminal(second["id"].as_str().unwrap())
        .await;
    assert_eq!(second_job["status"], "completed");
    assert_eq!(
        harness.mock.count("POST", "/v1/responses"),
        manual_request_count + 1
    );
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()]
            ["started_at"],
        datetime(reference_time() + Duration::seconds(cooldown))
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn failed_dispatched_job_still_owns_cooldown_and_retry_keeps_original_timestamp() {
    let cooldown = fixture()["oracle"]["automatic_dispatch_cooldown_seconds"]
        .as_i64()
        .unwrap();
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness
        .mock
        .set_model_failure_response(1, StatusCode::BAD_REQUEST.as_u16());
    harness.runtime().detection.tick().await.unwrap();
    let failed_job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let failed = harness.wait_job_terminal(&failed_job_id).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error_code"], "invalid_model_or_request");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    let first_start = harness.runtime().detection.store.read().unwrap()["automatic_starts"]
        [id.to_string()]
    .clone();
    assert_eq!(first_start["job_id"], failed_job_id);
    assert_eq!(first_start["started_at"], datetime(reference_time()));

    harness
        .runtime()
        .tests
        .store
        .transaction(|state| {
            state["jobs"] = json!({});
            state["accounts"] = json!({});
            state["requests"] = json!({});
            Ok(())
        })
        .unwrap();
    harness.restart_runtime().await;
    harness.runtime().detection.migrate_dispatches().unwrap();
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()],
        first_start
    );
    assert_eq!(
        harness.runtime().detection.next_allowed_at(id).unwrap(),
        datetime(reference_time() + Duration::seconds(cooldown))
    );

    harness.advance(cooldown - 1);
    let cooling = harness
        .runtime()
        .detection
        .trigger(id, &["error:fixture-failed-job".into()])
        .await
        .unwrap();
    assert!(cooling.is_null());
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()],
        first_start
    );

    harness.advance(1);
    let next = harness
        .runtime()
        .detection
        .trigger(id, &["error:fixture-failed-job-retry".into()])
        .await
        .unwrap();
    let completed = harness
        .wait_job_terminal(next["id"].as_str().unwrap())
        .await;
    assert_eq!(completed["status"], "completed");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 2);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()]
            ["started_at"],
        datetime(reference_time() + Duration::seconds(cooldown))
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn retryable_model_transport_failure_preserves_first_dispatch_timestamp() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.mock.set_model_failures(1);
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["status"], "completed");
    let attempts = job["attempts"].as_i64().unwrap();
    assert_eq!(attempts, 2);
    assert_eq!(
        harness.mock.count("POST", "/v1/responses"),
        attempts as usize
    );
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()],
        json!({"job_id":job_id,"started_at":datetime(reference_time())})
    );
    assert_eq!(
        harness.runtime().detection.next_allowed_at(id).unwrap(),
        datetime(reference_time() + Duration::seconds(300))
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn capacity_provider_and_slow_evidence_survive_bark_config_and_legacy_queue_upgrade() {
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .poll()
        .await
        .expect("initialize capacity cursor before inserting synthetic evidence");
    tokio::time::sleep(StdDuration::from_millis(20)).await;

    let provider_error_id = id * 100;
    let first_usage_id = id * 1_000;
    harness
        .insert_capacity_error(
            provider_error_id,
            id,
            Utc::now() - Duration::milliseconds(1),
        )
        .await;
    harness.insert_slow_usage_window(id, first_usage_id).await;
    harness
        .runtime()
        .detection
        .capacity
        .poll()
        .await
        .expect("poll real provider and slow-TTFT business evidence with valid Bark enabled");

    let provider_clue = format!("error:{provider_error_id}");
    let slow_clue = format!("slow:{id}:{first_usage_id}");
    let state = harness
        .runtime()
        .detection
        .capacity
        .store
        .snapshot()
        .unwrap();
    let observed = state["detection_events"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(observed, [provider_clue.clone(), slow_clue.clone()].into());
    assert_eq!(
        state["notifications"][provider_error_id.to_string()]["status"],
        "clue"
    );
    assert_eq!(state["notifications"][&slow_clue]["status"], "clue");
    assert_eq!(state["pending"], json!({}));
    assert_eq!(state["slow_pending"], json!({}));
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .expect("clue-only capacity state must not create a result Bark notification");
    harness
        .runtime()
        .detection
        .capacity
        .poll()
        .await
        .expect("repeated scan without new input must leave clues unchanged");
    assert_eq!(
        harness
            .runtime()
            .detection
            .capacity
            .store
            .snapshot()
            .unwrap()["detection_events"],
        state["detection_events"]
    );

    harness.runtime().detection.bark.configure(
        &json!({"enabled":false,"device_key":"fixture-only","server_url":harness.base_url}),
        true,
    );
    let disabled_error_id = provider_error_id + 1;
    harness
        .insert_capacity_error(
            disabled_error_id,
            id,
            Utc::now() + Duration::milliseconds(1),
        )
        .await;
    harness
        .runtime()
        .detection
        .capacity
        .poll()
        .await
        .expect("disabled Bark must not suppress a newly discovered clue");
    let disabled_clue = format!("error:{disabled_error_id}");
    let disabled_state = harness
        .runtime()
        .detection
        .capacity
        .store
        .snapshot()
        .unwrap();
    assert!(disabled_state["detection_events"]
        .get(&disabled_clue)
        .is_some());
    let disabled_observed = disabled_state["detection_events"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        disabled_observed,
        [
            provider_clue.clone(),
            slow_clue.clone(),
            disabled_clue.clone()
        ]
        .into()
    );
    assert_eq!(disabled_state["pending"], json!({}));
    assert_eq!(disabled_state["slow_pending"], json!({}));
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .expect("disabled Bark must not turn capacity clues into result notifications");
    assert_eq!(harness.mock.count("POST", "/push"), 0);

    harness.runtime().detection.bark.configure(
        &json!({"enabled":true,"device_key":"fixture-only","server_url":"invalid fixture URL"}),
        false,
    );
    let second_error_id = provider_error_id + 2;
    harness
        .insert_capacity_error(second_error_id, id, Utc::now() + Duration::milliseconds(1))
        .await;
    harness
        .runtime()
        .detection
        .capacity
        .poll()
        .await
        .expect("invalid optional Bark configuration must not suppress a new provider clue");
    let second_clue = format!("error:{second_error_id}");
    let repeated = harness
        .runtime()
        .detection
        .capacity
        .store
        .snapshot()
        .unwrap();
    assert!(repeated["detection_events"].get(&second_clue).is_some());
    let repeated_observed = repeated["detection_events"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        repeated_observed,
        [
            provider_clue.clone(),
            slow_clue.clone(),
            disabled_clue.clone(),
            second_clue.clone()
        ]
        .into()
    );
    assert_eq!(repeated["pending"], json!({}));
    assert_eq!(repeated["slow_pending"], json!({}));
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .expect("invalid optional Bark config must retain capacity clues without emitting results");
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    harness
        .runtime()
        .detection
        .capacity
        .poll()
        .await
        .expect("repeated provider and slow scans must remain deduplicated");
    assert_eq!(
        harness
            .runtime()
            .detection
            .capacity
            .store
            .snapshot()
            .unwrap()["detection_events"]
            .as_object()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);

    harness
        .runtime()
        .detection
        .capacity
        .store
        .transaction(|state| {
            state.as_object_mut().unwrap().remove("clue_mode_since");
            state.as_object_mut().unwrap().remove("detection_since");
            state["pending"]["fixture-legacy-error"] = json!({
                "id":provider_error_id + 90,
                "account_id":id,
                "account_name":"fixture account",
                "account_type":"apikey",
                "requested_model":"gpt-6-luna",
                "upstream_model":"gpt-6-luna",
                "message":"synthetic legacy notice",
                "created_at":datetime(Utc::now()),
                "next_at":datetime(Utc::now()),
                "attempts":2
            });
            state["slow_pending"]["fixture-legacy-slow"] = json!({
                "kind":"slow_ttft",
                "id":provider_error_id + 91,
                "account_id":id,
                "account_name":"fixture account",
                "account_type":"apikey",
                "requested_model":"gpt-6-luna",
                "upstream_model":"gpt-6-luna",
                "message":"synthetic legacy slow notice",
                "created_at":datetime(Utc::now()),
                "next_at":datetime(Utc::now()),
                "attempts":2,
                "sample_count":10,
                "slow_count":8,
                "threshold_ms":10000,
                "latest_first_token_ms":11001
            });
            Ok(())
        })
        .unwrap();
    harness
        .insert_capacity_error(
            provider_error_id + 92,
            id,
            Utc::now() - Duration::seconds(1),
        )
        .await;
    harness.restart_runtime().await;
    harness
        .runtime()
        .detection
        .capacity
        .poll()
        .await
        .expect("upgrade must retire legacy queues before polling new evidence");
    let upgraded = harness
        .runtime()
        .detection
        .capacity
        .store
        .snapshot()
        .unwrap();
    assert_eq!(upgraded["pending"], json!({}));
    assert_eq!(upgraded["slow_pending"], json!({}));
    assert_eq!(upgraded["detection_events"], json!({}));
    assert_eq!(
        upgraded["notifications"]["fixture-legacy-error"]["status"],
        "retired"
    );
    assert_eq!(
        upgraded["notifications"]["fixture-legacy-slow"]["status"],
        "retired"
    );
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .expect("retired legacy queues must not be replayed after restart");
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn automatic_sol_prediction_uses_requested_astra_model_and_stops_after_first_group() {
    let mut harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    harness
        .restart_with_bank_state(&bank_state_with_swapped_model_labels(
            model_detection::TARGET,
            "gpt-6-sol",
        ))
        .await;
    let bank = harness.runtime().tests.bank.snapshot();
    let models = bank.0["models"].as_array().unwrap();
    assert!(models
        .iter()
        .any(|model| model["id"] == model_detection::TARGET));
    assert!(models.iter().any(|model| model["id"] == "gpt-6-sol"));

    harness.mock.set_models(vec![
        json!({"id":"gpt-6-astra","name":"fixture requested model"}),
        json!({"id":"gpt-6-sol","name":"fixture forwarded model"}),
        json!({"id":"gpt-6-luna","name":"fixture model"}),
        json!({"id":model_detection::TARGET,"name":"fixture legacy target"}),
    ]);
    let row = harness.seed_account("oauth", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness
        .enable_detection_with_model(id, 1, "gpt-6-astra")
        .await;
    harness
        .runtime()
        .detection
        .update(id, json!({"model_id":"fixture-old-account-model"}))
        .unwrap();
    let budget = harness
        .runtime()
        .budget
        .clone()
        .acquire_many_owned(24)
        .await
        .unwrap();
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let queued = harness.runtime().tests.get(&job_id).unwrap();
    assert_eq!(queued["requested_model"], "gpt-6-astra");
    assert_eq!(queued["forwarded_model"], "gpt-6-astra");
    let visible = harness.runtime().detection.view(id).unwrap();
    assert_eq!(visible["model_id"], "gpt-6-astra");
    assert_eq!(visible["model_source"], "global");
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["model_id"],
        "fixture-old-account-model"
    );
    harness.set_detection_model("gpt-6-luna").await;
    let reused = harness
        .runtime()
        .detection
        .trigger(id, &["automatic-api".into()])
        .await
        .unwrap();
    assert_eq!(reused["id"], job_id);
    assert_eq!(reused["requested_model"], "gpt-6-astra");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    harness
        .runtime()
        .tests
        .update(&job_id, json!({"forwarded_model":"gpt-6-sol"}))
        .unwrap();
    drop(budget);

    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["requested_model"], "gpt-6-astra");
    assert_eq!(job["forwarded_model"], "gpt-6-sol");
    assert_eq!(job["first_group_valid"], true);
    assert_eq!(job["first_group_prediction"], "gpt-6-sol");
    assert_eq!(job["report"]["prediction"], "gpt-6-sol");
    assert_eq!(job["automatic_disposition"]["status"], "completed");
    assert_eq!(job["completion_reason"], "automatic_degradation");
    assert_eq!(
        job["groups"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|group| group["status"] == "completed")
            .count(),
        1
    );
    let responses = harness.mock.calls("POST", "/v1/responses");
    assert_eq!(responses.len(), 1);
    assert_native_oauth_calls_for(&responses, "gpt-6-astra");
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(
        harness.runtime().actions.account(id, None).await.unwrap()["schedulable"],
        false
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn automatic_first_group_verdict_confirms_schedulable_write_before_bark_and_deduplicates() {
    let mut harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    let account_name = row["name"].as_str().unwrap().to_owned();
    let clue_id = format!("error:fixture-automatic-bark-{id}");
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .transaction(|state| {
            state["detection_events"][&clue_id] =
                json!({"account_id":id,"created_at":datetime(harness.now())});
            Ok(())
        })
        .unwrap();

    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["completion_reason"], "automatic_degradation");
    assert_eq!(job["first_group_prediction"], "gpt-5.6-luna");
    assert_eq!(job["report"]["prediction"], "gpt-5.6-luna");
    assert!(job["report"]["probability"].as_f64().unwrap() < 0.99);
    assert_eq!(job["automatic_disposition"]["status"], "completed");
    assert_eq!(job["triggers"], json!([clue_id]));
    assert_eq!(job["groups"][0]["diagnostics"]["http_status"], 200);
    assert_eq!(
        job["groups"][0]["diagnostics"]["response_protocol"],
        "responses"
    );
    assert_eq!(job["groups"][0]["diagnostics"]["events"], 2);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);

    let schedule_writes = harness
        .mock
        .calls("POST", &format!("/api/v1/admin/accounts/{id}/schedulable"));
    assert_eq!(schedule_writes.len(), 1);
    assert!(schedule_writes[0].admin_authorized);
    assert!(!schedule_writes[0].bearer_authorized);
    assert_eq!(schedule_writes[0].payload, json!({"schedulable":false}));
    assert_eq!(
        schedule_writes[0].path,
        format!("/api/v1/admin/accounts/{id}/schedulable")
    );
    let control = harness.runtime().detection.view(id).unwrap();
    assert_eq!(control["disposition"]["status"], "completed");
    assert_eq!(control["disposition"]["marked"], true);
    assert_eq!(control["disposition"]["schedule_verified"], true);
    assert_eq!(control["hold"], true);
    assert_eq!(control["disposition_notification"]["status"], "queued");
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT schedulable FROM accounts WHERE id=$1")
            .bind(id)
            .fetch_one(&harness.runtime().db.pool)
            .await
            .unwrap()
    );

    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    let bark = harness.mock.calls("POST", "/push");
    assert_eq!(bark.len(), 1);
    assert_eq!(bark[0].payload["level"], "critical");
    assert_eq!(bark[0].payload["sound"], "alarm");
    assert!(bark[0].payload["body"]
        .as_str()
        .unwrap()
        .contains(&format!("#{id}")));
    assert!(bark[0].payload["body"]
        .as_str()
        .unwrap()
        .contains(&account_name));
    assert!(bark[0].payload["title"]
        .as_str()
        .unwrap()
        .contains("已停止调度"));
    assert!(bark[0].payload["title"].as_str().unwrap().contains("降智"));
    assert!(bark[0].payload["body"]
        .as_str()
        .unwrap()
        .contains("首组指纹推测：gpt-5.6-luna"));
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    assert_eq!(
        harness.runtime().detection.view(id).unwrap()["disposition_notification"]["status"],
        "delivered"
    );
    harness.restart_runtime().await;
    let resumed_disposition = harness
        .runtime()
        .detection
        .finish_disposition(id)
        .await
        .unwrap();
    assert_eq!(resumed_disposition["status"], "completed");
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["disposition_notifications"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    let consumed_clue =
        harness.runtime().detection.store.read().unwrap()["consumed"][&clue_id].clone();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .transaction(|state| {
            state["detection_events"][&clue_id] =
                json!({"account_id":id,"created_at":datetime(harness.now())});
            Ok(())
        })
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["consumed"][&clue_id],
        consumed_clue
    );
    assert!(harness
        .runtime()
        .detection
        .capacity
        .store
        .snapshot()
        .unwrap()["detection_events"]
        .get(&clue_id)
        .is_none());
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    assert_eq!(
        harness.runtime().detection.view(id).unwrap()["disposition_notification"]["status"],
        "delivered"
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn automatic_notification_retries_use_durable_backoff_and_send_once_after_success() {
    let expected = fixture();
    let delays = expected["oracle"]["notification_retry_seconds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_i64().unwrap())
        .collect::<Vec<_>>();
    let mut harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.mock.set_bark_failures(delays.len());
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["automatic_disposition"]["status"], "completed");
    assert_eq!(harness.mock.count("POST", "/push"), 0);

    for (index, delay) in delays.iter().enumerate() {
        harness
            .runtime()
            .detection
            .deliver_notifications()
            .await
            .unwrap();
        let attempt = index as i64 + 1;
        let event =
            harness.runtime().detection.view(id).unwrap()["disposition_notification"].clone();
        assert_eq!(event["status"], "retry");
        assert_eq!(event["attempts"], attempt);
        assert_eq!(
            event["next_at"],
            datetime(harness.now() + Duration::seconds(*delay))
        );
        assert_eq!(harness.mock.count("POST", "/push"), attempt as usize);
        harness
            .runtime()
            .detection
            .deliver_notifications()
            .await
            .unwrap();
        assert_eq!(harness.mock.count("POST", "/push"), attempt as usize);
        harness.advance(*delay);
    }
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), delays.len() + 1);
    assert_eq!(
        harness.runtime().detection.view(id).unwrap()["disposition_notification"]["status"],
        "delivered"
    );
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), delays.len() + 1);

    let body_error_row = harness.seed_account("apikey", true, json!({})).await;
    let body_error_id = integer(&body_error_row["id"]).unwrap();
    harness.enable_detection(body_error_id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            body_error_id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.mock.set_bark_response_failures(1);
    harness.runtime().detection.tick().await.unwrap();
    let body_error_job_id = harness.runtime().detection.control(body_error_id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body_error_job = harness.wait_job_terminal(&body_error_job_id).await;
    assert_eq!(
        body_error_job["automatic_disposition"]["status"],
        "completed"
    );
    let pushes_before_body_error = harness.mock.count("POST", "/push");
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    let body_error_retry = harness.runtime().detection.view(body_error_id).unwrap()
        ["disposition_notification"]
        .clone();
    assert_eq!(body_error_retry["status"], "retry");
    assert_eq!(body_error_retry["attempts"], 1);
    assert_eq!(body_error_retry["error_code"], "bark_response_code");
    assert_eq!(
        body_error_retry["next_at"],
        datetime(harness.now() + Duration::seconds(5))
    );
    assert_eq!(
        harness.mock.count("POST", "/push"),
        pushes_before_body_error + 1
    );

    harness.restart_runtime().await;
    let persisted_body_error = harness.runtime().detection.view(body_error_id).unwrap()
        ["disposition_notification"]
        .clone();
    assert_eq!(persisted_body_error["status"], "retry");
    assert_eq!(persisted_body_error["attempts"], 1);
    assert_eq!(persisted_body_error["error_code"], "bark_response_code");
    assert_eq!(persisted_body_error["next_at"], body_error_retry["next_at"]);
    harness.advance(4);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(
        harness.mock.count("POST", "/push"),
        pushes_before_body_error + 1
    );
    harness.advance(1);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(
        harness.mock.count("POST", "/push"),
        pushes_before_body_error + 2
    );
    let delivered_body_error = harness.runtime().detection.view(body_error_id).unwrap()
        ["disposition_notification"]
        .clone();
    assert_eq!(delivered_body_error["status"], "delivered");
    assert_eq!(delivered_body_error["attempts"], 2);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn preexisting_writing_disposition_becomes_unknown_after_restart_without_replay() {
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    let before_mark = harness.runtime().detection.mark(id).unwrap();
    let job_id = "d".repeat(32);
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            before_mark["version"].as_str().unwrap(),
            harness.now(),
            Some(&job_id),
        )
        .unwrap();
    let mark = harness.runtime().detection.mark(id).unwrap();
    let control = harness.runtime().detection.control(id).unwrap();
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({
                "hold":true,
                "status":"paused",
                "disposition":{
                    "status":"writing",
                    "job_id":job_id,
                    "generation":control["generation"],
                    "mark_version":mark["version"],
                    "marked":true,
                    "schedule_verified":false,
                    "created_at":datetime(harness.now()),
                    "model_id":"gpt-6-luna",
                    "prediction":"gpt-5.6-luna",
                    "probability":0.97,
                    "triggers":["scheduled"]
                }
            }),
        )
        .unwrap();

    harness.restart_runtime().await;
    harness.runtime().detection.tick().await.unwrap();

    let disposition = harness.runtime().detection.control(id).unwrap()["disposition"].clone();
    assert_eq!(disposition["status"], "needs_confirmation");
    assert!(disposition["reason"]
        .as_str()
        .unwrap()
        .contains("未重复写入"));
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["hold"],
        true
    );
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(
        harness.runtime().actions.account(id, None).await.unwrap()["schedulable"],
        true
    );
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn confirmed_bark_waits_for_enabled_valid_config_and_invalid_config_preserves_queue() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let first = harness.seed_account("apikey", true, json!({})).await;
    let first_id = integer(&first["id"]).unwrap();
    harness.enable_detection(first_id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            first_id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.bark.configure(
        &json!({"enabled":false,"device_key":"fixture-only","server_url":harness.base_url}),
        true,
    );
    harness.runtime().detection.tick().await.unwrap();
    let first_job_id = harness.runtime().detection.control(first_id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_job = harness.wait_job_terminal(&first_job_id).await;
    assert_eq!(first_job["automatic_disposition"]["status"], "completed");
    let first_notice =
        harness.runtime().detection.view(first_id).unwrap()["disposition_notification"].clone();
    assert_eq!(first_notice["status"], "queued");
    assert_eq!(first_notice["attempts"], 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    let suppressed =
        harness.runtime().detection.view(first_id).unwrap()["disposition_notification"].clone();
    assert_eq!(suppressed["status"], "suppressed");
    assert_eq!(suppressed["reason"], "disabled");
    assert_eq!(suppressed["attempts"], 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);

    let second = harness.seed_account("apikey", true, json!({})).await;
    let second_id = integer(&second["id"]).unwrap();
    harness.enable_detection(second_id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            second_id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.bark.configure(
        &json!({"enabled":true,"device_key":"fixture-only","server_url":harness.base_url}),
        true,
    );
    harness.runtime().detection.tick().await.unwrap();
    let second_job_id = harness.runtime().detection.control(second_id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_job = harness.wait_job_terminal(&second_job_id).await;
    assert_eq!(second_job["automatic_disposition"]["status"], "completed");
    harness.runtime().detection.bark.configure(
        &json!({"enabled":true,"device_key":"fixture-only","server_url":"invalid fixture URL"}),
        false,
    );
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    let invalid_config =
        harness.runtime().detection.view(second_id).unwrap()["disposition_notification"].clone();
    assert_eq!(invalid_config["status"], "queued");
    assert_eq!(invalid_config["attempts"], 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    harness.runtime().detection.bark.configure(
        &json!({"enabled":true,"device_key":"fixture-only","server_url":harness.base_url}),
        true,
    );
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    let bark = harness.mock.calls("POST", "/push");
    assert_eq!(bark.len(), 1);
    assert_eq!(bark[0].payload["level"], "critical");
    assert_eq!(bark[0].payload["sound"], "alarm");
    assert_eq!(
        harness.runtime().detection.view(second_id).unwrap()["disposition_notification"]["status"],
        "delivered"
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn disposition_response_timeout_verifies_committed_write_without_replay() {
    let mut harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    let gate = Gate::new();
    harness.mock.set_schedulable_write_before_gate(true);
    harness.mock.set_schedulable_gate(Some(gate.clone()));
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    tokio::time::timeout(StdDuration::from_secs(10), gate.entered.notified())
        .await
        .expect("verified disposition must reach the loopback scheduling write");
    let schedulable = sqlx::query_scalar::<_, bool>("SELECT schedulable FROM accounts WHERE id=$1")
        .bind(id)
        .fetch_one(&harness.fixture_db.pool)
        .await
        .unwrap();
    assert!(
        !schedulable,
        "the loopback write must commit before its response is gated"
    );
    tokio::time::timeout(StdDuration::from_secs(12), async {
        loop {
            let status = harness.runtime().detection.control(id).unwrap()["disposition"]["status"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            if status == "completed" {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect(
        "after the gated response times out, PostgreSQL readback must confirm the committed write",
    );
    let disposition = harness.runtime().detection.view(id).unwrap()["disposition"].clone();
    assert_eq!(disposition["status"], "completed");
    assert_eq!(disposition["schedule_verified"], true);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["disposition_notifications"][&job_id]
            ["status"],
        "queued"
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);

    harness.mock.set_schedulable_gate(None);
    gate.release.notify_one();
    let _ = harness.wait_job_terminal(&job_id).await;
    harness.restart_runtime().await;
    let confirmed = harness
        .runtime()
        .detection
        .finish_disposition(id)
        .await
        .unwrap();
    assert_eq!(confirmed["status"], "completed");
    assert_eq!(confirmed["schedule_verified"], true);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["disposition_notifications"][&job_id]
            ["status"],
        "queued"
    );
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

fn fresh_empty_credits(now: DateTime<Utc>) -> Value {
    json!({
        "codex_credits_snapshot":{
            "credits":{"has_credits":false,"unlimited":false,"balance":0},
            "fetched_at":now.timestamp() as f64
        }
    })
}

fn fresh_available_credits(now: DateTime<Utc>) -> Value {
    json!({
        "codex_credits_snapshot":{
            "credits":{"has_credits":true,"unlimited":false,"balance":3},
            "fetched_at":now.timestamp() as f64
        }
    })
}

fn full_usage_result(now: DateTime<Utc>) -> Value {
    json!({
        "template_type":"oauth",
        "success":true,
        "queried_at":datetime(now),
        "oauth_quota":{
            "plan_type":"plus",
            "updated_at":datetime(now),
            "ui_windows":[
                {"key":"codex_5h","used_percent":100,"reset_at":datetime(now+Duration::hours(1))},
                {"key":"codex_7d","used_percent":100,"reset_at":datetime(now+Duration::days(1))}
            ]
        }
    })
}

#[tokio::test]
async fn quota_evidence_uses_independent_freshness_precedence_and_configured_key_limits() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let now = harness.now();
    let empty_row = harness
        .seed_account("oauth", true, fresh_empty_credits(now))
        .await;
    let empty_id = integer(&empty_row["id"]).unwrap();
    let mut update = Update::default();
    update.results.insert(empty_id, full_usage_result(now));
    harness.runtime().monitor.store.commit(update).unwrap();
    let current_empty = harness.runtime().monitor.row(empty_id).await.unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(
                &current_empty,
                &harness.runtime().detection.mark(empty_id).unwrap(),
                false
            )
            .await
            .unwrap(),
        "已确认额度耗尽"
    );
    harness.enable_detection(empty_id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            empty_id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(
        harness.runtime().detection.control(empty_id).unwrap()["status"],
        "paused"
    );
    assert_eq!(
        harness.runtime().detection.control(empty_id).unwrap()["reason"],
        "已确认额度耗尽"
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);

    let available_row = harness
        .seed_account("oauth", true, fresh_available_credits(now))
        .await;
    let available_id = integer(&available_row["id"]).unwrap();
    let mut update = Update::default();
    update
        .results
        .insert(available_id, full_usage_result(now - Duration::hours(2)));
    harness.runtime().monitor.store.commit(update).unwrap();
    let available = harness.runtime().monitor.row(available_id).await.unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(
                &available,
                &harness.runtime().detection.mark(available_id).unwrap(),
                false
            )
            .await
            .unwrap(),
        ""
    );
    harness.enable_detection(available_id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            available_id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let allowed_job_id = harness.runtime().detection.control(available_id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let allowed = harness.wait_job_terminal(&allowed_job_id).await;
    assert_eq!(allowed["status"], "completed");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    let oauth_calls = harness.mock.calls("POST", "/v1/responses");
    assert_native_oauth_calls(&oauth_calls);

    let mut manual_close = available.clone();
    manual_close["schedulable"] = false.into();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(
                &manual_close,
                &harness.runtime().detection.mark(available_id).unwrap(),
                false
            )
            .await
            .unwrap(),
        "调度已关闭"
    );
    let mut upstream_limited = available.clone();
    upstream_limited["rate_limit_reset_at"] = datetime(now + Duration::minutes(5)).into();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(
                &upstream_limited,
                &harness.runtime().detection.mark(available_id).unwrap(),
                false
            )
            .await
            .unwrap(),
        "账号仍在上游限流中"
    );

    let key_extra = json!({
        "quota_daily_limit":100,
        "quota_daily_used":100,
        "quota_daily_reset_mode":"fixed",
        "quota_daily_reset_at":datetime(now+Duration::hours(1))
    });
    let key_row = harness.seed_account("apikey", true, key_extra).await;
    let key_id = integer(&key_row["id"]).unwrap();
    let key = harness.runtime().monitor.row(key_id).await.unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(
                &key,
                &harness.runtime().detection.mark(key_id).unwrap(),
                false
            )
            .await
            .unwrap(),
        "已确认额度耗尽"
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert!(harness.mock.unexpected().is_empty());

    let disabled_row = harness.seed_account("apikey", true, json!({})).await;
    let disabled_id = integer(&disabled_row["id"]).unwrap();
    harness.enable_detection(disabled_id, 1).await;
    let disabled_mark = harness.runtime().detection.mark(disabled_id).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            disabled_id,
            true,
            disabled_mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-disabled-recheck"),
        )
        .unwrap();
    sqlx::query("UPDATE accounts SET status='disabled',updated_at=$1 WHERE id=$2")
        .bind(harness.now())
        .bind(disabled_id)
        .execute(&harness.runtime().db.pool)
        .await
        .unwrap();
    let disabled_live = harness.runtime().monitor.row(disabled_id).await.unwrap();
    let disabled_mark = harness.runtime().detection.mark(disabled_id).unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(&disabled_live, &disabled_mark, true)
            .await
            .unwrap(),
        "账号停用或认证异常"
    );
    harness
        .runtime()
        .detection
        .update(
            disabled_id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let disabled_control = harness.runtime().detection.control(disabled_id).unwrap();
    assert_eq!(disabled_control["status"], "paused");
    assert_eq!(disabled_control["reason"], "账号停用或认证异常");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn marked_recheck_waits_for_shared_cooldown_then_clears_mark_without_schedule_write() {
    let expected = fixture();
    let cooldown = expected["oracle"]["marked_recheck_shared_cooldown_seconds"]
        .as_i64()
        .unwrap();
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    let mark = harness.runtime().detection.mark(id).unwrap();
    let marked = harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-previous-job"),
        )
        .unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({
                "next_at":datetime(harness.now()),
                "hold":true,
                "disposition":{"status":"completed","marked":true,"schedule_verified":true,"job_id":"fixture-previous-job"}
            }),
        )
        .unwrap();
    harness
        .runtime()
        .detection
        .store
        .transaction(|state| {
            state["automatic_starts"][id.to_string()] = json!({
                "job_id":"fixture-previous-job",
                "started_at":datetime(harness.now()-Duration::seconds(100))
            });
            Ok(())
        })
        .unwrap();

    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["next_at"],
        datetime(harness.now() + Duration::seconds(cooldown - 100))
    );
    harness.advance(cooldown - 101);
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    harness.advance(1);
    let live_account = harness.runtime().actions.account(id, None).await.unwrap();
    let expected_account_version =
        sub2ops_server::compat::operation_versions(&live_account, false)["model_test"].clone();
    harness.runtime().detection.tick().await.unwrap();
    harness.mock.wait_for("POST", "/v1/responses", 1).await;
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["automatic"], true);
    assert_eq!(job["degradation_recheck"], true);
    assert_eq!(job["requested_model"], "gpt-6-luna");
    assert_eq!(job["detection_mark_version"], marked["version"]);
    assert_eq!(job["recheck_schedulable"], true);
    assert_eq!(job["recheck_account_version"], expected_account_version);
    assert_eq!(
        job["detection_generation"],
        harness.runtime().detection.control(id).unwrap()["generation"]
    );
    assert_eq!(job["first_group_valid"], true);
    assert_eq!(job["first_group_prediction"], "claude-sonnet-4-6");
    harness.runtime().detection.tick().await.unwrap();
    let control = harness.runtime().detection.control(id).unwrap();
    assert_eq!(control["recheck_recovery"]["status"], "completed");
    assert_eq!(control["recheck_recovery"]["job_id"], job_id);
    assert_eq!(control["recheck_recovery"]["mark_cleared"], true);
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        false
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    let bark = harness.mock.calls("POST", "/push");
    assert_eq!(bark.len(), 1);
    assert!(bark[0].payload["title"]
        .as_str()
        .unwrap()
        .contains("检测恢复正常"));
    assert!(bark[0].payload["body"]
        .as_str()
        .unwrap()
        .contains("调度保持开启"));
    let delivered_event = harness.runtime().detection.store.read().unwrap()
        ["disposition_notifications"][&job_id]
        .clone();
    assert_eq!(delivered_event["status"], "delivered");
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["hold"],
        true
    );
    harness.restart_runtime().await;
    let repeated_receipt = harness
        .runtime()
        .detection
        .complete_recheck(&job)
        .await
        .unwrap();
    assert_eq!(repeated_receipt["status"], "completed");
    assert_eq!(repeated_receipt["mark_cleared"], true);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["recheck_recovery"]["status"],
        "completed"
    );
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["hold"],
        true
    );
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["disposition_notifications"][&job_id]
            ["status"],
        "delivered"
    );
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()]
            ["started_at"],
        datetime(reference_time() + Duration::seconds(cooldown - 100))
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn sol_request_with_astra_prediction_stays_unmarked_and_clears_a_qualified_recheck() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    harness.mock.set_models(vec![
        json!({"id":"gpt-6.1-sol","name":"fixture requested model"}),
        json!({"id":"gpt-6.1-astra","name":"fixture forwarded model"}),
        json!({"id":"gpt-6-astra","name":"fixture prediction"}),
    ]);
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness
        .enable_detection_with_model(id, 1, "gpt-6.1-sol")
        .await;

    let control = harness.runtime().detection.control(id).unwrap();
    let initial_mark = harness.runtime().detection.mark(id).unwrap();
    let verdict_job = json!({
        "id":"fixture-sol-astra-verdict",
        "account_id":id,
        "automatic":true,
        "degradation_recheck":false,
        "detection_generation":control["generation"],
        "detection_mark_version":initial_mark["version"],
        "requested_model":"gpt-6.1-sol",
        "forwarded_model":"gpt-6.1-astra",
        "triggers":["scheduled"]
    });
    let astra_report = json!({
        "prediction":"gpt-6-astra",
        "used_outputs":1,
        "probability":0.97
    });
    assert!(harness
        .runtime()
        .detection
        .verdict(&verdict_job, &astra_report)
        .await
        .unwrap()
        .is_null());
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        false
    );
    assert!(harness.runtime().detection.control(id).unwrap()["disposition"].is_null());
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );

    let marked = harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            initial_mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-prior-sol-astra-degradation"),
        )
        .unwrap();
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({
                "hold":true,
                "disposition":{"status":"completed","marked":true,"schedule_verified":true,"job_id":"fixture-prior-sol-astra-degradation"}
            }),
        )
        .unwrap();
    let current = harness.runtime().detection.control(id).unwrap();
    let account = harness.runtime().actions.account(id, None).await.unwrap();
    let account_version =
        sub2ops_server::compat::operation_versions(&account, false)["model_test"].clone();
    let mut recheck_job = completed_recheck_job(
        id,
        &current["generation"],
        &marked["version"],
        &account_version,
        "fixture-sol-astra-recheck",
        harness.now(),
    );
    recheck_job["requested_model"] = "gpt-6.1-sol".into();
    recheck_job["forwarded_model"] = "gpt-6.1-astra".into();
    recheck_job["first_group_prediction"] = "gpt-6-astra".into();
    recheck_job["report"]["prediction"] = "gpt-6-astra".into();

    let receipt = harness
        .runtime()
        .detection
        .complete_recheck(&recheck_job)
        .await
        .unwrap();
    assert_eq!(receipt["status"], "completed");
    assert_eq!(receipt["mark_cleared"], true);
    assert_eq!(receipt["model_id"], "gpt-6.1-sol");
    assert_eq!(receipt["prediction"], "gpt-6-astra");
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        false
    );
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["hold"],
        true
    );
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn failed_first_group_retry_scores_first_sample_separately_from_aggregate() {
    let cooldown = fixture()["oracle"]["automatic_dispatch_cooldown_seconds"]
        .as_i64()
        .unwrap();
    let harness = Harness::new(sample_text("above_99", 0)).await;
    harness.mock.set_model_outputs([
        sample_text("above_99", 2),
        sample_text("automatic_first_group", 0),
    ]);
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    let mark = harness.runtime().detection.mark(id).unwrap();
    let marked = harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-prior-degradation"),
        )
        .unwrap();
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({
                "next_at":datetime(harness.now()-Duration::seconds(1)),
                "hold":true,
                "disposition":{"status":"completed","marked":true,"schedule_verified":true,"job_id":"fixture-prior-degradation"}
            }),
        )
        .unwrap();
    harness
        .mock
        .set_model_failure_response(3, StatusCode::BAD_GATEWAY.as_u16());
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let initial_job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(initial_job["automatic"], true);
    assert_eq!(initial_job["degradation_recheck"], true);
    assert_eq!(initial_job["requested_model"], "gpt-6-luna");
    assert_eq!(initial_job["detection_mark_version"], marked["version"]);
    assert_eq!(initial_job["status"], "completed");
    assert_eq!(initial_job["can_retry"], true);
    assert_eq!(initial_job["first_group_valid"], false);
    assert!(initial_job["first_group_prediction"].is_null());
    assert_eq!(initial_job["groups"][0]["status"], "failed");
    assert_eq!(initial_job["groups"][0]["attempts"], 3);
    assert_eq!(initial_job["groups"][0]["retryable"], true);
    assert_eq!(initial_job["groups"][1]["status"], "completed");
    assert_eq!(initial_job["groups"][2]["status"], "completed");
    assert_eq!(initial_job["report"]["prediction"], "gpt-5.4");
    assert_eq!(initial_job["report"]["used_outputs"], 2);
    assert!(initial_job["report"]["probability"].as_f64().unwrap() < 0.99);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 5);
    let initial_dispatch = harness.runtime().detection.store.read().unwrap()["automatic_starts"]
        [id.to_string()]
    .clone();
    assert_eq!(initial_dispatch["job_id"], job_id);
    assert_eq!(initial_dispatch["started_at"], datetime(reference_time()));
    assert_eq!(
        harness.runtime().detection.next_allowed_at(id).unwrap(),
        datetime(reference_time() + Duration::seconds(cooldown))
    );

    harness.advance(cooldown);
    harness.mock.set_model_outputs([sample_text("above_99", 0)]);
    let gate = Gate::new();
    harness.mock.set_model_gate(Some(gate.clone()));
    let retry = harness
        .runtime()
        .tests
        .retry_failed(&job_id, &uuid::Uuid::new_v4().simple().to_string())
        .await
        .unwrap();
    assert_eq!(retry["status"], "queued");
    assert_eq!(retry["requested_model"], "gpt-6-luna");
    tokio::time::timeout(StdDuration::from_secs(10), gate.entered.notified())
        .await
        .expect("bounded retry must send only the failed first group to the fixture");
    harness.mock.wait_for("POST", "/v1/responses", 6).await;
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 6);
    let retry_in_flight = harness.runtime().tests.get(&job_id).unwrap();
    assert_eq!(retry_in_flight["groups"][1]["status"], "completed");
    assert_eq!(retry_in_flight["groups"][2]["status"], "completed");
    assert_eq!(retry_in_flight["report"]["prediction"], "gpt-5.4");
    assert_eq!(retry_in_flight["report"]["used_outputs"], 2);
    assert_eq!(retry_in_flight["automatic_disposition"], Value::Null);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    gate.release.notify_one();
    harness.mock.set_model_gate(None);

    let completed = harness.wait_job_terminal(&job_id).await;
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["requested_model"], "gpt-6-luna");
    assert_eq!(completed["first_group_valid"], true);
    assert_eq!(completed["first_group_prediction"], "claude-sonnet-4-6");
    assert_eq!(completed["report"]["prediction"], "gpt-5.6-luna");
    assert_eq!(completed["report"]["used_outputs"], 3);
    assert!(completed["report"]["probability"].as_f64().unwrap() < 0.99);
    assert_eq!(completed["automatic_disposition"], Value::Null);
    assert_eq!(completed["groups"][0]["status"], "completed");
    assert_eq!(completed["groups"][0]["attempts"], 1);
    assert_eq!(completed["groups"][1]["status"], "completed");
    assert_eq!(completed["groups"][1]["attempts"], 1);
    assert_eq!(completed["groups"][2]["status"], "completed");
    assert_eq!(completed["groups"][2]["attempts"], 1);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 6);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()],
        initial_dispatch
    );
    assert_eq!(
        harness.runtime().detection.next_allowed_at(id).unwrap(),
        datetime(reference_time() + Duration::seconds(cooldown))
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn reopening_marked_account_still_waits_for_shared_dispatch_cooldown() {
    let cooldown = fixture()["oracle"]["marked_recheck_shared_cooldown_seconds"]
        .as_i64()
        .unwrap();
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", false, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    let initial_mark = harness.runtime().detection.mark(id).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            initial_mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-closed-recheck-prior"),
        )
        .unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({
                "next_at":datetime(harness.now()-Duration::seconds(1)),
                "hold":true,
                "disposition":{"status":"completed","marked":true,"schedule_verified":true,"job_id":"fixture-closed-recheck-prior"}
            }),
        )
        .unwrap();
    harness
        .runtime()
        .detection
        .store
        .transaction(|state| {
            state["automatic_starts"][id.to_string()] = json!({
                "job_id":"fixture-previous-recheck",
                "started_at":datetime(harness.now())
            });
            Ok(())
        })
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["reason"],
        "调度已关闭"
    );

    harness.advance(120);
    let live = harness.runtime().actions.account(id, None).await.unwrap();
    let reopened = harness
        .runtime()
        .set_schedulable(
            id,
            &json!({"schedulable":true,"expected_version":account_version(&live)}),
            ADMIN_KEY,
        )
        .await
        .unwrap();
    assert_eq!(reopened["verified"], true);
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["next_at"],
        datetime(harness.now() + Duration::seconds(cooldown - 120))
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    harness.advance(cooldown - 121);
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    harness.advance(1);
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["degradation_recheck"], true);
    assert_eq!(job["first_group_valid"], true);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        false
    );
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["recheck_recovery"]["status"],
        "completed"
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    let schedules = harness
        .mock
        .calls("POST", &format!("/api/v1/admin/accounts/{id}/schedulable"));
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0].payload, json!({"schedulable":true}));
    assert!(schedules[0].admin_authorized);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn target_recheck_and_stale_generation_cannot_clear_a_mark_or_emit_notifications() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    let mark = harness.runtime().detection.mark(id).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-prior-job"),
        )
        .unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({
                "next_at":datetime(harness.now()-Duration::seconds(1)),
                "hold":true,
                "disposition":{"status":"completed","marked":true,"schedule_verified":true,"job_id":"fixture-prior-job"}
            }),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let target_job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let target_job = harness.wait_job_terminal(&target_job_id).await;
    assert_eq!(target_job["degradation_recheck"], true);
    assert_eq!(target_job["first_group_prediction"], "gpt-5.6-luna");
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert!(harness.runtime().detection.control(id).unwrap()["recheck_recovery"].is_null());
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["hold"],
        true
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );

    let mark = harness.runtime().detection.mark(id).unwrap();
    let stale_generation = harness.runtime().detection.control(id).unwrap()["generation"]
        .as_i64()
        .unwrap()
        - 1;
    let account = harness.runtime().actions.account(id, None).await.unwrap();
    let job = json!({
        "id":"fixture-stale-generation",
        "account_id":id,
        "automatic":true,
        "degradation_recheck":true,
        "status":"completed",
        "completed_at":datetime(harness.now()),
        "detection_generation":stale_generation,
        "detection_mark_version":mark["version"],
        "recheck_account_version":sub2ops_server::compat::operation_versions(&account,false)["model_test"],
        "recheck_schedulable":true,
        "first_group_valid":true,
        "first_group_prediction":"claude-sonnet-4-6",
        "report":{"prediction":"claude-sonnet-4-6","used_outputs":1},
        "groups":[{"status":"completed"}],
        "requested_model":"gpt-6-luna"
    });
    let stale_result = harness
        .runtime()
        .detection
        .complete_recheck(&job)
        .await
        .unwrap_err();
    assert_eq!(stale_result.status.as_u16(), 409);
    let stale_control = harness.runtime().detection.control(id).unwrap();
    assert_eq!(stale_control["hold"], true);
    assert!(stale_control["recheck_recovery"].is_null());
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn late_recheck_is_rejected_after_mark_change_manual_release_or_schedule_close() {
    let harness = Harness::new(sample_text("above_99", 0)).await;

    let changed_mark_row = harness.seed_account("apikey", true, json!({})).await;
    let changed_mark_id = integer(&changed_mark_row["id"]).unwrap();
    harness.enable_detection(changed_mark_id, 1).await;
    let initial_mark = harness.runtime().detection.mark(changed_mark_id).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            changed_mark_id,
            true,
            initial_mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-mark-version-job"),
        )
        .unwrap();
    harness
        .runtime()
        .detection
        .update(changed_mark_id, json!({"hold":true}))
        .unwrap();
    let mark = harness.runtime().detection.mark(changed_mark_id).unwrap();
    let control = harness
        .runtime()
        .detection
        .control(changed_mark_id)
        .unwrap();
    let account = harness
        .runtime()
        .actions
        .account(changed_mark_id, None)
        .await
        .unwrap();
    let late = completed_recheck_job(
        changed_mark_id,
        &control["generation"],
        &mark["version"],
        &sub2ops_server::compat::operation_versions(&account, false)["model_test"],
        "fixture-mark-version-late-result",
        harness.now(),
    );
    harness.advance(1);
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            changed_mark_id,
            true,
            mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-manual-mark-change"),
        )
        .unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .complete_recheck(&late)
            .await
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    assert_eq!(
        harness.runtime().detection.mark(changed_mark_id).unwrap()["marked"],
        true
    );
    assert!(harness
        .runtime()
        .detection
        .control(changed_mark_id)
        .unwrap()["recheck_recovery"]
        .is_null());

    let released_row = harness.seed_account("apikey", true, json!({})).await;
    let released_id = integer(&released_row["id"]).unwrap();
    harness.enable_detection(released_id, 1).await;
    let initial_mark = harness.runtime().detection.mark(released_id).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            released_id,
            true,
            initial_mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-release-hold-job"),
        )
        .unwrap();
    harness
        .runtime()
        .detection
        .update(
            released_id,
            json!({
                "hold":true,
                "disposition":{"status":"completed","marked":true,"schedule_verified":true,"job_id":"fixture-prior-degradation"}
            }),
        )
        .unwrap();
    let mark = harness.runtime().detection.mark(released_id).unwrap();
    let control = harness.runtime().detection.control(released_id).unwrap();
    let account = harness
        .runtime()
        .actions
        .account(released_id, None)
        .await
        .unwrap();
    let late = completed_recheck_job(
        released_id,
        &control["generation"],
        &mark["version"],
        &sub2ops_server::compat::operation_versions(&account, false)["model_test"],
        "fixture-release-hold-late-result",
        harness.now(),
    );
    harness
        .runtime()
        .detection
        .human_control(released_id, true)
        .unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .complete_recheck(&late)
            .await
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    assert_eq!(
        harness.runtime().detection.control(released_id).unwrap()["hold"],
        false
    );
    assert_eq!(
        harness.runtime().detection.mark(released_id).unwrap()["marked"],
        true
    );
    assert!(
        harness.runtime().detection.control(released_id).unwrap()["recheck_recovery"].is_null()
    );

    let closed_row = harness.seed_account("apikey", true, json!({})).await;
    let closed_id = integer(&closed_row["id"]).unwrap();
    harness.enable_detection(closed_id, 1).await;
    let initial_mark = harness.runtime().detection.mark(closed_id).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            closed_id,
            true,
            initial_mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-closed-late-result"),
        )
        .unwrap();
    harness
        .runtime()
        .detection
        .update(
            closed_id,
            json!({
                "hold":true,
                "disposition":{"status":"completed","marked":true,"schedule_verified":true,"job_id":"fixture-prior-degradation"}
            }),
        )
        .unwrap();
    let mark = harness.runtime().detection.mark(closed_id).unwrap();
    let control = harness.runtime().detection.control(closed_id).unwrap();
    let account = harness
        .runtime()
        .actions
        .account(closed_id, None)
        .await
        .unwrap();
    let recheck_id = "f".repeat(32);
    let late = completed_recheck_job(
        closed_id,
        &control["generation"],
        &mark["version"],
        &sub2ops_server::compat::operation_versions(&account, false)["model_test"],
        &recheck_id,
        harness.now(),
    );
    harness
        .runtime()
        .tests
        .store
        .transaction(|state| {
            state["jobs"][&recheck_id] = late.clone();
            state["accounts"][closed_id.to_string()] = late.clone();
            Ok(())
        })
        .unwrap();
    harness
        .runtime()
        .detection
        .update(
            closed_id,
            json!({"job_id":recheck_id,"status":"completed","next_at":null}),
        )
        .unwrap();
    let current = harness
        .runtime()
        .actions
        .account(closed_id, None)
        .await
        .unwrap();
    let close = harness
        .runtime()
        .set_schedulable(
            closed_id,
            &json!({"schedulable":false,"expected_version":account_version(&current)}),
            ADMIN_KEY,
        )
        .await
        .unwrap();
    assert_eq!(close["verified"], true);
    harness.runtime().detection.tick().await.unwrap();
    let closed_control = harness.runtime().detection.control(closed_id).unwrap();
    assert_eq!(closed_control["reason"], "调度已关闭");
    assert!(closed_control["recheck_recovery"].is_null());
    assert_eq!(
        harness.runtime().detection.mark(closed_id).unwrap()["marked"],
        true
    );
    assert!(object_field_is_empty_or_missing(
        &harness.runtime().detection.store.read().unwrap(),
        "disposition_notifications"
    ));
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert_eq!(
        harness.mock.count(
            "POST",
            &format!("/api/v1/admin/accounts/{closed_id}/schedulable")
        ),
        1
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn same_recheck_job_id_with_new_completion_time_is_processed_after_retry() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 15).await;
    let before_mark = harness.runtime().detection.mark(id).unwrap();
    let job_id = "b".repeat(32);
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            before_mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-prior-degradation"),
        )
        .unwrap();
    let mark = harness.runtime().detection.mark(id).unwrap();
    let control = harness.runtime().detection.control(id).unwrap();
    let account = harness.runtime().actions.account(id, None).await.unwrap();
    let account_version =
        sub2ops_server::compat::operation_versions(&account, false)["model_test"].clone();
    let first_completed_at = harness.now();
    let mut failed_first_group = completed_recheck_job(
        id,
        &control["generation"],
        &mark["version"],
        &account_version,
        &job_id,
        first_completed_at,
    );
    failed_first_group["first_group_valid"] = false.into();
    failed_first_group["report"]["used_outputs"] = 0.into();
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({
                "hold":true,
                "disposition":{"status":"completed","marked":true,"schedule_verified":true,"job_id":"fixture-prior-degradation"},
                "job_id":job_id,
                "status":"completed",
                "handled_job":job_id,
                "handled_completed_at":datetime(first_completed_at-Duration::seconds(1)),
                "next_at":null
            }),
        )
        .unwrap();
    harness
        .runtime()
        .tests
        .store
        .transaction(|state| {
            state["jobs"][&job_id] = failed_first_group.clone();
            state["accounts"][id.to_string()] = failed_first_group.clone();
            Ok(())
        })
        .unwrap();

    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["handled_completed_at"],
        datetime(first_completed_at)
    );
    assert!(harness.runtime().detection.control(id).unwrap()["recheck_recovery"].is_null());

    let retried_completed_at = first_completed_at + Duration::seconds(1);
    let retried = completed_recheck_job(
        id,
        &control["generation"],
        &mark["version"],
        &account_version,
        &job_id,
        retried_completed_at,
    );
    harness
        .runtime()
        .tests
        .store
        .transaction(|state| {
            state["jobs"][&job_id] = retried.clone();
            state["accounts"][id.to_string()] = retried.clone();
            Ok(())
        })
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();

    let after = harness.runtime().detection.control(id).unwrap();
    let receipt = after["recheck_recovery"].clone();
    let event = harness.runtime().detection.store.read().unwrap()["disposition_notifications"]
        [&job_id]
        .clone();
    assert_eq!(after["handled_job"], job_id);
    assert_eq!(
        after["handled_completed_at"],
        datetime(retried_completed_at)
    );
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        false
    );
    assert_eq!(receipt["status"], "completed");
    assert_eq!(receipt["job_id"], job_id);
    assert_eq!(receipt["mark_cleared"], true);
    assert_eq!(event["kind"], "recovered");
    assert_eq!(event["status"], "queued");
    assert_eq!(event["account_id"], id);
    assert_eq!(event["generation"], control["generation"]);
    assert_eq!(event["mark_version"], receipt["mark_version_after"]);
    assert_eq!(event["schedulable"], true);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(
        harness
            .mock
            .count("POST", "/api/v1/admin/accounts/1/schedulable"),
        0
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn dispatch_history_migration_survives_pruned_job_summaries_and_restart() {
    let cooldown = fixture()["oracle"]["automatic_dispatch_cooldown_seconds"]
        .as_i64()
        .unwrap();
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    let started_at = harness.now() - Duration::seconds(10);
    harness
        .runtime()
        .tests
        .store
        .transaction(|state| {
            state["jobs"] = json!({
                "fixture-auto-latest":{"id":"fixture-auto-latest","account_id":id,"automatic":true,"status":"failed","started_at":datetime(started_at)},
                "fixture-manual-newer":{"id":"fixture-manual-newer","account_id":id,"automatic":false,"status":"completed","started_at":datetime(harness.now())},
                "fixture-auto-no-start":{"id":"fixture-auto-no-start","account_id":id,"automatic":true,"status":"queued","started_at":null}
            });
            Ok(())
        })
        .unwrap();
    harness.runtime().detection.migrate_dispatches().unwrap();
    let dispatch = harness.runtime().detection.store.read().unwrap()["automatic_starts"]
        [id.to_string()]
    .clone();
    assert_eq!(dispatch["job_id"], "fixture-auto-latest");
    assert_eq!(dispatch["started_at"], datetime(started_at));
    assert_eq!(
        harness.runtime().detection.next_allowed_at(id).unwrap(),
        datetime(harness.now() + Duration::seconds(cooldown - 10))
    );
    harness
        .runtime()
        .tests
        .store
        .transaction(|state| {
            state["jobs"] = json!({});
            state["accounts"] = json!({});
            state["requests"] = json!({});
            Ok(())
        })
        .unwrap();
    harness.restart_runtime().await;
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()],
        dispatch
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(
        harness
            .mock
            .count("POST", "/api/v1/admin/accounts/1/schedulable"),
        0
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn invalid_or_unwritable_detection_state_fails_closed_without_external_dispatch() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    let path = harness.runtime().detection.store.path().to_path_buf();
    fs::write(&path, b"{fixture-corrupt-state}")
        .expect("corrupt only this fixture-owned state file");
    assert!(harness.runtime().detection.control(id).is_err());
    assert!(harness.runtime().detection.held(id));
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);

    fs::remove_file(&path).unwrap();
    let lock_path = path.with_extension("lock");
    if lock_path.is_file() {
        fs::remove_file(&lock_path).unwrap();
    }
    fs::write(&path, br#"{"version":2,"accounts":{},"consumed":{}}"#).unwrap();
    assert!(harness.runtime().detection.control(id).is_err());
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    fs::write(
        &path,
        br#"{"version":1,"accounts":{"9800000":{"enabled":"yes","hold":false,"generation":0,"interval_minutes":15,"model_id":"gpt-6-luna"}},"consumed":{}}"#,
    )
    .unwrap();
    assert!(harness.runtime().detection.control(id).is_err());
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&lock_path).expect("create fixture-owned blocking lock path");
    let result = harness
        .runtime()
        .detection
        .update(id, json!({"status":"paused"}));
    assert!(result.is_err());
    assert!(harness.runtime().detection.held(id));
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    fs::remove_dir(&lock_path).unwrap();
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn missing_result_report_variants_never_clear_marks_or_create_outbox_events() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let cases = [
        "target-first-group",
        "target-aggregate",
        "empty-report",
        "missing-prediction",
        "invalid-first-group",
        "retry-eligible",
        "failed-group",
        "manual-result",
    ];
    let mut accounts = Vec::new();
    for case in cases {
        let row = harness.seed_account("apikey", true, json!({})).await;
        let id = integer(&row["id"]).unwrap();
        harness.enable_detection(id, 1).await;
        let initial = harness.runtime().detection.mark(id).unwrap();
        harness
            .runtime()
            .detection
            .capacity
            .store
            .set_mark(
                id,
                true,
                initial["version"].as_str().unwrap(),
                harness.now(),
                Some("fixture-invalid-recheck"),
            )
            .unwrap();
        harness
            .runtime()
            .detection
            .update(id, json!({"hold":true}))
            .unwrap();
        let mark = harness.runtime().detection.mark(id).unwrap();
        let control = harness.runtime().detection.control(id).unwrap();
        let account = harness.runtime().actions.account(id, None).await.unwrap();
        let mut job = completed_recheck_job(
            id,
            &control["generation"],
            &mark["version"],
            &sub2ops_server::compat::operation_versions(&account, false)["model_test"],
            &format!("fixture-invalid-{}", uuid::Uuid::new_v4().simple()),
            harness.now(),
        );
        match case {
            "target-first-group" => job["first_group_prediction"] = model_detection::TARGET.into(),
            "target-aggregate" => job["report"]["prediction"] = model_detection::TARGET.into(),
            "empty-report" => job["report"]["used_outputs"] = 0.into(),
            "missing-prediction" => job["report"]["prediction"] = Value::Null,
            "invalid-first-group" => job["first_group_valid"] = false.into(),
            "retry-eligible" => job["can_retry"] = true.into(),
            "failed-group" => {
                job["groups"] = json!([{"index":1,"status":"failed","error":"fixture"}])
            }
            "manual-result" => job["automatic"] = false.into(),
            _ => unreachable!(),
        }
        accounts.push((id, case, job));
    }

    for (id, case, job) in accounts {
        assert!(
            harness
                .runtime()
                .detection
                .complete_recheck(&job)
                .await
                .unwrap()
                .is_null(),
            "case {case}"
        );
        assert_eq!(
            harness.runtime().detection.mark(id).unwrap()["marked"],
            true,
            "case {case}"
        );
        assert_eq!(
            harness.runtime().detection.control(id).unwrap()["hold"],
            true,
            "case {case}"
        );
        assert!(harness.runtime().detection.control(id).unwrap()["recheck_recovery"].is_null());
    }
    assert!(object_field_is_empty_or_missing(
        &harness.runtime().detection.store.read().unwrap(),
        "disposition_notifications"
    ));
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn non_degraded_and_empty_automatic_reports_create_no_disposition_notification() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let cases = [
        (
            "non-degraded",
            json!({"prediction":"gpt-6-luna","probability":0.91,"used_outputs":1}),
        ),
        (
            "empty-report",
            json!({"prediction":"gpt-5.6-luna","probability":null,"used_outputs":0}),
        ),
    ];
    let mut account_ids = Vec::new();
    for (case, report) in cases {
        let row = harness.seed_account("apikey", true, json!({})).await;
        let id = integer(&row["id"]).unwrap();
        harness.enable_detection(id, 1).await;
        let control = harness.runtime().detection.control(id).unwrap();
        let mark = harness.runtime().detection.mark(id).unwrap();
        let job = json!({
            "id":format!("fixture-verdict-{case}"),
            "account_id":id,
            "automatic":true,
            "degradation_recheck":false,
            "detection_generation":control["generation"],
            "detection_mark_version":mark["version"],
            "requested_model":"gpt-6-luna",
            "triggers":["scheduled"]
        });
        assert!(
            harness
                .runtime()
                .detection
                .verdict(&job, &report)
                .await
                .unwrap()
                .is_null(),
            "case {case}"
        );
        assert!(harness.runtime().detection.control(id).unwrap()["disposition"].is_null());
        assert_eq!(
            harness.runtime().detection.mark(id).unwrap()["marked"],
            false
        );
        account_ids.push(id);
    }
    assert!(object_field_is_empty_or_missing(
        &harness.runtime().detection.store.read().unwrap(),
        "disposition_notifications"
    ));
    assert_eq!(
        harness
            .mock
            .count("POST", "/api/v1/admin/accounts/1/schedulable"),
        0
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    for id in account_ids {
        assert_eq!(
            harness
                .mock
                .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
            0
        );
    }

    let automatic_row = harness.seed_account("apikey", true, json!({})).await;
    let automatic_id = integer(&automatic_row["id"]).unwrap();
    let normal_output = sample_text("above_99", 1);
    harness
        .mock
        .set_model_outputs([normal_output.clone(), normal_output.clone(), normal_output]);
    harness.enable_detection(automatic_id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            automatic_id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let automatic_job_id = harness.runtime().detection.control(automatic_id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let automatic_job = harness.wait_job_terminal(&automatic_job_id).await;
    assert_eq!(automatic_job["status"], "completed");
    assert_eq!(automatic_job["automatic"], true);
    assert_eq!(automatic_job["first_group_prediction"], "gpt-5.4");
    assert_eq!(automatic_job["first_group_valid"], true);
    assert_eq!(automatic_job["report"]["prediction"], "gpt-5.4");
    assert_eq!(automatic_job["report"]["used_outputs"], 3);
    assert!(automatic_job["report"]["probability"].as_f64().unwrap() < 0.99);
    assert_eq!(automatic_job["automatic_disposition"], Value::Null);
    assert_eq!(
        automatic_job["groups"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|group| group["status"] == "completed")
            .count(),
        3
    );
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(
        harness.runtime().detection.mark(automatic_id).unwrap()["marked"],
        false
    );
    let automatic_control = harness.runtime().detection.view(automatic_id).unwrap();
    assert_eq!(automatic_control["status"], "waiting");
    assert!(automatic_control["disposition"].is_null());
    assert_eq!(
        harness
            .runtime()
            .actions
            .account(automatic_id, None)
            .await
            .unwrap()["schedulable"],
        true
    );
    assert_eq!(
        harness.mock.count(
            "POST",
            &format!("/api/v1/admin/accounts/{automatic_id}/schedulable")
        ),
        0
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 3);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(object_field_is_empty_or_missing(
        &harness.runtime().detection.store.read().unwrap(),
        "disposition_notifications"
    ));
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn automatic_dispatch_registration_failure_never_reaches_upstream() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    let account = harness.runtime().actions.account(id, None).await.unwrap();
    let control = harness.runtime().detection.control(id).unwrap();
    let mark = harness.runtime().detection.mark(id).unwrap();
    let lease = harness
        .runtime()
        .actions
        .locks
        .try_account(&account)
        .expect("hold the fixture account until the model job is durably queued");
    let job = harness
        .runtime()
        .tests
        .start(
            id,
            &json!({
                "model_id":"gpt-6-luna",
                "expected_version":account_version(&account),
                "request_id":"fixture-dispatch-state-save-failure",
                "concurrency":1
            }),
            Some(&json!({
                "detection_generation":control["generation"],
                "detection_mark_version":mark["version"],
                "triggers":["scheduled"],
                "degradation_recheck":false
            })),
        )
        .await
        .unwrap();
    assert_eq!(job["status"], "queued");
    let lock = FileReadOnly::new(
        harness
            .runtime()
            .detection
            .store
            .path()
            .with_extension("lock"),
    );
    drop(lease);
    let terminal = harness.wait_job_terminal(job["id"].as_str().unwrap()).await;
    assert_eq!(terminal["status"], "failed");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"]
            .get(id.to_string())
            .is_none()
    );
    assert!(harness
        .runtime()
        .detection
        .next_allowed_at(id)
        .unwrap()
        .is_null());
    assert_eq!(
        harness
            .mock
            .count("POST", "/api/v1/admin/accounts/1/schedulable"),
        0
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    drop(lock);
    harness.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn control_store_failure_before_verdict_leaves_mark_schedule_and_outbox_untouched() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness
        .runtime()
        .detection
        .update(id, json!({"status":"waiting"}))
        .unwrap();
    let control = harness.runtime().detection.control(id).unwrap();
    let mark = harness.runtime().detection.mark(id).unwrap();
    let job = json!({
        "id":"fixture-control-save-failure",
        "account_id":id,
        "automatic":true,
        "degradation_recheck":false,
        "detection_generation":control["generation"],
        "detection_mark_version":mark["version"],
        "requested_model":"gpt-6-luna",
        "triggers":["scheduled"]
    });
    let read_only = DirectoryReadOnly::new(harness.settings.data_dir());
    assert!(harness
        .runtime()
        .detection
        .verdict(
            &job,
            &json!({"prediction":model_detection::TARGET,"probability":0.97,"used_outputs":1}),
        )
        .await
        .is_err());
    drop(read_only);
    assert!(harness.runtime().detection.control(id).unwrap()["disposition"].is_null());
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        false
    );
    assert_eq!(
        harness.runtime().actions.account(id, None).await.unwrap()["schedulable"],
        true
    );
    assert!(object_field_is_empty_or_missing(
        &harness.runtime().detection.store.read().unwrap(),
        "disposition_notifications"
    ));
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn manual_generation_change_rejects_a_stale_verdict_without_any_disposition() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    let before = harness.runtime().detection.control(id).unwrap();
    let mark = harness.runtime().detection.mark(id).unwrap();
    let stale_job = json!({
        "id":"fixture-stale-verdict-job",
        "account_id":id,
        "automatic":true,
        "degradation_recheck":false,
        "detection_generation":before["generation"],
        "detection_mark_version":mark["version"],
        "requested_model":"gpt-6-luna",
        "triggers":["scheduled"]
    });
    harness.runtime().detection.human_control(id, true).unwrap();
    let error = harness
        .runtime()
        .detection
        .verdict(
            &stale_job,
            &json!({"prediction":model_detection::TARGET,"probability":0.97,"used_outputs":1}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 409);
    assert!(error.detail.to_string().contains("人工操作"));
    assert!(harness.runtime().detection.control(id).unwrap()["disposition"].is_null());
    assert_eq!(
        harness
            .runtime()
            .detection
            .capacity
            .store
            .snapshot()
            .unwrap()["marks"],
        json!({})
    );
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn missing_admin_authorization_preserves_the_mark_without_schedule_or_bark() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    let gate = Gate::new();
    harness.mock.set_model_gate(Some(gate.clone()));
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    tokio::time::timeout(StdDuration::from_secs(10), gate.entered.notified())
        .await
        .expect("automatic ModelTests request must dispatch before admin authorization is removed");
    let model_calls = harness.mock.calls("POST", "/v1/responses");
    assert_eq!(model_calls.len(), 1);
    assert!(model_calls[0].bearer_authorized);
    assert!(!model_calls[0].admin_header_present);
    let catalog_path = format!("/fixture-models/{id}");
    let catalog_calls = harness.mock.calls("GET", &catalog_path);
    assert!(!catalog_calls.is_empty());
    assert!(catalog_calls
        .iter()
        .all(|call| !call.admin_header_present && call.bearer_authorized));
    harness
        .runtime()
        .monitor
        .store
        .transaction(|state| {
            state["settings"]
                .as_object_mut()
                .unwrap()
                .remove("sub2api_admin_token");
            Ok(())
        })
        .unwrap();
    gate.release.notify_one();
    harness.mock.set_model_gate(None);
    let job = harness.wait_job_terminal(&job_id).await;
    let current = harness.runtime().detection.view(id).unwrap();
    assert_eq!(job["first_group_prediction"], "gpt-5.6-luna");
    assert_eq!(current["disposition"]["status"], "needs_confirmation");
    assert_eq!(current["disposition"]["marked"], true);
    assert_eq!(current["disposition"]["schedule_verified"], false);
    assert_eq!(current["hold"], true);
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(current["disposition_notification"], Value::Null);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn capacity_mark_persistence_failure_does_not_write_schedule_or_queue_bark() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    let mark_path = harness
        .runtime()
        .detection
        .capacity
        .store
        .0
        .path()
        .to_path_buf();
    let _ = harness.runtime().detection.mark(id).unwrap();
    let lock_path = mark_path.with_extension("lock");
    if lock_path.is_file() {
        fs::remove_file(&lock_path).unwrap();
    }
    fs::create_dir(&lock_path).expect("block writes to this fixture-owned capacity state");
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    let account = harness.runtime().actions.account(id, None).await.unwrap();
    assert_eq!(job["first_group_prediction"], "gpt-5.6-luna");
    assert_eq!(account["schedulable"], true);
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        false
    );
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );
    assert!(object_field_is_empty_or_missing(
        &harness.runtime().detection.store.read().unwrap(),
        "disposition_notifications"
    ));
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    fs::remove_dir(&lock_path).unwrap();
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn verified_schedule_response_is_required_before_a_degraded_result_enters_the_outbox() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.mock.set_invalid_schedulable_response(true);
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["first_group_prediction"], "gpt-5.6-luna");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    let current = harness.runtime().detection.view(id).unwrap();
    assert_eq!(current["disposition"]["marked"], true);
    assert_eq!(current["disposition"]["schedule_verified"], false);
    assert_eq!(current["disposition"]["status"], "checking");
    assert_eq!(current["disposition_notification"], Value::Null);
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    persist_synthetic_schedulable(&harness.mock, id, false).await;
    assert_eq!(
        harness.runtime().actions.account(id, None).await.unwrap()["schedulable"],
        false
    );
    let confirmed = harness
        .runtime()
        .detection
        .finish_disposition(id)
        .await
        .unwrap();
    assert_eq!(confirmed["status"], "completed");
    assert_eq!(confirmed["schedule_verified"], true);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert_eq!(
        harness.runtime().detection.view(id).unwrap()["disposition_notification"]["status"],
        "queued"
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    assert_eq!(
        harness.runtime().detection.view(id).unwrap()["disposition_notification"]["status"],
        "delivered"
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn disposition_outbox_and_bark_receipts_recover_from_local_write_failures() {
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    let before = harness.runtime().detection.control(id).unwrap();
    let mark = harness.runtime().detection.mark(id).unwrap();
    let marked = harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-atomic-disposition"),
        )
        .unwrap();
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({
                "hold":true,
                "disposition":{
                    "status":"marked",
                    "job_id":"fixture-atomic-disposition",
                    "generation":before["generation"],
                    "mark_version":marked["version"],
                    "marked":true,
                    "schedule_verified":false,
                    "created_at":datetime(harness.now()),
                    "model_id":"gpt-6-luna",
                    "prediction":"gpt-5.6-luna",
                    "probability":0.97,
                    "triggers":["scheduled"]
                }
            }),
        )
        .unwrap();

    let schedule_gate = Gate::new();
    harness
        .mock
        .set_schedulable_gate(Some(schedule_gate.clone()));
    let detection = harness.runtime().detection.clone();
    let finish = tokio::spawn(async move { detection.finish_disposition(id).await });
    tokio::time::timeout(StdDuration::from_secs(10), schedule_gate.entered.notified())
        .await
        .expect("disposition must persist its writing state before the loopback schedule request");
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["disposition"]["status"],
        "writing"
    );
    let state_dir = harness.settings.data_dir();
    let read_only = DirectoryReadOnly::new(state_dir.clone());
    schedule_gate.release.notify_one();
    harness.mock.set_schedulable_gate(None);
    let result = tokio::time::timeout(StdDuration::from_secs(10), finish)
        .await
        .expect("loopback write and final commit must reach a terminal result")
        .unwrap();
    assert!(result.is_err());
    drop(read_only);

    let account = harness.runtime().actions.account(id, None).await.unwrap();
    assert_eq!(account["schedulable"], false);
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["disposition"]["status"],
        "writing"
    );
    assert!(object_field_is_empty_or_missing(
        &harness.runtime().detection.store.read().unwrap(),
        "disposition_notifications"
    ));
    assert_eq!(harness.mock.count("POST", "/push"), 0);

    harness.restart_runtime().await;
    let resumed = harness
        .runtime()
        .detection
        .finish_disposition(id)
        .await
        .unwrap();
    assert_eq!(resumed["status"], "completed");
    assert_eq!(resumed["schedule_verified"], true);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert_eq!(
        harness.runtime().detection.view(id).unwrap()["disposition_notification"]["status"],
        "queued"
    );

    let read_only = DirectoryReadOnly::new(state_dir.clone());
    assert!(harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .is_err());
    drop(read_only);
    let reservation =
        harness.runtime().detection.view(id).unwrap()["disposition_notification"].clone();
    assert_eq!(reservation["status"], "queued");
    assert_eq!(reservation["attempts"], 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);

    let bark_gate = Gate::new();
    harness.mock.set_bark_gate(Some(bark_gate.clone()));
    let detection = harness.runtime().detection.clone();
    let delivery = tokio::spawn(async move { detection.deliver_notifications().await });
    tokio::time::timeout(StdDuration::from_secs(10), bark_gate.entered.notified())
        .await
        .expect("durably reserved Bark attempt must reach the loopback endpoint");
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    let read_only = DirectoryReadOnly::new(state_dir);
    bark_gate.release.notify_one();
    harness.mock.set_bark_gate(None);
    let delivery_result = tokio::time::timeout(StdDuration::from_secs(10), delivery)
        .await
        .expect("Bark response and attempted receipt save must settle")
        .unwrap();
    assert!(delivery_result.is_err());
    drop(read_only);
    let receipt_pending =
        harness.runtime().detection.view(id).unwrap()["disposition_notification"].clone();
    assert_eq!(receipt_pending["status"], "queued");
    assert_eq!(receipt_pending["attempts"], 1);
    assert_eq!(
        receipt_pending["next_at"],
        datetime(harness.now() + Duration::seconds(5))
    );
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    let receipt_retry_at = receipt_pending["next_at"].clone();
    harness.restart_runtime().await;
    let persisted_receipt =
        harness.runtime().detection.view(id).unwrap()["disposition_notification"].clone();
    assert_eq!(persisted_receipt["status"], "queued");
    assert_eq!(persisted_receipt["attempts"], 1);
    assert_eq!(persisted_receipt["next_at"], receipt_retry_at);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    harness.advance(4);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    harness.advance(1);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 2);
    assert_eq!(
        harness.runtime().detection.view(id).unwrap()["disposition_notification"]["status"],
        "delivered"
    );
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn manual_generation_change_rejects_a_stale_detection_guard() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"hold":true,"disposition":{"status":"pending","job_id":"fixture-prior-job"}}),
        )
        .unwrap();
    let before = harness.runtime().detection.control(id).unwrap();
    let stale_job = json!({
        "account_id":id,
        "detection_generation":before["generation"],
        "detection_mark_version":harness.runtime().detection.mark(id).unwrap()["version"]
    });
    harness.runtime().detection.human_control(id, true).unwrap();
    let after = harness.runtime().detection.control(id).unwrap();
    assert_eq!(
        after["generation"],
        before["generation"].as_i64().unwrap() + 1
    );
    assert_eq!(after["hold"], false);
    assert_eq!(after["status"], "paused");
    assert_eq!(after["disposition"]["status"], "overridden");
    let stale_guard = harness
        .runtime()
        .detection
        .guard(&stale_job)
        .await
        .unwrap_err();
    assert_eq!(stale_guard.status.as_u16(), 409);
    assert!(stale_guard.detail.to_string().contains("新的人工操作"));
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(
        harness
            .mock
            .count("POST", "/api/v1/admin/accounts/1/schedulable"),
        0
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn manual_override_suppresses_a_queued_verified_disposition_notification() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    let before = harness.runtime().detection.view(id).unwrap();
    assert_eq!(job["automatic_disposition"]["status"], "completed");
    assert_eq!(before["disposition_notification"]["status"], "queued");
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    harness.runtime().detection.human_control(id, true).unwrap();
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    let current = harness.runtime().detection.view(id).unwrap();
    assert_eq!(
        current["generation"],
        before["generation"].as_i64().unwrap() + 1
    );
    assert_eq!(current["disposition"]["status"], "overridden");
    assert_eq!(current["disposition_notification"]["status"], "suppressed");
    assert_eq!(
        current["disposition_notification"]["reason"],
        "manual_intervention"
    );
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn manual_generation_change_during_schedule_write_blocks_receipt_and_bark() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    let gate = Gate::new();
    harness.mock.set_schedulable_gate(Some(gate.clone()));
    harness.runtime().detection.tick().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(12), gate.entered.notified())
        .await
        .expect("automatic target must reach its gated loopback schedule write");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["disposition"]["status"],
        "writing"
    );
    let generation = harness.runtime().detection.control(id).unwrap()["generation"]
        .as_i64()
        .unwrap();
    harness.runtime().detection.human_control(id, true).unwrap();
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["generation"],
        generation + 1
    );
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["disposition"]["status"],
        "overridden"
    );
    gate.release.notify_one();
    harness.mock.set_schedulable_gate(None);

    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    let current = harness.runtime().detection.view(id).unwrap();
    assert_eq!(job["automatic_disposition"]["status"], "overridden");
    assert_eq!(current["status"], "paused");
    assert_eq!(current["hold"], false);
    assert_eq!(current["disposition"]["status"], "overridden");
    assert_eq!(current["disposition_notification"], Value::Null);
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn completed_worker_anchors_next_interval_and_closed_schedule_cancels_active_request() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    let model_gate = Gate::new();
    harness.mock.set_model_gate(Some(model_gate.clone()));
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    tokio::time::timeout(StdDuration::from_secs(10), model_gate.entered.notified())
        .await
        .expect("running model request must reach the loopback endpoint");
    let initial_count = harness.mock.count("POST", "/v1/responses");
    assert_eq!(initial_count, 1);
    let account = harness.runtime().actions.account(id, None).await.unwrap();
    let close = harness
        .runtime()
        .set_schedulable(
            id,
            &json!({
                "schedulable":false,
                "expected_version":account_version(&account)
            }),
            ADMIN_KEY,
        )
        .await
        .unwrap();
    assert_eq!(close["verified"], true);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        1
    );
    assert_eq!(
        harness
            .mock
            .calls("POST", &format!("/api/v1/admin/accounts/{id}/schedulable"))[0]
            .payload,
        json!({"schedulable":false})
    );
    harness.runtime().detection.tick().await.unwrap();
    model_gate.release.notify_one();
    harness.mock.set_model_gate(None);
    let terminal = harness.wait_job_terminal(&job_id).await;
    assert_eq!(terminal["status"], "cancelled");
    harness.runtime().detection.tick().await.unwrap();
    let paused = harness.runtime().detection.control(id).unwrap();
    assert_eq!(paused["status"], "paused");
    assert!(paused["next_at"].is_null());
    assert_eq!(harness.mock.count("POST", "/v1/responses"), initial_count);
    assert!(harness.mock.unexpected().is_empty());

    let queued_row = harness.seed_account("apikey", true, json!({})).await;
    let queued_id = integer(&queued_row["id"]).unwrap();
    harness.enable_detection(queued_id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            queued_id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    let queued_lease = harness
        .runtime()
        .actions
        .locks
        .try_account(&queued_row)
        .expect("fixture lease keeps the automatic job queued before launch");
    harness.runtime().detection.tick().await.unwrap();
    let queued_job_id = harness.runtime().detection.control(queued_id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let queued = tokio::time::timeout(StdDuration::from_secs(10), async {
        loop {
            let job = harness.runtime().tests.get(&queued_job_id).unwrap();
            if job["status"] == "queued" {
                break job;
            }
            tokio::time::sleep(StdDuration::from_millis(5)).await;
        }
    })
    .await
    .expect("automatic job must stay queued while its fixture account lease is held");
    assert_eq!(queued["automatic"], true);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), initial_count);
    assert!(harness
        .runtime()
        .detection
        .next_allowed_at(queued_id)
        .unwrap()
        .is_null());
    let queued_account = harness
        .runtime()
        .actions
        .account(queued_id, None)
        .await
        .unwrap();
    let queued_close = harness
        .runtime()
        .set_schedulable(
            queued_id,
            &json!({
                "schedulable":false,
                "expected_version":account_version(&queued_account)
            }),
            ADMIN_KEY,
        )
        .await
        .unwrap();
    assert_eq!(queued_close["verified"], true);
    harness.runtime().detection.tick().await.unwrap();
    let queued_cancelled = harness.wait_job_terminal(&queued_job_id).await;
    assert_eq!(queued_cancelled["status"], "cancelled");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), initial_count);
    assert!(harness
        .runtime()
        .detection
        .next_allowed_at(queued_id)
        .unwrap()
        .is_null());
    drop(queued_lease);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn automatic_detection_state_persists_across_runtime_restart() {
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    let default = harness.runtime().detection.view(id).unwrap();
    assert_eq!(default["enabled"], false);
    assert_eq!(
        default["interval_minutes"],
        fixture()["oracle"]["default_interval_minutes"]
    );
    assert_eq!(default["status"], "disabled");
    assert_eq!(
        default["model_id"],
        fixture()["oracle"]["default_detection_model"]
    );
    assert!(default["next_at"].is_null());

    let saved = harness.enable_detection(id, 20).await;
    assert_eq!(saved["status"], "waiting");
    assert_eq!(
        saved["generation"],
        default["generation"].as_i64().unwrap() + 1
    );
    assert_eq!(
        saved["next_at"],
        datetime(harness.now() + Duration::minutes(20))
    );
    harness.restart_runtime().await;
    let resumed = harness.runtime().detection.view(id).unwrap();
    assert_eq!(resumed["enabled"], true);
    assert_eq!(resumed["interval_minutes"], 20);
    assert_eq!(resumed["generation"], saved["generation"]);
    assert_eq!(resumed["next_at"], saved["next_at"]);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn completed_worker_anchors_interval_from_current_clock_without_catch_up() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 15).await;
    harness.advance(60 * 60);
    harness.runtime().detection.tick().await.unwrap();
    let first_job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_job = harness.wait_job_terminal(&first_job_id).await;
    assert_eq!(first_job["status"], "completed");
    let calls_after_first = harness.mock.calls("POST", "/v1/responses").len();
    assert_eq!(
        calls_after_first,
        first_job["groups"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|group| group["status"] == "completed")
            .count()
    );

    harness.advance(3 * 60 * 60);
    harness.runtime().detection.tick().await.unwrap();
    let state = harness.runtime().detection.control(id).unwrap();
    assert_eq!(state["job_id"], first_job_id);
    assert_eq!(state["status"], "waiting");
    assert_eq!(
        state["next_at"],
        datetime(harness.now() + Duration::minutes(15))
    );
    assert_eq!(
        harness.mock.count("POST", "/v1/responses"),
        calls_after_first
    );
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()]
            ["started_at"],
        datetime(reference_time() + Duration::hours(1))
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn disabling_generation_drops_old_marked_clue_without_model_recheck() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    let enabled = harness.enable_detection(id, 1).await;
    let mark = harness.runtime().detection.mark(id).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-prior-degradation"),
        )
        .unwrap();
    let current = harness.runtime().detection.control(id).unwrap();
    let disabled = harness
        .runtime()
        .detection
        .save(
            id,
            &json!({
                "enabled":false,
                "interval_minutes":1,
                "model_id":"gpt-6-luna",
                "expected_version":model_detection::version(&current)
            }),
        )
        .await
        .unwrap();
    assert_eq!(disabled["enabled"], false);
    assert_eq!(
        disabled["generation"],
        enabled["generation"].as_i64().unwrap() + 1
    );
    assert_eq!(disabled["status"], "disabled");
    assert!(disabled["next_at"].is_null());

    let clue = "error:fixture-disabled-old";
    harness
        .runtime()
        .detection
        .capacity
        .store
        .transaction(|state| {
            state["detection_events"][clue] =
                json!({"account_id":id,"created_at":datetime(harness.now())});
            Ok(())
        })
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .capacity
            .store
            .snapshot()
            .unwrap()["detection_events"],
        json!({})
    );
    assert!(
        harness.runtime().detection.store.read().unwrap()["consumed"]
            .get(clue)
            .is_some()
    );
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"]
            .get(id.to_string())
            .is_none()
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn closed_oauth_and_key_accounts_do_not_dispatch_marked_or_unmarked() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let mut cases = Vec::new();
    for kind in ["oauth", "apikey"] {
        for marked in [false, true] {
            let row = harness.seed_account(kind, false, json!({})).await;
            let id = integer(&row["id"]).unwrap();
            harness.enable_detection(id, 1).await;
            if marked {
                let mark = harness.runtime().detection.mark(id).unwrap();
                harness
                    .runtime()
                    .detection
                    .capacity
                    .store
                    .set_mark(
                        id,
                        true,
                        mark["version"].as_str().unwrap(),
                        harness.now(),
                        Some("fixture-closed-account-mark"),
                    )
                    .unwrap();
                harness
                    .runtime()
                    .detection
                    .update(
                        id,
                        json!({
                            "hold":true,
                            "disposition":{"status":"completed","marked":true,"schedule_verified":true,"job_id":"fixture-closed-account-mark"}
                        }),
                    )
                    .unwrap();
            }
            harness
                .runtime()
                .detection
                .update(
                    id,
                    json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
                )
                .unwrap();
            cases.push((kind, id, marked));
        }
    }

    harness.runtime().detection.tick().await.unwrap();
    for (kind, id, marked) in cases {
        let state = harness.runtime().detection.control(id).unwrap();
        assert_eq!(state["enabled"], true, "{kind} marked={marked}");
        assert_eq!(state["status"], "paused", "{kind} marked={marked}");
        assert!(state["next_at"].is_null(), "{kind} marked={marked}");
        assert_eq!(state["reason"], "调度已关闭", "{kind} marked={marked}");
        assert_eq!(
            harness.runtime().detection.mark(id).unwrap()["marked"],
            marked
        );
        assert!(
            harness.runtime().detection.store.read().unwrap()["automatic_starts"]
                .get(id.to_string())
                .is_none()
        );
    }
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn reopening_after_long_unschedulable_pause_preserves_settings_and_full_interval() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", false, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 15).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let paused = harness.runtime().detection.control(id).unwrap();
    assert_eq!(paused["enabled"], true);
    assert_eq!(paused["interval_minutes"], 15);
    assert_eq!(paused["model_id"], "gpt-6-luna");
    assert_eq!(paused["status"], "paused");
    assert!(paused["next_at"].is_null());
    assert_eq!(paused["reason"], "调度已关闭");

    harness.advance(60 * 60);
    let current = harness.runtime().actions.account(id, None).await.unwrap();
    let reopened = harness
        .runtime()
        .set_schedulable(
            id,
            &json!({"schedulable":true,"expected_version":account_version(&current)}),
            ADMIN_KEY,
        )
        .await
        .unwrap();
    assert_eq!(reopened["verified"], true);
    harness.runtime().detection.tick().await.unwrap();
    let waiting = harness.runtime().detection.control(id).unwrap();
    assert_eq!(waiting["enabled"], true);
    assert_eq!(waiting["interval_minutes"], 15);
    assert_eq!(waiting["model_id"], "gpt-6-luna");
    assert_eq!(waiting["status"], "waiting");
    assert_eq!(
        waiting["next_at"],
        datetime(harness.now() + Duration::minutes(15))
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);

    harness.advance(14 * 60 + 59);
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    harness.advance(1);
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["status"], "completed");
    let completed_groups = job["groups"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|group| group["status"] == "completed")
        .count();
    assert_eq!(
        harness.mock.count("POST", "/v1/responses"),
        completed_groups
    );
    let schedule_writes = harness
        .mock
        .calls("POST", &format!("/api/v1/admin/accounts/{id}/schedulable"));
    assert_eq!(schedule_writes.len(), 1);
    assert_eq!(schedule_writes[0].payload, json!({"schedulable":true}));
    assert!(schedule_writes[0].admin_authorized);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn account_pause_and_resume_after_long_gap_starts_a_fresh_full_interval() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 15).await;

    let mark = harness.runtime().detection.mark(id).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-account-pause"),
        )
        .unwrap();
    sqlx::query("UPDATE accounts SET status='disabled',updated_at=$1 WHERE id=$2")
        .bind(harness.now())
        .bind(id)
        .execute(&harness.runtime().db.pool)
        .await
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let paused = harness.runtime().detection.control(id).unwrap();
    assert_eq!(paused["status"], "paused");
    assert!(paused["next_at"].is_null());
    assert_eq!(paused["enabled"], true);
    assert_eq!(paused["interval_minutes"], 15);
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );

    harness.advance(4 * 60 * 60);
    let mark = harness.runtime().detection.mark(id).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            false,
            mark["version"].as_str().unwrap(),
            harness.now(),
            Some("fixture-account-resume"),
        )
        .unwrap();
    sqlx::query("UPDATE accounts SET status='active',updated_at=$1 WHERE id=$2")
        .bind(harness.now())
        .bind(id)
        .execute(&harness.runtime().db.pool)
        .await
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();

    let resumed = harness.runtime().detection.control(id).unwrap();
    assert_eq!(resumed["enabled"], true);
    assert_eq!(resumed["interval_minutes"], 15);
    assert_eq!(resumed["status"], "waiting");
    assert_eq!(
        resumed["next_at"],
        datetime(harness.now() + Duration::minutes(15))
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"]
            .get(id.to_string())
            .is_none()
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn manual_model_request_has_priority_over_detection_using_shared_parent_credentials() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    harness.mock.set_model_outputs([
        sample_text("automatic_first_group", 0),
        sample_text("above_99", 0),
        sample_text("above_99", 2),
        sample_text("automatic_first_group", 0),
    ]);
    let parent = harness.seed_account("apikey", true, json!({})).await;
    let parent_id = integer(&parent["id"]).unwrap();
    let manual_child = harness.seed_shared_child_account(parent_id).await;
    let automatic_child = harness.seed_shared_child_account(parent_id).await;
    let manual_id = integer(&manual_child["id"]).unwrap();
    let automatic_id = integer(&automatic_child["id"]).unwrap();
    harness.enable_detection(automatic_id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            automatic_id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();

    let gate = Gate::new();
    harness.mock.set_model_gate(Some(gate.clone()));
    let manual_live = harness
        .runtime()
        .actions
        .account(manual_id, None)
        .await
        .unwrap();
    let manual = harness
        .runtime()
        .tests
        .start(
            manual_id,
            &json!({
                "model_id":"gpt-6-luna",
                "expected_version":account_version(&manual_live),
                "request_id":uuid::Uuid::new_v4().simple().to_string(),
                "concurrency":1
            }),
            None,
        )
        .await
        .unwrap();
    tokio::time::timeout(StdDuration::from_secs(10), gate.entered.notified())
        .await
        .expect("manual ModelTests job must enter the loopback provider first");

    harness.runtime().detection.tick().await.unwrap();
    tokio::time::sleep(StdDuration::from_millis(150)).await;
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    let manual_calls = harness.mock.calls("POST", "/v1/responses");
    assert!(manual_calls[0].bearer_authorized);
    assert!(!manual_calls[0].admin_authorized);
    assert_eq!(manual_calls[0].payload["model"], "gpt-6-luna");
    assert_eq!(manual_calls[0].payload["stream"], true);
    assert_eq!(
        harness
            .runtime()
            .tests
            .get(manual["id"].as_str().unwrap())
            .unwrap()["trigger_source"],
        "manual"
    );
    let queued_automatic = harness.runtime().detection.control(automatic_id).unwrap();
    let queued_automatic_id = queued_automatic["job_id"].as_str().unwrap();
    let queued_automatic = harness.runtime().tests.get(queued_automatic_id).unwrap();
    assert_eq!(queued_automatic["status"], "queued");
    assert_eq!(queued_automatic["trigger_source"], "automatic");
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"]
            .get(automatic_id.to_string()),
        None
    );

    gate.release.notify_one();
    harness.mock.set_model_gate(None);
    let manual_job = harness
        .wait_job_terminal(manual["id"].as_str().unwrap())
        .await;
    assert_eq!(manual_job["status"], "completed");
    harness.runtime().detection.tick().await.unwrap();
    harness.mock.wait_for("POST", "/v1/responses", 2).await;
    let automatic_job_id = harness.runtime().detection.control(automatic_id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let automatic_job = harness.wait_job_terminal(&automatic_job_id).await;
    assert_eq!(automatic_job["automatic"], true);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 4);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"]
            .get(manual_id.to_string()),
        None
    );
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"]
            .get(automatic_id.to_string())
            .unwrap()["job_id"],
        automatic_job_id
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn manual_request_is_exempt_from_automatic_timestamp_and_same_job_registration_is_idempotent()
{
    let harness = Harness::new(sample_text("above_99", 0)).await;
    harness.mock.set_model_outputs([
        sample_text("above_99", 0),
        sample_text("below_99", 0),
        sample_text("below_99", 1),
        sample_text("below_99", 2),
    ]);
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let automatic_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let automatic = harness.wait_job_terminal(&automatic_id).await;
    assert_eq!(automatic["automatic"], true);
    let dispatch_before_manual = harness.runtime().detection.store.read().unwrap()
        ["automatic_starts"][id.to_string()]
    .clone();
    let next_allowed_before_manual = harness.runtime().detection.next_allowed_at(id).unwrap();
    let before_manual_requests = harness.mock.count("POST", "/v1/responses");

    harness.advance(1);
    let live = harness.runtime().actions.account(id, None).await.unwrap();
    let manual = harness
        .runtime()
        .tests
        .start(
            id,
            &json!({
                "model_id":"gpt-6-luna",
                "expected_version":account_version(&live),
                "request_id":"fixture-manual-cooldown-exemption",
                "concurrency":1
            }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(manual["trigger_source"], "manual");
    let manual = harness
        .wait_job_terminal(manual["id"].as_str().unwrap())
        .await;
    assert_eq!(manual["status"], "completed");
    let completed_groups = manual["groups"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|group| group["status"] == "completed")
        .count();
    assert_eq!(completed_groups, 3);
    assert_eq!(
        harness.mock.count("POST", "/v1/responses"),
        before_manual_requests + completed_groups
    );
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()],
        dispatch_before_manual
    );
    assert_eq!(
        harness.runtime().detection.next_allowed_at(id).unwrap(),
        next_allowed_before_manual
    );
    assert!(harness
        .runtime()
        .detection
        .trigger(id, &["scheduled".into()])
        .await
        .unwrap()
        .is_null());
    assert_eq!(
        harness.mock.count("POST", "/v1/responses"),
        before_manual_requests + completed_groups
    );

    let same_job = json!({"id":automatic_id,"account_id":id,"automatic":true});
    harness.advance(30);
    harness
        .runtime()
        .detection
        .register_dispatch(&same_job)
        .unwrap();
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()],
        dispatch_before_manual
    );
    let different_job = json!({"id":"fixture-different-auto-job","account_id":id,"automatic":true});
    assert_eq!(
        harness
            .runtime()
            .detection
            .register_dispatch(&different_job)
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    let manual_registration = json!({"id":"fixture-manual-registration","automatic":false});
    harness
        .runtime()
        .detection
        .register_dispatch(&manual_registration)
        .unwrap();
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()],
        dispatch_before_manual
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn shared_parent_children_have_independent_automatic_dispatch_records() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let parent = harness.seed_account("apikey", true, json!({})).await;
    let parent_id = integer(&parent["id"]).unwrap();
    let first = harness.seed_shared_child_account(parent_id).await;
    let second = harness.seed_shared_child_account(parent_id).await;
    let third = harness.seed_shared_child_account(parent_id).await;
    let first_id = integer(&first["id"]).unwrap();
    let second_id = integer(&second["id"]).unwrap();
    let third_id = integer(&third["id"]).unwrap();
    let child_ids = [first_id, second_id, third_id];
    for id in child_ids {
        harness.enable_detection(id, 1).await;
    }
    for id in child_ids {
        harness
            .runtime()
            .detection
            .update(
                id,
                json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
            )
            .unwrap();
    }

    harness.runtime().detection.tick().await.unwrap();
    let first_job_id = harness.runtime().detection.control(first_id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_job_id = harness.runtime().detection.control(second_id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let third_job_id = harness.runtime().detection.control(third_id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_job = harness.wait_job_terminal(&first_job_id).await;
    let second_job = harness.wait_job_terminal(&second_job_id).await;
    let third_job = harness.wait_job_terminal(&third_job_id).await;
    assert_eq!(first_job["automatic"], true);
    assert_eq!(second_job["automatic"], true);
    assert_eq!(third_job["automatic"], true);
    let expected_requests = [first_job, second_job, third_job]
        .iter()
        .map(|job| {
            job["groups"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|group| group["status"] == "completed")
                .count()
        })
        .sum::<usize>();
    assert_eq!(
        harness.mock.count("POST", "/v1/responses"),
        expected_requests
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 3);
    let dispatches = harness.runtime().detection.store.read().unwrap()["automatic_starts"].clone();
    assert_eq!(dispatches.as_object().unwrap().len(), 3);
    assert_eq!(dispatches[first_id.to_string()]["job_id"], first_job_id);
    assert_eq!(dispatches[second_id.to_string()]["job_id"], second_job_id);
    assert_eq!(dispatches[third_id.to_string()]["job_id"], third_job_id);
    assert!(dispatches.get(parent_id.to_string()).is_none());
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn queued_cancel_does_not_consume_cooldown_but_dispatched_cancel_survives_restart() {
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    let live = harness.runtime().actions.account(id, None).await.unwrap();
    let lease = harness
        .runtime()
        .actions
        .locks
        .try_account(&live)
        .expect("hold the fixture account so its automatic job remains queued");
    let queued = harness
        .runtime()
        .detection
        .trigger(id, &["error:fixture-queued-cancel".into()])
        .await
        .unwrap();
    assert_eq!(queued["status"], "queued");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert!(harness
        .runtime()
        .detection
        .next_allowed_at(id)
        .unwrap()
        .is_null());
    let cancelled = harness
        .runtime()
        .tests
        .cancel(queued["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert!(harness
        .runtime()
        .detection
        .next_allowed_at(id)
        .unwrap()
        .is_null());
    drop(lease);

    let after_queued_cancel = harness
        .runtime()
        .detection
        .trigger(id, &["error:fixture-after-queued-cancel".into()])
        .await
        .unwrap();
    let after_queued_cancel = harness
        .wait_job_terminal(after_queued_cancel["id"].as_str().unwrap())
        .await;
    assert_eq!(after_queued_cancel["status"], "completed");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    let dispatch_time = harness.runtime().detection.store.read().unwrap()["automatic_starts"]
        [id.to_string()]["started_at"]
        .clone();

    harness.advance(300);
    let gate = Gate::new();
    harness.mock.set_model_gate(Some(gate.clone()));
    let dispatched = harness
        .runtime()
        .detection
        .trigger(id, &["error:fixture-dispatched-cancel".into()])
        .await
        .unwrap();
    tokio::time::timeout(StdDuration::from_secs(10), gate.entered.notified())
        .await
        .expect("second automatic job must reach the synthetic provider before cancellation");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 2);
    assert_ne!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()]
            ["started_at"],
        dispatch_time
    );
    let cancelled = harness
        .runtime()
        .tests
        .cancel(dispatched["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    gate.release.notify_one();
    harness.mock.set_model_gate(None);
    harness.restart_runtime().await;
    let saved_dispatch = harness.runtime().detection.store.read().unwrap()["automatic_starts"]
        [id.to_string()]
    .clone();
    let saved_at = sub2ops_server::compat::parse_datetime(&saved_dispatch["started_at"]).unwrap();

    harness.advance(299);
    let cooling = harness
        .runtime()
        .detection
        .trigger(id, &["error:fixture-after-restart".into()])
        .await
        .unwrap();
    assert!(cooling.is_null());
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 2);
    assert_eq!(
        harness.runtime().detection.next_allowed_at(id).unwrap(),
        datetime(saved_at + Duration::seconds(300))
    );
    harness.advance(1);
    let after_cooldown = harness
        .runtime()
        .detection
        .trigger(id, &["error:fixture-exact-restart-boundary".into()])
        .await
        .unwrap();
    assert!(!after_cooldown.is_null());
    let after_cooldown = harness
        .wait_job_terminal(after_cooldown["id"].as_str().unwrap())
        .await;
    assert_eq!(after_cooldown["status"], "completed");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 3);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn manual_model_test_never_runs_automatic_disposition_for_a_target_first_group() {
    let harness = Harness::new(sample_text("automatic_first_group", 0)).await;
    harness.mock.set_model_outputs([
        sample_text("automatic_first_group", 0),
        sample_text("above_99", 0),
        sample_text("above_99", 2),
    ]);
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    let account = harness.runtime().actions.account(id, None).await.unwrap();
    let started = harness
        .runtime()
        .tests
        .start(
            id,
            &json!({
                "model_id":"gpt-6-luna",
                "expected_version":account_version(&account),
                "request_id":uuid::Uuid::new_v4().simple().to_string(),
                "concurrency":1
            }),
            None,
        )
        .await
        .unwrap();
    let job = harness
        .wait_job_terminal(started["id"].as_str().unwrap())
        .await;
    assert_eq!(job["trigger_source"], "manual");
    assert_eq!(job["automatic"], false);
    assert_eq!(job["first_group_prediction"], "gpt-5.6-luna");
    assert!(job["automatic_disposition"].is_null());
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 3);
    assert!(harness
        .mock
        .calls("POST", "/v1/responses")
        .iter()
        .all(|call| {
            call.payload["model"] == "gpt-6-luna"
                && call.payload["stream"] == true
                && call.bearer_authorized
                && !call.admin_authorized
        }));
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        false
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn detection_credits_unknown_or_stale_evidence_does_not_override_fresh_usage_clock() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let now = harness.now();
    let row = harness
        .seed_account(
            "oauth",
            true,
            json!({
                "codex_credits_snapshot":{
                    "credits":{"has_credits":true,"unlimited":false,"balance":null},
                    "fetched_at":(now-Duration::minutes(51)).timestamp() as f64
                }
            }),
        )
        .await;
    let id = integer(&row["id"]).unwrap();
    let mut update = Update::default();
    update.results.insert(id, full_usage_result(now));
    harness.runtime().monitor.store.commit(update).unwrap();
    let current = harness.runtime().monitor.row(id).await.unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(
                &current,
                &harness.runtime().detection.mark(id).unwrap(),
                false
            )
            .await
            .unwrap(),
        ""
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["status"], "completed");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert_native_oauth_calls(&harness.mock.calls("POST", "/v1/responses"));
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn native_credit_and_usage_evidence_matrix_matches_full_runtime_admission() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let now = harness.now();
    let empty_credits = json!({
        "codex_credits_snapshot":{
            "credits":{"has_credits":false,"unlimited":false,"balance":0},
            "fetched_at":now.timestamp() as f64
        }
    });
    let snapshot =
        |has_credits: bool, unlimited: bool, balance: Value, fetched_at: DateTime<Utc>| {
            json!({
                "codex_credits_snapshot":{
                    "credits":{"has_credits":has_credits,"unlimited":unlimited,"balance":balance},
                    "fetched_at":fetched_at.timestamp() as f64
                }
            })
        };
    let native_usage = |used: f64, reset_at: DateTime<Utc>, credits: Value| {
        json!({
            "codex_usage_updated_at":datetime(now),
            "codex_5h_used_percent":used,
            "codex_7d_used_percent":used,
            "codex_5h_reset_at":datetime(reset_at),
            "codex_7d_reset_at":datetime(if reset_at <= now {reset_at} else {reset_at+Duration::days(1)}),
            "codex_credits_snapshot":credits["codex_credits_snapshot"].clone()
        })
    };
    let full_quota = full_usage_result(now);
    let cases = vec![
        (
            "unlimited",
            snapshot(false, true, json!(0), now),
            Some(full_quota.clone()),
            false,
        ),
        ("missing", json!({}), Some(full_quota.clone()), false),
        (
            "stale",
            snapshot(false, false, json!(0), now - Duration::seconds(3_601)),
            Some(full_quota.clone()),
            false,
        ),
        (
            "contradictory",
            snapshot(false, false, json!(1), now),
            Some(full_quota.clone()),
            false,
        ),
        (
            "nan-balance",
            snapshot(true, false, json!("NaN"), now),
            Some(full_quota.clone()),
            false,
        ),
        (
            "negative-balance",
            snapshot(true, false, json!(-1), now),
            Some(full_quota.clone()),
            false,
        ),
        (
            "future-snapshot",
            snapshot(false, false, json!(0), now + Duration::seconds(1)),
            Some(full_quota.clone()),
            false,
        ),
        (
            "below-full-native-window",
            native_usage(99.999, now + Duration::hours(1), empty_credits.clone()),
            None,
            false,
        ),
        (
            "exact-full-empty",
            native_usage(100.0, now + Duration::hours(1), empty_credits.clone()),
            None,
            true,
        ),
        (
            "elapsed-window-reset",
            native_usage(100.0, now - Duration::seconds(1), empty_credits.clone()),
            None,
            false,
        ),
        (
            "fractional-positive-credit",
            native_usage(
                100.0,
                now + Duration::hours(1),
                snapshot(true, false, json!("0.01"), now),
            ),
            None,
            false,
        ),
    ];
    let mut rows = Vec::new();
    let mut update = Update::default();
    for (label, extra, result, should_pause) in cases {
        let row = harness.seed_account("oauth", true, extra).await;
        let id = integer(&row["id"]).unwrap();
        if let Some(result) = result {
            update.results.insert(id, result);
        }
        harness.enable_detection(id, 1).await;
        harness
            .runtime()
            .detection
            .update(
                id,
                json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
            )
            .unwrap();
        rows.push((label, id, should_pause));
    }
    harness.runtime().monitor.store.commit(update).unwrap();
    harness.runtime().detection.tick().await.unwrap();

    let mut allowed_jobs = Vec::new();
    for (label, id, should_pause) in &rows {
        let control = harness.runtime().detection.control(*id).unwrap();
        if *should_pause {
            assert_eq!(control["status"], "paused", "case {label}");
            assert_eq!(control["reason"], "已确认额度耗尽", "case {label}");
            assert!(control["job_id"].is_null(), "case {label}");
            assert!(
                harness.runtime().detection.store.read().unwrap()["automatic_starts"]
                    .get(id.to_string())
                    .is_none()
            );
        } else {
            assert_eq!(control["status"], "queued", "case {label}");
            let job_id = control["job_id"].as_str().unwrap().to_owned();
            allowed_jobs.push((job_id, *label));
        }
    }
    let mut expected_requests = 0;
    for (job_id, label) in allowed_jobs {
        let job = harness.wait_job_terminal(&job_id).await;
        assert_eq!(job["status"], "completed", "case {label}");
        expected_requests += job["groups"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|group| group["status"] == "completed")
            .count();
    }
    assert_eq!(
        harness.mock.count("POST", "/v1/responses"),
        expected_requests
    );
    assert_eq!(
        harness.mock.calls("POST", "/v1/responses").len(),
        expected_requests
    );
    let oauth_calls = harness.mock.calls("POST", "/v1/responses");
    assert_native_oauth_calls(&oauth_calls);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn oauth_recovery_restarts_interval_but_preserves_shared_cooldown() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let now = harness.now();
    let row = harness
        .seed_account("oauth", true, fresh_empty_credits(now))
        .await;
    let id = integer(&row["id"]).unwrap();
    let mut update = Update::default();
    update.results.insert(id, full_usage_result(now));
    harness.runtime().monitor.store.commit(update).unwrap();
    harness.enable_detection(id, 1).await;
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    harness.runtime().detection.tick().await.unwrap();
    let paused = harness.runtime().detection.control(id).unwrap();
    assert_eq!(paused["status"], "paused");
    assert_eq!(paused["reason"], "已确认额度耗尽");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);

    let prior_dispatch_at = harness.now() - Duration::seconds(100);
    harness
        .runtime()
        .detection
        .store
        .transaction(|state| {
            state["automatic_starts"][id.to_string()] = json!({
                "job_id":"fixture-prior-credit-dispatch",
                "started_at":datetime(prior_dispatch_at)
            });
            Ok(())
        })
        .unwrap();
    sqlx::query("UPDATE accounts SET extra=$1 WHERE id=$2")
        .bind(SqlJson(fresh_available_credits(harness.now())))
        .bind(id)
        .execute(&harness.runtime().db.pool)
        .await
        .unwrap();
    let recovered_row = harness.runtime().monitor.row(id).await.unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(
                &recovered_row,
                &harness.runtime().detection.mark(id).unwrap(),
                false
            )
            .await
            .unwrap(),
        ""
    );
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["status"],
        "waiting"
    );
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["next_at"],
        datetime(harness.now() + Duration::seconds(200))
    );
    harness.advance(199);
    harness.runtime().detection.tick().await.unwrap();
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    harness.advance(1);
    harness.runtime().detection.tick().await.unwrap();
    let job_id = harness.runtime().detection.control(id).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = harness.wait_job_terminal(&job_id).await;
    assert_eq!(job["status"], "completed");
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    let oauth_calls = harness.mock.calls("POST", "/v1/responses");
    assert_native_oauth_calls(&oauth_calls);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["automatic_starts"][id.to_string()]
            ["started_at"],
        datetime(harness.now())
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn stale_oauth_rejection_is_ignored_but_fresh_rejection_blocks_recheck() {
    let harness = Harness::new(sample_text("above_99", 0)).await;
    let now = harness.now();
    let row = harness
        .seed_account("oauth", true, fresh_available_credits(now))
        .await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 1).await;
    let mark = harness.runtime().detection.mark(id).unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            mark["version"].as_str().unwrap(),
            now,
            Some("fixture-credit-mark"),
        )
        .unwrap();
    let mut old_rejection = Update::default();
    old_rejection.results.insert(
        id,
        json!({
            "template_type":"oauth",
            "success":false,
            "error_code":"quota_exceeded",
            "queried_at":datetime(now-Duration::hours(2))
        }),
    );
    harness
        .runtime()
        .monitor
        .store
        .commit(old_rejection)
        .unwrap();
    let current = harness.runtime().monitor.row(id).await.unwrap();
    let mark = harness.runtime().detection.mark(id).unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(&current, &mark, true)
            .await
            .unwrap(),
        ""
    );

    let mut auth_state = Update::default();
    auth_state.scheduler.insert(
        id,
        json!({
            "quota_query":{
                "auth_fingerprint":sub2ops_server::oauth_admission::credential_fingerprint(&current)
            }
        }),
    );
    harness.runtime().monitor.store.commit(auth_state).unwrap();
    let authenticated = harness.runtime().monitor.row(id).await.unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(&authenticated, &mark, true)
            .await
            .unwrap(),
        "认证异常，等待凭据更新"
    );
    let mut manually_closed = authenticated.clone();
    manually_closed["schedulable"] = false.into();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(&manually_closed, &mark, true)
            .await
            .unwrap(),
        "调度已关闭"
    );

    let account_version =
        sub2ops_server::compat::operation_versions(&current, false)["model_test"].clone();
    let job = completed_recheck_job(
        id,
        &harness.runtime().detection.control(id).unwrap()["generation"],
        &mark["version"],
        &account_version,
        "fixture-stale-rejection-job",
        now,
    );
    let mut clear_auth = Update::default();
    clear_auth
        .scheduler
        .insert(id, json!({"quota_query":{"auth_fingerprint":null}}));
    harness.runtime().monitor.store.commit(clear_auth).unwrap();
    let mut fresh_rejection = Update::default();
    fresh_rejection.results.insert(
        id,
        json!({
            "template_type":"oauth",
            "success":false,
            "error_code":"quota_exceeded",
            "queried_at":datetime(now)
        }),
    );
    harness
        .runtime()
        .monitor
        .store
        .commit(fresh_rejection)
        .unwrap();
    assert_eq!(
        harness
            .runtime()
            .detection
            .reason(
                &harness.runtime().monitor.row(id).await.unwrap(),
                &mark,
                true
            )
            .await
            .unwrap(),
        "上游已拒绝额度请求"
    );
    assert_eq!(
        harness
            .runtime()
            .detection
            .complete_recheck(&job)
            .await
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        true
    );
    assert!(harness.runtime().detection.control(id).unwrap()["recheck_recovery"].is_null());
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn recovery_receipt_failure_is_adopted_after_restart_and_bark_uses_five_second_retry() {
    let mut harness = Harness::new(sample_text("above_99", 0)).await;
    let row = harness.seed_account("apikey", true, json!({})).await;
    let id = integer(&row["id"]).unwrap();
    harness.enable_detection(id, 15).await;
    let initial_mark = harness.runtime().detection.mark(id).unwrap();
    let job_id = "e".repeat(32);
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            initial_mark["version"].as_str().unwrap(),
            harness.now(),
            Some(&job_id),
        )
        .unwrap();
    let marked = harness.runtime().detection.mark(id).unwrap();
    let control = harness.runtime().detection.control(id).unwrap();
    let account = harness.runtime().actions.account(id, None).await.unwrap();
    let account_version =
        sub2ops_server::compat::operation_versions(&account, false)["model_test"].clone();
    let receipt = json!({
        "status":"pending",
        "job_id":job_id,
        "generation":control["generation"],
        "mark_version":marked["version"],
        "mark_cleared":false,
        "recheck_account_version":account_version,
        "recheck_schedulable":true,
        "created_at":datetime(harness.now()),
        "model_id":"gpt-6-luna",
        "prediction":"claude-sonnet-4-6",
        "first_group_prediction":"claude-sonnet-4-6",
        "probability":0.97,
        "valid_groups":1
    });
    harness
        .runtime()
        .detection
        .update(
            id,
            json!({
                "hold":true,
                "disposition":{"status":"completed","marked":true,"schedule_verified":true,"job_id":"fixture-prior-degradation"},
                "recheck_recovery":receipt
            }),
        )
        .unwrap();
    harness
        .runtime()
        .detection
        .capacity
        .store
        .set_mark(
            id,
            false,
            marked["version"].as_str().unwrap(),
            harness.now() + Duration::milliseconds(1),
            Some(&job_id),
        )
        .unwrap();
    let state_dir = harness.settings.data_dir();
    let read_only = DirectoryReadOnly::new(state_dir);
    assert!(harness
        .runtime()
        .detection
        .finish_recheck_recovery(id)
        .await
        .is_err());
    drop(read_only);
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        false
    );
    assert_eq!(
        harness.runtime().detection.control(id).unwrap()["recheck_recovery"]["status"],
        "pending"
    );
    assert!(object_field_is_empty_or_missing(
        &harness.runtime().detection.store.read().unwrap(),
        "disposition_notifications"
    ));
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(harness.mock.count("POST", "/push"), 0);

    harness.restart_runtime().await;
    harness.runtime().detection.tick().await.unwrap();
    let recovered = harness.runtime().detection.control(id).unwrap()["recheck_recovery"].clone();
    assert_eq!(recovered["status"], "completed");
    assert_eq!(recovered["job_id"], job_id);
    assert_eq!(recovered["mark_cleared"], true);
    assert!(recovered["completed_at"].is_string());
    assert!(recovered["mark_version_after"].is_string());
    assert_eq!(
        harness.runtime().detection.mark(id).unwrap()["marked"],
        false
    );
    let event = harness.runtime().detection.store.read().unwrap()["disposition_notifications"]
        [&job_id]
        .clone();
    assert_eq!(event["kind"], "recovered");
    assert_eq!(event["status"], "queued");
    assert_eq!(event["attempts"], 0);
    assert_eq!(event["account_id"], id);
    assert_eq!(event["generation"], control["generation"]);
    assert_eq!(event["mark_version"], recovered["mark_version_after"]);
    assert_eq!(event["schedulable"], true);
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(
        harness
            .mock
            .count("POST", &format!("/api/v1/admin/accounts/{id}/schedulable")),
        0
    );

    harness.mock.set_bark_failures(1);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    let retry = harness.runtime().detection.store.read().unwrap()["disposition_notifications"]
        [&job_id]
        .clone();
    assert_eq!(retry["kind"], "recovered");
    assert_eq!(retry["status"], "retry");
    assert_eq!(retry["attempts"], 1);
    assert_eq!(
        retry["next_at"],
        datetime(harness.now() + Duration::seconds(5))
    );
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 1);
    harness.advance(5);
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 2);
    assert_eq!(
        harness.runtime().detection.store.read().unwrap()["disposition_notifications"][&job_id]
            ["status"],
        "delivered"
    );
    let completed_job = completed_recheck_job(
        id,
        &control["generation"],
        &marked["version"],
        &account_version,
        &job_id,
        harness.now(),
    );
    assert_eq!(
        harness
            .runtime()
            .detection
            .complete_recheck(&completed_job)
            .await
            .unwrap()["status"],
        "completed"
    );
    harness
        .runtime()
        .detection
        .deliver_notifications()
        .await
        .unwrap();
    assert_eq!(harness.mock.count("POST", "/push"), 2);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}
