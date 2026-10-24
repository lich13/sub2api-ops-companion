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
    collections::{BTreeMap, VecDeque},
    fs,
    ops::Deref,
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration as StdDuration,
};
use sub2ops_server::{
    compat::{account_version, datetime, integer, parse_datetime},
    db::Database,
    oauth_admission,
    oauth_queries::Source,
    oauth_state::Update,
    quota, recovery_events, recovery_rules,
    runtime::Runtime,
    settings::Settings,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

const ADMIN_KEY: &str = "fixture-admin-manual-dispatch";
const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";
const SUCCESS_TEST_SSE: &str =
    "data: {\"type\":\"test_start\",\"model\":\"fixture-model\"}\n\ndata: {\"type\":\"test_complete\",\"success\":true}\n\n";
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(98_000_000);

fn reference_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2031, 4, 5, 6, 7, 8).unwrap()
}

fn method_name(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
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
}

#[derive(Clone, Copy)]
enum TestMutation {
    NewRateLimit,
    ManualPause,
}

#[derive(Clone)]
struct MockUpstream {
    pool: PgPool,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    calls: Arc<StdMutex<Vec<Call>>>,
    unexpected: Arc<StdMutex<Vec<String>>>,
    usage_gates: Arc<StdMutex<BTreeMap<i64, Gate>>>,
    test_gates: Arc<StdMutex<BTreeMap<i64, Gate>>>,
    test_sse: Arc<StdMutex<VecDeque<String>>>,
    test_mutations: Arc<StdMutex<BTreeMap<i64, TestMutation>>>,
    test_state_fault: Arc<StdMutex<Option<(PathBuf, PathBuf)>>>,
    recovery_state_fault: Arc<StdMutex<Option<(PathBuf, PathBuf)>>>,
    active_usage: Arc<AtomicUsize>,
    max_active_usage: Arc<AtomicUsize>,
    active_tests: Arc<AtomicUsize>,
    max_active_tests: Arc<AtomicUsize>,
}

impl MockUpstream {
    fn new(pool: PgPool, clock: Arc<StdMutex<DateTime<Utc>>>) -> Self {
        Self {
            pool,
            clock,
            calls: Arc::new(StdMutex::new(Vec::new())),
            unexpected: Arc::new(StdMutex::new(Vec::new())),
            usage_gates: Arc::new(StdMutex::new(BTreeMap::new())),
            test_gates: Arc::new(StdMutex::new(BTreeMap::new())),
            test_sse: Arc::new(StdMutex::new(VecDeque::new())),
            test_mutations: Arc::new(StdMutex::new(BTreeMap::new())),
            test_state_fault: Arc::new(StdMutex::new(None)),
            recovery_state_fault: Arc::new(StdMutex::new(None)),
            active_usage: Arc::new(AtomicUsize::new(0)),
            max_active_usage: Arc::new(AtomicUsize::new(0)),
            active_tests: Arc::new(AtomicUsize::new(0)),
            max_active_tests: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn record(&self, method: &Method, path: &str) {
        self.calls.lock().unwrap().push(Call {
            method: method_name(method).into(),
            path: path.into(),
        });
    }

    fn count(&self, method: &str, suffix: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.method == method && call.path.ends_with(suffix))
            .count()
    }

    fn count_for(&self, id: i64, method: &str, suffix: &str) -> usize {
        let account_path = format!("/{id}{suffix}");
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.method == method && call.path.ends_with(&account_path))
            .count()
    }

    fn unexpected(&self) -> Vec<String> {
        self.unexpected.lock().unwrap().clone()
    }

    fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }

    fn set_usage_gate(&self, id: i64, gate: Gate) {
        self.usage_gates.lock().unwrap().insert(id, gate);
    }

    fn set_test_gate(&self, id: i64, gate: Gate) {
        self.test_gates.lock().unwrap().insert(id, gate);
    }

    fn set_test_sse_sequence(&self, values: impl IntoIterator<Item = String>) {
        *self.test_sse.lock().unwrap() = values.into_iter().collect();
    }

    fn mutate_account_on_test(&self, id: i64, mutation: TestMutation) {
        self.test_mutations.lock().unwrap().insert(id, mutation);
    }

    fn fail_state_write_after_test(&self, state: PathBuf, backup: PathBuf) {
        *self.test_state_fault.lock().unwrap() = Some((state, backup));
    }

    fn fail_final_state_write_after_recovery(&self, state: PathBuf, backup: PathBuf) {
        *self.recovery_state_fault.lock().unwrap() = Some((state, backup));
    }

    async fn wait_for_account(&self, id: i64, method: &str, suffix: &str, expected: usize) {
        tokio::time::timeout(StdDuration::from_secs(8), async {
            loop {
                if self.count_for(id, method, suffix) >= expected {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("loopback mock did not receive the expected account request");
    }
}

fn enter(active: &AtomicUsize, maximum: &AtomicUsize) {
    let now = active.fetch_add(1, Ordering::AcqRel) + 1;
    maximum.fetch_max(now, Ordering::AcqRel);
}

fn json_response(value: Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

fn path_id(path: &str, suffix: &str) -> Option<i64> {
    path.strip_prefix("/api/v1/admin/accounts/")?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

fn move_state_file(state: PathBuf, backup: PathBuf) -> Result<(), String> {
    fs::rename(&state, &backup).map_err(|_| "fixture state path could not be moved".to_owned())?;
    fs::create_dir(&state)
        .map_err(|_| "fixture state fault directory could not be created".to_owned())
}

async fn mock_upstream(State(state): State<MockUpstream>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let method = parts.method;
    let path = parts.uri.path().to_owned();
    let bytes = to_bytes(body, 2_000_000).await.unwrap_or_default();
    let payload = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    state.record(&method, &path);

    if method == Method::GET && path.starts_with("/fixture-models/") {
        assert!(!parts.headers.contains_key("x-api-key"));
        assert!(parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("Bearer fixture-")));
        return json_response(json!({
            "data": [
                {"id":"fixture-model","name":"fixture model"},
                {"id":"gpt-6-luna","name":"fixture default model"}
            ]
        }));
    }
    if method == Method::GET && path.ends_with("/models") {
        return json_response(json!({
            "code": 0,
            "data": [{"id":"fixture-model","name":"fixture model"}]
        }));
    }

    if method == Method::GET && path.ends_with("/usage") {
        enter(&state.active_usage, &state.max_active_usage);
        if let Some(id) = path_id(&path, "/usage") {
            let gate = state.usage_gates.lock().unwrap().get(&id).cloned();
            if let Some(gate) = gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
        }
        let now = state.now();
        let data = json!({
            "five_hour": {
                "utilization": 10,
                "resets_at": datetime(now + Duration::hours(4)),
                "remaining_seconds": 14400,
                "window_minutes": 300
            },
            "seven_day": {
                "utilization": 20,
                "resets_at": datetime(now + Duration::days(6)),
                "remaining_seconds": 518400,
                "window_minutes": 10080
            }
        });
        state.active_usage.fetch_sub(1, Ordering::AcqRel);
        return json_response(json!({"code":0,"data":data}));
    }

    if method == Method::POST && path.ends_with("/test") {
        enter(&state.active_tests, &state.max_active_tests);
        let id = path_id(&path, "/test");
        if let Some(id) = id {
            let gate = state.test_gates.lock().unwrap().get(&id).cloned();
            if let Some(gate) = gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            let mutation = { state.test_mutations.lock().unwrap().remove(&id) };
            if let Some(mutation) = mutation {
                let now = state.now();
                match mutation {
                    TestMutation::NewRateLimit => {
                        sqlx::query(
                            "UPDATE accounts SET rate_limited_at=$1,rate_limit_reset_at=$2,updated_at=$1 WHERE id=$3",
                        )
                        .bind(now)
                        .bind(now + Duration::hours(2))
                        .bind(id)
                        .execute(&state.pool)
                        .await
                        .expect("add a synthetic new rate block during the loopback test");
                    }
                    TestMutation::ManualPause => {
                        sqlx::query(
                            "UPDATE accounts SET schedulable=false,temp_unschedulable_reason='manual pause',updated_at=$1 WHERE id=$2",
                        )
                        .bind(now)
                        .bind(id)
                        .execute(&state.pool)
                        .await
                        .expect("apply a synthetic manual pause during the loopback test");
                    }
                }
            }
        }
        if let Some((state_path, backup)) = state.test_state_fault.lock().unwrap().take() {
            move_state_file(state_path, backup)
                .expect("inject only the synthetic OAuth state write failure");
        }
        let response_body = state
            .test_sse
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| SUCCESS_TEST_SSE.into());
        state.active_tests.fetch_sub(1, Ordering::AcqRel);
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(response_body))
            .expect("synthetic loopback test response");
    }

    if method == Method::POST && path.ends_with("/recover-state") {
        let Some(id) = path_id(&path, "/recover-state") else {
            return (StatusCode::NOT_FOUND, Json(json!({"code":1}))).into_response();
        };
        sqlx::query("UPDATE accounts SET rate_limited_at=NULL,rate_limit_reset_at=NULL,overload_until=NULL,temp_unschedulable_until=NULL,temp_unschedulable_reason=NULL,updated_at=$1 WHERE id=$2")
            .bind(state.now())
            .bind(id)
            .execute(&state.pool)
            .await
            .expect("clear only the synthetic recovery block in temporary PostgreSQL");
        if let Some((state_path, backup)) = state.recovery_state_fault.lock().unwrap().take() {
            move_state_file(state_path, backup)
                .expect("inject only the synthetic post-recovery state write failure");
        }
        return json_response(json!({"code":0,"data":{"id":id}}));
    }

    if method == Method::POST && path.ends_with("/schedulable") {
        let Some(id) = path_id(&path, "/schedulable") else {
            return (StatusCode::NOT_FOUND, Json(json!({"code":1}))).into_response();
        };
        let enabled = payload["schedulable"].as_bool().unwrap_or(false);
        sqlx::query("UPDATE accounts SET schedulable=$1,updated_at=$2 WHERE id=$3")
            .bind(enabled)
            .bind(state.now())
            .bind(id)
            .execute(&state.pool)
            .await
            .expect("apply a manual schedule change only to temporary PostgreSQL");
        return json_response(json!({"code":0,"data":{"id":id,"schedulable":enabled}}));
    }

    state
        .unexpected
        .lock()
        .unwrap()
        .push(format!("{} {}", method_name(&method), path));
    (StatusCode::NOT_FOUND, Json(json!({"code":1}))).into_response()
}

struct RuntimeHandle(Option<Arc<Runtime>>);

impl Deref for RuntimeHandle {
    type Target = Runtime;

    fn deref(&self) -> &Self::Target {
        self.0.as_deref().expect("fixture Runtime is available")
    }
}

struct Harness {
    _temp: tempfile::TempDir,
    database: Database,
    runtime: RuntimeHandle,
    mock: MockUpstream,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    pool: PgPool,
    settings: Settings,
    account_ids: Vec<i64>,
    server: Option<JoinHandle<()>>,
}

impl Harness {
    async fn new() -> Self {
        let database_url = std::env::var("DATABASE_URL")
            .expect("manual dispatch contract runner must provide temporary PostgreSQL");
        let database = Database::open(&database_url, false)
            .await
            .expect("open runner-owned temporary PostgreSQL");
        let pool = database.pool.clone();
        let temp = tempfile::tempdir().expect("create synthetic manual dispatch state directory");
        let clock = Arc::new(StdMutex::new(reference_time()));
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind a loopback-only Sub2API fixture");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let mock = MockUpstream::new(pool.clone(), clock.clone());
        let server_state = mock.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .fallback(mock_upstream)
                    .with_state(server_state),
            )
            .await
            .expect("serve synthetic loopback Sub2API fixture");
        });

        let connection_path = temp.path().join("sub2api-config.json");
        fs::write(
            &connection_path,
            serde_json::to_vec(&json!({"base_url":base_url,"verify_base_url":""})).unwrap(),
        )
        .expect("write synthetic loopback upstream configuration");
        let settings = Settings {
            database_url,
            bind: "127.0.0.1:0".into(),
            base_path: String::new(),
            audit_path: temp.path().join("audit.jsonl"),
            usage_path: temp.path().join("oauth-state.json"),
            oauth_path: temp.path().join("oauth-config.json"),
            bark_path: temp.path().join("bark-config.json"),
            connection_path,
            model_path: temp.path().join("group-models.json"),
            keepalive_path: temp.path().join("keepalive-state.json"),
            oauth: json!({
                "oauth_auto_reset_credit_enabled": false,
                "oauth_daily_test_enabled": false,
                "oauth_daily_test_time": "05:00",
                "oauth_early_probe_batch_size": 4,
                "oauth_recovery_connection_account_ids": [],
                "oauth_recovery_model_account_ids": [],
                "oauth_recovery_monitor_enabled": true,
                "oauth_recovery_test_concurrency": 1,
                "oauth_recovery_test_model_id": "fixture-model",
                "oauth_usage_refresh_concurrency": 1
            }),
            bark: json!({"enabled":false,"device_key":"fixture-only","server_url":base_url.clone()}),
            bark_valid: true,
            base_url: base_url.clone(),
            verify_url: String::new(),
            read_only: false,
        };
        let runtime_clock = clock.clone();
        let runtime = Runtime::new_with_clock(
            settings.clone(),
            Arc::new(move || *runtime_clock.lock().unwrap()),
        )
        .await
        .expect("construct real Runtime with deterministic clock and loopback upstream");
        runtime
            .actions
            .set_model_directory_endpoint(&format!("{base_url}/fixture-models"))
            .expect("use the loopback-only provider directory fixture");
        runtime
            .monitor
            .store
            .save_admin_token(ADMIN_KEY)
            .expect("save a synthetic loopback admin key");
        Self {
            _temp: temp,
            database,
            runtime: RuntimeHandle(Some(runtime)),
            mock,
            clock,
            pool,
            settings,
            account_ids: Vec::new(),
            server: Some(server),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }

    fn set_now(&self, at: DateTime<Utc>) {
        *self.clock.lock().unwrap() = at;
    }

    async fn seed_account(
        &mut self,
        name: &str,
        schedulable: bool,
        blocked: bool,
        passive: bool,
    ) -> Value {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        self.account_ids.push(id);
        let now = self.now();
        let observed = now - Duration::seconds(30);
        let extra = if passive {
            passive_extra(
                observed,
                now + Duration::hours(4),
                now + Duration::days(6),
                10,
                20,
            )
        } else {
            json!({})
        };
        sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,rate_limited_at,rate_limit_reset_at,updated_at,concurrency) VALUES ($1,$2,'openai','oauth','active',$3,$4,$5,$6,$7,$8,1)")
            .bind(id)
            .bind(format!("fixture-manual-dispatch-{name}-{id}"))
            .bind(schedulable)
            .bind(SqlJson(json!({
                "plan_type":"plus",
                "access_token":"fixture-access-token",
                "refresh_token":"fixture-refresh-token"
            })))
            .bind(SqlJson(extra))
            .bind(blocked.then_some(now - Duration::minutes(5)))
            .bind(blocked.then_some(now - Duration::seconds(61)))
            .bind(now - Duration::minutes(2))
            .execute(&self.pool)
            .await
            .expect("insert only a synthetic OAuth account");
        self.runtime
            .monitor
            .row(id)
            .await
            .expect("read fixture account through real Runtime projection")
    }

    async fn set_schedulable(
        &self,
        id: i64,
        enabled: bool,
    ) -> sub2ops_server::error::Result<Value> {
        let row = self.runtime.actions.account(id, None).await?;
        self.runtime
            .set_schedulable(
                id,
                &json!({
                    "schedulable":enabled,
                    "expected_version":account_version(&row)
                }),
                ADMIN_KEY,
            )
            .await
    }

    async fn restart_runtime(&mut self) {
        let old = self.runtime.0.take().expect("old Runtime exists");
        old.close().await;
        drop(old);
        let clock = self.clock.clone();
        let runtime = Runtime::new_with_clock(
            self.settings.clone(),
            Arc::new(move || *clock.lock().unwrap()),
        )
        .await
        .expect("restart Runtime against the same temporary database and state files");
        runtime
            .actions
            .set_model_directory_endpoint(&format!("{}/fixture-models", self.settings.base_url))
            .expect("retain the loopback-only provider directory fixture after restart");
        runtime
            .monitor
            .store
            .save_admin_token(ADMIN_KEY)
            .expect("retain the synthetic fixture admin key after restart");
        self.runtime.0 = Some(runtime);
    }

    async fn close(mut self) {
        if let Some(runtime) = self.runtime.0.take() {
            runtime.close().await;
            if !self.account_ids.is_empty() {
                sqlx::query("DELETE FROM accounts WHERE id = ANY($1)")
                    .bind(self.account_ids)
                    .execute(&self.pool)
                    .await
                    .expect("remove only this harness's synthetic accounts");
            }
            drop(runtime);
        }
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
        self.database.close().await;
    }
}

