use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{json, Value};
use sqlx::types::Json as SqlJson;
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration as StdDuration, Instant},
};
use sub2ops_server::{
    api,
    bark::{self, Bark},
    capacity_alerts::{mark_view, CapacityAlerts},
    compat::datetime,
    db::Database,
    error_evidence,
    oauth_state::OAuthState,
    queries::{self, Queries},
    runtime::Runtime,
    security,
    settings::Settings,
    sql,
    store::write_json as write_atomic_json,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle, time::sleep};

static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(9_800_000);
static NEXT_ERROR_ID: AtomicI64 = AtomicI64::new(9_900_000);
static DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/contracts/notifications/notifications-fixtures.json"
    ))
    .expect("pinned Python notification fixture must parse")
}

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .expect("notification contract runner must supply temporary PostgreSQL")
}

fn reference_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2031, 4, 5, 6, 7, 8).unwrap()
}

fn settings(
    root: &Path,
    database_url: String,
    bark_value: Value,
    bark_valid: bool,
    base_url: String,
) -> Settings {
    Settings {
        database_url,
        bind: "127.0.0.1:0".into(),
        base_path: String::new(),
        audit_path: root.join("audit.jsonl"),
        usage_path: root.join("usage-query-state.json"),
        oauth_path: root.join("oauth-config.json"),
        bark_path: root.join("bark-config.json"),
        fallback_path: root.join("key-fallback-config.json"),
        connection_path: root.join("sub2api-config.json"),
        model_path: root.join("group-model-config.json"),
        oauth: json!({
            "oauth_auto_reset_credit_enabled": false,
            "oauth_daily_test_enabled": false,
            "oauth_daily_test_time": "23:59",
            "oauth_early_probe_batch_size": 8,
            "oauth_recovery_connection_account_ids": [],
            "oauth_recovery_model_account_ids": [],
            "oauth_recovery_monitor_enabled": false,
            "oauth_recovery_test_concurrency": 1,
            "oauth_recovery_test_model_id": "fixture-model",
            "oauth_usage_refresh_concurrency": 1
        }),
        bark: bark_value,
        bark_valid,
        base_url,
        verify_url: String::new(),
        read_only: false,
    }
}

fn bark_settings(root: &Path, url: String, key: &str, enabled: bool, valid: bool) -> Settings {
    settings(
        root,
        "postgresql://example.invalid/fixture".into(),
        json!({"enabled":enabled,"device_key":key,"server_url":url}),
        valid,
        String::new(),
    )
}

fn write_json(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create owned fixture parent");
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize synthetic JSON fixture"),
    )
    .expect("write synthetic JSON fixture");
}

fn fixture_env(root: &Path, bark_path: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        "DATABASE_URL".into(),
        "postgresql://example.invalid/fixture".into(),
    );
    env.insert("BARK_CONFIG_PATH".into(), bark_path.display().to_string());
    env.insert(
        "OAUTH_CONFIG_PATH".into(),
        root.join("missing-oauth-config.json").display().to_string(),
    );
    env
}

fn runtime_env(root: &Path, bark_path: &Path, base_url: &str) -> BTreeMap<String, String> {
    let mut env = fixture_env(root, bark_path);
    env.insert("DATABASE_URL".into(), database_url());
    env.insert(
        "USAGE_QUERY_STATE_PATH".into(),
        root.join("usage-query-state.json").display().to_string(),
    );
    env.insert(
        "AUDIT_PATH".into(),
        root.join("audit.jsonl").display().to_string(),
    );
    env.insert("SUB2API_BASE_URL".into(), base_url.into());
    env.insert(
        "KEY_FALLBACK_CONFIG_PATH".into(),
        root.join("missing-fallback.json").display().to_string(),
    );
    env.insert(
        "SUB2API_CONFIG_PATH".into(),
        root.join("missing-connection.json").display().to_string(),
    );
    env.insert(
        "GROUP_MODEL_CONFIG_PATH".into(),
        root.join("missing-model.json").display().to_string(),
    );
    env
}

#[derive(Clone, Debug)]
struct PlannedResponse {
    status: u16,
    body: Vec<u8>,
    location: Option<String>,
}

impl PlannedResponse {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            body: serde_json::to_vec(&value).unwrap(),
            location: None,
        }
    }

    fn raw(status: u16, body: &[u8]) -> Self {
        Self {
            status,
            body: body.to_vec(),
            location: None,
        }
    }

    fn redirect(mut self, location: &str) -> Self {
        self.location = Some(location.into());
        self
    }
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    path: String,
    content_type: String,
    payload: Value,
}

#[derive(Clone)]
struct RequestGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    used: Arc<AtomicBool>,
}

impl RequestGate {
    fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            used: Arc::new(AtomicBool::new(false)),
        }
    }
}

struct CaptureState {
    requests: Mutex<Vec<CapturedRequest>>,
    responses: Mutex<VecDeque<PlannedResponse>>,
    gate: Mutex<Option<RequestGate>>,
    next_delay_ms: AtomicU64,
    upstream_calls: AtomicUsize,
    landing_calls: AtomicUsize,
}

async fn capture_handler(State(state): State<Arc<CaptureState>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    if path == "/api/v1/admin/groups/all" {
        state.upstream_calls.fetch_add(1, Ordering::Relaxed);
        return (StatusCode::OK, Json(json!({"code":0,"data":[]}))).into_response();
    }
    if path == "/landing" {
        state.landing_calls.fetch_add(1, Ordering::Relaxed);
        return (StatusCode::OK, Json(json!({"code":200}))).into_response();
    }
    if !path.ends_with("/push") {
        return StatusCode::NOT_FOUND.into_response();
    }

    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = to_bytes(request.into_body(), 65_536)
        .await
        .unwrap_or_default();
    let payload = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
    state.requests.lock().unwrap().push(CapturedRequest {
        path,
        content_type,
        payload,
    });

    let gate = state.gate.lock().unwrap().clone();
    if let Some(gate) = gate {
        if !gate.used.swap(true, Ordering::AcqRel) {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }
    let delay = state.next_delay_ms.swap(0, Ordering::AcqRel);
    if delay > 0 {
        sleep(StdDuration::from_millis(delay)).await;
    }

    let response = state
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| PlannedResponse::json(200, json!({"code":200})));
    let mut builder = Response::builder()
        .status(response.status)
        .header("content-type", HeaderValue::from_static("application/json"))
        .header("connection", HeaderValue::from_static("close"));
    if let Some(location) = response.location {
        builder = builder.header("location", location);
    }
    builder.body(Body::from(response.body)).unwrap()
}

struct CaptureServer {
    url: String,
    state: Arc<CaptureState>,
    task: Option<JoinHandle<()>>,
}

impl CaptureServer {
    async fn start(responses: Vec<PlannedResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback notification fixture");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(CaptureState {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into()),
            gate: Mutex::new(None),
            next_delay_ms: AtomicU64::new(0),
            upstream_calls: AtomicUsize::new(0),
            landing_calls: AtomicUsize::new(0),
        });
        let router = Router::new()
            .fallback(capture_handler)
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve loopback notification fixture");
        });
        Self {
            url,
            state,
            task: Some(task),
        }
    }

    fn bark_requests(&self) -> Vec<CapturedRequest> {
        self.state.requests.lock().unwrap().clone()
    }

    fn bark_request_count(&self) -> usize {
        self.state.requests.lock().unwrap().len()
    }

    fn upstream_request_count(&self) -> usize {
        self.state.upstream_calls.load(Ordering::Relaxed)
    }

    fn landing_request_count(&self) -> usize {
        self.state.landing_calls.load(Ordering::Relaxed)
    }

    fn delay_next_bark(&self, milliseconds: u64) {
        self.state
            .next_delay_ms
            .store(milliseconds, Ordering::Release);
    }

    fn gate_next_bark(&self, gate: RequestGate) {
        *self.state.gate.lock().unwrap() = Some(gate);
    }

    fn queue_response(&self, response: PlannedResponse) {
        self.state.responses.lock().unwrap().push_back(response);
    }
}

impl Drop for CaptureServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn runtime_for(
    root: &Path,
    base_url: String,
    bark_url: String,
    enabled: bool,
    valid: bool,
) -> Arc<Runtime> {
    let config = settings(
        root,
        database_url(),
        json!({"enabled":enabled,"device_key":"fixture-bark-device-key","server_url":bark_url}),
        valid,
        base_url,
    );
    Runtime::new_with_clock(config, Arc::new(reference_time))
        .await
        .expect("construct Runtime against runner-owned PostgreSQL")
}

async fn assert_no_bark_call_from_notifications(runtime: &Runtime, capture: &CaptureServer) {
    runtime.monitor.deliver().await.unwrap();
    runtime.detection.deliver_notifications().await.unwrap();
    assert_eq!(capture.bark_request_count(), 0);
}

async fn database() -> Database {
    Database::open(&database_url(), false)
        .await
        .expect("open runner-owned temporary PostgreSQL")
}

fn oauth_event(key: &str, status: &str, stage: &str) -> Value {
    json!({
        "account_id": 7,
        "account_name": "fixture-oauth-account",
        "plan_type": "plus",
        "status": status,
        "stage": stage,
        "window_labels": ["fixture-window"],
        "model_id": "fixture-model",
        "error_code": "fixture-error",
        "error": "synthetic notification error",
        "checked_at": datetime(reference_time()),
        "dedupe_key": key
    })
}