fn passive_extra(
    observed: DateTime<Utc>,
    five_reset: DateTime<Utc>,
    seven_reset: DateTime<Utc>,
    five_used: i64,
    seven_used: i64,
) -> Value {
    json!({
        "codex_usage_updated_at":datetime(observed),
        "codex_5h_used_percent":five_used,
        "codex_5h_reset_at":datetime(five_reset),
        "codex_7d_used_percent":seven_used,
        "codex_7d_reset_at":datetime(seven_reset)
    })
}

fn usage_data(
    five_used: i64,
    seven_used: i64,
    five_reset: DateTime<Utc>,
    seven_reset: DateTime<Utc>,
) -> Value {
    json!({
        "five_hour":{"utilization":five_used,"resets_at":datetime(five_reset),"window_minutes":300},
        "seven_day":{"utilization":seven_used,"resets_at":datetime(seven_reset),"window_minutes":10080}
    })
}

fn cached_result(row: &Value, observed: DateTime<Utc>, data: Value) -> Value {
    let summary = quota::from_usage_data(&data, row, observed);
    json!({
        "account_id":row["id"],
        "template_type":"oauth",
        "success":true,
        "queried_at":datetime(observed),
        "data":data,
        "oauth_quota":summary,
        "source":"sub2api_admin_usage"
    })
}

async fn configure_recovery(
    harness: &Harness,
    ids: &[i64],
    usage_concurrency: usize,
    test_concurrency: usize,
    method: &str,
) {
    let ids = json!(ids);
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_recovery_monitor_enabled"] = true.into();
            config["oauth_recovery_connection_account_ids"] = if method == "connection" {
                ids.clone()
            } else {
                json!([])
            };
            config["oauth_recovery_model_account_ids"] = if method == "model" {
                ids.clone()
            } else {
                json!([])
            };
            config["oauth_recovery_test_model_id"] = "fixture-model".into();
            config["oauth_usage_refresh_concurrency"] = (usage_concurrency as i64).into();
            config["oauth_recovery_test_concurrency"] = (test_concurrency as i64).into();
            Ok(())
        })
        .expect("select only synthetic recovery accounts");
}

async fn set_auto_reset_credit(harness: &Harness, enabled: bool) {
    harness
        .runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_auto_reset_credit_enabled"] = enabled.into();
            Ok(())
        })
        .expect("change only the synthetic fixture credit switch");
}

async fn save_result(harness: &Harness, id: i64, result: Value) {
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["oauth_results"][id.to_string()] = result;
            Ok(())
        })
        .expect("save synthetic quota evidence to fixture state");
}

async fn seed_intent(harness: &Harness, row: &Value, status: &str, at: DateTime<Utc>) -> Value {
    let id = integer(&row["id"]).unwrap();
    let snapshot = harness.runtime.monitor.state().unwrap();
    let result = quota::latest_result(row, &snapshot["oauth_results"][id.to_string()], at);
    let summary = quota::summary_from_result(row, &result);
    let descriptor = recovery_rules::rate_descriptor(row, &summary, at);
    assert!(
        descriptor.is_object(),
        "synthetic account must have rate block evidence"
    );
    let mut intent =
        recovery_events::new_intent(&descriptor, status, Some(at - Duration::minutes(2)));
    intent["due_at"] = datetime(at - Duration::minutes(2)).into();
    if status == "recovered" {
        intent["fingerprint"] = "fixture-prior-recovered-block".into();
        intent["recovered_at"] = datetime(at - Duration::minutes(10)).into();
    }
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]["recovery_method_generation"] = 0.into();
            state["scheduler"][id.to_string()]["recovery_intent"] = intent.clone();
            Ok(())
        })
        .expect("seed one synthetic recovery intent");
    intent
}

async fn wait_for_intent<F>(harness: &Harness, id: i64, predicate: F) -> Value
where
    F: Fn(&Value) -> bool,
{
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let state = harness.runtime.monitor.state().unwrap();
            let intent = state["scheduler"][id.to_string()]["recovery_intent"].clone();
            if predicate(&intent) {
                return state;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("Runtime recovery state did not reach the expected fixture boundary")
}

async fn wait_for_recovered(harness: &Harness, id: i64) -> Value {
    wait_for_intent(harness, id, |intent| intent["status"] == "recovered").await
}

async fn refresh_inventory(harness: &Harness) {
    harness
        .runtime
        .monitor
        .inventory(true)
        .await
        .expect("refresh the Runtime inventory from temporary PostgreSQL");
}

async fn update_block(
    harness: &Harness,
    id: i64,
    blocked_at: Option<DateTime<Utc>>,
    reset_at: Option<DateTime<Utc>>,
) {
    sqlx::query(
        "UPDATE accounts SET rate_limited_at=$1,rate_limit_reset_at=$2,updated_at=$3 WHERE id=$4",
    )
    .bind(blocked_at)
    .bind(reset_at)
    .bind(harness.now())
    .bind(id)
    .execute(&harness.pool)
    .await
    .expect("update only the fixture account's synthetic rate block");
}

async fn update_passive_extra(
    harness: &Harness,
    id: i64,
    observed: DateTime<Utc>,
    five_reset: DateTime<Utc>,
    seven_reset: DateTime<Utc>,
) {
    let extra = passive_extra(observed, five_reset, seven_reset, 10, 20);
    sqlx::query("UPDATE accounts SET extra=$1,updated_at=$2 WHERE id=$3")
        .bind(SqlJson(extra))
        .bind(harness.now())
        .bind(id)
        .execute(&harness.pool)
        .await
        .expect("update only the fixture account's passive quota snapshot");
}

fn restore_moved_state(state: &std::path::Path, backup: &std::path::Path) {
    fs::remove_dir(state).expect("remove only the synthetic state-fault directory");
    fs::rename(backup, state).expect("restore the exact synthetic OAuth state file");
}

#[test]
fn pinned_manual_dispatch_fixture_matches_the_old_reference_scope() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/contracts/manual-dispatch/fixtures.json"
    ))
    .expect("pinned manual dispatch fixture must parse");
    assert_eq!(fixture["reference_sha"], REFERENCE_SHA);
    assert_eq!(fixture["scope"]["controls022"], 6);
    assert_eq!(fixture["scope"]["recovery_dispatch036"], 12);
    assert_eq!(fixture["scope"]["recovery019_monitor"], 5);
    assert_eq!(fixture["scope"]["superseded_desktop_lock"], 1);
}