fn seed_pending(state: &OAuthState, events: &[Value]) {
    state
        .transaction(|data| {
            for event in events {
                let key = event["dedupe_key"].as_str().unwrap();
                data["pending_events"][key] = event.clone();
            }
            Ok(())
        })
        .expect("seed synthetic notification events");
}

fn error_base() -> i64 {
    NEXT_ERROR_ID.fetch_add(1_000, Ordering::Relaxed)
}

fn account_id() -> i64 {
    NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed)
}

async fn insert_account(
    database: &Database,
    id: i64,
    name: &str,
    kind: &str,
    parent: Option<i64>,
    deleted: bool,
) {
    sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,parent_account_id,deleted_at,updated_at) VALUES ($1,$2,'openai',$3,'active',false,$4,$5,now())")
        .bind(id)
        .bind(name)
        .bind(kind)
        .bind(parent)
        .bind(deleted.then_some(Utc::now()))
        .execute(&database.pool)
        .await
        .expect("insert synthetic account into temporary Sub2API table");
}

#[derive(Clone)]
struct ErrorInput {
    id: i64,
    account_id: i64,
    created_at: DateTime<Utc>,
    owner: String,
    phase: String,
    source: String,
    stream: bool,
    error_message: Option<String>,
    upstream_error_message: Option<String>,
    error_body: Option<String>,
    upstream_error_detail: Option<String>,
}

impl ErrorInput {
    fn provider(id: i64, account_id: i64, created_at: DateTime<Utc>) -> Self {
        Self {
            id,
            account_id,
            created_at,
            owner: "provider".into(),
            phase: "upstream".into(),
            source: "fixture".into(),
            stream: false,
            error_message: None,
            upstream_error_message: Some(
                "Our servers are currently overloaded. Please try again later.".into(),
            ),
            error_body: None,
            upstream_error_detail: None,
        }
    }

    fn gateway(id: i64, account_id: i64, created_at: DateTime<Utc>, phrase: &str) -> Self {
        let mut value = Self::provider(id, account_id, created_at);
        value.owner = "platform".into();
        value.phase = "internal".into();
        value.source = "gateway".into();
        value.stream = true;
        value.error_message = Some(phrase.into());
        value.upstream_error_message = None;
        value.error_body = Some(
            serde_json::to_string(&json!({"error":{"code":"fixture-overloaded","message":phrase}}))
                .unwrap(),
        );
        value
    }
}

async fn insert_error(database: &Database, value: &ErrorInput) {
    sqlx::query("INSERT INTO ops_error_logs (id,account_id,created_at,platform,error_owner,error_phase,error_source,stream,model,requested_model,upstream_model,status_code,upstream_status_code,error_message,upstream_error_message,error_body,upstream_error_detail,upstream_errors,resolved) VALUES ($1,$2,$3,'openai',$4,$5,$6,$7,'fixture-model','fixture-model','fixture-model',503,503,$8,$9,$10,$11,$12,false)")
        .bind(value.id)
        .bind(value.account_id)
        .bind(value.created_at)
        .bind(&value.owner)
        .bind(&value.phase)
        .bind(&value.source)
        .bind(value.stream)
        .bind(&value.error_message)
        .bind(&value.upstream_error_message)
        .bind(&value.error_body)
        .bind(&value.upstream_error_detail)
        .bind(SqlJson(json!([])))
        .execute(&database.pool)
        .await
        .expect("insert synthetic error into temporary Sub2API table");
}

fn capacity_alerts(root: &Path, database: Database) -> CapacityAlerts {
    let settings = settings(
        root,
        database_url(),
        json!({"enabled":false,"device_key":"","server_url":""}),
        true,
        String::new(),
    );
    CapacityAlerts::new(database, &settings)
}

fn seed_capacity_baseline(alerts: &CapacityAlerts, at: DateTime<Utc>) {
    let since = datetime(at - Duration::minutes(1));
    alerts
        .store
        .transaction(|data| {
            data["cursor"] = json!(0);
            data["initialized_at"] = since.clone().into();
            data["gateway_since"] = since.clone().into();
            data["apikey_since"] = since.clone().into();
            data["detection_since"] = since.clone().into();
            data["clue_mode_since"] = since.into();
            Ok(())
        })
        .expect("seed notification clue watermark");
}

fn synthetic_provider_row(id: i64, account_id: i64, name: &str, at: DateTime<Utc>) -> Value {
    json!({
        "id":id,
        "account_id":account_id,
        "created_at":datetime(at),
        "error_owner":"provider",
        "error_phase":"upstream",
        "error_source":"fixture",
        "stream":false,
        "upstream_error_message":"Our servers are currently overloaded. Please try again later.",
        "error_message":null,
        "error_body":null,
        "upstream_error_detail":null,
        "upstream_errors":[],
        "account_name":name,
        "account_platform":"openai",
        "account_type":"oauth",
        "account_deleted_at":null
    })
}

fn assert_collected_fixture_case(snapshot: &Value, case: &Value) {
    let id = case["row"]["id"].as_i64().unwrap();
    let event_key = format!("error:{id}");
    let notification = &snapshot["notifications"][id.to_string()];
    if let Some(expected) = case["expected"].as_str() {
        assert!(
            !expected.is_empty(),
            "fixture expected a concrete matched message for {}",
            case["id"]
        );
        assert_eq!(
            error_evidence::match_message(&case["row"]),
            Some(expected),
            "case {} is accepted by the Rust matcher",
            case["id"]
        );
        assert_eq!(notification["status"], "clue", "case {}", case["id"]);
        assert_eq!(
            snapshot["detection_events"]
                .get(event_key.as_str())
                .unwrap()["account_id"],
            1
        );
    } else {
        assert!(
            notification.is_null(),
            "case {} should not emit a clue",
            case["id"]
        );
        assert!(snapshot["detection_events"]
            .get(event_key.as_str())
            .is_none());
    }
}

fn matcher_row_created_after(row: &Value, at: DateTime<Utc>) -> Value {
    let mut row = row.clone();
    row["created_at"] = datetime(at).into();
    row
}

fn match_fixture_cases(root: &Path, database: Database, cases: &[Value]) -> Value {
    let alerts = capacity_alerts(root, database);
    let at = Utc::now();
    let rows = cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            let mut row = matcher_row_created_after(
                &case["row"],
                at + Duration::milliseconds(index as i64 + 1),
            );
            row["account_id"] = json!(1);
            row
        })
        .collect::<Vec<_>>();
    alerts
        .store
        .transaction(|data| {
            data["cursor"] = json!(0);
            data["initialized_at"] = datetime(at - Duration::minutes(1)).into();
            data["detection_since"] = datetime(at - Duration::minutes(1)).into();
            Ok(())
        })
        .expect("seed synthetic notification watermark");
    alerts
        .collect(&rows, true, at)
        .expect("collect fixture evidence through production CapacityAlerts");
    alerts.store.snapshot().expect("read matcher state")
}

fn legacy_pending(account_id: i64, id: i64, at: DateTime<Utc>) -> Value {
    json!({
        "id": id,
        "account_id": account_id,
        "account_name": "fixture-account",
        "requested_model": "fixture-model",
        "upstream_model": "fixture-model",
        "message": "Our servers are currently overloaded. Please try again later.",
        "created_at": datetime(at),
        "next_at": datetime(at),
        "attempts": 1
    })
}

#[test]
fn bark_url_validation_and_utf8_parts_match_pinned_python() {
    let reference = fixture();
    let url = &reference["bark"];
    for (input, expected) in url["normalize_inputs"]
        .as_array()
        .unwrap()
        .iter()
        .zip(url["normalized"].as_array().unwrap())
    {
        assert_eq!(
            bark::normalize_url(input.as_str().unwrap()).unwrap(),
            expected.as_str().unwrap()
        );
    }
    let push_inputs = [
        "https://fixture.example.invalid/root/",
        "https://fixture.example.invalid/自建/",
        "https://fixture.example.invalid/%E8%87%AA%E5%BB%BA/",
    ];
    for (input, expected) in push_inputs.iter().zip(url["push_urls"].as_array().unwrap()) {
        let normalized = bark::normalize_url(input).unwrap();
        assert_eq!(format!("{normalized}/push"), expected.as_str().unwrap());
    }
    for value in url["allowed_urls"].as_array().unwrap() {
        let value = value.as_str().unwrap();
        assert_eq!(bark::normalize_url(value).unwrap(), value);
    }
    for value in url["rejected_urls"].as_array().unwrap() {
        assert!(
            bark::normalize_url(value.as_str().unwrap()).is_err(),
            "URL should be rejected: {}",
            value
        );
    }

    let payload = &url["payload"];
    let text = payload["text"].as_str().unwrap();
    let max = payload["max_bytes"].as_u64().unwrap() as usize;
    let chunks = bark::split_chunks(text, max).unwrap();
    assert_eq!(chunks.concat(), text);
    assert_eq!(json!(chunks), payload["chunks"]);
    assert!(chunks.iter().all(|part| part.len() <= max));
    let parts = bark::body_parts(text, max).unwrap();
    assert_eq!(json!(parts), payload["parts"]);
    assert_eq!(
        parts
            .iter()
            .map(|part| part.split_once('\n').unwrap().1)
            .collect::<String>(),
        text
    );
    assert!(parts.iter().all(|part| part.len() <= max));
}