#[tokio::test]
async fn manual_control_preempts_an_inflight_monitor_request_and_busy_runtime_locks() {
    // First reproduce the fixed-SHA manual-control assertion: hold both
    // synthetic account leases and the runtime configuration/test locks while
    // the real schedule endpoint persists and reads back two manual changes.
    let mut locked = Harness::new().await;
    let first = locked
        .seed_account("idle-lock-first", true, false, false)
        .await;
    let second = locked
        .seed_account("idle-lock-second", true, false, false)
        .await;
    let first_id = integer(&first["id"]).unwrap();
    let second_id = integer(&second["id"]).unwrap();
    let first_lease = locked.runtime.monitor.lease(&first).unwrap();
    let second_lease = locked.runtime.monitor.lease(&second).unwrap();
    let held_slot = locked
        .runtime
        .slots
        .acquire()
        .expect("hold one synthetic test slot");
    let config_guard = locked.runtime.config.lock.lock().await;

    for id in [first_id, second_id] {
        let response =
            tokio::time::timeout(StdDuration::from_secs(3), locked.set_schedulable(id, false))
                .await
                .expect(
                    "manual schedule mutation must ignore held account/config/fallback/test locks",
                )
                .expect("manual schedule mutation must verify through loopback readback");
        assert_eq!(response["verified"], true);
        assert_eq!(response["schedulable"], false);
        let control = locked.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]
            ["manual_control"]
            .clone();
        assert_eq!(control["generation"], 1);
        assert_eq!(control["enabled"], false);
    }
    assert_eq!(locked.mock.count("GET", "/usage"), 0);
    assert_eq!(locked.mock.count("POST", "/test"), 0);
    assert_eq!(locked.mock.count("POST", "/recover-state"), 0);
    assert_eq!(locked.mock.count("POST", "/schedulable"), 2);
    assert!(locked.mock.unexpected().is_empty());

    drop(config_guard);
    drop(held_slot);
    drop(second_lease);
    drop(first_lease);
    locked.close().await;

    // Then hold the real monitor cycle/account lease at an active HTTP request.
    // A manual close must still complete and invalidate that worker's result.
    let mut harness = Harness::new().await;
    let row = harness
        .seed_account("lock-priority", true, false, false)
        .await;
    let id = integer(&row["id"]).unwrap();
    let expected_inventory_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts WHERE deleted_at IS NULL AND lower(coalesce(platform,''))='openai' AND lower(coalesce(type,''))='oauth'",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("count the same eligible OAuth inventory before force refresh");
    let gate = Gate::new();
    harness.mock.set_usage_gate(id, gate.clone());

    // force_refresh holds the Runtime's monitor-cycle lock and the worker keeps
    // the real account lease while its loopback usage request is blocked.
    let monitor = harness.runtime.0.as_ref().unwrap().clone();
    let force = tokio::spawn(async move {
        monitor
            .monitor
            .force_refresh_with_timeout(StdDuration::from_secs(20))
            .await
    });
    tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
        .await
        .expect("manual-priority fixture must reach its loopback usage request");
    assert_eq!(harness.runtime.slots.active(), 0);
    let test_slot = harness
        .runtime
        .slots
        .acquire()
        .expect("hold the one test slot");
    let config_guard = harness.runtime.config.lock.lock().await;

    let response = tokio::time::timeout(
        StdDuration::from_secs(3),
        harness.set_schedulable(id, false),
    )
    .await
    .expect("manual control must not wait for the monitor cycle, account lease, config lock, fallback lock, or test slot")
    .expect("real Runtime manual schedule write must verify through loopback");
    assert_eq!(response["verified"], true);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    let control = harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]
        ["manual_control"]
        .clone();
    assert_eq!(control["generation"], 1);
    assert_eq!(control["enabled"], false);

    drop(config_guard);
    drop(test_slot);
    gate.release.notify_one();
    let report = tokio::time::timeout(StdDuration::from_secs(8), force)
        .await
        .expect("monitor force cycle must settle after the loopback release")
        .expect("force cycle task must join")
        .expect("force cycle should return a report");
    assert_eq!(
        report["total_count"].as_u64(),
        Some(expected_inventory_count as u64)
    );
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn manual_open_does_not_revive_old_recovery_after_restart_but_new_evidence_can_run() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let row = harness
        .seed_account("manual-open-restart", true, true, true)
        .await;
    let id = integer(&row["id"]).unwrap();
    configure_recovery(&harness, &[id], 1, 1, "connection").await;
    seed_intent(&harness, &row, "pending", now).await;
    sqlx::query("UPDATE accounts SET schedulable=false,updated_at=$1 WHERE id=$2")
        .bind(now)
        .bind(id)
        .execute(&harness.pool)
        .await
        .expect("prepare the old manual-unschedulable account after seeding its recovery intent");
    refresh_inventory(&harness).await;

    let opened = harness
        .set_schedulable(id, true)
        .await
        .expect("open the synthetic OAuth account through the real manual action");
    assert_eq!(opened["verified"], true);
    assert_eq!(harness.mock.count("POST", "/schedulable"), 1);
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"]
            ["status"],
        "cancelled"
    );

    harness.restart_runtime().await;
    harness.runtime.monitor.startup().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(harness.mock.count("GET", "/usage"), 0);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"]
            ["status"],
        "cancelled"
    );

    // A post-control passive observation is independent evidence. It may
    // create a new intent for the same still-blocked account after restart.
    harness.set_now(now + Duration::seconds(2));
    update_passive_extra(
        &harness,
        id,
        now + Duration::seconds(1),
        now + Duration::hours(4),
        now + Duration::days(6),
    )
    .await;
    refresh_inventory(&harness).await;
    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for_account(id, "POST", "/test", 1).await;
    let state = wait_for_recovered(&harness, id).await;
    assert_eq!(
        state["scheduler"][id.to_string()]["manual_control"]["generation"],
        1
    );
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 1);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn manual_close_during_dispatched_recovery_test_discards_late_result() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let row = harness
        .seed_account("manual-close-inflight-test", true, true, true)
        .await;
    let id = integer(&row["id"]).unwrap();
    configure_recovery(&harness, &[id], 1, 1, "connection").await;
    seed_intent(&harness, &row, "pending", now).await;
    let gate = Gate::new();
    harness.mock.set_test_gate(id, gate.clone());
    harness.runtime.monitor.startup().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
        .await
        .expect("recovery model test must be blocked at the loopback boundary");
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1);

    let closed = harness
        .set_schedulable(id, false)
        .await
        .expect("manual close must finish while verification is in flight");
    assert_eq!(closed["verified"], true);
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"]
            ["status"],
        "cancelled"
    );
    gate.release.notify_one();

    harness.runtime.monitor.drain(8).await;
    let state = harness.runtime.monitor.state().unwrap();
    assert_eq!(
        state["scheduler"][id.to_string()]["recovery_intent"]["status"],
        "cancelled"
    );
    assert!(state["recovery_history"].as_object().unwrap().is_empty());
    assert!(state["pending_events"].as_object().unwrap().is_empty());
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[test]
fn manual_generation_filters_stale_history_and_results_without_losing_query_budget() {
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../tests/contracts/manual-dispatch/fixtures.json"
    ))
    .unwrap();
    let now = parse_datetime(&fixture["reference_time"]).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let state =
        sub2ops_server::oauth_state::OAuthState::new(temp.path().join("oauth-state.json"), false);
    let mut row = fixture["manual_generation"]["row"].take();
    let id = integer(&row["id"]).unwrap();
    row["schedulable"] = true.into();
    state
        .transaction(|snapshot| {
            snapshot["scheduler"][id.to_string()]["quota_query"] = json!({
                "automatic_attempts":[datetime(now - Duration::minutes(10))],
                "last_query_at":datetime(now - Duration::minutes(10)),
                "last_source":"automatic"
            });
            Ok(())
        })
        .unwrap();
    let generation = state.manual_control(&row, false, now, "manual").unwrap();

    let mut update = Update::default();
    update
        .results
        .insert(id, json!({"template_type":"oauth","stale_result":true}));
    update
        .scheduler
        .insert(id, json!({"stale_scheduler_marker":true}));
    update.pending.insert(
        "fixture-late-event".into(),
        json!({"account_id":id,"status":"recovered"}),
    );
    update.history.insert(
        "fixture-late-history".into(),
        json!({"account_id":id,"status":"recovered"}),
    );
    update.generations = Some(BTreeMap::from([(id, generation - 1)]));
    state.commit(update).unwrap();

    let snapshot = state.snapshot().unwrap();
    assert_eq!(
        snapshot["scheduler"][id.to_string()]["manual_control"]["generation"],
        generation
    );
    assert_eq!(
        snapshot["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"],
        json!([datetime(now - Duration::minutes(10))])
    );
    assert!(snapshot["scheduler"][id.to_string()]
        .get("stale_scheduler_marker")
        .is_none());
    assert!(snapshot["oauth_results"].get(id.to_string()).is_none());
    assert!(snapshot["pending_events"].as_object().unwrap().is_empty());
    assert!(snapshot["recovery_history"].as_object().unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn failed_manual_control_persistence_never_sends_schedulable_http() {
    let mut harness = Harness::new().await;
    let row = harness
        .seed_account("manual-write-failure", false, false, false)
        .await;
    let id = integer(&row["id"]).unwrap();
    let state_path = harness.settings.usage_path.clone();
    let backup = harness._temp.path().join("oauth-state-preserved.json");
    fs::rename(&state_path, &backup).expect("move only the synthetic OAuth state file");
    fs::create_dir(&state_path)
        .expect("replace the synthetic OAuth state file with a read failure");

    let result = harness.set_schedulable(id, true).await;
    assert!(
        result.is_err(),
        "manual state persistence failure must abort the schedule request"
    );
    assert_eq!(harness.mock.count("POST", "/schedulable"), 0);

    fs::remove_dir(&state_path).expect("remove only the synthetic state-fault directory");
    fs::rename(&backup, &state_path).expect("restore the exact synthetic OAuth state file");
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[test]
fn reset_not_before_uses_exact_estimated_and_latest_window_deadlines() {
    let now = reference_time();
    let row = json!({
        "id":1,
        "platform":"openai",
        "type":"oauth",
        "status":"active",
        "schedulable":true,
        "credentials":{"plan_type":"plus","access_token":"fixture-token"},
        "extra":{},
        "rate_limited_at":null,
        "rate_limit_reset_at":null,
        "overload_until":null,
        "temp_unschedulable_until":null
    });

    let exact_at = now + Duration::seconds(17);
    let exact_data = usage_data(100, 0, exact_at, now + Duration::days(7));
    let exact_result = cached_result(&row, now, exact_data);
    assert_eq!(
        oauth_admission::reset_not_before(&row, &exact_result),
        Some(exact_at)
    );

    let estimated_data = json!({
        "five_hour":{"utilization":100,"remaining_seconds":17,"window_minutes":300},
        "seven_day":{"utilization":0,"resets_at":datetime(now + Duration::days(7)),"window_minutes":10080}
    });
    let estimated_result = cached_result(&row, now, estimated_data);
    assert_eq!(
        oauth_admission::reset_not_before(&row, &estimated_result),
        Some(now + Duration::seconds(77))
    );

    let mixed_data = json!({
        "five_hour":{"utilization":100,"remaining_seconds":17,"window_minutes":300},
        "seven_day":{"utilization":100,"resets_at":datetime(now + Duration::seconds(90)),"window_minutes":10080}
    });
    let mixed_result = cached_result(&row, now, mixed_data);
    assert_eq!(
        oauth_admission::reset_not_before(&row, &mixed_result),
        Some(now + Duration::seconds(90))
    );
}

#[tokio::test]
async fn dispatcher_waits_for_live_reset_deadline_and_uses_advanced_account_state() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let row = harness
        .seed_account("live-reset-deadline", true, true, false)
        .await;
    let id = integer(&row["id"]).unwrap();
    configure_recovery(&harness, &[id], 1, 1, "connection").await;
    let old_data = usage_data(100, 100, now - Duration::hours(2), now - Duration::hours(2));
    save_result(
        &harness,
        id,
        cached_result(&row, now - Duration::hours(2), old_data),
    )
    .await;
    update_block(
        &harness,
        id,
        Some(now - Duration::minutes(5)),
        Some(now + Duration::minutes(10)),
    )
    .await;
    harness.runtime.monitor.startup().await.unwrap();

    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 0);

    harness.set_now(now + Duration::minutes(10));
    update_block(
        &harness,
        id,
        Some(now - Duration::minutes(5)),
        Some(now + Duration::minutes(20)),
    )
    .await;
    refresh_inventory(&harness).await;
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 0);
    let waiting_intent = harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]
        ["recovery_intent"]
        .clone();
    assert_eq!(waiting_intent["waiting_reason"], "waiting_reset");

    update_block(
        &harness,
        id,
        Some(now - Duration::minutes(5)),
        Some(now + Duration::minutes(5)),
    )
    .await;
    refresh_inventory(&harness).await;
    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for_account(id, "GET", "/usage", 1).await;
    harness.runtime.monitor.drain(8).await;
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 1);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn passive_available_quota_dispatches_at_its_deadline_without_a_query_delay() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let row = harness
        .seed_account("passive-available-deadline", true, true, true)
        .await;
    let id = integer(&row["id"]).unwrap();
    configure_recovery(&harness, &[id], 1, 1, "connection").await;
    save_result(
        &harness,
        id,
        cached_result(
            &row,
            now,
            usage_data(10, 20, now + Duration::hours(4), now + Duration::days(6)),
        ),
    )
    .await;
    seed_intent(&harness, &row, "pending", now).await;
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]["recovery_intent"]["due_at"] = datetime(now).into();
            Ok(())
        })
        .unwrap();
    harness.runtime.monitor.startup().await.unwrap();

    harness.set_now(now - Duration::seconds(1));
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 0);

    let started = std::time::Instant::now();
    harness.set_now(now);
    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for_account(id, "POST", "/test", 1).await;
    wait_for_recovered(&harness, id).await;
    assert!(started.elapsed() < StdDuration::from_secs(2));
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 1);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn slow_quota_worker_does_not_block_cached_dispatch_or_duplicate_requests() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let slow = harness.seed_account("slow-quota", true, true, false).await;
    let fast = harness.seed_account("cached-quota", true, true, true).await;
    let slow_id = integer(&slow["id"]).unwrap();
    let fast_id = integer(&fast["id"]).unwrap();
    configure_recovery(&harness, &[slow_id, fast_id], 2, 2, "connection").await;
    save_result(
        &harness,
        slow_id,
        cached_result(
            &slow,
            now - Duration::hours(2),
            usage_data(100, 100, now - Duration::hours(2), now - Duration::hours(2)),
        ),
    )
    .await;
    seed_intent(&harness, &fast, "pending", now).await;
    let gate = Gate::new();
    harness.mock.set_usage_gate(slow_id, gate.clone());
    harness.runtime.monitor.startup().await.unwrap();

    harness.runtime.monitor.tick().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
        .await
        .expect("slow-account quota GET must remain blocked at loopback");
    harness
        .mock
        .wait_for_account(fast_id, "POST", "/test", 1)
        .await;
    assert_eq!(harness.mock.count_for(slow_id, "GET", "/usage"), 1);
    assert_eq!(harness.mock.count_for(fast_id, "POST", "/test"), 1);

    harness.set_now(now + Duration::seconds(1));
    harness.runtime.monitor.tick().await.unwrap();
    harness.set_now(now + Duration::seconds(2));
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(harness.mock.count_for(slow_id, "GET", "/usage"), 1);
    assert_eq!(harness.mock.count_for(fast_id, "POST", "/test"), 1);

    gate.release.notify_one();
    wait_for_recovered(&harness, slow_id).await;
    wait_for_recovered(&harness, fast_id).await;
    harness.runtime.monitor.drain(8).await;
    assert_eq!(harness.mock.count_for(slow_id, "GET", "/usage"), 1);
    assert_eq!(harness.mock.count_for(slow_id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(fast_id, "POST", "/test"), 1);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn query_http_capacity_is_global_and_rejected_workers_consume_no_query_budget() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let first = harness
        .seed_account("query-slot-first", true, true, false)
        .await;
    let second = harness
        .seed_account("query-slot-second", true, true, false)
        .await;
    let first_id = integer(&first["id"]).unwrap();
    let second_id = integer(&second["id"]).unwrap();
    configure_recovery(&harness, &[first_id, second_id], 1, 1, "connection").await;
    for (row, id) in [(&first, first_id), (&second, second_id)] {
        save_result(
            &harness,
            id,
            cached_result(
                row,
                now - Duration::hours(2),
                usage_data(100, 100, now - Duration::hours(2), now - Duration::hours(2)),
            ),
        )
        .await;
    }
    let first_gate = Gate::new();
    let second_gate = Gate::new();
    harness.mock.set_usage_gate(first_id, first_gate.clone());
    harness.mock.set_usage_gate(second_id, second_gate.clone());
    harness.runtime.monitor.startup().await.unwrap();
    let before = harness.runtime.monitor.state().unwrap();

    harness.runtime.monitor.tick().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if harness.mock.count("GET", "/usage") == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("one query must occupy the global Runtime quota slot");
    let first_active = if harness.mock.count_for(first_id, "GET", "/usage") == 1 {
        first_id
    } else {
        second_id
    };
    let second_waiting = if first_active == first_id {
        second_id
    } else {
        first_id
    };
    let gate = if first_active == first_id {
        &first_gate
    } else {
        &second_gate
    };
    tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
        .await
        .expect("the sole active query must reach its loopback gate");
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.max_active_usage.load(Ordering::Acquire), 1);

    harness.set_now(now + Duration::seconds(1));
    harness.runtime.monitor.tick().await.unwrap();
    let wait_state = wait_for_intent(&harness, second_waiting, |intent| {
        intent["waiting_reason"] == "query_slots"
    })
    .await;
    let before_budget = &before["scheduler"][second_waiting.to_string()]["quota_query"];
    let after_budget = &wait_state["scheduler"][second_waiting.to_string()]["quota_query"];
    assert_eq!(
        after_budget["last_query_at"],
        before_budget["last_query_at"]
    );
    assert_eq!(
        after_budget["automatic_attempts"],
        before_budget["automatic_attempts"]
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.max_active_usage.load(Ordering::Acquire), 1);

    gate.release.notify_one();
    wait_for_recovered(&harness, first_active).await;
    harness.runtime.monitor.tick().await.unwrap();
    let waiting_gate = if second_waiting == first_id {
        &first_gate
    } else {
        &second_gate
    };
    tokio::time::timeout(StdDuration::from_secs(8), waiting_gate.entered.notified())
        .await
        .expect("the account rejected only for slot capacity must retry after capacity frees");
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    assert_eq!(harness.mock.max_active_usage.load(Ordering::Acquire), 1);
    waiting_gate.release.notify_one();
    wait_for_recovered(&harness, second_waiting).await;
    harness.runtime.monitor.drain(8).await;
    assert_eq!(harness.mock.count("GET", "/usage"), 2);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn connection_test_http_capacity_is_global_across_recovery_workers() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let first = harness
        .seed_account("test-slot-first", true, true, true)
        .await;
    let second = harness
        .seed_account("test-slot-second", true, true, true)
        .await;
    let first_id = integer(&first["id"]).unwrap();
    let second_id = integer(&second["id"]).unwrap();
    configure_recovery(&harness, &[first_id, second_id], 2, 1, "connection").await;
    seed_intent(&harness, &first, "pending", now).await;
    seed_intent(&harness, &second, "pending", now).await;
    let first_gate = Gate::new();
    let second_gate = Gate::new();
    harness.mock.set_test_gate(first_id, first_gate.clone());
    harness.mock.set_test_gate(second_id, second_gate.clone());
    harness.runtime.monitor.startup().await.unwrap();

    harness.runtime.monitor.tick().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if harness.mock.count("POST", "/test") == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("one connection test must occupy the global test slot");
    let active_id = if harness.mock.count_for(first_id, "POST", "/test") == 1 {
        first_id
    } else {
        second_id
    };
    let waiting_id = if active_id == first_id {
        second_id
    } else {
        first_id
    };
    let gate = if active_id == first_id {
        &first_gate
    } else {
        &second_gate
    };
    tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
        .await
        .expect("the active recovery test must pause at loopback");
    assert_eq!(harness.mock.active_tests.load(Ordering::Acquire), 1);
    assert_eq!(harness.mock.max_active_tests.load(Ordering::Acquire), 1);

    harness.set_now(now + Duration::seconds(1));
    harness.runtime.monitor.tick().await.unwrap();
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert_eq!(harness.mock.max_active_tests.load(Ordering::Acquire), 1);
    assert_eq!(harness.mock.count_for(waiting_id, "GET", "/usage"), 0);

    gate.release.notify_one();
    wait_for_recovered(&harness, active_id).await;
    harness.runtime.monitor.tick().await.unwrap();
    let waiting_gate = if waiting_id == first_id {
        &first_gate
    } else {
        &second_gate
    };
    tokio::time::timeout(StdDuration::from_secs(8), waiting_gate.entered.notified())
        .await
        .expect("the deferred account must test once the shared slot becomes free");
    assert_eq!(harness.mock.count("POST", "/test"), 2);
    assert_eq!(harness.mock.max_active_tests.load(Ordering::Acquire), 1);
    waiting_gate.release.notify_one();
    wait_for_recovered(&harness, waiting_id).await;
    harness.runtime.monitor.drain(8).await;
    assert_eq!(harness.mock.count_for(first_id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(second_id, "POST", "/test"), 1);
    assert_eq!(harness.mock.max_active_tests.load(Ordering::Acquire), 1);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn confirmed_receipt_resumes_after_final_history_commit_failure_without_retest() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let row = harness
        .seed_account("final-history-write-failure", true, true, true)
        .await;
    let id = integer(&row["id"]).unwrap();
    configure_recovery(&harness, &[id], 1, 1, "connection").await;
    seed_intent(&harness, &row, "pending", now).await;
    let state_path = harness.settings.usage_path.clone();
    let backup = harness
        ._temp
        .path()
        .join("oauth-state-before-final-history.json");
    harness
        .mock
        .fail_final_state_write_after_recovery(state_path.clone(), backup.clone());
    harness.runtime.monitor.startup().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    harness
        .mock
        .wait_for_account(id, "POST", "/recover-state", 1)
        .await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if state_path.is_dir() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the loopback recovery response must inject the synthetic final-state write fault");
    harness.runtime.monitor.drain(8).await;
    assert_eq!(
        harness.runtime.monitor.row(id).await.unwrap()["rate_limited_at"],
        Value::Null
    );
    restore_moved_state(&state_path, &backup);

    harness.restart_runtime().await;
    harness.runtime.monitor.startup().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    let state = wait_for_recovered(&harness, id).await;
    assert_eq!(
        state["scheduler"][id.to_string()]["recovery_intent"]["status"],
        "recovered"
    );
    assert_eq!(state["recovery_history"].as_object().unwrap().len(), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 1);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn method_generation_change_blocks_completed_test_without_replay() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let row = harness
        .seed_account("method-generation-inflight", true, true, true)
        .await;
    let id = integer(&row["id"]).unwrap();
    configure_recovery(&harness, &[id], 1, 1, "connection").await;
    seed_intent(&harness, &row, "pending", now).await;
    let gate = Gate::new();
    harness.mock.set_test_gate(id, gate.clone());
    harness.runtime.monitor.startup().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
        .await
        .expect("the recovery test must be in flight before changing its method generation");

    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]["recovery_method_generation"] = 1.into();
            Ok(())
        })
        .unwrap();
    gate.release.notify_one();
    harness.runtime.monitor.drain(8).await;
    harness.runtime.monitor.reconcile_unknown(id).await.unwrap();
    let before_restart = harness.runtime.monitor.state().unwrap();
    let intent = &before_restart["scheduler"][id.to_string()]["recovery_intent"];
    assert_eq!(intent["status"], "blocked");
    assert_eq!(intent["last_error_code"], "verification_uncertain");
    assert!(intent["verification_receipt"].is_null());
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 0);

    harness.restart_runtime().await;
    harness.runtime.monitor.startup().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn busy_account_does_not_overwrite_cancelled_method_selection() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let row = harness
        .seed_account("busy-cancelled-method", true, true, true)
        .await;
    let id = integer(&row["id"]).unwrap();
    configure_recovery(&harness, &[id], 1, 1, "connection").await;
    seed_intent(&harness, &row, "pending", now).await;
    let state = harness.runtime.monitor.state().unwrap();
    let mut candidates = recovery_rules::candidates(
        std::slice::from_ref(&row),
        &state["oauth_results"],
        &state["scheduler"],
        now,
        true,
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    let candidate = candidates.remove(0);
    let lease = harness.runtime.monitor.lease(&row).unwrap();
    let snapshot = harness.runtime.config.snapshot(Some("oauth")).unwrap();
    harness
        .runtime
        .config
        .save(
            "oauth",
            &json!({
                "oauth_recovery_connection_account_ids":[],
                "oauth_recovery_model_account_ids":[id]
            }),
            "fixture-manual-dispatch",
            Some(snapshot["oauth"]["revision"].as_str().unwrap()),
        )
        .await
        .expect("change the synthetic account's selected verification method");
    let run = harness
        .runtime
        .monitor
        .run_item(candidate, Source::Automatic, true)
        .await
        .unwrap();
    assert_eq!(run["skipped"], true);
    let cancelled = harness.runtime.monitor.state().unwrap();
    assert_eq!(
        cancelled["scheduler"][id.to_string()]["recovery_intent"]["status"],
        "cancelled"
    );
    assert_eq!(
        cancelled["scheduler"][id.to_string()]["recovery_method_generation"],
        1
    );

    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 0);
    assert_eq!(
        harness.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"]
            ["status"],
        "cancelled"
    );
    assert!(harness.mock.unexpected().is_empty());
    drop(lease);
    harness.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn failed_test_receipt_persistence_is_uncertain_and_restart_does_not_replay() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let row = harness
        .seed_account("receipt-write-failure", true, true, true)
        .await;
    let id = integer(&row["id"]).unwrap();
    configure_recovery(&harness, &[id], 1, 1, "connection").await;
    seed_intent(&harness, &row, "pending", now).await;
    let state_path = harness.settings.usage_path.clone();
    let backup = harness
        ._temp
        .path()
        .join("oauth-state-before-test-receipt.json");
    harness
        .mock
        .fail_state_write_after_test(state_path.clone(), backup.clone());
    harness.runtime.monitor.startup().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    harness.mock.wait_for_account(id, "POST", "/test", 1).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            if state_path.is_dir() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("successful test must inject only the synthetic receipt-state failure");
    harness.runtime.monitor.drain(8).await;
    restore_moved_state(&state_path, &backup);

    harness.restart_runtime().await;
    harness.runtime.monitor.startup().await.unwrap();
    let state = harness.runtime.monitor.state().unwrap();
    let intent = &state["scheduler"][id.to_string()]["recovery_intent"];
    assert_eq!(intent["status"], "blocked");
    assert_eq!(intent["last_error_code"], "verification_uncertain");
    assert!(intent["verification_receipt"].is_null());
    harness.runtime.monitor.tick().await.unwrap();
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn new_block_or_manual_change_during_test_is_never_cleared() {
    let now = reference_time();
    for (name, mutation) in [
        ("new-block", TestMutation::NewRateLimit),
        ("manual-pause", TestMutation::ManualPause),
    ] {
        let mut harness = Harness::new().await;
        let row = harness.seed_account(name, true, true, true).await;
        let id = integer(&row["id"]).unwrap();
        configure_recovery(&harness, &[id], 1, 1, "connection").await;
        seed_intent(&harness, &row, "pending", now).await;
        let gate = Gate::new();
        harness.mock.set_test_gate(id, gate.clone());
        harness.mock.mutate_account_on_test(id, mutation);
        harness.runtime.monitor.startup().await.unwrap();
        harness.runtime.monitor.tick().await.unwrap();
        tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
            .await
            .expect("test request must be active before applying the synthetic state change");
        gate.release.notify_one();
        harness.runtime.monitor.drain(8).await;
        let after = harness.runtime.monitor.row(id).await.unwrap();
        assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1, "{name}");
        assert_eq!(
            harness.mock.count_for(id, "POST", "/recover-state"),
            0,
            "{name}"
        );
        match mutation {
            TestMutation::NewRateLimit => {
                assert!(after["rate_limited_at"].is_string());
                assert_eq!(
                    parse_datetime(&after["rate_limit_reset_at"]),
                    Some(now + Duration::hours(2))
                );
            }
            TestMutation::ManualPause => {
                assert_eq!(after["schedulable"], false);
                assert_eq!(after["temp_unschedulable_reason"], "manual pause");
            }
        }
        let state = harness.runtime.monitor.state().unwrap();
        assert!(state["recovery_history"].as_object().unwrap().is_empty());
        let events = state["pending_events"].as_object().unwrap();
        assert_eq!(events.len(), 1, "{name}");
        let event = events.values().next().unwrap();
        assert_eq!(event["status"], "recovery_failed", "{name}");
        assert_eq!(event["stage"], "recovery", "{name}");
        assert_eq!(event["error_code"], "recovery_state_changed", "{name}");
        assert_eq!(event["test_success"], true, "{name}");
        assert!(harness.mock.unexpected().is_empty());
        harness.close().await;
    }
}