#[test]
fn bark_https_rejects_non_public_literal_ips_and_allows_public_hosts() {
    let accepted = [
        "https://api.day.app",
        "https://example.invalid/self-hosted",
        "http://localhost:8080",
        "http://127.0.0.1:8080",
        "http://[::1]:8080",
    ];
    for url in accepted {
        assert!(
            bark::normalize_url(url).is_ok(),
            "expected public HTTPS host or existing HTTP loopback to pass"
        );
    }

    let rejected = [
        // IPv4 loopback, unspecified, link-local, private, shared and multicast.
        "https://127.0.0.1",
        "https://0.0.0.0",
        "https://169.254.1.1",
        "https://10.0.0.1",
        "https://172.16.0.1",
        "https://192.168.1.1",
        "https://100.64.0.1",
        "https://224.0.0.1",
        "https://255.255.255.255",
        // Reserved and documentation ranges.
        "https://192.0.0.1",
        "https://192.0.2.1",
        "https://198.18.0.1",
        "https://198.51.100.1",
        "https://203.0.113.1",
        "https://240.0.0.1",
        // IPv6 unspecified, loopback, link-local, ULA, site-local, multicast,
        // documentation, benchmarking, ORCHID and 6to4 ranges.
        "https://[::]",
        "https://[::1]",
        "https://[fe80::1]",
        "https://[fc00::1]",
        "https://[fd00::1]",
        "https://[fec0::1]",
        "https://[ff02::1]",
        "https://[2001:db8::1]",
        "https://[2001:2::1]",
        "https://[2001:20::1]",
        "https://[2002:7f00:1::1]",
        // IPv4-mapped IPv6 and WHATWG-style numeric IPv4 host spellings.
        "https://[::ffff:127.0.0.1]",
        "https://[::ffff:192.168.1.1]",
        "https://2130706433",
        "https://0x7f000001",
        "https://0177.0.0.1",
    ];
    for url in rejected {
        assert!(
            bark::normalize_url(url).is_err(),
            "non-public HTTPS literal must be rejected"
        );
    }

    let secret_url = "https://192.0.2.1/fixture-secret-device-key";
    let error = bark::normalize_url(secret_url).unwrap_err();
    assert_eq!(error.status.as_u16(), 422);
    let public_error = error.detail.to_string();
    assert!(!public_error.contains(secret_url));
    assert!(!public_error.contains("fixture-secret-device-key"));
}

#[test]
fn bark_oauth_event_titles_and_bodies_match_pinned_python() {
    for case in fixture()["bark"]["message_cases"].as_array().unwrap() {
        let (title, body) = bark::oauth_message(&case["event"]);
        assert_eq!(
            json!([title, body]),
            case["expected"],
            "pinned notification message case {}",
            case["id"]
        );
    }
}

#[test]
fn bark_error_sanitization_matches_pinned_python_for_synthetic_markers() {
    let reference = fixture();
    let cases = &reference["bark"]["sanitization"];
    let actual = security::sanitize(cases["input"].as_str().unwrap(), 400);
    assert_eq!(actual, cases["expected"]);
    let long = security::sanitize(&"汉".repeat(400), 400);
    assert_eq!(long, cases["long_utf8"]);
    assert!(long.len() <= cases["long_utf8_expected_length_at_most"].as_u64().unwrap() as usize);
}

#[tokio::test]
async fn bark_loopback_request_uses_v2_json_and_percent_encoded_path() {
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().expect("create synthetic Bark state root");
    let config = Arc::new(bark_settings(
        root.path(),
        capture.url.clone(),
        "fixture-device-key",
        true,
        true,
    ));
    let notifier = Bark::new(config).expect("construct Rust Bark client");
    let result = notifier
        .push("fixture-title", "fixture-body", 3, &Value::Null)
        .await;
    assert!(result.success);
    assert_eq!(result.sent_parts, 1);
    let requests = capture.bark_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/push");
    assert!(requests[0].content_type.starts_with("application/json"));
    assert_eq!(
        requests[0].payload,
        json!({"device_key":"fixture-device-key","title":"fixture-title","body":"fixture-body"})
    );

    let unicode_config = Arc::new(bark_settings(
        root.path(),
        format!("{}/自建", capture.url),
        "fixture-device-key",
        true,
        true,
    ));
    let unicode_notifier = Bark::new(unicode_config).unwrap();
    assert!(
        unicode_notifier
            .push("fixture-title", "fixture-body", 3, &Value::Null)
            .await
            .success
    );
    let requests = capture.bark_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].path, "/%E8%87%AA%E5%BB%BA/push");
}

#[tokio::test]
async fn bark_http_status_business_code_json_decode_and_redirect_are_classified() {
    let capture = CaptureServer::start(vec![
        PlannedResponse::json(503, json!({"code":200})),
        PlannedResponse::json(200, json!({"code":500})),
        PlannedResponse::json(200, json!({"code":"200"})),
        PlannedResponse::raw(200, b"not-json"),
        PlannedResponse::json(302, json!({"code":200})).redirect("/landing"),
    ])
    .await;
    let root = tempfile::tempdir().unwrap();
    let notifier = Bark::new(Arc::new(bark_settings(
        root.path(),
        capture.url.clone(),
        "fixture-device-key",
        true,
        true,
    )))
    .unwrap();
    let expected = [
        "http_status",
        "bark_response_code",
        "bark_response_code",
        "response_decode",
        "http_status",
    ];
    for error_code in expected {
        let result = notifier
            .push("fixture-title", "fixture-body", 3, &Value::Null)
            .await;
        assert!(!result.success);
        assert_eq!(result.error_code, error_code);
    }
    assert_eq!(capture.bark_request_count(), 5);
    assert_eq!(
        capture.landing_request_count(),
        0,
        "Bark redirects are not followed"
    );

    let missing_key = Bark::new(Arc::new(bark_settings(
        root.path(),
        capture.url.clone(),
        "",
        true,
        true,
    )))
    .unwrap()
    .push("fixture-title", "fixture-body", 3, &Value::Null)
    .await;
    assert_eq!(missing_key.error_code, "missing_device_key");
    let invalid_url = Bark::new(Arc::new(bark_settings(
        root.path(),
        "http://fixture.example.invalid".into(),
        "fixture-device-key",
        true,
        true,
    )))
    .unwrap()
    .push("fixture-title", "fixture-body", 3, &Value::Null)
    .await;
    assert_eq!(invalid_url.error_code, "invalid_server_url");
    assert_eq!(capture.bark_request_count(), 5);
}

#[tokio::test]
async fn bark_timeout_connection_failure_and_invalid_runtime_config_are_stable() {
    let reference = fixture();
    let capture = CaptureServer::start(vec![]).await;
    let gate = RequestGate::new();
    capture.gate_next_bark(gate.clone());
    let root = tempfile::tempdir().unwrap();
    let notifier = Bark::new(Arc::new(bark_settings(
        root.path(),
        capture.url.clone(),
        "fixture-device-key",
        true,
        true,
    )))
    .unwrap();
    let timed = notifier
        .push("fixture-title", "fixture-body", 1, &Value::Null)
        .await;
    gate.release.notify_one();
    assert!(!timed.success);
    assert_eq!(
        timed.error_code,
        reference["bark"]["transport_expected"]["timeout"]["error_code"]
    );
    assert_eq!(capture.bark_request_count(), 1);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let disconnected = Bark::new(Arc::new(bark_settings(
        root.path(),
        closed_url,
        "fixture-device-key",
        true,
        true,
    )))
    .unwrap()
    .push("fixture-title", "fixture-body", 1, &Value::Null)
    .await;
    assert!(!disconnected.success);
    assert_eq!(
        disconnected.error_code,
        reference["bark"]["transport_expected"]["request_error"]["error_code"]
    );

    let invalid = Bark::new(Arc::new(bark_settings(
        root.path(),
        capture.url.clone(),
        "fixture-device-key",
        true,
        false,
    )))
    .unwrap()
    .push("fixture-title", "fixture-body", 3, &Value::Null)
    .await;
    assert!(!invalid.success);
    assert_eq!(invalid.error_code, "invalid_config");
    assert_eq!(capture.bark_request_count(), 1);
}

#[tokio::test]
async fn bark_runtime_config_swaps_never_mix_key_and_server_path() {
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let notifier = Bark::new(Arc::new(bark_settings(
        root.path(),
        format!("{}/a", capture.url),
        "fixture-key-a",
        true,
        true,
    )))
    .unwrap();
    let ready = Arc::new(std::sync::Barrier::new(2));
    let swapping = notifier.clone();
    let swap_ready = ready.clone();
    let base = capture.url.clone();
    let swapper = thread::spawn(move || {
        swap_ready.wait();
        for _ in 0..1500 {
            swapping.configure(
                &json!({"enabled":true,"device_key":"fixture-key-b","server_url":format!("{base}/b")}),
                true,
            );
            swapping.configure(
                &json!({"enabled":true,"device_key":"fixture-key-a","server_url":format!("{base}/a")}),
                true,
            );
        }
    });
    ready.wait();
    for _ in 0..1500 {
        assert!(
            notifier
                .push("fixture-title", "fixture-body", 3, &Value::Null)
                .await
                .success
        );
    }
    swapper
        .join()
        .expect("Bark runtime config swapper completed");
    let requests = capture.bark_requests();
    assert_eq!(requests.len(), 1500);
    assert!(requests.iter().all(|request| {
        matches!(
            (
                request.path.as_str(),
                request.payload["device_key"].as_str()
            ),
            ("/a/push", Some("fixture-key-a")) | ("/b/push", Some("fixture-key-b"))
        )
    }));
}

#[tokio::test]
async fn bark_json_settings_override_environment_and_missing_files_use_environment() {
    let reference = fixture();
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();

    let reference_path = root.path().join("reference-bark-config.json");
    write_json(
        &reference_path,
        &json!({
            "enabled":false,
            "device_key":"fixture-file-key",
            "server_url":"https://file.example.invalid"
        }),
    );
    let mut reference_env = fixture_env(root.path(), &reference_path);
    reference_env.insert("BARK_ENABLED".into(), "true".into());
    reference_env.insert("BARK_DEVICE_KEY".into(), "fixture-env-key".into());
    reference_env.insert(
        "BARK_SERVER_URL".into(),
        "https://env.example.invalid".into(),
    );
    let reference_settings = Settings::from_env(&reference_env, false).unwrap();
    assert_eq!(
        json!({
            "enabled":reference_settings.bark["enabled"],
            "config_valid":reference_settings.bark_valid,
            "device_key":reference_settings.bark["device_key"],
            "server_url":reference_settings.bark["server_url"]
        }),
        reference["bark"]["settings_file_overrides_environment"]
    );

    let file_path = root.path().join("file-priority.json");
    write_json(
        &file_path,
        &json!({
            "enabled":true,
            "device_key":"fixture-file-priority-key",
            "server_url":format!("{}/from-file", capture.url)
        }),
    );
    let mut env = fixture_env(root.path(), &file_path);
    env.insert("BARK_ENABLED".into(), "false".into());
    env.insert("BARK_DEVICE_KEY".into(), "fixture-env-priority-key".into());
    env.insert(
        "BARK_SERVER_URL".into(),
        format!("{}/from-environment", capture.url),
    );
    let loaded = Settings::from_env(&env, false).unwrap();
    assert_eq!(loaded.bark["enabled"], true);
    assert_eq!(loaded.bark["device_key"], "fixture-file-priority-key");
    assert_eq!(
        loaded.bark["server_url"],
        format!("{}/from-file", capture.url)
    );
    let file_notifier = Bark::new(Arc::new(loaded)).unwrap();
    let event = oauth_event("fixture:file-priority", "recovered", "active_usage");
    assert_eq!(file_notifier.notify_oauth(&[event], None).await.len(), 1);
    let request = capture.bark_requests().pop().unwrap();
    assert_eq!(request.path, "/from-file/push");
    assert_eq!(request.payload["device_key"], "fixture-file-priority-key");

    let missing_path = root.path().join("missing-bark-config.json");
    let mut env = fixture_env(root.path(), &missing_path);
    env.insert("BARK_ENABLED".into(), "true".into());
    env.insert("BARK_DEVICE_KEY".into(), "fixture-env-fallback-key".into());
    env.insert(
        "BARK_SERVER_URL".into(),
        format!("{}/from-environment", capture.url),
    );
    let loaded = Settings::from_env(&env, false).unwrap();
    let expected = &reference["bark"]["settings_environment_fallback"];
    assert_eq!(loaded.bark["enabled"], expected["enabled"]);
    assert_eq!(loaded.bark_valid, expected["config_valid"]);
    assert_eq!(loaded.bark["device_key"], "fixture-env-fallback-key");
    assert_eq!(
        loaded.bark["server_url"],
        format!("{}/from-environment", capture.url)
    );
    let environment_notifier = Bark::new(Arc::new(loaded)).unwrap();
    let event = oauth_event("fixture:environment-fallback", "recovered", "active_usage");
    assert_eq!(
        environment_notifier
            .notify_oauth(&[event], None)
            .await
            .len(),
        1
    );
    let requests = capture.bark_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].path, "/from-environment/push");
    assert_eq!(
        requests[1].payload["device_key"],
        "fixture-env-fallback-key"
    );
}

#[test]
fn malformed_and_wrong_type_bark_config_remain_invalid() {
    let reference = fixture();
    let root = tempfile::tempdir().unwrap();
    let malformed_path = root.path().join("invalid-bark.json");
    fs::write(&malformed_path, "{broken").unwrap();
    let env = fixture_env(root.path(), &malformed_path);
    let invalid = Settings::from_env(&env, false).unwrap();
    assert_eq!(
        invalid.bark_valid,
        reference["bark"]["invalid_existing_config"]["config_valid"]
    );
    assert!(!invalid.bark_valid);

    for case in fixture()["bark"]["invalid_schema_cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["id"] != "invalid_effective_url")
    {
        let path = root
            .path()
            .join(format!("{}.json", case["id"].as_str().unwrap()));
        write_json(&path, &case["input"]);
        let settings = Settings::from_env(&fixture_env(root.path(), &path), false).unwrap();
        assert_eq!(
            settings.bark_valid, case["expected_config_valid"],
            "invalid schema case {}",
            case["id"]
        );
        assert!(!settings.bark_valid);
    }
}

#[test]
fn bark_invalid_field_and_effective_url_cases_match_pinned_python() {
    let reference = fixture();
    let root = tempfile::tempdir().unwrap();
    for case in reference["bark"]["invalid_schema_cases"]
        .as_array()
        .unwrap()
    {
        let path = root
            .path()
            .join(format!("{}.json", case["id"].as_str().unwrap()));
        write_json(&path, &case["input"]);
        let loaded = Settings::from_env(&fixture_env(root.path(), &path), false).unwrap();
        assert_eq!(
            loaded.bark_valid, case["expected_config_valid"],
            "pinned invalid configuration case {}",
            case["id"]
        );
        assert!(!loaded.bark_valid);
    }
}

#[test]
fn non_utf8_bark_config_keeps_invalid_config_panel_readable_like_pinned_python() {
    let reference = fixture();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("non-utf8-bark-config.json");
    fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    let loaded = Settings::from_env(&fixture_env(root.path(), &path), false).unwrap();
    assert!(!loaded.bark_valid);
    let notifier = Bark::new(Arc::new(loaded)).unwrap();
    let panel = notifier.panel().unwrap_or_else(|error| {
        panic!(
            "pinned Python returns config_valid={}, Rust Bark panel failed with HTTP {}",
            reference["bark"]["invalid_utf8_config"]["panel_config_valid"],
            error.status.as_u16()
        )
    });
    assert_eq!(
        panel["config_valid"],
        reference["bark"]["invalid_utf8_config"]["panel_config_valid"]
    );
}

#[tokio::test]
async fn bark_config_service_save_preserves_fields_and_redacts_runtime_key() {
    let reference = fixture();
    let expected = &reference["bark"]["config_save"];
    let configured_url = "http://127.0.0.1:50126";
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;
    let initial = runtime.config.snapshot(Some("bark")).unwrap()["bark"].clone();
    let first = runtime
        .config
        .save(
            "bark",
            &json!({"enabled":true,"device_key":"fixture-save-one","server_url":configured_url}),
            "fixture-admin",
            Some(initial["revision"].as_str().unwrap()),
        )
        .await
        .unwrap();
    let second = runtime
        .config
        .save(
            "bark",
            &json!({"device_key":"fixture-save-two"}),
            "fixture-admin",
            Some(first["revision"].as_str().unwrap()),
        )
        .await
        .unwrap();
    let path = &runtime.settings.bark_path;
    let persisted: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let mut comparable_persisted = persisted.clone();
    comparable_persisted["updated_at"] = expected["persisted"]["updated_at"].clone();
    assert_eq!(comparable_persisted, expected["persisted"]);
    assert_eq!(persisted["updated_by"], "fixture-admin");
    assert_eq!(second["device_key_status"], "已设置");
    assert_eq!(second["config_updated_at"], persisted["updated_at"]);
    let rendered = serde_json::to_string(&second).unwrap();
    assert!(!rendered.contains("fixture-save-two"));
    assert!(!rendered.contains(configured_url));
    assert!(second.get("device_key").is_none());
    assert!(second.get("server_url").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            format!(
                "{:04o}",
                fs::metadata(path).unwrap().permissions().mode() & 0o777
            ),
            expected["mode_octal"]
        );
    }
    let leftovers = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".sub2ops-state-")
        })
        .count();
    assert_eq!(
        leftovers,
        expected["temporary_files"].as_array().unwrap().len()
    );
    runtime.close().await;
}

#[test]
fn bark_store_write_json_is_atomic_private_and_leaves_no_temporary_file() {
    let expected = &fixture()["bark"]["atomic_save"];
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bark-config.json");
    write_atomic_json(&path, &json!({"device_key":"fixture-save-one"})).unwrap();
    write_atomic_json(&path, &json!({"device_key":"fixture-save-two"})).unwrap();

    let persisted: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted, expected["persisted"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            format!(
                "{:04o}",
                fs::metadata(&path).unwrap().permissions().mode() & 0o777
            ),
            expected["mode_octal"]
        );
    }
    let leftovers = fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".sub2ops-state-")
        })
        .count();
    assert_eq!(
        leftovers,
        expected["temporary_files"].as_array().unwrap().len()
    );
}