#[tokio::test]
async fn new_rate_limit_after_recovered_intent_retests_from_passive_quota() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let row = harness
        .seed_account("new-rate-limit-after-recovery", true, true, true)
        .await;
    let id = integer(&row["id"]).unwrap();
    configure_recovery(&harness, &[id], 1, 1, "connection").await;
    seed_intent(&harness, &row, "recovered", now).await;
    // Keep the passive sample newer than both the new block and its reset boundary.
    update_block(
        &harness,
        id,
        Some(now - Duration::seconds(90)),
        Some(now - Duration::seconds(30)),
    )
    .await;
    update_passive_extra(
        &harness,
        id,
        now - Duration::seconds(10),
        now + Duration::hours(4),
        now + Duration::days(6),
    )
    .await;
    refresh_inventory(&harness).await;
    harness.runtime.monitor.startup().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    wait_for_recovered(&harness, id).await;

    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 1);
    let state = harness.runtime.monitor.state().unwrap();
    assert!(state["pending_events"]
        .as_object()
        .unwrap()
        .values()
        .any(|event| { event["account_id"] == id && event["status"] == "recovered" }));
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn failed_test_retries_after_one_minute_without_quota_cooldown_or_budget() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let row = harness
        .seed_account("one-minute-test-retry", true, true, true)
        .await;
    let id = integer(&row["id"]).unwrap();
    configure_recovery(&harness, &[id], 1, 1, "connection").await;
    seed_intent(&harness, &row, "pending", now).await;
    harness.mock.set_test_sse_sequence([
        "data: {\"type\":\"error\",\"code\":\"http_502\",\"message\":\"fixture test failure\"}\n\n"
            .to_owned(),
        SUCCESS_TEST_SSE.to_owned(),
    ]);
    harness.runtime.monitor.startup().await.unwrap();
    harness.runtime.monitor.tick().await.unwrap();
    // Wait for the stored HTTP failure, not an intermediate retry-shaped state.
    let failed = wait_for_intent(&harness, id, |intent| {
        intent["status"] == "retry" && intent["last_error_code"] == "http_502"
    })
    .await;
    let failed_intent = &failed["scheduler"][id.to_string()]["recovery_intent"];
    assert_eq!(failed_intent["last_error_code"], "http_502");
    assert_eq!(
        parse_datetime(&failed_intent["next_retry_at"]),
        Some(now + Duration::seconds(60))
    );
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 0);

    let attempts = (0..6)
        .map(|hours| datetime(now - Duration::hours(hours)))
        .collect::<Vec<_>>();
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]["quota_query"] = json!({
                "last_query_at":datetime(now),
                "last_source":"automatic",
                "automatic_attempts":attempts,
                "failure_count":0,
                "retry_at":""
            });
            Ok(())
        })
        .unwrap();
    harness.set_now(now + Duration::seconds(60));
    harness.runtime.monitor.tick().await.unwrap();
    let recovered = wait_for_recovered(&harness, id).await;
    let query = &recovered["scheduler"][id.to_string()]["quota_query"];
    assert_eq!(query["last_query_at"], datetime(now));
    assert_eq!(query["automatic_attempts"], json!(attempts));
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 2);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 1);
    assert_eq!(
        recovered["scheduler"][id.to_string()]["recovery_intent"]["status"],
        "recovered"
    );
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn passive_quota_crossing_reset_point_remains_reusable() {
    let mut harness = Harness::new().await;
    let now = harness.now();
    let initial = harness
        .seed_account("passive-quota-crossed-reset", true, true, true)
        .await;
    let id = integer(&initial["id"]).unwrap();
    update_passive_extra(
        &harness,
        id,
        now - Duration::seconds(30),
        now - Duration::minutes(2),
        now - Duration::minutes(2),
    )
    .await;
    let row = harness.runtime.monitor.row(id).await.unwrap();
    let result = quota::latest_result(&row, &Value::Null, now);
    let metadata = json!({
        "recovery_intent":{"due_at":datetime(now - Duration::minutes(1))}
    });
    assert_eq!(result["source"], "passive");
    assert!(recovery_rules::quota_ready(&row, &result, &metadata, now));
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 0);
    assert_eq!(harness.mock.count_for(id, "POST", "/recover-state"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn unknown_or_zero_reset_credit_skips_dispatcher_http_before_any_worker_request() {
    // Synthetic account-rate-limit evidence plus cached depleted quota drive
    // credit admission through the real Runtime tick without upstream HTTP.
    for (name, snapshot) in [
        ("credit-unknown", json!({})),
        ("credit-zero", json!({"available_count":0,"credits":[]})),
    ] {
        let mut harness = Harness::new().await;
        let now = harness.now();
        let row = harness.seed_account(name, true, true, false).await;
        let id = integer(&row["id"]).unwrap();
        configure_recovery(&harness, &[id], 1, 1, "connection").await;
        set_auto_reset_credit(&harness, true).await;
        let extra = json!({"codex_reset_credit_snapshot":snapshot});
        // Keep credit admission inside its five-minute consume window so its credit reason is observable.
        sqlx::query(
            "UPDATE accounts SET extra=$1,rate_limited_at=$2,rate_limit_reset_at=$3,updated_at=$4 WHERE id=$5",
        )
        .bind(SqlJson(extra))
        .bind(now - Duration::minutes(1))
        .bind(now + Duration::hours(1))
        .bind(now)
        .bind(id)
        .execute(&harness.pool)
        .await
        .unwrap();
        let row = harness.runtime.monitor.row(id).await.unwrap();
        save_result(
            &harness,
            id,
            cached_result(
                &row,
                now,
                usage_data(100, 100, now + Duration::hours(1), now + Duration::days(5)),
            ),
        )
        .await;
        harness.runtime.monitor.startup().await.unwrap();
        harness.runtime.monitor.tick().await.unwrap();
        harness.runtime.monitor.drain(8).await;
        let state = harness.runtime.monitor.state().unwrap();
        let expected_reason = if name == "credit-zero" {
            "no_credit"
        } else {
            "credit_unconfirmed"
        };
        assert_eq!(
            state["scheduler"][id.to_string()]["credit_admission"]["reason"],
            expected_reason,
            "{name}"
        );
        assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 0, "{name}");
        assert_eq!(harness.mock.count_for(id, "POST", "/test"), 0, "{name}");
        assert_eq!(
            harness.mock.count_for(id, "POST", "/recover-state"),
            0,
            "{name}"
        );
        assert!(harness.mock.unexpected().is_empty(), "{name}");
        harness.close().await;
    }
}

#[tokio::test]
async fn continuous_appointment_uses_real_runtime_quota_refresh_and_stops_independently_of_credit_policy(
) {
    let mut harness = Harness::new().await;
    let row = harness
        .seed_account("continuous-appointment", true, false, false)
        .await;
    let id = integer(&row["id"]).unwrap();
    let account = harness.runtime.actions.account(id, None).await.unwrap();
    let request = json!({"action":"test","payload":{"confirmed":true,"mode":"default","model_id":""},
        "repeat":"quota_cycle","expected_version":sub2ops_server::compat::operation_versions(&account)["test"],
        "request_id":"fixture-runtime-continuous-1","client_id":"fixture-runtime-cycle-client"});
    let job = harness
        .runtime
        .operations
        .submit(id, &request, ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(job["status"], "queued", "{}", job["reason"]);
    assert_eq!(job["requested"]["model_id"], "fixture-model");
    assert_eq!(
        parse_datetime(&job["scheduled_at"]).unwrap(),
        harness.now() + Duration::hours(4) + Duration::seconds(3)
    );
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 0);
    let at = harness.now();
    sqlx::query(
        "UPDATE accounts SET extra=$2,rate_limited_at=$3,rate_limit_reset_at=$4 WHERE id=$1",
    )
    .bind(id)
    .bind(SqlJson(passive_extra(
        at,
        at + Duration::hours(4),
        at + Duration::days(6),
        100,
        100,
    )))
    .bind(at - Duration::seconds(30))
    .bind(at + Duration::hours(4))
    .execute(&harness.pool)
    .await
    .unwrap();
    harness.runtime.operations.tick().await.unwrap();
    let stopped = harness
        .runtime
        .operations
        .get(job["id"].as_str().unwrap(), 0)
        .unwrap();
    assert_eq!(stopped["repeat_state"], "stopped");
    assert_eq!(stopped["status"], "cancelled");
    assert_eq!(harness.mock.count_for(id, "GET", "/usage"), 1);
    assert_eq!(harness.mock.count_for(id, "POST", "/test"), 0);
    assert_eq!(
        harness.runtime.config.current().unwrap()["oauth_auto_reset_credit_enabled"],
        false
    );
    let second = harness
        .seed_account("continuous-expired-auth", true, false, false)
        .await;
    let second_id = integer(&second["id"]).unwrap();
    let account = harness
        .runtime
        .actions
        .account(second_id, None)
        .await
        .unwrap();
    let mut request = request.clone();
    request["expected_version"] =
        sub2ops_server::compat::operation_versions(&account)["test"].clone();
    request["request_id"] = "fixture-runtime-continuous-2".into();
    let pending = harness
        .runtime
        .operations
        .submit(second_id, &request, ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(pending["status"], "queued");
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["scheduler"][second_id.to_string()]["quota_query"]["auth_fingerprint"] =
                oauth_admission::credential_fingerprint(&second).into();
            Ok(())
        })
        .unwrap();
    harness.runtime.operations.tick().await.unwrap();
    let blocked = harness
        .runtime
        .operations
        .get(pending["id"].as_str().unwrap(), 0)
        .unwrap();
    assert_eq!(blocked["repeat_state"], "needs_confirmation");
    assert!(blocked["repeat_reason"]
        .as_str()
        .unwrap()
        .contains("授权失效"));
    assert_eq!(harness.mock.count_for(second_id, "GET", "/usage"), 1);
    assert_eq!(harness.mock.count_for(second_id, "POST", "/test"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}