#[tokio::test]
async fn oauth_notification_queue_ack_retry_dedup_and_suppression_match_pinned_python() {
    let _serial = DB_TEST_LOCK.lock().await;
    let reference = fixture();
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;

    let success = oauth_event("fixture:queue-success", "recovered", "active_usage");
    seed_pending(&runtime.monitor.store, std::slice::from_ref(&success));
    runtime.monitor.deliver().await.unwrap();
    runtime.monitor.deliver().await.unwrap();
    let state = runtime.monitor.store.snapshot().unwrap();
    assert!(state["pending_events"].as_object().unwrap().is_empty());
    assert_eq!(capture.bark_request_count(), 1);
    assert_eq!(
        state["scheduler"]["7"]["notified_keys"],
        json!(["fixture:queue-success"])
    );
    assert_eq!(
        state["scheduler"]["7"]["last_notification_suppressed"],
        false
    );
    assert_eq!(reference["bark"]["queue"]["success_dedup"]["requests"], 1);

    let mut credit = oauth_event("fixture:queue-credit", "recovered", "active_usage");
    credit["reset_credit"] = json!({
        "consumed":true,
        "completed_at":datetime(reference_time()),
        "verification_method":"connection"
    });
    credit["test_success"] = true.into();
    credit["recovered_at"] = datetime(reference_time()).into();
    seed_pending(&runtime.monitor.store, std::slice::from_ref(&credit));
    runtime.monitor.deliver().await.unwrap();
    runtime.monitor.deliver().await.unwrap();
    assert_eq!(capture.bark_request_count(), 2);
    assert_eq!(
        capture.bark_requests()[1].payload["title"],
        reference["bark"]["queue"]["confirmed_credit"]["title"]
    );
    assert!(runtime.monitor.store.snapshot().unwrap()["pending_events"]
        .get("fixture:queue-credit")
        .is_none());
    let restarted = OAuthState::new(runtime.settings.usage_path.clone(), false);
    assert!(
        restarted.snapshot().unwrap()["scheduler"]["7"]["notified_keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "fixture:queue-credit")
    );

    let scheduler_before_failure = runtime.monitor.store.snapshot().unwrap()["scheduler"].clone();
    let failed = oauth_event("fixture:queue-failure", "recovered", "active_usage");
    seed_pending(&runtime.monitor.store, std::slice::from_ref(&failed));
    capture.queue_response(PlannedResponse::json(200, json!({"code":500})));
    runtime.monitor.deliver().await.unwrap();
    let after_failure = runtime.monitor.store.snapshot().unwrap();
    assert_eq!(after_failure["scheduler"], scheduler_before_failure);
    assert_eq!(
        after_failure["pending_events"]["fixture:queue-failure"],
        failed
    );
    assert!(after_failure["scheduler"]["7"]["notified_keys"]
        .as_array()
        .unwrap()
        .iter()
        .all(|value| value != "fixture:queue-failure"));
    assert_eq!(capture.bark_request_count(), 3);
    capture.queue_response(PlannedResponse::json(200, json!({"code":500})));
    runtime.monitor.deliver().await.unwrap();
    assert_eq!(
        capture.bark_request_count(),
        4,
        "an unacknowledged event remains retryable"
    );
    assert_eq!(
        runtime.monitor.store.snapshot().unwrap()["pending_events"]["fixture:queue-failure"],
        failed
    );

    let suppressed = oauth_event("fixture:queue-disabled", "recovered", "active_usage");
    seed_pending(&runtime.monitor.store, std::slice::from_ref(&suppressed));
    runtime.config.bark.configure(
        &json!({"enabled":false,"device_key":"fixture-device-key","server_url":capture.url}),
        true,
    );
    runtime.monitor.deliver().await.unwrap();
    let state = runtime.monitor.store.snapshot().unwrap();
    assert!(state["pending_events"]
        .get("fixture:queue-disabled")
        .is_none());
    assert_eq!(
        state["scheduler"]["7"]["last_notification_suppressed"],
        true
    );
    assert_eq!(capture.bark_request_count(), 4);
    runtime.close().await;

    let invalid_root = tempfile::tempdir().unwrap();
    let invalid_path = invalid_root.path().join("malformed-bark-config.json");
    fs::write(&invalid_path, b"{broken").unwrap();
    let invalid_env = runtime_env(invalid_root.path(), &invalid_path, &capture.url);
    let invalid_settings = Settings::from_env(&invalid_env, false).unwrap();
    assert_eq!(
        invalid_settings.bark_valid,
        reference["bark"]["invalid_existing_config"]["config_valid"]
    );
    let invalid_runtime = Runtime::new_with_clock(invalid_settings, Arc::new(reference_time))
        .await
        .unwrap();
    let invalid = oauth_event("fixture:queue-invalid", "recovered", "active_usage");
    seed_pending(
        &invalid_runtime.monitor.store,
        std::slice::from_ref(&invalid),
    );
    invalid_runtime.monitor.deliver().await.unwrap();
    let invalid_state = invalid_runtime.monitor.store.snapshot().unwrap();
    assert_eq!(
        invalid_state["pending_events"]["fixture:queue-invalid"],
        invalid
    );
    assert!(invalid_state["scheduler"].as_object().unwrap().is_empty());
    assert_eq!(capture.bark_request_count(), 4);
    invalid_runtime.close().await;

    let env_root = tempfile::tempdir().unwrap();
    let missing_bark = env_root.path().join("missing-bark-config.json");
    let mut env = runtime_env(env_root.path(), &missing_bark, &capture.url);
    env.insert("BARK_ENABLED".into(), "treu".into());
    env.insert("BARK_DEVICE_KEY".into(), "fixture-environment-key".into());
    env.insert("BARK_SERVER_URL".into(), capture.url.clone());
    let env_settings = Settings::from_env(&env, false).unwrap();
    assert_eq!(
        env_settings.bark_valid,
        reference["bark"]["queue"]["invalid_environment"]["config_valid"]
    );
    let env_runtime = Runtime::new_with_clock(env_settings, Arc::new(reference_time))
        .await
        .unwrap();
    let invalid_environment_event = oauth_event(
        "fixture:queue-invalid-environment",
        "recovered",
        "active_usage",
    );
    seed_pending(
        &env_runtime.monitor.store,
        std::slice::from_ref(&invalid_environment_event),
    );
    env_runtime.monitor.deliver().await.unwrap();
    let env_state = env_runtime.monitor.store.snapshot().unwrap();
    assert_eq!(
        env_state["pending_events"]["fixture:queue-invalid-environment"],
        invalid_environment_event
    );
    assert!(env_state["scheduler"].as_object().unwrap().is_empty());
    assert_eq!(capture.bark_request_count(), 4);
    env_runtime.close().await;

    let multi_root = tempfile::tempdir().unwrap();
    let multi_capture = CaptureServer::start(vec![
        PlannedResponse::json(200, json!({"code":200})),
        PlannedResponse::json(200, json!({"code":500})),
    ])
    .await;
    let multi_runtime = runtime_for(
        multi_root.path(),
        multi_capture.url.clone(),
        multi_capture.url.clone(),
        true,
        true,
    )
    .await;
    let multi_reference = &reference["bark"]["queue"]["multisegment_failure"];
    let long_event = multi_reference["event"].clone();
    seed_pending(
        &multi_runtime.monitor.store,
        std::slice::from_ref(&long_event),
    );
    multi_runtime.monitor.deliver().await.unwrap();
    let multi_state = multi_runtime.monitor.store.snapshot().unwrap();
    let multi_requests = multi_capture.bark_requests();
    assert_eq!(
        multi_requests.len(),
        multi_reference["requests"].as_u64().unwrap() as usize
    );
    let dedupe_key = long_event["dedupe_key"].as_str().unwrap();
    assert_eq!(multi_state["pending_events"][dedupe_key], long_event);
    assert_eq!(
        multi_state["pending_events"].as_object().unwrap().len(),
        multi_reference["pending_count"].as_u64().unwrap() as usize
    );
    let notified_keys = multi_state["scheduler"]["7"]["notified_keys"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(json!(notified_keys), multi_reference["notified_keys"]);
    assert_eq!(
        json!(multi_requests
            .iter()
            .map(|request| request.payload["title"].clone())
            .collect::<Vec<_>>()),
        multi_reference["titles"]
    );
    assert!(multi_requests
        .iter()
        .all(|request| { request.payload["body"].as_str().unwrap().len() <= 2400 }));
    assert_eq!(
        multi_requests
            .iter()
            .map(|request| request.payload["body"].as_str().unwrap().len())
            .collect::<Vec<_>>(),
        multi_reference["body_byte_lengths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_u64().unwrap() as usize)
            .collect::<Vec<_>>()
    );
    multi_runtime.close().await;
}

#[tokio::test]
async fn manual_control_cancels_copied_recovery_notification_before_bark_delivery() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;

    let pre_revoked = oauth_event("fixture:pre-revoked", "recovered", "recovery");
    seed_pending(&runtime.monitor.store, std::slice::from_ref(&pre_revoked));
    runtime
        .monitor
        .store
        .manual_control(&json!({"id":7}), false, reference_time(), "manual")
        .unwrap();
    runtime.monitor.deliver().await.unwrap();
    assert_eq!(capture.bark_request_count(), 0);
    assert!(runtime.monitor.store.snapshot().unwrap()["pending_events"]
        .as_object()
        .unwrap()
        .is_empty());

    let active = oauth_event("a-fixture-active", "recovered", "active_usage");
    let in_flight_recovery = oauth_event("z-fixture-recovery", "recovered", "recovery");
    seed_pending(
        &runtime.monitor.store,
        &[active.clone(), in_flight_recovery.clone()],
    );
    let gate = RequestGate::new();
    capture.gate_next_bark(gate.clone());
    let monitor = runtime.monitor.clone();
    let delivery = tokio::spawn(async move { monitor.deliver().await });
    gate.entered.notified().await;
    assert_eq!(capture.bark_request_count(), 1);
    runtime
        .monitor
        .store
        .manual_control(&json!({"id":7}), false, reference_time(), "manual")
        .unwrap();
    gate.release.notify_one();
    delivery.await.unwrap().unwrap();

    let state = runtime.monitor.store.snapshot().unwrap();
    assert_eq!(capture.bark_request_count(), 1);
    assert!(state["pending_events"].as_object().unwrap().is_empty());
    assert!(state["scheduler"]["7"]["notified_keys"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "a-fixture-active"));
    assert!(state["scheduler"]["7"]["notified_keys"]
        .as_array()
        .unwrap()
        .iter()
        .all(|value| value != "z-fixture-recovery"));
    runtime.close().await;
}

#[tokio::test]
async fn bark_monitor_and_test_endpoint_observe_three_and_eight_second_timeouts() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;

    let event = oauth_event("fixture:timeout-monitor", "recovered", "active_usage");
    seed_pending(&runtime.monitor.store, std::slice::from_ref(&event));
    capture.delay_next_bark(3_500);
    let started = Instant::now();
    runtime.monitor.deliver().await.unwrap();
    assert!(started.elapsed() >= StdDuration::from_millis(2700));
    assert!(started.elapsed() < StdDuration::from_secs(5));
    assert_eq!(capture.bark_request_count(), 1);
    assert!(runtime.monitor.store.snapshot().unwrap()["pending_events"]
        .get("fixture:timeout-monitor")
        .is_some());

    runtime
        .monitor
        .store
        .save_admin_token("fixture-admin-key")
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = api::router(runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    capture.delay_next_bark(7_000);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let started = Instant::now();
    let response = client
        .post(format!("http://{address}/api/desktop/v1/actions/bark-test"))
        .header("x-api-key", "fixture-admin-key")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(started.elapsed() >= StdDuration::from_millis(6500));
    assert!(started.elapsed() < StdDuration::from_secs(8));
    assert_eq!(capture.bark_request_count(), 2);
    let request = capture.bark_requests().pop().unwrap();
    assert_eq!(request.payload["title"], "Sub2API Ops Companion");
    assert_eq!(request.payload["body"], "Bark 推送通道正常。");
    assert_eq!(capture.upstream_request_count(), 1);
    runtime.config.bark.configure(
        &json!({
            "enabled":true,
            "device_key":"fixture-device-key",
            "server_url":capture.url
        }),
        false,
    );
    let rejected = client
        .post(format!("http://{address}/api/desktop/v1/actions/bark-test"))
        .header("x-api-key", "fixture-admin-key")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(capture.bark_request_count(), 2);
    server.abort();
    let _ = server.await;
    runtime.close().await;
}

#[tokio::test]
async fn malformed_oauth_event_does_not_block_a_later_valid_event_matches_pinned_python() {
    let reference = fixture();
    let expected = &reference["bark"]["queue"]["malformed_then_valid"];
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let notifier = Bark::new(Arc::new(bark_settings(
        root.path(),
        capture.url.clone(),
        "fixture-device-key",
        true,
        true,
    )))
    .unwrap();
    let malformed = expected["malformed_input"].clone();
    let valid = expected["valid_event"].clone();
    let delivered = notifier.notify_oauth(&[malformed, valid], None).await;
    assert_eq!(json!(delivered), expected["delivered"]);
    assert_eq!(
        json!(delivered
            .iter()
            .filter_map(|event| event.get("dedupe_key").cloned())
            .collect::<Vec<_>>()),
        expected["delivered_dedupe_keys"]
    );
    assert_eq!(capture.bark_request_count(), expected["requests"]);
}

#[tokio::test]
async fn capacity_and_gateway_match_cases_flow_through_clue_state_machine() {
    let _serial = DB_TEST_LOCK.lock().await;
    let database = database().await;
    let capacity_root = tempfile::tempdir().unwrap();
    let capacity_cases = fixture()["capacity"]["match_cases"].clone();
    let capacity_state = match_fixture_cases(
        capacity_root.path(),
        database.clone(),
        capacity_cases.as_array().unwrap(),
    );
    for case in capacity_cases.as_array().unwrap() {
        assert_collected_fixture_case(&capacity_state, case);
    }

    let gateway_root = tempfile::tempdir().unwrap();
    let gateway_cases = fixture()["gateway"]["match_cases"].clone();
    let gateway_state = match_fixture_cases(
        gateway_root.path(),
        database,
        gateway_cases.as_array().unwrap(),
    );
    for case in gateway_cases.as_array().unwrap() {
        assert_collected_fixture_case(&gateway_state, case);
    }
}

#[tokio::test]
async fn capacity_watermark_replay_pagination_and_restart_preserve_clue_events() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;
    let database = runtime.db.clone();
    let first_account = account_id();
    let second_account = account_id();
    insert_account(
        &database,
        first_account,
        "fixture-capacity-one",
        "oauth",
        None,
        false,
    )
    .await;
    insert_account(
        &database,
        second_account,
        "fixture-capacity-two",
        "oauth",
        None,
        false,
    )
    .await;
    let base = error_base();
    let watermark =
        ErrorInput::provider(base + 2, first_account, Utc::now() - Duration::minutes(1));
    insert_error(&database, &watermark).await;
    let alerts = &runtime.detection.capacity;
    alerts.poll().await.unwrap();
    let first = alerts.store.snapshot().unwrap();
    assert_eq!(first["cursor"], base + 2);
    assert!(first["detection_events"].as_object().unwrap().is_empty());
    assert!(first["pending"].as_object().unwrap().is_empty());

    let delayed = ErrorInput::provider(base + 1, first_account, Utc::now());
    insert_error(&database, &delayed).await;
    alerts.poll().await.unwrap();
    let replayed = alerts.store.snapshot().unwrap();
    assert!(replayed["detection_events"]
        .get(format!("error:{}", base + 1).as_str())
        .is_some());

    let first_new_time = Utc::now() + Duration::milliseconds(5);
    for id in (base + 3)..=(base + 4) {
        let row = ErrorInput::provider(id, first_account, first_new_time);
        insert_error(&database, &row).await;
    }
    alerts.poll().await.unwrap();
    let two_new = alerts.store.snapshot().unwrap();
    assert!(two_new["detection_events"]
        .get(format!("error:{}", base + 3).as_str())
        .is_some());
    assert!(two_new["detection_events"]
        .get(format!("error:{}", base + 4).as_str())
        .is_some());
    assert_eq!(two_new["detection_events"].as_object().unwrap().len(), 3);
    assert!(two_new["pending"].as_object().unwrap().is_empty());
    alerts.poll().await.unwrap();
    assert_eq!(
        alerts.store.snapshot().unwrap()["detection_events"]
            .as_object()
            .unwrap()
            .len(),
        3,
        "poll replay does not duplicate clue events"
    );

    let page_time = Utc::now() + Duration::milliseconds(10);
    for id in (base + 5)..=(base + 403) {
        let row = ErrorInput::provider(id, first_account, page_time);
        insert_error(&database, &row).await;
    }
    alerts.poll().await.unwrap();
    let paged = alerts.store.snapshot().unwrap();
    assert_eq!(paged["detection_events"].as_object().unwrap().len(), 402);
    assert_eq!(paged["cursor"], base + 403);
    assert!(paged["pending"].as_object().unwrap().is_empty());
    assert!(paged["detection_events"]
        .get(format!("error:{}", base + 2).as_str())
        .is_none());

    let restarted = CapacityAlerts::new(database.clone(), &runtime.settings);
    let restored = restarted.store.snapshot().unwrap();
    assert_eq!(restored["cursor"], base + 403);
    assert_eq!(restored["detection_events"].as_object().unwrap().len(), 402);
    let after_restart = ErrorInput::provider(base + 404, second_account, Utc::now());
    insert_error(&database, &after_restart).await;
    restarted.poll().await.unwrap();
    let resumed = restarted.store.snapshot().unwrap();
    assert_eq!(resumed["cursor"], base + 404);
    assert_eq!(resumed["detection_events"].as_object().unwrap().len(), 403);
    assert!(resumed["pending"].as_object().unwrap().is_empty());
    assert_no_bark_call_from_notifications(&runtime, &capture).await;
    runtime.close().await;
}

#[tokio::test]
async fn raw_capacity_clues_never_enter_retry_windows_or_bark_request_schedule() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;
    let alerts = &runtime.detection.capacity;
    let at = Utc::now();
    seed_capacity_baseline(alerts, at);
    let row = synthetic_provider_row(9_991_001, 1, "fixture-clue-account", at);
    alerts.collect(&[row], true, at).unwrap();
    let reference = fixture();
    let event_key = "error:9991001";
    for delay in reference["capacity"]["retry_windows"].as_array().unwrap() {
        let next_at = at + Duration::seconds(delay.as_i64().unwrap());
        alerts.collect(&[], false, next_at).unwrap();
        let state = alerts.store.snapshot().unwrap();
        assert!(state["detection_events"].get(event_key).is_some());
        assert_eq!(state["notifications"]["9991001"]["status"], "clue");
        assert!(state["pending"].as_object().unwrap().is_empty());
        assert!(state["slow_pending"].as_object().unwrap().is_empty());
        assert_no_bark_call_from_notifications(&runtime, &capture).await;
    }
    assert_eq!(
        alerts.store.snapshot().unwrap()["notifications"]["9991001"]["status"],
        "clue"
    );
    assert_eq!(capture.bark_request_count(), 0);
    runtime.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn capacity_save_failures_do_not_advance_or_lose_cursor_and_evidence() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let state_parent = root.path().join("state-parent");
    fs::create_dir(&state_parent).unwrap();
    let mut config = settings(
        root.path(),
        database_url(),
        json!({
            "enabled":true,
            "device_key":"fixture-bark-device-key",
            "server_url":capture.url
        }),
        true,
        capture.url.clone(),
    );
    config.usage_path = state_parent.join("usage-state.json");
    let runtime = Runtime::new_with_clock(config.clone(), Arc::new(reference_time))
        .await
        .unwrap();
    let database = runtime.db.clone();
    let account = account_id();
    insert_account(
        &database,
        account,
        "fixture-save-failure",
        "oauth",
        None,
        false,
    )
    .await;
    let base = error_base();
    let old = ErrorInput::provider(base + 1, account, Utc::now() - Duration::minutes(1));
    insert_error(&database, &old).await;

    let first_attempt = runtime.detection.capacity.clone();
    let path = first_attempt.store.0.path().to_owned();
    let lock_path = path.with_extension("lock");
    fs::write(&lock_path, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let writable_mode = fs::metadata(&state_parent).unwrap().permissions().mode() & 0o777;
        fs::set_permissions(&state_parent, fs::Permissions::from_mode(0o500)).unwrap();
        let failed = first_attempt.poll().await;
        fs::set_permissions(&state_parent, fs::Permissions::from_mode(writable_mode)).unwrap();
        assert!(
            failed.is_err(),
            "the first checkpoint must fail when atomic save is blocked"
        );
    }
    assert!(!path.exists());
    assert!(first_attempt.store.snapshot().unwrap()["cursor"].is_null());
    let restarted_after_failed_save = CapacityAlerts::new(database.clone(), &config);
    assert!(restarted_after_failed_save.store.snapshot().is_err());

    first_attempt.poll().await.unwrap();
    let initial = first_attempt.store.snapshot().unwrap();
    assert_eq!(initial["cursor"], base + 1);
    let bytes_before = fs::read(&path).unwrap();
    let next = ErrorInput::provider(base + 2, account, Utc::now() + Duration::milliseconds(10));
    insert_error(&database, &next).await;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let writable_mode = fs::metadata(&state_parent).unwrap().permissions().mode() & 0o777;
        fs::set_permissions(&state_parent, fs::Permissions::from_mode(0o500)).unwrap();
        let failed = first_attempt.poll().await;
        fs::set_permissions(&state_parent, fs::Permissions::from_mode(writable_mode)).unwrap();
        assert!(
            failed.is_err(),
            "the read-only state directory must reject atomic save"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes_before);
        let unchanged = first_attempt.store.snapshot().unwrap();
        assert_eq!(unchanged["cursor"], base + 1);
        assert!(unchanged["detection_events"]
            .as_object()
            .unwrap()
            .is_empty());
        assert!(unchanged["pending"].as_object().unwrap().is_empty());
        first_attempt.poll().await.unwrap();
        let recovered = first_attempt.store.snapshot().unwrap();
        assert_eq!(recovered["cursor"], base + 2);
        assert!(recovered["detection_events"]
            .get(format!("error:{}", base + 2).as_str())
            .is_some());
        assert!(recovered["pending"].as_object().unwrap().is_empty());
    }
    assert_no_bark_call_from_notifications(&runtime, &capture).await;
    runtime.close().await;
}

#[tokio::test]
async fn capacity_mark_suppresses_only_its_account_and_preserves_other_clues() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;
    let alerts = &runtime.detection.capacity;
    let at = Utc::now();
    seed_capacity_baseline(alerts, at);
    let expected = mark_view(1, &Value::Null)["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let marked = alerts.store.set_mark(1, true, &expected, at, None).unwrap();
    assert_eq!(marked["marked"], true);

    let rows = vec![
        synthetic_provider_row(
            9_992_001,
            1,
            "fixture-shared-name",
            at + Duration::seconds(1),
        ),
        synthetic_provider_row(
            9_992_002,
            2,
            "fixture-other-account",
            at + Duration::seconds(1),
        ),
    ];
    alerts
        .collect(&rows, true, at + Duration::seconds(2))
        .unwrap();
    let state = alerts.store.snapshot().unwrap();
    assert!(state["detection_events"].get("error:9992001").is_none());
    assert!(state["detection_events"].get("error:9992002").is_some());
    assert_eq!(state["notifications"]["9992001"]["status"], "suppressed");
    assert_eq!(
        state["notifications"]["9992001"]["reason"],
        "degradation_mark"
    );
    assert_eq!(state["notifications"]["9992002"]["status"], "clue");
    assert!(state["pending"].as_object().unwrap().is_empty());
    assert_no_bark_call_from_notifications(&runtime, &capture).await;
    runtime.close().await;
}

#[tokio::test]
async fn capacity_mark_clears_legacy_pending_and_retry_only_for_marked_account() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;
    let alerts = &runtime.detection.capacity;
    let at = Utc::now();
    seed_capacity_baseline(alerts, at);
    alerts
        .store
        .transaction(|data| {
            data["pending"]["1"] = legacy_pending(1, 41, at);
            data["pending"]["2"] = legacy_pending(2, 42, at);
            Ok(())
        })
        .unwrap();
    let expected = mark_view(1, &Value::Null)["version"]
        .as_str()
        .unwrap()
        .to_owned();
    alerts.store.set_mark(1, true, &expected, at, None).unwrap();
    let state = alerts.store.snapshot().unwrap();
    assert!(state["pending"].get("1").is_none());
    assert!(state["pending"].get("2").is_some());
    assert_eq!(state["notifications"]["1"]["status"], "suppressed");
    assert_eq!(state["notifications"]["1"]["reason"], "degradation_mark");
    assert_no_bark_call_from_notifications(&runtime, &capture).await;
    runtime.close().await;
}

#[tokio::test]
async fn capacity_mark_scope_uses_account_id_instead_of_parent_or_name() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;
    let database = runtime.db.clone();
    let parent = account_id();
    let child = account_id();
    insert_account(
        &database,
        parent,
        "fixture-shared-name",
        "oauth",
        None,
        false,
    )
    .await;
    insert_account(
        &database,
        child,
        "fixture-shared-name",
        "oauth",
        Some(parent),
        false,
    )
    .await;
    let base = error_base();
    let watermark = ErrorInput::provider(base + 1, parent, Utc::now() - Duration::minutes(1));
    insert_error(&database, &watermark).await;
    let alerts = &runtime.detection.capacity;
    alerts.poll().await.unwrap();
    let expected = mark_view(parent, &Value::Null)["version"]
        .as_str()
        .unwrap()
        .to_owned();
    alerts
        .store
        .set_mark(parent, true, &expected, Utc::now(), None)
        .unwrap();
    let event_at = Utc::now() + Duration::milliseconds(20);
    insert_error(&database, &ErrorInput::provider(base + 2, parent, event_at)).await;
    insert_error(&database, &ErrorInput::provider(base + 3, child, event_at)).await;
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    alerts.poll().await.unwrap();
    let state = alerts.store.snapshot().unwrap();
    assert!(state["detection_events"]
        .get(format!("error:{}", base + 2).as_str())
        .is_none());
    assert!(state["detection_events"]
        .get(format!("error:{}", base + 3).as_str())
        .is_some());
    assert_eq!(
        state["notifications"][(base + 2).to_string()]["reason"],
        "degradation_mark"
    );
    assert_eq!(
        state["notifications"][(base + 3).to_string()]["status"],
        "clue"
    );
    assert_eq!(
        state["marks"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec![parent.to_string()]
    );
    assert_no_bark_call_from_notifications(&runtime, &capture).await;
    runtime.close().await;
}

#[tokio::test]
async fn capacity_unmark_allows_only_newer_evidence_and_persists_across_restart() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;
    let alerts = &runtime.detection.capacity;
    let at = Utc::now();
    seed_capacity_baseline(alerts, at);
    let initial = mark_view(1, &Value::Null)["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let marked = alerts.store.set_mark(1, true, &initial, at, None).unwrap();
    let changed_at = at + Duration::seconds(1);
    let unmarked = alerts
        .store
        .set_mark(
            1,
            false,
            marked["version"].as_str().unwrap(),
            changed_at,
            None,
        )
        .unwrap();
    assert_eq!(unmarked["marked"], false);
    let rows = vec![
        synthetic_provider_row(
            9_994_001,
            1,
            "fixture-unmark",
            changed_at - Duration::milliseconds(1),
        ),
        synthetic_provider_row(
            9_994_002,
            1,
            "fixture-unmark",
            changed_at + Duration::milliseconds(1),
        ),
    ];
    alerts
        .collect(&rows, true, changed_at + Duration::seconds(1))
        .unwrap();
    let state = alerts.store.snapshot().unwrap();
    assert!(state["detection_events"].get("error:9994001").is_none());
    assert!(state["detection_events"].get("error:9994002").is_some());
    assert_eq!(state["notifications"]["9994001"]["reason"], "mark_changed");

    let restarted = CapacityAlerts::new(runtime.db.clone(), &runtime.settings);
    let restored = restarted.store.snapshot().unwrap();
    assert_eq!(restored["marks"]["1"]["marked"], false);
    assert_eq!(restored["marks"]["1"]["changed_at"], datetime(changed_at));
    let delayed = synthetic_provider_row(
        9_994_003,
        1,
        "fixture-unmark",
        changed_at - Duration::milliseconds(2),
    );
    restarted
        .collect(&[delayed], false, changed_at + Duration::seconds(2))
        .unwrap();
    let resumed = restarted.store.snapshot().unwrap();
    assert!(resumed["detection_events"].get("error:9994003").is_none());
    assert_eq!(
        alerts
            .store
            .set_mark(1, true, &"0".repeat(64), changed_at, None)
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    assert_no_bark_call_from_notifications(&runtime, &capture).await;
    runtime.close().await;
}

#[tokio::test]
async fn capacity_mark_and_account_guard_serialize_per_account() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let database = database().await;
    let root = tempfile::tempdir().unwrap();
    let alerts = capacity_alerts(root.path(), database);
    let at = Utc::now();
    seed_capacity_baseline(&alerts, at);
    let expected = mark_view(1, &Value::Null)["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let store = alerts.store.clone();
    let (entered_sender, entered_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let guarded = thread::spawn(move || {
        let _guard = store.account_guard(1).unwrap();
        entered_sender.send(()).unwrap();
        release_receiver.recv().unwrap();
    });
    entered_receiver
        .recv_timeout(StdDuration::from_secs(2))
        .expect("account guard acquired before mark begins");

    let started = Arc::new(AtomicBool::new(false));
    let mark_started = started.clone();
    let mark_store = alerts.store.clone();
    let mark_expected = expected.clone();
    let marking = thread::spawn(move || {
        mark_started.store(true, Ordering::Release);
        mark_store
            .set_mark(1, true, &mark_expected, at, None)
            .unwrap()
    });
    let wait_started = Instant::now();
    while !started.load(Ordering::Acquire) && wait_started.elapsed() < StdDuration::from_secs(2) {
        thread::yield_now();
    }
    assert!(started.load(Ordering::Acquire));
    thread::sleep(StdDuration::from_millis(50));
    assert!(
        !marking.is_finished(),
        "mark waits for the per-account guard"
    );
    release_sender.send(()).unwrap();
    guarded.join().unwrap();
    assert_eq!(marking.join().unwrap()["marked"], true);
    assert!(alerts.store.snapshot().unwrap()["pending"]
        .as_object()
        .unwrap()
        .is_empty());
    assert_eq!(capture.bark_request_count(), 0);
}

#[tokio::test]
async fn gateway_history_remains_queryable_while_upgrade_clues_are_not_backfilled() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;
    let database = runtime.db.clone();
    let first_account = account_id();
    let marked_account = account_id();
    insert_account(
        &database,
        first_account,
        "fixture-gateway-one",
        "oauth",
        None,
        false,
    )
    .await;
    insert_account(
        &database,
        marked_account,
        "fixture-gateway-two",
        "oauth",
        None,
        false,
    )
    .await;
    let base = error_base();
    let old_at = Utc::now() - Duration::seconds(30);
    let old_provider = ErrorInput::provider(base + 1, first_account, old_at);
    let old_gateway = ErrorInput::gateway(
        base + 2,
        marked_account,
        old_at,
        "Our servers are currently overloaded. Please try again later.",
    );
    insert_error(&database, &old_provider).await;
    insert_error(&database, &old_gateway).await;

    let queries = Queries {
        db: database.clone(),
        oauth: OAuthState::new(root.path().join("oauth-state.json"), false),
    };
    let reference = fixture();
    let input = &reference["gateway"]["sql_contract"]["dto_input"];
    assert_eq!(
        queries::error_dto(input, false),
        reference["gateway"]["sql_contract"]["dto_expected"]
    );
    assert!(sql::ERRORS_ERROR_WHERE.contains("e.error_owner='provider'"));
    assert!(sql::ERRORS_ERROR_WHERE.contains("e.error_owner='platform'"));
    assert!(sql::ERRORS_ERROR_WHERE.contains("EXISTS (SELECT 1 FROM accounts evidence_account"));
    let history = queries.errors(None, None, 100, None).await.unwrap();
    let ids = history["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["id"].as_i64())
        .collect::<Vec<_>>();
    assert!(
        ids.contains(&(base + 1)),
        "provider history remains visible"
    );
    assert!(ids.contains(&(base + 2)), "gateway history remains visible");
    let scoped = queries
        .errors(Some(marked_account), None, 100, None)
        .await
        .unwrap();
    assert_eq!(scoped["items"].as_array().unwrap().len(), 1);
    assert_eq!(scoped["items"][0]["id"], base + 2);

    let alerts = &runtime.detection.capacity;
    alerts.poll().await.unwrap();
    let initial = alerts.store.snapshot().unwrap();
    assert_eq!(initial["cursor"], base + 2);
    assert!(initial["detection_events"].as_object().unwrap().is_empty());
    let expected = mark_view(marked_account, &Value::Null)["version"]
        .as_str()
        .unwrap()
        .to_owned();
    alerts
        .store
        .set_mark(marked_account, true, &expected, Utc::now(), None)
        .unwrap();
    let event_at = Utc::now() + Duration::milliseconds(20);
    let new_gateway = ErrorInput::gateway(
        base + 3,
        marked_account,
        event_at,
        "Our servers are currently overloaded. Please try again later.",
    );
    let new_provider = ErrorInput::provider(base + 4, first_account, event_at);
    insert_error(&database, &new_gateway).await;
    insert_error(&database, &new_provider).await;
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    alerts.poll().await.unwrap();
    let state = alerts.store.snapshot().unwrap();
    assert!(state["detection_events"]
        .get(format!("error:{}", base + 1).as_str())
        .is_none());
    assert!(state["detection_events"]
        .get(format!("error:{}", base + 2).as_str())
        .is_none());
    assert!(state["detection_events"]
        .get(format!("error:{}", base + 3).as_str())
        .is_none());
    assert!(state["detection_events"]
        .get(format!("error:{}", base + 4).as_str())
        .is_some());
    assert_eq!(
        state["notifications"][(base + 3).to_string()]["reason"],
        "degradation_mark"
    );
    assert_eq!(
        state["notifications"][(base + 4).to_string()]["status"],
        "clue"
    );
    assert!(state["pending"].as_object().unwrap().is_empty());
    assert_no_bark_call_from_notifications(&runtime, &capture).await;
    runtime.close().await;
}

#[tokio::test]
async fn runtime_persists_provider_and_gateway_clues_within_three_seconds_without_bark_delivery() {
    let _serial = DB_TEST_LOCK.lock().await;
    let capture = CaptureServer::start(vec![]).await;
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime_for(
        root.path(),
        capture.url.clone(),
        capture.url.clone(),
        true,
        true,
    )
    .await;
    let account = account_id();
    insert_account(
        &runtime.db,
        account,
        "fixture-low-latency-gateway",
        "oauth",
        None,
        false,
    )
    .await;
    runtime.start().await.unwrap();

    let init_started = Instant::now();
    while runtime.detection.capacity.store.snapshot().unwrap()["cursor"].is_null()
        && init_started.elapsed() < StdDuration::from_secs(5)
    {
        tokio::time::sleep(StdDuration::from_millis(25)).await;
    }
    assert!(!runtime.detection.capacity.store.snapshot().unwrap()["cursor"].is_null());
    let base = error_base();
    let event_at = Utc::now() + Duration::milliseconds(10);
    let provider = ErrorInput::provider(base + 1, account, event_at);
    let gateway = ErrorInput::gateway(
        base + 2,
        account,
        event_at,
        "Our servers are currently overloaded. Please try again later.",
    );
    insert_error(&runtime.db, &provider).await;
    insert_error(&runtime.db, &gateway).await;
    let unrelated = tokio::spawn(tokio::time::sleep(StdDuration::from_secs(30)));
    let started = Instant::now();
    let provider_key = format!("error:{}", base + 1);
    let gateway_key = format!("error:{}", base + 2);
    while (runtime.detection.capacity.store.snapshot().unwrap()["detection_events"]
        .get(provider_key.as_str())
        .is_none()
        || runtime.detection.capacity.store.snapshot().unwrap()["detection_events"]
            .get(gateway_key.as_str())
            .is_none())
        && started.elapsed() < StdDuration::from_secs(3)
    {
        tokio::time::sleep(StdDuration::from_millis(25)).await;
    }
    let elapsed = started.elapsed();
    let state = runtime.detection.capacity.store.snapshot().unwrap();
    assert!(elapsed <= StdDuration::from_secs(3));
    assert!(state["detection_events"]
        .get(provider_key.as_str())
        .is_some());
    assert!(state["detection_events"]
        .get(gateway_key.as_str())
        .is_some());
    assert_eq!(
        state["notifications"][(base + 1).to_string()]["status"],
        "clue"
    );
    assert_eq!(
        state["notifications"][(base + 2).to_string()]["status"],
        "clue"
    );
    assert_eq!(state["notifications"].as_object().unwrap().len(), 2);
    assert!(state["pending"].as_object().unwrap().is_empty());
    assert!(capture.bark_requests().is_empty());
    assert!(!unrelated.is_finished());
    unrelated.abort();
    let _ = unrelated.await;
    runtime.close().await;
}
