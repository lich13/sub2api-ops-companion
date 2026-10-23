use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use sqlx::{types::Json as SqlJson, PgPool};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration as StdDuration, Instant as StdInstant},
};
use sub2ops_server::{
    account_locks::AccountLocks,
    account_templates::Templates,
    actions::Actions,
    bark::Bark,
    compat::{datetime, integer, parse_datetime},
    credit::{CreditController, Hooks},
    credit_ownership, credit_rules,
    db::Database,
    error::Result,
    execution::TestSlots,
    oauth_admission, oauth_native,
    oauth_queries::{Coordinator, QueryOptions, Source},
    oauth_state::{normalized_credits, OAuthState},
    queries::Queries,
    reset_credit_observation,
    settings::Settings,
    upstream::Upstream,
};
use tokio::{
    net::TcpListener,
    sync::{Notify, Semaphore},
    task::JoinHandle,
};

const ADMIN_KEY: &str = "fixture-admin-credit-contract";
const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(9_300_000);

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../tests/contracts/credit-fixtures.json"))
        .expect("pinned Python credit fixture must parse")
}

fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 30, 8, 0, 0).unwrap()
}

fn usage_data(at: DateTime<Utc>, used: f64) -> Value {
    json!({
        "five_hour": {
            "utilization": used,
            "resets_at": datetime(at + Duration::hours(5)),
            "remaining_seconds": 18000,
            "window_minutes": 300
        },
        "seven_day": {
            "utilization": used,
            "resets_at": datetime(at + Duration::days(5)),
            "remaining_seconds": 432000,
            "window_minutes": 10080
        }
    })
}

fn pinned_quota_data() -> Value {
    fixture()["quota_result"]["input"]["data"].clone()
}

#[derive(Clone)]
struct RecordedCall {
    method: Method,
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
    state_at_arrival: Option<Value>,
}

type RecoveryGate = (Arc<Notify>, Arc<Notify>);
type ScheduleGate = (bool, Arc<Notify>);

#[derive(Clone)]
struct JsonScenario {
    status: u16,
    body: Value,
}

#[derive(Clone)]
struct MockUpstream {
    pool: PgPool,
    calls: Arc<StdMutex<Vec<RecordedCall>>>,
    resets: Arc<StdMutex<BTreeMap<i64, JsonScenario>>>,
    recovery_statuses: Arc<StdMutex<BTreeMap<i64, u16>>>,
    sse: Arc<StdMutex<BTreeMap<i64, VecDeque<String>>>>,
    test_statuses: Arc<StdMutex<BTreeMap<i64, u16>>>,
    usages: Arc<StdMutex<BTreeMap<i64, JsonScenario>>>,
    usage_writeback: Arc<StdMutex<BTreeMap<i64, bool>>>,
    usage_credential_change: Arc<StdMutex<BTreeMap<i64, bool>>>,
    recovery_gate: Arc<StdMutex<Option<RecoveryGate>>>,
    schedule_gate: Arc<StdMutex<Option<ScheduleGate>>>,
    test_gate: Arc<StdMutex<Option<Arc<Notify>>>>,
    reset_gate: Arc<StdMutex<Option<Arc<Notify>>>>,
    push_gate: Arc<StdMutex<Option<Arc<Notify>>>>,
    bark_codes: Arc<StdMutex<VecDeque<i64>>>,
    state_path: Arc<StdMutex<Option<PathBuf>>>,
    trip_lock_after: Arc<StdMutex<Option<(Method, String)>>>,
    clock: Arc<StdMutex<DateTime<Utc>>>,
}

impl MockUpstream {
    fn new(pool: PgPool, clock: Arc<StdMutex<DateTime<Utc>>>) -> Self {
        Self {
            pool,
            calls: Arc::new(StdMutex::new(Vec::new())),
            resets: Arc::new(StdMutex::new(BTreeMap::new())),
            recovery_statuses: Arc::new(StdMutex::new(BTreeMap::new())),
            sse: Arc::new(StdMutex::new(BTreeMap::new())),
            test_statuses: Arc::new(StdMutex::new(BTreeMap::new())),
            usages: Arc::new(StdMutex::new(BTreeMap::new())),
            usage_writeback: Arc::new(StdMutex::new(BTreeMap::new())),
            usage_credential_change: Arc::new(StdMutex::new(BTreeMap::new())),
            recovery_gate: Arc::new(StdMutex::new(None)),
            schedule_gate: Arc::new(StdMutex::new(None)),
            test_gate: Arc::new(StdMutex::new(None)),
            reset_gate: Arc::new(StdMutex::new(None)),
            push_gate: Arc::new(StdMutex::new(None)),
            bark_codes: Arc::new(StdMutex::new(VecDeque::new())),
            state_path: Arc::new(StdMutex::new(None)),
            trip_lock_after: Arc::new(StdMutex::new(None)),
            clock,
        }
    }

    fn record(
        &self,
        method: Method,
        path: String,
        headers: BTreeMap<String, String>,
        body: Value,
        state_at_arrival: Option<Value>,
    ) {
        self.calls.lock().unwrap().push(RecordedCall {
            method,
            path,
            headers,
            body,
            state_at_arrival,
        });
    }

    fn count(&self, method: Method, suffix: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.method == method && call.path.ends_with(suffix))
            .count()
    }

    fn total(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn set_reset(&self, id: i64, status: u16, body: Value) {
        self.resets
            .lock()
            .unwrap()
            .insert(id, JsonScenario { status, body });
    }

    fn set_recovery_status(&self, id: i64, status: u16) {
        self.recovery_statuses.lock().unwrap().insert(id, status);
    }

    fn set_sse(&self, id: i64, payload: impl Into<String>) {
        self.set_sse_sequence(id, [payload.into()]);
    }

    fn set_sse_sequence<I, S>(&self, id: i64, payloads: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.sse
            .lock()
            .unwrap()
            .insert(id, payloads.into_iter().map(Into::into).collect());
    }

    fn set_test_status(&self, id: i64, status: u16) {
        self.test_statuses.lock().unwrap().insert(id, status);
    }

    fn set_usage(&self, id: i64, status: u16, body: Value) {
        self.usages
            .lock()
            .unwrap()
            .insert(id, JsonScenario { status, body });
    }

    fn set_usage_writeback(&self, id: i64, enabled: bool) {
        self.usage_writeback.lock().unwrap().insert(id, enabled);
    }

    fn set_usage_credential_change(&self, id: i64, enabled: bool) {
        self.usage_credential_change
            .lock()
            .unwrap()
            .insert(id, enabled);
    }

    fn set_recovery_gate(&self, entered: Arc<Notify>, release: Arc<Notify>) {
        *self.recovery_gate.lock().unwrap() = Some((entered, release));
    }

    fn set_schedule_gate(&self, enabled: bool, gate: Arc<Notify>) {
        *self.schedule_gate.lock().unwrap() = Some((enabled, gate));
    }

    fn release_schedule(&self) {
        if let Some((_, gate)) = self.schedule_gate.lock().unwrap().clone() {
            gate.notify_one();
        }
    }

    fn set_test_gate(&self, gate: Arc<Notify>) {
        *self.test_gate.lock().unwrap() = Some(gate);
    }

    fn release_test(&self) {
        if let Some(gate) = self.test_gate.lock().unwrap().clone() {
            gate.notify_one();
        }
    }

    fn set_reset_gate(&self, gate: Arc<Notify>) {
        *self.reset_gate.lock().unwrap() = Some(gate);
    }

    fn release_reset(&self) {
        if let Some(gate) = self.reset_gate.lock().unwrap().clone() {
            gate.notify_one();
        }
    }

    fn set_push_gate(&self, gate: Arc<Notify>) {
        *self.push_gate.lock().unwrap() = Some(gate);
    }

    fn release_push(&self) {
        if let Some(gate) = self.push_gate.lock().unwrap().clone() {
            gate.notify_one();
        }
    }

    fn set_state_path(&self, path: PathBuf) {
        *self.state_path.lock().unwrap() = Some(path);
    }

    fn state_snapshot(&self) -> Option<Value> {
        let path = self.state_path.lock().unwrap().clone()?;
        let bytes = fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn trip_lock_after(&self, method: Method, suffix: impl Into<String>) {
        *self.trip_lock_after.lock().unwrap() = Some((method, suffix.into()));
    }

    fn maybe_trip_state_lock(&self, method: &Method, path: &str) {
        let trip = self.trip_lock_after.lock().unwrap().clone();
        let Some((expected_method, suffix)) = trip else {
            return;
        };
        if expected_method != *method || !path.ends_with(&suffix) {
            return;
        }
        self.trip_lock_after.lock().unwrap().take();
        let state_path = self
            .state_path
            .lock()
            .unwrap()
            .clone()
            .expect("fixture state path must be configured before a lock failure");
        let lock_path = PathBuf::from(format!("{}.lock", state_path.display()));
        if lock_path.is_file() {
            fs::remove_file(&lock_path).expect("remove fixture state lock before write failure");
        }
        fs::create_dir(&lock_path).expect("make fixture state lock unwritable");
    }

    fn set_bark_codes(&self, codes: impl IntoIterator<Item = i64>) {
        *self.bark_codes.lock().unwrap() = codes.into_iter().collect();
    }

    fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }

    async fn wait_for(&self, method: Method, suffix: &str, count: usize) {
        tokio::time::timeout(StdDuration::from_secs(5), async {
            loop {
                if self.count(method.clone(), suffix) >= count {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("mock request did not arrive");
    }
}

fn path_id(path: &str, prefix: &str, suffix: &str) -> Option<i64> {
    path.strip_prefix(prefix)?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

fn json_response(status: u16, body: Value) -> Response {
    (
        StatusCode::from_u16(status).expect("fixture status code"),
        Json(body),
    )
        .into_response()
}

async fn mock_upstream(State(state): State<MockUpstream>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let method = parts.method;
    let path = parts.uri.path().to_owned();
    let bytes = to_bytes(body, 8_000_000).await.unwrap_or_default();
    let payload = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    let headers = parts
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect();
    state.record(
        method.clone(),
        path.clone(),
        headers,
        payload.clone(),
        state.state_snapshot(),
    );

    if method == Method::GET && path.ends_with("/usage") {
        let Some(id) = path_id(&path, "/api/v1/admin/accounts/", "/usage") else {
            return json_response(404, json!({"code": 1}));
        };
        let usage_scenario = state.usages.lock().unwrap().get(&id).cloned();
        if let Some(scenario) = usage_scenario {
            let data = &scenario.body["data"];
            let writeback = state
                .usage_writeback
                .lock()
                .unwrap()
                .get(&id)
                .copied()
                .unwrap_or(true);
            if writeback
                && (200..300).contains(&scenario.status)
                && !data["rate_limit_reset_credits"].is_null()
                && data["cache_persisted"] != false
            {
                let mut snapshot = data["rate_limit_reset_credits"].clone();
                if snapshot.is_object() && !data["fetched_at"].is_null() {
                    snapshot["fetched_at"] = data["fetched_at"].clone();
                }
                sqlx::query(
                    "UPDATE accounts SET extra=jsonb_set(COALESCE(extra,'{}'::jsonb),'{codex_reset_credit_snapshot}',$1,true),updated_at=$2 WHERE id=$3",
                )
                .bind(SqlJson(snapshot))
                .bind(state.now())
                .bind(id)
                .execute(&state.pool)
                .await
                .expect("persist only the synthetic usage-card fixture");
            }
            let credential_changed = state
                .usage_credential_change
                .lock()
                .unwrap()
                .get(&id)
                .copied()
                .unwrap_or(false);
            if credential_changed {
                sqlx::query(
                    "UPDATE accounts SET credentials=jsonb_set(COALESCE(credentials,'{}'::jsonb),'{access_token}',to_jsonb($1::text),true),updated_at=$2 WHERE id=$3",
                )
                .bind(ADMIN_KEY)
                .bind(state.now())
                .bind(id)
                .execute(&state.pool)
                .await
                .expect("apply synthetic credential fingerprint change");
            }
            state.maybe_trip_state_lock(&method, &path);
            return json_response(scenario.status, scenario.body);
        }
        let at = state.now();
        state.maybe_trip_state_lock(&method, &path);
        return json_response(200, json!({"code": 0, "data": usage_data(at, 20.0)}));
    }

    if method == Method::POST && path.ends_with("/schedulable") {
        let Some(id) = path_id(&path, "/api/v1/admin/accounts/", "/schedulable") else {
            return json_response(404, json!({"code": 1}));
        };
        let enabled = payload["schedulable"].as_bool().unwrap_or(false);
        let schedule_gate = {
            state
                .schedule_gate
                .lock()
                .unwrap()
                .as_ref()
                .filter(|(expected, _)| *expected == enabled)
                .map(|(_, gate)| gate.clone())
        };
        if let Some(gate) = schedule_gate {
            gate.notified().await;
        }
        let at = state.now();
        sqlx::query("UPDATE accounts SET schedulable=$1,updated_at=$2 WHERE id=$3")
            .bind(enabled)
            .bind(at)
            .bind(id)
            .execute(&state.pool)
            .await
            .expect("apply synthetic schedulable write to fixture PostgreSQL");
        state.maybe_trip_state_lock(&method, &path);
        return json_response(
            200,
            json!({"code": 0, "data": {"id": id, "schedulable": enabled}}),
        );
    }

    if method == Method::POST && path.ends_with("/reset-quota") {
        let Some(id) = path_id(&path, "/api/v1/admin/openai/accounts/", "/reset-quota") else {
            return json_response(404, json!({"code": 1}));
        };
        let gate = { state.reset_gate.lock().unwrap().clone() };
        if let Some(gate) = gate {
            gate.notified().await;
        }
        let scenario = state
            .resets
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or(JsonScenario {
                status: 200,
                body: json!({
                    "code": 0,
                    "data": {"code": "success", "windows_reset": 1, "credit": {"status": "redeemed"}}
                }),
            });
        if reset_credit_observation::consumption_result(&scenario.body["data"])["consumed"] == true
        {
            sqlx::query(
                "UPDATE accounts SET extra=jsonb_set(COALESCE(extra,'{}'::jsonb),'{codex_reset_credit_snapshot}',$1,true),updated_at=$2 WHERE id=$3",
            )
            .bind(SqlJson(json!({
                "available_count":0,
                "credits":[],
                "fetched_at":datetime(state.now())
            })))
            .bind(state.now())
            .bind(id)
            .execute(&state.pool)
            .await
            .expect("apply synthetic redeemed-card readback");
        }
        state.maybe_trip_state_lock(&method, &path);
        return json_response(scenario.status, scenario.body);
    }

    if method == Method::POST && path.ends_with("/test") {
        let Some(id) = path_id(&path, "/api/v1/admin/accounts/", "/test") else {
            return json_response(404, json!({"code": 1}));
        };
        let gate = { state.test_gate.lock().unwrap().clone() };
        if let Some(gate) = gate {
            gate.notified().await;
        }
        let status = state
            .test_statuses
            .lock()
            .unwrap()
            .remove(&id)
            .unwrap_or(200);
        if !(200..300).contains(&status) {
            state.maybe_trip_state_lock(&method, &path);
            return json_response(status, json!({"code":1,"message":"fixture-status"}));
        }
        let sse = state
            .sse
            .lock()
            .unwrap()
            .get_mut(&id)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| "data: {\"type\":\"test_complete\",\"success\":true}\n\n".into());
        state.maybe_trip_state_lock(&method, &path);
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(sse))
            .expect("fixture SSE response");
    }

    if method == Method::POST && path.ends_with("/recover-state") {
        let Some(id) = path_id(&path, "/api/v1/admin/accounts/", "/recover-state") else {
            return json_response(404, json!({"code": 1}));
        };
        let status = state
            .recovery_statuses
            .lock()
            .unwrap()
            .get(&id)
            .copied()
            .unwrap_or(200);
        if !(200..300).contains(&status) {
            state.maybe_trip_state_lock(&method, &path);
            return json_response(
                status,
                json!({"code":1,"message":"fixture recovery result unknown"}),
            );
        }
        sqlx::query(
            "UPDATE accounts SET status='active',rate_limited_at=NULL,rate_limit_reset_at=NULL,temp_unschedulable_until=NULL,temp_unschedulable_reason='',overload_until=NULL,error_message='',updated_at=$1 WHERE id=$2",
        )
        .bind(state.now())
        .bind(id)
        .execute(&state.pool)
        .await
        .expect("apply synthetic recovery readback");
        let recovery_gate = { state.recovery_gate.lock().unwrap().clone() };
        if let Some((entered, release)) = recovery_gate {
            entered.notify_one();
            release.notified().await;
        }
        state.maybe_trip_state_lock(&method, &path);
        return json_response(200, json!({"code":0,"data":{"success":true}}));
    }

    if method == Method::POST && path == "/push" {
        let gate = { state.push_gate.lock().unwrap().clone() };
        if let Some(gate) = gate {
            gate.notified().await;
        }
        let code = state.bark_codes.lock().unwrap().pop_front().unwrap_or(200);
        return json_response(200, json!({"code": code}));
    }

    json_response(404, json!({"code": 1, "message": "fixture-not-found"}))
}

struct HookShared {
    config: StdMutex<Value>,
    bump_generation: AtomicBool,
    manual_after_test: AtomicBool,
    change_method_after_test: AtomicBool,
    detection_held: AtomicBool,
    detection_version: StdMutex<Value>,
    fail_detection_version: AtomicBool,
    model_calls: AtomicUsize,
    connection_calls: AtomicUsize,
}

struct FixtureHooks {
    actions: Actions,
    state: OAuthState,
    shared: Arc<HookShared>,
    clock: Arc<StdMutex<DateTime<Utc>>>,
}

impl Hooks for FixtureHooks {
    fn config(&self) -> Result<Value> {
        Ok(self.shared.config.lock().unwrap().clone())
    }

    fn detection_held(&self, _id: i64) -> Result<bool> {
        Ok(self.shared.detection_held.load(Ordering::SeqCst))
    }

    fn detection_version(&self, _id: i64) -> Result<Value> {
        if self.shared.fail_detection_version.load(Ordering::SeqCst) {
            return Err(sub2ops_server::error::Error::new(
                503,
                "fixture detection state unavailable",
            ));
        }
        Ok(self.shared.detection_version.lock().unwrap().clone())
    }

    fn model_verify(
        &self,
        id: i64,
        _episode: String,
        attempt: i64,
        consume: bool,
    ) -> BoxFuture<'_, Result<Value>> {
        let state = self.state.clone();
        let shared = self.shared.clone();
        let clock = self.clock.clone();
        Box::pin(async move {
            shared.model_calls.fetch_add(1, Ordering::SeqCst);
            if !consume && shared.bump_generation.swap(false, Ordering::SeqCst) {
                let at = *clock.lock().unwrap();
                state.transaction(|document| {
                    let metadata = &mut document["scheduler"][id.to_string()];
                    if !metadata.is_object() {
                        *metadata = json!({});
                    }
                    let next = integer(&metadata["manual_control"]["generation"]).unwrap_or(0) + 1;
                    metadata["manual_control"] = json!({
                        "generation": next,
                        "enabled": false,
                        "at": datetime(at)
                    });
                    Ok(())
                })?;
            }
            Ok(json!({
                "success": true,
                "consumed": consume,
                "uncertain": false,
                "consumption_outcome": if consume {"consumed"} else {"not_applicable"},
                "attempt": attempt,
                "receipt": {"fixture": "model-verifier", "attempt": attempt, "consume": consume}
            }))
        })
    }

    fn cancel_verification(&self, _job_id: String, _episode: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn connection_test(
        &self,
        id: i64,
        model: String,
        _generation: i64,
        seconds: u64,
        _reserved: bool,
    ) -> BoxFuture<'_, Result<Value>> {
        let actions = self.actions.clone();
        let shared = self.shared.clone();
        let state = self.state.clone();
        let clock = self.clock.clone();
        Box::pin(async move {
            shared.connection_calls.fetch_add(1, Ordering::SeqCst);
            let result = oauth_native::test(&actions, id, &model, ADMIN_KEY, seconds).await;
            if shared
                .change_method_after_test
                .swap(false, Ordering::SeqCst)
            {
                let mut config = shared.config.lock().unwrap();
                config["oauth_recovery_connection_account_ids"] = json!([]);
                config["oauth_recovery_model_account_ids"] = json!([id]);
            }
            if shared.manual_after_test.swap(false, Ordering::SeqCst) {
                let row = credit_ownership::account(&actions.queries.db, id).await?;
                let at = *clock.lock().unwrap();
                state.manual_control(&row, false, at, "manual")?;
            }
            Ok(result)
        })
    }

    fn recover(&self, id: i64, key: String) -> BoxFuture<'_, Result<Value>> {
        let actions = self.actions.clone();
        Box::pin(async move { Ok(oauth_native::recover(&actions, id, &key, 30).await) })
    }
}

struct Harness {
    _temp: tempfile::TempDir,
    db: Database,
    settings: Arc<Settings>,
    upstream: Upstream,
    mock: MockUpstream,
    controller: CreditController,
    hooks: Arc<HookShared>,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    server: Option<JoinHandle<()>>,
}

impl Harness {
    async fn new(read_only: bool) -> Self {
        let database_url = std::env::var("DATABASE_URL")
            .expect("contract runner must provide temporary PostgreSQL");
        let db = Database::open(&database_url, false)
            .await
            .expect("connect to runner-owned PostgreSQL");
        let temp = tempfile::tempdir().expect("create test state directory");
        let clock = Arc::new(StdMutex::new(fixed_now()));
        let mock = MockUpstream::new(db.pool.clone(), clock.clone());
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback upstream fixture");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .fallback(mock_upstream)
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve loopback upstream fixture");
        });

        let state_path = temp.path().join("oauth-state.json");
        mock.set_state_path(state_path.clone());
        let connection_path = temp.path().join("sub2api-config.json");
        fs::write(
            &connection_path,
            serde_json::to_vec(&json!({"base_url": base_url, "verify_base_url": ""})).unwrap(),
        )
        .expect("write loopback upstream config");
        let settings = Arc::new(Settings {
            database_url,
            bind: "127.0.0.1:0".into(),
            base_path: String::new(),
            audit_path: temp.path().join("audit.jsonl"),
            usage_path: temp.path().join("usage-state.json"),
            oauth_path: state_path,
            bark_path: temp.path().join("bark-config.json"),
            connection_path,
            model_path: temp.path().join("group-models.json"),
            keepalive_path: temp.path().join("keepalive-state.json"),
            oauth: json!({}),
            bark: json!({
                "enabled": true,
                "device_key": "fixture-bark-device-key",
                "server_url": base_url
            }),
            bark_valid: true,
            base_url,
            verify_url: String::new(),
            read_only,
        });
        let upstream = Upstream::new(settings.clone()).expect("construct fixture upstream");
        let hooks = Arc::new(HookShared {
            config: StdMutex::new(json!({
                "oauth_auto_reset_credit_enabled": true,
                "oauth_recovery_monitor_enabled": true,
                "oauth_recovery_connection_account_ids": [],
                "oauth_recovery_model_account_ids": [],
                "oauth_recovery_test_model_id": "fixture-model"
            })),
            bump_generation: AtomicBool::new(false),
            manual_after_test: AtomicBool::new(false),
            change_method_after_test: AtomicBool::new(false),
            detection_held: AtomicBool::new(false),
            detection_version: StdMutex::new(json!("1".repeat(64))),
            fail_detection_version: AtomicBool::new(false),
            model_calls: AtomicUsize::new(0),
            connection_calls: AtomicUsize::new(0),
        });
        let oauth = OAuthState::new(settings.oauth_path.clone(), read_only);
        if !read_only {
            oauth
                .save_admin_token(ADMIN_KEY)
                .expect("save synthetic fixture admin key");
        }
        let controller =
            Self::make_controller(&db, &settings, &upstream, &mock, &hooks, &clock, oauth);
        Self {
            _temp: temp,
            db,
            settings,
            upstream,
            mock,
            controller,
            hooks,
            clock,
            server: Some(server),
        }
    }

    fn make_controller(
        db: &Database,
        settings: &Arc<Settings>,
        upstream: &Upstream,
        _mock: &MockUpstream,
        shared: &Arc<HookShared>,
        clock: &Arc<StdMutex<DateTime<Utc>>>,
        oauth: OAuthState,
    ) -> CreditController {
        let queries = Queries {
            db: db.clone(),
            oauth: oauth.clone(),
        };
        let actions = Actions::new(
            queries,
            upstream.clone(),
            AccountLocks::default(),
            Templates::new(settings.model_path.clone(), settings.read_only),
            settings.clone(),
        )
        .expect("construct fixture actions");
        let mut coordinator = Coordinator::new(actions.clone(), Arc::new(|| 4));
        let query_clock = clock.clone();
        coordinator.clock = Arc::new(move || *query_clock.lock().unwrap());
        let hooks: Arc<dyn Hooks> = Arc::new(FixtureHooks {
            actions: actions.clone(),
            state: oauth,
            shared: shared.clone(),
            clock: clock.clone(),
        });
        let mut controller = CreditController::new(
            actions,
            coordinator,
            hooks,
            TestSlots::new(Arc::new(|| 2)),
            Arc::new(Semaphore::new(2)),
        );
        let now = clock.clone();
        controller.clock = Arc::new(move || *now.lock().unwrap());
        controller
    }

    fn restart_controller(&self) -> CreditController {
        let oauth = OAuthState::new(self.settings.oauth_path.clone(), self.settings.read_only);
        Self::make_controller(
            &self.db,
            &self.settings,
            &self.upstream,
            &self.mock,
            &self.hooks,
            &self.clock,
            oauth,
        )
    }

    fn set_config(&self, value: Value) {
        *self.hooks.config.lock().unwrap() = value;
    }

    fn set_now(&self, at: DateTime<Utc>) {
        *self.clock.lock().unwrap() = at;
    }

    fn set_detection_version(&self, value: Value) {
        *self.hooks.detection_version.lock().unwrap() = value;
    }

    fn hold_detection(&self, held: bool) {
        self.hooks.detection_held.store(held, Ordering::SeqCst);
    }

    fn fail_detection_version(&self, fail: bool) {
        self.hooks
            .fail_detection_version
            .store(fail, Ordering::SeqCst);
    }

    async fn seed_account(&self, schedulable: bool, depleted: bool) -> Value {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        let now = fixed_now();
        let limited_at = depleted.then_some(now - Duration::hours(1));
        let reset_at = depleted.then_some(now + Duration::days(5));
        let snapshot = json!({
            "available_count": 1,
            "credits": [{"expires_at": datetime(now + Duration::days(2))}]
        });
        let extra = json!({"codex_reset_credit_snapshot": snapshot});
        sqlx::query(
            "INSERT INTO accounts \
             (id,name,platform,type,status,schedulable,credentials,extra,rate_limited_at,rate_limit_reset_at,updated_at) \
             VALUES ($1,$2,'openai','oauth','active',$3,$4,$5,$6,$7,$8)",
        )
        .bind(id)
        .bind(format!("fixture-credit-account-{id}"))
        .bind(schedulable)
        .bind(SqlJson(json!({"plan_type": "plus"})))
        .bind(SqlJson(extra))
        .bind(limited_at)
        .bind(reset_at)
        .bind(now - Duration::minutes(1))
        .execute(&self.db.pool)
        .await
        .expect("insert synthetic OAuth account");
        credit_ownership::account(&self.db, id)
            .await
            .expect("read synthetic OAuth account")
    }

    async fn seed_depletion(&self, controller: &CreditController) -> (Value, Value) {
        let row = self.seed_account(true, true).await;
        let id = integer(&row["id"]).unwrap();
        // The fixed legacy fixture leaves both account-id lists unset, which
        // selects the compatibility connection-recovery policy by default.
        self.set_config(recovery_config(id, "connection"));
        let now = fixed_now();
        let mut data = pinned_quota_data();
        data["fetched_at"] = json!(datetime(now));
        data["rate_limit"]["primary_window"]["used_percent"] = json!(100);
        data["rate_limit"]["secondary_window"]["used_percent"] = json!(100);
        let mut result =
            credit_rules::quota_result(&data, &row, now, Some(now - Duration::seconds(2)));
        result["data"] = data;
        controller
            .store
            .transaction(|state| {
                state["oauth_results"][id.to_string()] = result.clone();
                Ok(())
            })
            .expect("persist synthetic quota result");

        let saved = controller
            .saved_quota(&row, now)
            .expect("project synthetic quota result");
        let reset = parse_datetime(&saved["oauth_quota"]["ui_windows"][1]["reset_at"])
            .expect("fixture seven day reset");
        let credits = normalized_credits(&row["extra"]["codex_reset_credit_snapshot"])
            .expect("normalize synthetic credit snapshot");
        let observation = reset_credit_observation::create(
            &row,
            &credits,
            now - Duration::seconds(1),
            &oauth_admission::credential_fingerprint(&row),
            reset,
            &json!(10080),
        )
        .expect("create fixture credit observation");
        controller
            .store
            .transaction(|state| {
                state["scheduler"][id.to_string()]["reset_credit_observation"] =
                    observation.clone();
                Ok(())
            })
            .expect("persist synthetic credit observation");
        sqlx::query(
            "INSERT INTO ops_error_logs \
             (id,account_id,created_at,platform,error_owner,error_phase,status_code,upstream_status_code,error_message,upstream_error_message) \
             VALUES ($1,$2,$3,'openai','provider','upstream',429,429,'fixture-upstream-429','fixture-upstream-429')",
        )
        .bind(id + 1_000_000_000)
        .bind(id)
        .bind(now - Duration::seconds(60))
        .execute(&self.db.pool)
        .await
        .expect("insert synthetic upstream 429 evidence");
        let evidence = controller
            .depletion(&row, now, false)
            .await
            .expect("derive synthetic depletion evidence");
        assert!(
            evidence.is_object(),
            "fixture must satisfy depletion preconditions"
        );
        (row, evidence)
    }

    async fn seed_notice(&self, id: i64, episode: &str, at: DateTime<Utc>) {
        let notice = json!({
            "episode": episode,
            "account_id": id,
            "account_name": format!("fixture-credit-account-{id}"),
            "outcome": "unknown",
            "reason": "fixture reason",
            "reason_code": "result_uncertain",
            "completed_at": datetime(at),
            "status": "pending",
            "attempts": 0,
            "next_at": datetime(at)
        });
        self.controller
            .store
            .transaction(|state| {
                state["scheduler"][id.to_string()]["credit_result_notifications"][episode] =
                    notice.clone();
                Ok(())
            })
            .expect("persist synthetic Bark notification");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

fn recovery_config(id: i64, method: &str) -> Value {
    json!({
        "oauth_auto_reset_credit_enabled": true,
        "oauth_recovery_monitor_enabled": true,
        "oauth_recovery_connection_account_ids": if method == "connection" {json!([id])} else {json!([])},
        "oauth_recovery_model_account_ids": if method == "model" {json!([id])} else {json!([])},
        "oauth_recovery_test_model_id": "fixture-model"
    })
}

fn native_quota_data(at: DateTime<Utc>, five: f64, seven: f64) -> Value {
    let mut data = usage_data(at, seven);
    data["five_hour"]["utilization"] = json!(five);
    data["fetched_at"] = json!(datetime(at));
    data
}

fn reset_response(at: DateTime<Utc>, five: f64, seven: f64, code: &str, windows: Value) -> Value {
    json!({
        "code": 0,
        "data": {
            "code": code,
            "windows_reset": windows,
            "credit": {"status": if code == "success" {"redeemed"} else {"available"}},
            "quota": native_quota_data(at, five, seven),
            "cache_refreshed": true,
            "cache_persisted": true
        }
    })
}

fn explicit_sse_error(code: &str) -> String {
    format!(
        "data: {{\"type\":\"error\",\"code\":\"{code}\",\"message\":\"fixture verification failure\"}}\n\n"
    )
}

fn successful_sse() -> String {
    "data: {\"type\":\"test_start\",\"model\":\"fixture-model\"}\n\ndata: {\"type\":\"test_complete\",\"success\":true}\n\n".into()
}

fn snapshot(count: i64, expiry: DateTime<Utc>, fetched_at: DateTime<Utc>) -> Value {
    let cards: Vec<Value> = (0..count.max(0))
        .map(|_| json!({"expires_at":datetime(expiry)}))
        .collect();
    json!({
        "available_count": count,
        "fetched_at": datetime(fetched_at),
        "credits": cards
    })
}

fn usage_response(
    at: DateTime<Utc>,
    five: f64,
    seven: f64,
    card: Option<Value>,
    fetched_at: DateTime<Utc>,
    cache_persisted: bool,
) -> Value {
    let mut data = native_quota_data(at, five, seven);
    data["fetched_at"] = json!(datetime(fetched_at));
    data["cache_persisted"] = json!(cache_persisted);
    if let Some(card) = card {
        data["rate_limit_reset_credits"] = card;
    }
    json!({"code":0,"data":data})
}

async fn reload_account(harness: &Harness, id: i64) -> Value {
    credit_ownership::account(&harness.db, id)
        .await
        .expect("reload synthetic account from PostgreSQL")
}

async fn set_credit_snapshot(harness: &Harness, id: i64, value: Value) -> Value {
    sqlx::query(
        "UPDATE accounts SET extra=jsonb_set(COALESCE(extra,'{}'::jsonb),'{codex_reset_credit_snapshot}',$1,true) WHERE id=$2",
    )
    .bind(SqlJson(value))
    .bind(id)
    .execute(&harness.db.pool)
    .await
    .expect("update synthetic card snapshot");
    reload_account(harness, id).await
}

fn reset_stored_quota(controller: &CreditController, row: &Value, at: DateTime<Utc>, data: Value) {
    let mut result = credit_rules::quota_result(&data, row, at, Some(at - Duration::seconds(2)));
    result["data"] = data;
    controller
        .store
        .transaction(|state| {
            state["oauth_results"][integer(&row["id"]).unwrap().to_string()] = result.clone();
            Ok(())
        })
        .expect("persist synthetic quota observation");
}

async fn insert_error(
    harness: &Harness,
    id: i64,
    event_id: i64,
    at: DateTime<Utc>,
    status: i32,
    owner: &str,
    phase: &str,
) {
    sqlx::query(
        "INSERT INTO ops_error_logs (id,account_id,created_at,platform,error_owner,error_phase,status_code,upstream_status_code,error_message,upstream_error_message) VALUES ($1,$2,$3,'openai',$4,$5,$6,$6,'fixture-error','fixture-error')",
    )
    .bind(event_id)
    .bind(id)
    .bind(at)
    .bind(owner)
    .bind(phase)
    .bind(status)
    .execute(&harness.db.pool)
    .await
    .expect("insert synthetic error evidence");
}

async fn run_automatic(harness: &Harness, row: &Value, at: DateTime<Utc>) {
    harness
        .controller
        .run_account(row, at)
        .await
        .expect("run the real credit controller on synthetic PostgreSQL evidence");
}

fn bark_calls(harness: &Harness) -> Vec<RecordedCall> {
    harness
        .mock
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.method == Method::POST && call.path == "/push")
        .cloned()
        .collect()
}

#[test]
fn legacy_python_oracle_and_migration_fragment_cover_the_fixed_scope() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../tests/contracts/credit-lifecycle/fixtures.json"
    ))
    .expect("fixed-SHA legacy oracle fixture must parse");
    let migration: Value = serde_json::from_str(include_str!(
        "../../tests/contracts/credit-lifecycle/migration-fragment.json"
    ))
    .expect("credit lifecycle migration fragment must parse");
    assert_eq!(oracle["reference_sha"], REFERENCE_SHA);
    assert_eq!(oracle["legacy_run"]["status"], "passed");
    assert_eq!(oracle["legacy_run"]["selected"], 100);
    assert_eq!(oracle["legacy_run"]["passed"], 100);
    assert_eq!(oracle["legacy_run"]["failed"], 0);
    assert_eq!(oracle["legacy_test_ids"].as_array().unwrap().len(), 100);
    assert_eq!(migration["reference_sha"], REFERENCE_SHA);
    assert_eq!(migration["mappings"].as_array().unwrap().len(), 100);
    let mapped = migration["mappings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["status"] == "mapped")
        .count();
    assert_eq!(mapped, 100);
    assert_eq!(
        migration["mappings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["legacy_test_id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        100
    );
}

#[tokio::test]
async fn account_and_live_window_preconditions_block_every_native_action() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let cases = [
        "platform",
        "type",
        "status",
        "unschedulable",
        "deleted",
        "parent_column",
        "parent_extra",
        "missing_limit_reset",
        "current_limit_reset",
        "missing_limited_at",
        "manual_pause_reason",
        "expired_account",
    ];
    for case in cases {
        let (mut row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        harness.set_config(recovery_config(id, "connection"));
        match case {
            "platform" => {
                sqlx::query("UPDATE accounts SET platform='grok' WHERE id=$1")
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "type" => {
                sqlx::query("UPDATE accounts SET type='apikey' WHERE id=$1")
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "status" => {
                sqlx::query("UPDATE accounts SET status='error' WHERE id=$1")
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "unschedulable" => {
                sqlx::query("UPDATE accounts SET schedulable=false WHERE id=$1")
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "deleted" => {
                sqlx::query("UPDATE accounts SET deleted_at=$1 WHERE id=$2")
                    .bind(now)
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
                row["deleted_at"] = json!(datetime(now));
            }
            "parent_column" => {
                sqlx::query("UPDATE accounts SET parent_account_id=8 WHERE id=$1")
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "parent_extra" => {
                sqlx::query(
                    "UPDATE accounts SET extra=jsonb_set(COALESCE(extra,'{}'::jsonb),'{parent_account_id}','8'::jsonb,true) WHERE id=$1",
                )
                .bind(id)
                .execute(&harness.db.pool)
                .await
                .unwrap();
            }
            "missing_limit_reset" => {
                sqlx::query("UPDATE accounts SET rate_limit_reset_at=NULL WHERE id=$1")
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "current_limit_reset" => {
                sqlx::query("UPDATE accounts SET rate_limit_reset_at=$1 WHERE id=$2")
                    .bind(now)
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "missing_limited_at" => {
                sqlx::query("UPDATE accounts SET rate_limited_at=NULL WHERE id=$1")
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "manual_pause_reason" => {
                sqlx::query(
                    "UPDATE accounts SET temp_unschedulable_reason='manual pause' WHERE id=$1",
                )
                .bind(id)
                .execute(&harness.db.pool)
                .await
                .unwrap();
            }
            "expired_account" => {
                sqlx::query(
                    "UPDATE accounts SET expires_at=$1,auto_pause_on_expired=true WHERE id=$2",
                )
                .bind(now)
                .bind(id)
                .execute(&harness.db.pool)
                .await
                .unwrap();
            }
            _ => unreachable!(),
        }
        if case != "deleted" {
            row = reload_account(&harness, id).await;
        }
        let before = harness.mock.total();
        run_automatic(&harness, &row, now).await;
        assert_eq!(harness.mock.total(), before, "unexpected I/O for {case}");
        assert!(
            harness
                .controller
                .state(id)
                .unwrap()
                .as_object()
                .unwrap()
                .is_empty(),
            "task for {case}"
        );
    }
}

#[tokio::test]
async fn raw_weekly_exhaustion_and_observation_time_are_required() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    for (weekly, expected) in [
        (99.99, false),
        (99.9999, false),
        (100.0, true),
        (100.01, true),
    ] {
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        let data = native_quota_data(now, 100.0, weekly);
        reset_stored_quota(&harness.controller, &row, now, data.clone());
        let evidence = harness
            .controller
            .depletion(&row, now, false)
            .await
            .unwrap();
        assert_eq!(evidence.is_object(), expected, "weekly={weekly}");
        if !expected {
            let before = harness.mock.total();
            harness.set_config(recovery_config(id, "connection"));
            run_automatic(&harness, &row, now).await;
            assert_eq!(
                harness.mock.total(),
                before,
                "unexpected I/O for weekly={weekly}"
            );
        }
    }

    for (name, observed, reset_delta, weekly) in [
        ("missing", Some(now), Some(5), Value::Null),
        ("below", Some(now), Some(5), json!(0)),
        ("expired_reset", Some(now), Some(0), json!(100)),
        ("invalid_reset", Some(now), None, json!(100)),
        ("old", Some(now - Duration::days(8)), Some(5), json!(100)),
        (
            "future",
            Some(now + Duration::seconds(1)),
            Some(5),
            json!(100),
        ),
    ] {
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        let mut data = native_quota_data(now, 100.0, weekly.as_f64().unwrap_or(100.0));
        if let Some(observed) = observed {
            data["fetched_at"] = json!(datetime(observed));
        } else {
            data["fetched_at"] = Value::Null;
        }
        if let Some(reset_delta) = reset_delta {
            let reset = now + Duration::days(reset_delta);
            data["seven_day"]["resets_at"] = json!(datetime(reset));
        } else {
            data["seven_day"]["resets_at"] = json!("fixture-invalid-time");
            data["seven_day"]["remaining_seconds"] = Value::Null;
        }
        if name == "missing" {
            data["seven_day"]["utilization"] = Value::Null;
        }
        reset_stored_quota(&harness.controller, &row, now, data);
        let evidence = harness
            .controller
            .depletion(&row, now, false)
            .await
            .unwrap();
        assert!(!evidence.is_object(), "invalid weekly fixture {name}");
        let before = harness.mock.total();
        harness.set_config(recovery_config(id, "connection"));
        run_automatic(&harness, &row, now).await;
        assert_eq!(
            harness.mock.total(),
            before,
            "unexpected I/O for invalid weekly fixture {name}"
        );
    }
}

#[tokio::test]
async fn fixed_five_minute_admission_boundary_and_clock_rollback_never_extend_dispatch() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    for (offset, dispatches) in [(299, 1), (300, 0)] {
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        harness.set_config(recovery_config(id, "connection"));
        harness.mock.set_sse(id, successful_sse());
        let evidence = harness
            .controller
            .depletion(&row, now, false)
            .await
            .unwrap();
        let trigger = parse_datetime(&evidence["evidence_at"]).unwrap();
        let at = trigger + Duration::seconds(offset);
        harness.set_now(at);
        harness
            .mock
            .set_reset(id, 200, reset_response(at, 20.0, 20.0, "success", json!(1)));
        let before = harness.mock.count(Method::POST, "/reset-quota");
        run_automatic(&harness, &row, at).await;
        assert_eq!(
            harness.mock.count(Method::POST, "/reset-quota") - before,
            dispatches,
            "trigger offset {offset}"
        );
        if offset == 299 {
            assert_eq!(harness.controller.state(id).unwrap()["stage"], "recovered");
        } else {
            let state = harness.controller.store.snapshot().unwrap();
            assert!(state["scheduler"][id.to_string()]["auto_reset_credit"].is_null());
            assert_eq!(
                state["scheduler"][id.to_string()]["credit_admission"]["reason"],
                "consume_deadline"
            );
        }
    }

    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    let evidence = harness
        .controller
        .depletion(&row, now, false)
        .await
        .unwrap();
    let trigger = parse_datetime(&evidence["evidence_at"]).unwrap();
    let no_card =
        set_credit_snapshot(&harness, id, snapshot(0, now + Duration::days(2), now)).await;
    run_automatic(&harness, &no_card, now).await;
    let initial_admission = harness.controller.store.snapshot().unwrap()["scheduler"]
        [id.to_string()]["credit_admission"]
        .clone();
    assert_eq!(initial_admission["reason"], "no_credit");
    assert_eq!(initial_admission["trigger_at"], datetime(trigger));

    let observed_at = now + Duration::seconds(60);
    let card = snapshot(1, observed_at + Duration::days(2), observed_at);
    harness.mock.set_usage(
        id,
        200,
        usage_response(observed_at, 100.0, 100.0, Some(card), observed_at, true),
    );
    harness.set_now(observed_at);
    let query = harness
        .controller
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Manual,
                reason: "fixture-card-arrived-before-deadline".into(),
                at: observed_at,
                timeout: 3,
                ..QueryOptions::default()
            },
        )
        .await;
    assert_eq!(query["success"], true, "{query}");
    let observed_row = reload_account(&harness, id).await;
    harness.mock.set_reset(
        id,
        200,
        reset_response(observed_at, 20.0, 20.0, "success", json!(1)),
    );
    run_automatic(&harness, &observed_row, observed_at).await;
    let admitted = harness.controller.state(id).unwrap();
    assert_eq!(admitted["stage"], "recovered", "{admitted}");
    assert_eq!(admitted["trigger_at"], datetime(trigger));
    assert_eq!(
        admitted["consume_deadline_at"],
        datetime(trigger + Duration::seconds(300))
    );
    assert_eq!(harness.mock.count(Method::GET, &format!("/{id}/usage")), 1);
    assert_eq!(
        harness
            .mock
            .count(Method::POST, &format!("/{id}/reset-quota")),
        1
    );
    assert_eq!(harness.mock.count(Method::POST, &format!("/{id}/test")), 1);

    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    let evidence = harness
        .controller
        .depletion(&row, now, false)
        .await
        .unwrap();
    let trigger = parse_datetime(&evidence["evidence_at"]).unwrap();
    let at_deadline = trigger + Duration::seconds(301);
    let zero = set_credit_snapshot(&harness, id, snapshot(0, at_deadline, now)).await;
    harness.set_now(at_deadline);
    run_automatic(&harness, &zero, at_deadline).await;
    assert_eq!(
        harness.controller.store.snapshot().unwrap()["scheduler"][id.to_string()]
            ["credit_admission"]["reason"],
        "consume_deadline"
    );
    let rolled_back = trigger + Duration::seconds(299);
    let positive = set_credit_snapshot(
        &harness,
        id,
        snapshot(1, now + Duration::days(2), rolled_back),
    )
    .await;
    harness.set_now(rolled_back);
    let reset_before = harness.mock.count(Method::POST, "/reset-quota");
    run_automatic(&harness, &positive, rolled_back).await;
    assert_eq!(
        harness.mock.count(Method::POST, "/reset-quota"),
        reset_before
    );
    let state = harness.controller.store.snapshot().unwrap();
    assert_eq!(
        state["scheduler"][id.to_string()]["credit_admission"]["trigger_at"],
        datetime(trigger)
    );
    assert_eq!(
        state["scheduler"][id.to_string()]["credit_admission"]["reason"],
        "clock_untrusted"
    );
    assert_eq!(
        state["scheduler"][id.to_string()]["credit_admission"]["checked_at"],
        datetime(at_deadline)
    );
}

#[tokio::test]
async fn card_observed_at_the_weekly_window_start_can_be_consumed_without_refresh() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    let window_start = now - Duration::days(2);
    let row = set_credit_snapshot(
        &harness,
        id,
        snapshot(1, now + Duration::days(2), window_start),
    )
    .await;
    harness
        .controller
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]["reset_credit_observation"]["observed_at"] =
                json!(datetime(window_start));
            Ok(())
        })
        .unwrap();
    harness.set_config(recovery_config(id, "connection"));
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse(id, successful_sse());
    run_automatic(&harness, &row, now).await;
    assert_eq!(harness.controller.state(id).unwrap()["stage"], "recovered");
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
}

#[tokio::test]
async fn provider_error_and_account_fallback_evidence_are_distinct_and_redacted() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let (runtime_row, _) = harness.seed_depletion(&harness.controller).await;
    let runtime_id = integer(&runtime_row["id"]).unwrap();
    harness.set_config(recovery_config(runtime_id, "connection"));
    run_automatic(&harness, &runtime_row, now).await;
    let runtime_task = harness.controller.state(runtime_id).unwrap();
    assert_eq!(runtime_task["evidence_source"], "upstream_error");
    assert_eq!(
        runtime_task["evidence_id"],
        (runtime_id + 1_000_000_000).to_string()
    );
    assert!(runtime_task["evidence_fingerprint"].as_str().unwrap().len() == 64);
    assert!(!runtime_task.to_string().contains(ADMIN_KEY));
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);

    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    let first = harness
        .controller
        .depletion(&row, now, false)
        .await
        .unwrap();
    assert_eq!(first["evidence_source"], "upstream_error");
    assert_eq!(first["evidence_id"], (id + 1_000_000_000).to_string());
    assert!(first["evidence_fingerprint"].as_str().unwrap().len() == 64);
    assert!(!first.to_string().contains(ADMIN_KEY));

    sqlx::query("DELETE FROM ops_error_logs WHERE account_id=$1")
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    let fallback = harness
        .controller
        .depletion(&row, now, false)
        .await
        .unwrap();
    assert_eq!(fallback["evidence_source"], "account_rate_limit");
    assert!(fallback["evidence_id"]
        .as_str()
        .unwrap()
        .starts_with("account-rate-limit:"));
    assert!(fallback["evidence_fingerprint"].as_str().unwrap().len() == 64);

    insert_error(
        &harness,
        id,
        id + 1_000_000_100,
        now - Duration::seconds(30),
        429,
        "client",
        "request",
    )
    .await;
    assert!(harness
        .controller
        .depletion(&row, now, false)
        .await
        .unwrap()
        .is_null());

    sqlx::query("DELETE FROM ops_error_logs WHERE account_id=$1")
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    insert_error(
        &harness,
        id,
        id + 1_000_000_101,
        now - Duration::seconds(60),
        429,
        "provider",
        "upstream",
    )
    .await;
    insert_error(
        &harness,
        id,
        id + 1_000_000_102,
        now - Duration::seconds(10),
        401,
        "provider",
        "upstream",
    )
    .await;
    assert!(harness
        .controller
        .depletion(&row, now, false)
        .await
        .unwrap()
        .is_null());

    for status in [401, 402] {
        sqlx::query("DELETE FROM ops_error_logs WHERE account_id=$1")
            .bind(id)
            .execute(&harness.db.pool)
            .await
            .unwrap();
        insert_error(
            &harness,
            id,
            id + 1_000_000_200 + status as i64,
            now - Duration::seconds(10),
            status,
            "provider",
            "upstream",
        )
        .await;
        assert!(harness
            .controller
            .depletion(&row, now, false)
            .await
            .unwrap()
            .is_null());
    }

    let mut invalid_fallback_row = row.clone();
    invalid_fallback_row["rate_limit_reset_at"] = json!(datetime(now));
    let invalid_fallback = harness
        .controller
        .depletion(&invalid_fallback_row, now, false)
        .await
        .unwrap();
    assert!(invalid_fallback.is_null());

    let (fallback_seed, _) = harness.seed_depletion(&harness.controller).await;
    let fallback_id = integer(&fallback_seed["id"]).unwrap();
    let recent_limit_start = now - Duration::seconds(60);
    sqlx::query("UPDATE accounts SET rate_limited_at=$1 WHERE id=$2")
        .bind(recent_limit_start)
        .bind(fallback_id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    let fallback_row = reload_account(&harness, fallback_id).await;
    let saved = harness.controller.saved_quota(&fallback_row, now).unwrap();
    let reset = parse_datetime(&saved["oauth_quota"]["ui_windows"][1]["reset_at"]).unwrap();
    let credits =
        normalized_credits(&fallback_row["extra"]["codex_reset_credit_snapshot"]).unwrap();
    let observation = reset_credit_observation::create(
        &fallback_row,
        &credits,
        now - Duration::seconds(1),
        &oauth_admission::credential_fingerprint(&fallback_row),
        reset,
        &json!(10080),
    )
    .unwrap();
    harness
        .controller
        .store
        .transaction(|state| {
            state["scheduler"][fallback_id.to_string()]["reset_credit_observation"] =
                observation.clone();
            Ok(())
        })
        .unwrap();
    sqlx::query("DELETE FROM ops_error_logs WHERE account_id=$1")
        .bind(fallback_id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    let fallback_evidence = harness
        .controller
        .depletion(&fallback_row, now, false)
        .await
        .unwrap();
    assert_eq!(fallback_evidence["evidence_source"], "account_rate_limit");
    assert_eq!(
        parse_datetime(&fallback_evidence["evidence_at"]),
        Some(recent_limit_start)
    );
    harness.set_config(recovery_config(fallback_id, "connection"));
    run_automatic(&harness, &fallback_row, now).await;
    let fallback_task = harness.controller.state(fallback_id).unwrap();
    assert_eq!(fallback_task["evidence_source"], "account_rate_limit");
    assert!(fallback_task["evidence_id"]
        .as_str()
        .unwrap()
        .starts_with("account-rate-limit:"));
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 2);

    for name in [
        "limit_reset_at_current",
        "missing_limited_at",
        "expired_account",
        "weekly_not_exhausted",
        "stale_usage",
        "future_usage",
    ] {
        let (mut candidate, _) = harness.seed_depletion(&harness.controller).await;
        let candidate_id = integer(&candidate["id"]).unwrap();
        sqlx::query("DELETE FROM ops_error_logs WHERE account_id=$1")
            .bind(candidate_id)
            .execute(&harness.db.pool)
            .await
            .unwrap();
        match name {
            "limit_reset_at_current" => candidate["rate_limit_reset_at"] = json!(datetime(now)),
            "missing_limited_at" => candidate["rate_limited_at"] = Value::Null,
            "expired_account" => {
                candidate["expires_at"] = json!(datetime(now - Duration::seconds(1)));
                candidate["auto_pause_on_expired"] = true.into();
            }
            "weekly_not_exhausted" => candidate["extra"]["codex_7d_used_percent"] = json!(99.999),
            "stale_usage" => {
                candidate["extra"]["codex_usage_updated_at"] =
                    json!(datetime(now - Duration::days(8)))
            }
            "future_usage" => {
                candidate["extra"]["codex_usage_updated_at"] =
                    json!(datetime(now + Duration::seconds(1)))
            }
            _ => unreachable!(),
        }
        if matches!(
            name,
            "weekly_not_exhausted" | "stale_usage" | "future_usage"
        ) {
            let mut quota = native_quota_data(
                now,
                100.0,
                if name == "weekly_not_exhausted" {
                    99.999
                } else {
                    100.0
                },
            );
            if name == "stale_usage" {
                quota["fetched_at"] = json!(datetime(now - Duration::days(8)));
            } else if name == "future_usage" {
                quota["fetched_at"] = json!(datetime(now + Duration::seconds(1)));
            }
            reset_stored_quota(&harness.controller, &candidate, now, quota);
        }
        let blocked = harness
            .controller
            .depletion(&candidate, now, false)
            .await
            .unwrap();
        assert!(
            blocked.is_null(),
            "fallback incorrectly accepted {name}: {blocked}"
        );
    }
}

#[tokio::test]
async fn zero_expired_or_unconfirmed_cards_remain_silent_across_rounds() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let bark = Bark::new(harness.settings.clone()).unwrap();
    let cases = vec![
        (
            "zero",
            snapshot(0, now + Duration::days(2), now - Duration::hours(2)),
            "no_credit",
        ),
        (
            "expired",
            snapshot(1, now, now - Duration::hours(2)),
            "no_credit",
        ),
        (
            "malformed",
            json!({"available_count":"1","credits":[]}),
            "credit_unconfirmed",
        ),
        ("missing", json!({}), "credit_unconfirmed"),
        ("budget_exhausted", json!({}), "credit_unconfirmed"),
        (
            "contradictory",
            json!({"available_count":2,"credits":[{"expires_at":datetime(now + Duration::days(2))}]}),
            "credit_unconfirmed",
        ),
    ];
    for (kind, value, reason) in cases {
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        let row = set_credit_snapshot(&harness, id, value).await;
        harness.set_config(recovery_config(id, "connection"));
        let exhausted_attempts = (0..6)
            .map(|hour| datetime(now - Duration::hours(hour)))
            .collect::<Vec<_>>();
        if kind == "budget_exhausted" {
            harness
                .controller
                .store
                .transaction(|state| {
                    state["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"] =
                        json!(exhausted_attempts.clone());
                    Ok(())
                })
                .unwrap();
        }
        run_automatic(&harness, &row, now).await;
        let state = harness.controller.store.snapshot().unwrap();
        let meta = &state["scheduler"][id.to_string()];
        assert!(meta["auto_reset_credit"].is_null(), "{kind} created a task");
        assert_eq!(meta["credit_admission"]["reason"], reason, "{kind}");
        assert!(meta["credit_result_notifications"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty));
        if kind == "budget_exhausted" {
            assert_eq!(
                meta["quota_query"]["automatic_attempts"],
                json!(exhausted_attempts)
            );
        } else {
            assert!(meta["quota_query"]["automatic_attempts"]
                .as_array()
                .is_none_or(Vec::is_empty));
        }
        let next = if kind == "missing" {
            now + Duration::hours(1)
        } else {
            now + Duration::seconds(30)
        };
        harness.set_now(next);
        run_automatic(&harness, &row, next).await;
        if kind == "missing" {
            let after = harness.controller.store.snapshot().unwrap();
            assert_eq!(
                after["scheduler"][id.to_string()]["credit_admission"]["reason"],
                "consume_deadline"
            );
            assert!(after["scheduler"][id.to_string()]["auto_reset_credit"].is_null());
        }
        if kind == "budget_exhausted" {
            let after = harness.controller.store.snapshot().unwrap();
            assert_eq!(
                after["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"],
                json!(exhausted_attempts)
            );
        }
        assert_eq!(harness.mock.count(Method::GET, "/usage"), 0);
        assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
        assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
        assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 0);
        harness.controller.deliver_results(&bark).await.unwrap();
        assert_eq!(harness.mock.count(Method::POST, "/push"), 0);
    }
}

#[tokio::test]
async fn legacy_unissued_zero_card_task_is_suppressed_and_unconfirmed_pause_stays_blocked() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let bark = Bark::new(harness.settings.clone()).unwrap();
    for (kind, snapshot_value, expected_reason) in [
        (
            "zero",
            snapshot(0, now + Duration::days(2), now),
            "no_credit",
        ),
        ("unconfirmed", json!({}), "credit_unconfirmed"),
    ] {
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        let episode = format!("fixture-legacy-unissued-zero-card-{kind}");
        let notification_episode = episode.clone();
        let legacy = json!({
            "episode":episode,
            "stage":"waiting",
            "policy_version":1,
            "evidence_at":datetime(now - Duration::seconds(30)),
            "trigger_at":datetime(now - Duration::seconds(30)),
            "revision":1,
            "attempt_at":null,
            "consumed":false,
            "owns_pause":false,
            "credit_admitted_at":null
        });
        harness
            .controller
            .store
            .transaction(|state| {
                state["scheduler"][id.to_string()]["auto_reset_credit"] = legacy.clone();
                state["scheduler"][id.to_string()]["credit_result_notifications"]
                    [notification_episode.clone()] = json!({
                    "episode":episode.clone(),
                    "account_id":id,
                    "account_name":format!("fixture-credit-account-{id}"),
                    "outcome":"not_executed",
                    "reason":"fixture legacy admission was skipped",
                    "reason_code":expected_reason,
                    "completed_at":datetime(now),
                    "status":"pending",
                    "attempts":0,
                    "next_at":datetime(now)
                });
                Ok(())
            })
            .unwrap();
        let row = set_credit_snapshot(&harness, id, snapshot_value).await;
        let migrated = harness
            .controller
            .migrate_admission(id, &row, &legacy, now)
            .unwrap();
        assert_eq!(migrated["stage"], "finished", "{kind}");
        assert_eq!(migrated["credit_gate_skipped"], true, "{kind}");
        assert_eq!(migrated["outcome"], "not_executed", "{kind}");
        assert_eq!(migrated["result_reason"], expected_reason, "{kind}");
        let state = harness.controller.store.snapshot().unwrap();
        assert_eq!(
            state["scheduler"][id.to_string()]["credit_result_notifications"][episode]["status"],
            "suppressed",
            "{kind}"
        );
        assert!(credit_rules::project(
            &migrated,
            &state["scheduler"][id.to_string()]["credit_result_notifications"]
        )
        .is_null());
        assert_eq!(harness.mock.total(), 0);
    }
    harness.controller.deliver_results(&bark).await.unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/push"), 0);

    let blocked = json!({
        "episode":"fixture-unconfirmed-pause",
        "stage":"finished",
        "policy_version":1,
        "trigger_at":datetime(now),
        "evidence_at":datetime(now),
        "attempt_at":null,
        "consumed":false,
        "owns_pause":true,
        "credit_gate_skipped":true
    });
    let projected = credit_rules::project(&blocked, &json!({}));
    assert_eq!(projected["stage"], "blocked");
    assert_eq!(projected["label"], "调度暂停待核对");
    assert!(projected["next_at"].is_null());
}

#[tokio::test]
async fn unissued_episode_requires_manual_reopen_recovery_and_newer_depletion_after_restart() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    let old_episode;
    {
        let slot_a = harness.controller.slots.acquire().unwrap();
        let slot_b = harness.controller.slots.acquire().unwrap();
        run_automatic(&harness, &row, now).await;
        let waiting = harness.controller.state(id).unwrap();
        assert_eq!(waiting["stage"], "waiting");
        assert!(waiting["attempt_at"].is_null());
        old_episode = waiting["episode"].as_str().unwrap().to_owned();
        assert_eq!(harness.mock.total(), 0);
        drop(slot_a);
        drop(slot_b);
    }

    let manual_at = now + Duration::seconds(1);
    let row = reload_account(&harness, id).await;
    harness
        .controller
        .store
        .manual_control(&row, false, manual_at, "manual")
        .unwrap();
    sqlx::query("UPDATE accounts SET schedulable=false,updated_at=$1 WHERE id=$2")
        .bind(manual_at)
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    harness.set_now(now + Duration::seconds(30));
    let paused = reload_account(&harness, id).await;
    run_automatic(&harness, &paused, now + Duration::seconds(30)).await;
    let stopped = harness.controller.state(id).unwrap();
    assert_eq!(stopped["episode"], old_episode);
    assert_eq!(stopped["stage"], "finished");
    assert_eq!(stopped["result_reason"], "manual_intervention");
    assert_eq!(harness.mock.total(), 0);

    let recovered_at = now + Duration::minutes(1);
    sqlx::query("UPDATE accounts SET schedulable=true,rate_limited_at=NULL,rate_limit_reset_at=NULL,status='active',temp_unschedulable_until=NULL,temp_unschedulable_reason='',overload_until=NULL,error_message='',updated_at=$1 WHERE id=$2")
        .bind(recovered_at)
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    let recovered_row = reload_account(&harness, id).await;
    harness
        .controller
        .store
        .manual_control(&recovered_row, true, recovered_at, "manual")
        .unwrap();
    reset_stored_quota(
        &harness.controller,
        &recovered_row,
        recovered_at,
        native_quota_data(recovered_at, 20.0, 20.0),
    );
    harness.set_now(recovered_at);
    run_automatic(&harness, &recovered_row, recovered_at).await;
    let recovered = harness.controller.state(id).unwrap();
    assert_eq!(recovered["episode"], old_episode);
    assert_eq!(recovered["stage"], "finished");
    assert!(recovered["recovered_evidence_at"].is_string());
    assert_eq!(harness.mock.total(), 0);

    let trigger = recovered_at + Duration::seconds(60);
    let reset = trigger + Duration::days(5);
    sqlx::query("UPDATE accounts SET rate_limited_at=$1,rate_limit_reset_at=$2,schedulable=true,status='active',updated_at=$1 WHERE id=$3")
        .bind(trigger)
        .bind(reset)
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ops_error_logs WHERE account_id=$1")
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    insert_error(
        &harness,
        id,
        id + 1_000_000_900,
        trigger,
        429,
        "provider",
        "upstream",
    )
    .await;
    let card = snapshot(1, trigger + Duration::days(2), trigger);
    harness.set_now(trigger);
    harness.mock.set_usage(
        id,
        200,
        usage_response(trigger, 100.0, 100.0, Some(card), trigger, true),
    );
    let observation = harness
        .controller
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Manual,
                reason: "fixture-reopened-new-depletion".into(),
                at: trigger,
                timeout: 3,
                ..QueryOptions::default()
            },
        )
        .await;
    assert_eq!(observation["success"], true, "{observation}");
    let new_row = reload_account(&harness, id).await;
    let slot_a = harness.controller.slots.acquire().unwrap();
    let slot_b = harness.controller.slots.acquire().unwrap();
    harness.set_now(trigger + Duration::seconds(1));
    run_automatic(&harness, &new_row, trigger + Duration::seconds(1)).await;
    let next = harness.controller.state(id).unwrap();
    assert_ne!(next["episode"], old_episode);
    assert_eq!(next["stage"], "waiting");
    assert_eq!(next["policy_version"], 1);
    assert!(next["attempt_at"].is_null());
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 0);
    drop(slot_a);
    drop(slot_b);
}

#[tokio::test]
async fn waiting_episode_finishes_on_manual_disable_or_deletion_without_dispatch() {
    let now = fixed_now();
    for (change, expected_reason) in [
        ("manual", "manual_intervention"),
        ("disabled", "account_disabled"),
        ("deleted", "account_deleted"),
    ] {
        let harness = Harness::new(false).await;
        let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
        harness.set_config(recovery_config(id, "connection"));
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let slot_a = harness.controller.slots.acquire().unwrap();
        let slot_b = harness.controller.slots.acquire().unwrap();
        run_automatic(&harness, &row, now).await;
        assert_eq!(harness.controller.state(id).unwrap()["stage"], "waiting");
        drop(slot_a);
        drop(slot_b);

        let changed_at = now + Duration::seconds(1);
        match change {
            "manual" => {
                let live = reload_account(&harness, id).await;
                harness
                    .controller
                    .store
                    .manual_control(&live, false, changed_at, "fixture-stop")
                    .unwrap();
                sqlx::query("UPDATE accounts SET schedulable=false,updated_at=$1 WHERE id=$2")
                    .bind(changed_at)
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "disabled" => {
                sqlx::query("UPDATE accounts SET status='disabled',updated_at=$1 WHERE id=$2")
                    .bind(changed_at)
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "deleted" => {
                sqlx::query("UPDATE accounts SET deleted_at=$1,updated_at=$1 WHERE id=$2")
                    .bind(changed_at)
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        harness.set_now(now + Duration::seconds(30));
        let changed = if change == "deleted" {
            // The synthetic PostgreSQL row is now soft-deleted and correctly
            // hidden by the account lookup. Preserve the pre-delete request
            // input while the controller observes the deletion from PostgreSQL.
            row.clone()
        } else {
            reload_account(&harness, id).await
        };
        run_automatic(&harness, &changed, now + Duration::seconds(30)).await;
        let state = harness.controller.state(id).unwrap();
        assert_eq!(state["stage"], "finished", "{change}");
        assert_eq!(state["result_reason"], expected_reason, "{change}");
        assert_eq!(harness.mock.total(), 0, "{change}");
        assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
        assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    }
}

#[tokio::test]
async fn auth_pause_conflict_and_feature_toggle_gate_real_admission() {
    let harness = Harness::new(false).await;
    let now = fixed_now();

    for code in ["http_401", "http_402"] {
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        harness.set_config(recovery_config(id, "connection"));
        let credential = oauth_admission::credential_fingerprint(&row);
        harness
            .controller
            .store
            .transaction(|state| {
                state["scheduler"][id.to_string()]["last_error_code"] = json!(code);
                state["scheduler"][id.to_string()]["quota_query"]["auth_fingerprint"] =
                    json!(credential);
                Ok(())
            })
            .unwrap();
        run_automatic(&harness, &row, now).await;
        assert!(harness
            .controller
            .state(id)
            .unwrap()
            .as_object()
            .unwrap()
            .is_empty());
    }

    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    sqlx::query(
        "UPDATE accounts SET extra=jsonb_set(COALESCE(extra,'{}'::jsonb),'{auto_reset_credit_enabled}','true'::jsonb,true) WHERE id=$1",
    )
    .bind(id)
    .execute(&harness.db.pool)
    .await
    .unwrap();
    let row = reload_account(&harness, id).await;
    run_automatic(&harness, &row, now).await;
    assert_eq!(
        harness.controller.state(id).unwrap()["result_reason"],
        "conflict"
    );
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);

    let next_id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(next_id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    assert_eq!(id, next_id);
    let mut config = recovery_config(id, "connection");
    config["oauth_auto_reset_credit_enabled"] = false.into();
    harness.set_config(config);
    run_automatic(&harness, &row, now).await;
    assert!(harness
        .controller
        .state(id)
        .unwrap()
        .as_object()
        .unwrap()
        .is_empty());
    assert_eq!(harness.mock.total(), 0);
    let mut config = recovery_config(id, "connection");
    config["oauth_auto_reset_credit_enabled"] = false.into();
    harness.set_config(config);
    harness.set_now(now + Duration::seconds(2));
    run_automatic(&harness, &row, now + Duration::seconds(2)).await;
    assert_eq!(harness.mock.total(), 0);
    let config = recovery_config(id, "connection");
    harness.set_config(config);
    run_automatic(&harness, &row, now + Duration::seconds(3)).await;
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
}

#[tokio::test]
async fn automatic_pause_is_read_back_owned_before_receipt_and_recovery_is_once_only() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    assert_eq!(integer(&row["id"]), Some(id));
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse(id, successful_sse());

    run_automatic(&harness, &row, now).await;

    let state = harness.controller.state(id).unwrap();
    let automatic_attempts = harness.controller.store.snapshot().unwrap()["scheduler"]
        [id.to_string()]["quota_query"]["automatic_attempts"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(state["stage"], "recovered");
    assert_eq!(state["consumed"], true);
    assert_eq!(automatic_attempts, 1);
    assert_eq!(state["consumption_outcome"], "consumed");
    assert_eq!(state["receipt"]["http_status"], 200);
    assert_eq!(state["receipt"]["business_code"], "success");
    assert_eq!(state["receipt"]["windows_reset"], 1);
    assert!(state["reset_completed_at"].is_string());
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 2);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 1);
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 0);
    assert_eq!(reload_account(&harness, id).await["schedulable"], true);

    let calls = harness.mock.calls.lock().unwrap().clone();
    let reset_call = calls
        .iter()
        .find(|call| call.path.ends_with("/reset-quota"))
        .unwrap();
    let before_reset = &reset_call.state_at_arrival.as_ref().unwrap()["scheduler"][id.to_string()]
        ["auto_reset_credit"];
    assert_eq!(before_reset["stage"], "resetting");
    assert_eq!(before_reset["owns_pause"], true);
    assert!(before_reset["owned_version"].is_string());
    assert!(before_reset["attempt_at"].is_string());
    let test_call = calls
        .iter()
        .find(|call| call.path.ends_with("/test"))
        .unwrap();
    let before_test = &test_call.state_at_arrival.as_ref().unwrap()["scheduler"][id.to_string()]
        ["auto_reset_credit"];
    assert_eq!(before_test["consumed"], true);
    assert_eq!(before_test["receipt"]["business_code"], "success");
    assert_eq!(before_test["stage"], "testing");

    let serialized = harness.controller.store.snapshot().unwrap();
    assert_eq!(serialized["settings"]["sub2api_admin_token"], ADMIN_KEY);
    assert!(!state.to_string().contains(ADMIN_KEY));
    assert!(!serialized.to_string().contains("fixture-bark-device-key"));
    assert!(!fs::read_to_string(&harness.settings.audit_path)
        .unwrap_or_default()
        .contains(ADMIN_KEY));

    let receipt = state["receipt"].clone();
    let completed = state["reset_completed_at"].clone();
    let reset_history = harness.controller.store.snapshot().unwrap()["recovery_history"].clone();
    let restarted = harness.restart_controller();
    restarted
        .run_account(&row, now + Duration::seconds(2))
        .await
        .unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 1);
    assert_eq!(harness.controller.state(id).unwrap()["receipt"], receipt);
    assert_eq!(
        harness.controller.state(id).unwrap()["reset_completed_at"],
        completed
    );
    assert_eq!(
        harness.controller.store.snapshot().unwrap()["recovery_history"],
        reset_history
    );
}

#[tokio::test]
async fn releasing_restart_reconciles_the_confirmed_history_without_repeating_mutations() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse(id, successful_sse());
    let gate = Arc::new(Notify::new());
    harness.mock.set_schedule_gate(true, gate);
    let controller = harness.controller.clone();
    let scheduled_row = row.clone();
    let operation = tokio::spawn(async move { controller.run_account(&scheduled_row, now).await });
    harness.mock.wait_for(Method::POST, "/schedulable", 2).await;
    let calls = harness.mock.calls.lock().unwrap().clone();
    let release = calls
        .iter()
        .find(|call| {
            call.method == Method::POST
                && call.path.ends_with("/schedulable")
                && call.body["schedulable"] == true
        })
        .expect("final schedule release request");
    let at_release = &release.state_at_arrival.as_ref().unwrap()["scheduler"][id.to_string()]
        ["auto_reset_credit"];
    assert_eq!(at_release["stage"], "releasing");
    assert_eq!(at_release["consumed"], true);
    assert!(at_release["release_requested_at"].is_string());

    harness.mock.trip_lock_after(Method::POST, "/schedulable");
    harness.mock.release_schedule();
    assert!(operation.await.unwrap().is_err());
    let lock_path = PathBuf::from(format!("{}.lock", harness.settings.oauth_path.display()));
    assert!(lock_path.is_dir());
    fs::remove_dir(&lock_path).expect("remove only the fixture release-write fault");
    assert_eq!(harness.controller.state(id).unwrap()["stage"], "releasing");
    let counts = [
        harness.mock.count(Method::POST, "/reset-quota"),
        harness.mock.count(Method::POST, "/test"),
        harness.mock.count(Method::POST, "/recover-state"),
        harness.mock.count(Method::POST, "/schedulable"),
    ];
    assert_eq!(counts, [1, 1, 1, 2]);

    harness
        .restart_controller()
        .run_account(&row, now + Duration::seconds(30))
        .await
        .unwrap();
    let finished = harness.controller.state(id).unwrap();
    assert_eq!(finished["stage"], "recovered");
    assert_eq!(finished["outcome"], "restored");
    assert_eq!(finished["consumed"], true);
    assert_eq!(finished["receipt"]["business_code"], "success");
    assert_eq!(
        [
            harness.mock.count(Method::POST, "/reset-quota"),
            harness.mock.count(Method::POST, "/test"),
            harness.mock.count(Method::POST, "/recover-state"),
            harness.mock.count(Method::POST, "/schedulable"),
        ],
        counts
    );
    let state = harness.controller.store.snapshot().unwrap();
    assert_eq!(state["recovery_history"].as_object().unwrap().len(), 1);
    assert_eq!(
        state["scheduler"][id.to_string()]["credit_result_notifications"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn releasing_restart_preserves_manual_changes_and_new_account_blocks() {
    let now = fixed_now();
    for change in [
        "disabled",
        "schedule_off",
        "configuration",
        "new_limit",
        "manual_generation",
    ] {
        let harness = Harness::new(false).await;
        let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
        harness.set_config(recovery_config(id, "connection"));
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        harness.mock.set_reset(
            id,
            200,
            reset_response(now, 20.0, 20.0, "success", json!(1)),
        );
        harness.mock.set_sse(id, successful_sse());

        let gate = Arc::new(Notify::new());
        harness.mock.set_schedule_gate(true, gate);
        let controller = harness.controller.clone();
        let run_row = row.clone();
        let operation = tokio::spawn(async move { controller.run_account(&run_row, now).await });
        harness.mock.wait_for(Method::POST, "/schedulable", 2).await;
        let pending = harness.mock.calls.lock().unwrap().clone();
        let release = pending
            .iter()
            .find(|call| {
                call.method == Method::POST
                    && call.path.ends_with("/schedulable")
                    && call.body["schedulable"] == true
            })
            .expect("final schedule release request");
        assert_eq!(
            release.state_at_arrival.as_ref().unwrap()["scheduler"][id.to_string()]
                ["auto_reset_credit"]["stage"],
            "releasing",
            "{change}"
        );
        harness.mock.trip_lock_after(Method::POST, "/schedulable");
        harness.mock.release_schedule();
        assert!(operation.await.unwrap().is_err(), "{change}");
        let lock_path = PathBuf::from(format!("{}.lock", harness.settings.oauth_path.display()));
        assert!(lock_path.is_dir());
        fs::remove_dir(&lock_path).expect("remove only this fixture release-write fault");
        assert_eq!(harness.controller.state(id).unwrap()["stage"], "releasing");

        let changed_at = now + Duration::seconds(10);
        match change {
            "disabled" => {
                sqlx::query("UPDATE accounts SET status='disabled' WHERE id=$1")
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "schedule_off" => {
                sqlx::query("UPDATE accounts SET schedulable=false WHERE id=$1")
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "configuration" => {
                sqlx::query("UPDATE accounts SET concurrency=2 WHERE id=$1")
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
            }
            "new_limit" => {
                sqlx::query(
                    "UPDATE accounts SET rate_limited_at=$1,rate_limit_reset_at=$2 WHERE id=$3",
                )
                .bind(changed_at)
                .bind(changed_at + Duration::hours(5))
                .bind(id)
                .execute(&harness.db.pool)
                .await
                .unwrap();
            }
            "manual_generation" => {
                let changed = reload_account(&harness, id).await;
                harness
                    .controller
                    .store
                    .manual_control(&changed, true, changed_at, "fixture-reopen")
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let expected = reload_account(&harness, id).await;
        let before_calls = harness.mock.total();
        let generation_before = harness.controller.store.snapshot().unwrap()["scheduler"]
            [id.to_string()]["manual_control"]["generation"]
            .clone();
        harness
            .restart_controller()
            .run_account(&expected, now + Duration::seconds(30))
            .await
            .unwrap();

        let actual = reload_account(&harness, id).await;
        for field in [
            "status",
            "schedulable",
            "concurrency",
            "rate_limited_at",
            "rate_limit_reset_at",
        ] {
            assert_eq!(actual[field], expected[field], "{change}: {field}");
        }
        assert_eq!(harness.mock.total(), before_calls, "{change} replayed I/O");
        assert_ne!(harness.controller.state(id).unwrap()["stage"], "recovered");
        assert_eq!(
            harness.controller.store.snapshot().unwrap()["scheduler"][id.to_string()]
                ["manual_control"]["generation"],
            generation_before,
            "{change} changed manual generation"
        );
        assert!(
            harness.controller.store.snapshot().unwrap()["recovery_history"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn uncertain_recovery_write_persists_clear_request_and_only_reconciles_after_restart() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse(id, successful_sse());
    harness.mock.set_recovery_status(id, 500);

    run_automatic(&harness, &row, now).await;
    let first = harness.controller.state(id).unwrap();
    assert_eq!(first["stage"], "confirming");
    assert_eq!(first["consumed"], true);
    assert_eq!(first["test_success"], true);
    assert!(first["clear_requested_at"].is_string());
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);

    harness.set_now(now + Duration::seconds(2));
    harness
        .restart_controller()
        .run_account(&row, now + Duration::seconds(2))
        .await
        .unwrap();
    let reconciled = harness.controller.state(id).unwrap();
    assert_eq!(reconciled["stage"], "confirming");
    assert_eq!(
        reconciled["clear_requested_at"],
        first["clear_requested_at"]
    );
    assert_eq!(reconciled["consumed"], true);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
}

#[tokio::test]
async fn successful_verification_does_not_release_while_another_required_window_is_exhausted() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 100.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse(id, successful_sse());
    run_automatic(&harness, &row, now).await;
    let state = harness.controller.state(id).unwrap();
    assert_eq!(state["stage"], "finished");
    assert_eq!(state["result_reason"], "incomplete_quota");
    assert_eq!(state["outcome"], "unrecovered");
    assert_eq!(state["consumed"], true);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
    assert_eq!(reload_account(&harness, id).await["schedulable"], false);
}

#[tokio::test]
async fn configuration_change_while_pause_write_is_in_flight_prevents_card_dispatch() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    let gate = Arc::new(Notify::new());
    harness.mock.set_schedule_gate(false, gate);
    let controller = harness.controller.clone();
    let run_row = row.clone();
    let operation = tokio::spawn(async move { controller.run_account(&run_row, now).await });
    harness.mock.wait_for(Method::POST, "/schedulable", 1).await;
    harness.set_config(recovery_config(id, "model"));
    harness.mock.release_schedule();
    let _ = operation.await.unwrap();
    let state = harness.controller.state(id).unwrap();
    assert_eq!(state["stage"], "finished");
    assert!(state["consumed"].is_null() || state["consumed"] == false);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 0);
    assert_eq!(reload_account(&harness, id).await["schedulable"], false);
}

#[tokio::test]
async fn account_updated_at_change_during_retry_relinquishes_pause_without_second_test() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse(id, explicit_sse_error("http_502"));
    run_automatic(&harness, &row, now).await;
    let retry = harness.controller.state(id).unwrap();
    assert_eq!(retry["stage"], "retry");
    assert_eq!(retry["test_attempts"], 1);
    sqlx::query("UPDATE accounts SET updated_at=$1 WHERE id=$2")
        .bind(now + Duration::seconds(1))
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    let changed = reload_account(&harness, id).await;
    harness.set_now(now + Duration::seconds(60));
    run_automatic(&harness, &changed, now + Duration::seconds(60)).await;
    let relinquished = harness.controller.state(id).unwrap();
    assert_eq!(relinquished["stage"], "blocked");
    assert_eq!(relinquished["owns_pause"], false);
    assert_eq!(relinquished["consumed"], true);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
    assert_eq!(reload_account(&harness, id).await["schedulable"], false);
}

#[tokio::test]
async fn native_auth_test_responses_finish_paused_and_never_retry_after_credential_change() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    for status in [401, 402] {
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        let reset_before = harness.mock.count(Method::POST, "/reset-quota");
        let test_before = harness.mock.count(Method::POST, "/test");
        let schedule_before = harness.mock.count(Method::POST, "/schedulable");
        harness.set_config(recovery_config(id, "connection"));
        harness.mock.set_reset(
            id,
            200,
            reset_response(now, 20.0, 20.0, "success", json!(1)),
        );
        harness.mock.set_test_status(id, status);
        run_automatic(&harness, &row, now).await;
        let state = harness.controller.state(id).unwrap();
        let code = format!("http_{status}");
        assert_eq!(state["stage"], "finished");
        assert_eq!(state["error_code"], code);
        assert_eq!(state["result_reason"], code);
        assert_eq!(state["outcome"], "unrecovered");
        assert_eq!(state["consumed"], true);
        assert_eq!(
            harness.mock.count(Method::POST, "/reset-quota"),
            reset_before + 1
        );
        assert_eq!(harness.mock.count(Method::POST, "/test"), test_before + 1);
        assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 0);
        assert_eq!(
            harness.mock.count(Method::POST, "/schedulable"),
            schedule_before + 1
        );

        sqlx::query("UPDATE accounts SET credentials=jsonb_set(credentials,'{access_token}',to_jsonb($1::text),true) WHERE id=$2")
            .bind(format!("fixture-rotated-token-{id}"))
            .bind(id)
            .execute(&harness.db.pool)
            .await
            .unwrap();
        harness.set_now(now + Duration::seconds(30));
        let changed = reload_account(&harness, id).await;
        run_automatic(&harness, &changed, now + Duration::seconds(30)).await;
        assert_eq!(harness.controller.state(id).unwrap()["stage"], "finished");
        assert_eq!(
            harness.mock.count(Method::POST, "/reset-quota"),
            reset_before + 1
        );
        assert_eq!(harness.mock.count(Method::POST, "/test"), test_before + 1);
        assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 0);
        harness.set_now(now);
    }
}

#[tokio::test]
async fn native_reset_receipt_matrix_preserves_integer_http_and_redaction_boundaries() {
    let harness = Harness::new(false).await;
    let mut cases = vec![
        (
            "positive_reset",
            200,
            json!(0),
            json!({"code":"reset","windows_reset":1,"credit":{"status":"redeemed"}}),
            true,
            false,
            "",
        ),
        (
            "integer_success",
            200,
            json!(0),
            json!({"code":0,"windows_reset":2}),
            true,
            false,
            "",
        ),
        (
            "string_success_alias",
            200,
            json!(0),
            json!({"code":"ok","windows_reset":1}),
            true,
            false,
            "",
        ),
        (
            "explicit_no_credit",
            200,
            json!(0),
            json!({"code":"no_credit","windows_reset":0,"credit":{"status":"available"}}),
            false,
            false,
            "no_credit",
        ),
        (
            "explicit_nothing_to_reset",
            200,
            json!(0),
            json!({"code":"nothing_to_reset","windows_reset":0}),
            false,
            false,
            "nothing_to_reset",
        ),
    ];
    for (name, windows) in [
        ("missing_count", Value::Null),
        ("zero_reset", json!(0)),
        ("negative_count", json!(-1)),
        ("boolean_true_count", json!(true)),
        ("boolean_false_count", json!(false)),
        ("floating_count", json!(1.0)),
        ("string_count", json!("1")),
        ("null_count", Value::Null),
    ] {
        let mut data = json!({"code":"reset"});
        if name != "missing_count" {
            data["windows_reset"] = windows;
        }
        cases.push((name, 200, json!(0), data, false, true, "result_uncertain"));
    }
    for (name, data) in [
        (
            "unknown_code_zero",
            json!({"code":"unrecognized_fixture","windows_reset":0}),
        ),
        (
            "unknown_code_positive",
            json!({"code":"unrecognized_fixture","windows_reset":1}),
        ),
        (
            "no_credit_positive",
            json!({"code":"no_credit","windows_reset":1}),
        ),
        (
            "no_credit_boolean",
            json!({"code":"no_credit","windows_reset":true}),
        ),
        ("no_credit_missing", json!({"code":"no_credit"})),
        (
            "nothing_to_reset_positive",
            json!({"code":"nothing_to_reset","windows_reset":1}),
        ),
        (
            "already_redeemed_zero",
            json!({"code":"already_redeemed","windows_reset":0}),
        ),
        (
            "already_redeemed_positive",
            json!({"code":"already_redeemed","windows_reset":1}),
        ),
        (
            "boolean_business_code",
            json!({"code":false,"windows_reset":1}),
        ),
        ("empty_business_code", json!({"code":"","windows_reset":1})),
        ("missing_business_code", json!({"windows_reset":1})),
        (
            "conflicting_credit_status",
            json!({"code":"reset","windows_reset":1,"credit":{"status":"available"}}),
        ),
    ] {
        let expected_error = if name.starts_with("already_redeemed") {
            "already_redeemed"
        } else {
            "result_uncertain"
        };
        cases.push((name, 200, json!(0), data, false, true, expected_error));
    }
    for (name, code) in [
        ("boolean_true_business_code", json!(true)),
        ("null_business_code", Value::Null),
        ("openai_business_code", json!("OPENAI_CARD_REJECTED")),
    ] {
        cases.push((
            name,
            200,
            json!(0),
            json!({"code":code,"windows_reset":1}),
            false,
            true,
            "result_uncertain",
        ));
    }
    cases.push((
        "failed_integer_envelope",
        200,
        json!(1),
        json!({"code":"reset","windows_reset":1,"message":"fixture-access-secret"}),
        false,
        true,
        "result_uncertain",
    ));
    cases.push((
        "failed_boolean_envelope",
        200,
        json!(false),
        json!({"code":"reset","windows_reset":1}),
        false,
        true,
        "result_uncertain",
    ));
    cases.push((
        "failed_http_status",
        409,
        json!(0),
        json!({
            "code":"OPENAI_CARD_EXPIRED",
            "windows_reset":1,
            "cache_persisted":false,
            "message":"fixture-card-secret",
            "card_id":"fixture-card-id"
        }),
        false,
        true,
        "http_409",
    ));

    for (name, status, envelope, data, expected_consumed, expected_uncertain, expected_error) in
        cases
    {
        let row = harness.seed_account(true, false).await;
        let id = integer(&row["id"]).unwrap();
        let before = harness.mock.count(Method::POST, "/reset-quota");
        harness
            .mock
            .set_reset(id, status, json!({"code":envelope,"data":data}));
        let result = harness
            .controller
            .manual(&row, "reset", ADMIN_KEY)
            .await
            .expect("send actual loopback reset request");
        assert!(
            result["consumed"].is_null() || result["consumed"].is_boolean(),
            "{name}: consumed must be absent or boolean: {result}"
        );
        assert_eq!(
            result["consumed"].as_bool().unwrap_or(false),
            expected_consumed,
            "{name}: {result}"
        );
        assert_eq!(result["uncertain"], expected_uncertain, "{name}: {result}");
        assert_eq!(result["error_code"], expected_error, "{name}: {result}");
        assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), before + 1);
        let saved = harness.controller.state(id).unwrap();
        assert_eq!(saved["consumed"], expected_consumed, "{name}");
        assert_eq!(
            saved["consumption_outcome"],
            if expected_consumed {
                "consumed"
            } else if expected_uncertain {
                "unknown"
            } else {
                "not_consumed"
            },
            "{name}"
        );
        assert!(saved["attempt_at"].is_string(), "{name}");
        assert_eq!(saved["receipt"], result["diagnostics"], "{name}");
        for private_value in [
            "fixture-access-secret",
            "fixture-card-secret",
            "fixture-card-id",
        ] {
            assert!(
                !saved.to_string().contains(private_value),
                "{name} retained {private_value}"
            );
        }

        let restarted = harness.restart_controller();
        let replay = restarted
            .manual(&row, "reset", ADMIN_KEY)
            .await
            .expect("an existing attempt returns a read-only uncertainty result");
        assert_eq!(replay["error_code"], "result_uncertain", "{name}");
        assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), before + 1);
        assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    }
}

#[tokio::test]
async fn true_http_502_and_truncated_sse_are_uncertain_and_never_replayed() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    harness
        .mock
        .set_reset(id, 502, json!({"code":1,"message":"fixture HTTP failure"}));
    run_automatic(&harness, &row, now).await;
    let reset_state = harness.controller.state(id).unwrap();
    assert_eq!(reset_state["stage"], "finished");
    assert_eq!(reset_state["consumption_outcome"], "unknown");
    assert_eq!(reset_state["consumed"], false);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    harness
        .restart_controller()
        .run_account(&row, now + Duration::minutes(6))
        .await
        .unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);

    let second_id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(second_id, "connection"));
    let (second_row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_test_status(second_id, 502);
    harness.mock.set_reset(
        second_id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    run_automatic(&harness, &second_row, now).await;
    let uncertain_test = harness.controller.state(second_id).unwrap();
    assert_eq!(uncertain_test["stage"], "finished");
    assert_eq!(uncertain_test["result_reason"], "verification_interrupted");
    assert_eq!(uncertain_test["consumed"], true);
    assert_eq!(uncertain_test["receipt"]["business_code"], "success");
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 2);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    let calls = harness.mock.calls.lock().unwrap().clone();
    let late = calls
        .iter()
        .find(|call| call.path.ends_with("/test"))
        .unwrap();
    assert_eq!(late.body["model_id"], "fixture-model");
    harness
        .restart_controller()
        .run_account(&second_row, now + Duration::minutes(6))
        .await
        .unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 2);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);

    let third_id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(third_id, "connection"));
    let (third_row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_reset(
        third_id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse(
        third_id,
        "data: {\"type\":\"test_start\",\"model\":\"fixture-model\"}\n\ndata: {\"type\":\"test_complete\",\"success\":".to_owned(),
    );
    run_automatic(&harness, &third_row, now).await;
    assert_eq!(
        harness.controller.state(third_id).unwrap()["result_reason"],
        "verification_interrupted"
    );
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 3);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 2);
    harness
        .restart_controller()
        .run_account(&third_row, now + Duration::minutes(6))
        .await
        .unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 3);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 2);
}

#[tokio::test]
async fn explicit_sse_http_502_errors_retry_at_sixty_seconds_twice_without_reconsuming() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse_sequence(
        id,
        [
            explicit_sse_error("http_502"),
            explicit_sse_error("http_502"),
        ],
    );
    run_automatic(&harness, &row, now).await;
    let first = harness.controller.state(id).unwrap();
    assert_eq!(first["stage"], "retry");
    assert_eq!(first["test_attempts"], 1);
    assert_eq!(first["error_code"], "test_failed");
    assert_eq!(first["next_at"], datetime(now + Duration::seconds(60)));
    assert_eq!(
        first["verification_deadline_at"],
        datetime(now + Duration::seconds(300))
    );
    let receipt = first["receipt"].clone();
    let reset_completed = first["reset_completed_at"].clone();
    let restarted = harness.restart_controller();
    harness.set_now(now + Duration::seconds(59));
    restarted
        .run_account(&row, now + Duration::seconds(59))
        .await
        .unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    harness.set_now(now + Duration::seconds(60));
    restarted
        .run_account(&row, now + Duration::seconds(60))
        .await
        .unwrap();
    let second = harness.controller.state(id).unwrap();
    assert_eq!(second["stage"], "finished");
    assert_eq!(second["result_reason"], "verification_limit");
    assert_eq!(second["test_attempts"], 2);
    assert_eq!(second["receipt"], receipt);
    assert_eq!(second["reset_completed_at"], reset_completed);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 2);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    harness.set_now(now + Duration::seconds(300));
    restarted
        .run_account(&row, now + Duration::seconds(300))
        .await
        .unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/test"), 2);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
}

#[tokio::test]
async fn a_retry_whose_sixty_second_delay_hits_the_five_minute_deadline_is_not_dispatched() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse(id, explicit_sse_error("http_502"));
    let gate = Arc::new(Notify::new());
    harness.mock.set_test_gate(gate);
    let controller = harness.controller.clone();
    let first = tokio::spawn(async move { controller.run_account(&row, now).await });
    harness.mock.wait_for(Method::POST, "/test", 1).await;
    harness.set_now(now + Duration::seconds(240));
    harness.mock.release_test();
    first.await.unwrap().unwrap();
    let task = harness.controller.state(id).unwrap();
    assert_eq!(task["stage"], "finished");
    assert_eq!(task["result_reason"], "verification_deadline");
    assert_eq!(
        task["verification_deadline_at"],
        datetime(now + Duration::seconds(300))
    );
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
}

#[tokio::test]
async fn late_manual_control_and_method_changes_cannot_release_or_repeat_a_consumed_card() {
    let now = fixed_now();
    for manual in [true, false] {
        let harness = Harness::new(false).await;
        let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
        harness.set_config(recovery_config(id, "connection"));
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        harness.mock.set_reset(
            id,
            200,
            reset_response(now, 20.0, 20.0, "success", json!(1)),
        );
        let gate = Arc::new(Notify::new());
        harness.mock.set_test_gate(gate);
        let controller = harness.controller.clone();
        let scheduled_row = row.clone();
        let operation =
            tokio::spawn(async move { controller.run_account(&scheduled_row, now).await });
        harness.mock.wait_for(Method::POST, "/test", 1).await;
        if manual {
            let paused = reload_account(&harness, id).await;
            harness
                .controller
                .store
                .manual_control(&paused, false, now + Duration::seconds(1), "manual")
                .unwrap();
        } else {
            harness.set_config(recovery_config(id, "model"));
        }
        harness.mock.release_test();
        operation.await.unwrap().unwrap();
        let late = harness.controller.state(id).unwrap();
        assert_eq!(late["consumed"], true);
        assert_eq!(late["receipt"]["business_code"], "success");
        assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
        assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
        assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 0);
        assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
        if manual {
            assert_eq!(late["stage"], "manual");
            assert_eq!(late["owns_pause"], false);
        } else {
            assert_eq!(late["stage"], "testing");
            assert_eq!(late["verification_dispatched"], true);
        }
        let receipt = late["receipt"].clone();
        let restarted = harness.restart_controller();
        restarted
            .run_account(&row, now + Duration::seconds(2))
            .await
            .unwrap();
        let final_task = harness.controller.state(id).unwrap();
        assert_eq!(final_task["receipt"], receipt);
        assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
        assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
        assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
        if !manual {
            assert_eq!(final_task["stage"], "finished");
            assert_eq!(final_task["result_reason"], "manual_intervention");
        }
        assert_eq!(reload_account(&harness, id).await["schedulable"], false);
    }
}

#[tokio::test]
async fn late_reset_receipt_after_watchdog_stop_keeps_consumption_without_replay() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    let gate = Arc::new(Notify::new());
    harness.mock.set_reset_gate(gate);
    let controller = harness.controller.clone();
    let scheduled_row = row.clone();
    let operation = tokio::spawn(async move { controller.run_account(&scheduled_row, now).await });
    harness.mock.wait_for(Method::POST, "/reset-quota", 1).await;

    let changed = reload_account(&harness, id).await;
    harness
        .controller
        .store
        .manual_control(&changed, false, now + Duration::seconds(2), "fixture-stop")
        .unwrap();
    harness
        .controller
        .sweep(&[changed], now + Duration::seconds(2))
        .await
        .unwrap();
    harness.mock.release_reset();
    operation.await.unwrap().unwrap();

    let finished = harness.controller.state(id).unwrap();
    assert_eq!(finished["stage"], "finished");
    assert_eq!(finished["result_reason"], "manual_intervention");
    assert_eq!(finished["consumed"], true);
    assert_eq!(finished["receipt"]["business_code"], "success");
    assert!(finished["reset_completed_at"].is_string());
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 0);

    harness
        .restart_controller()
        .run_account(&row, now + Duration::minutes(6))
        .await
        .unwrap();
    let after_restart = harness.controller.state(id).unwrap();
    assert_eq!(after_restart["receipt"], finished["receipt"]);
    assert_eq!(after_restart["result_reason"], "manual_intervention");
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(reload_account(&harness, id).await["schedulable"], false);
}

#[tokio::test]
async fn disabled_or_auth_error_account_is_never_resumed_by_a_late_result() {
    let now = fixed_now();
    for boundary in ["test", "recovery"] {
        let harness = Harness::new(false).await;
        let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
        harness.set_config(recovery_config(id, "connection"));
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        harness.mock.set_reset(
            id,
            200,
            reset_response(now, 20.0, 20.0, "success", json!(1)),
        );
        harness.mock.set_sse(id, successful_sse());

        let controller = harness.controller.clone();
        let run_row = row.clone();
        let operation = if boundary == "test" {
            let gate = Arc::new(Notify::new());
            harness.mock.set_test_gate(gate);
            let operation =
                tokio::spawn(async move { controller.run_account(&run_row, now).await });
            harness.mock.wait_for(Method::POST, "/test", 1).await;
            sqlx::query("UPDATE accounts SET status='disabled',schedulable=false WHERE id=$1")
                .bind(id)
                .execute(&harness.db.pool)
                .await
                .unwrap();
            harness.mock.release_test();
            operation
        } else {
            let entered = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            harness
                .mock
                .set_recovery_gate(entered.clone(), release.clone());
            let operation =
                tokio::spawn(async move { controller.run_account(&run_row, now).await });
            tokio::time::timeout(StdDuration::from_secs(5), entered.notified())
                .await
                .expect("recovery request did not reach its post-readback gate");
            sqlx::query("UPDATE accounts SET status='disabled',schedulable=false WHERE id=$1")
                .bind(id)
                .execute(&harness.db.pool)
                .await
                .unwrap();
            release.notify_one();
            operation
        };

        operation.await.unwrap().unwrap();
        let state = harness.controller.state(id).unwrap();
        assert_eq!(state["consumed"], true, "{boundary}");
        assert_ne!(state["stage"], "recovered", "{boundary}");
        assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
        assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
        let schedule_calls = harness.mock.calls.lock().unwrap().clone();
        assert_eq!(
            schedule_calls
                .iter()
                .filter(|call| {
                    call.method == Method::POST
                        && call.path.ends_with("/schedulable")
                        && call.body["schedulable"] == true
                })
                .count(),
            0,
            "{boundary} late result re-enabled a disabled account"
        );
        let disabled = reload_account(&harness, id).await;
        assert_eq!(disabled["status"], "disabled");
        assert_eq!(disabled["schedulable"], false);

        harness
            .restart_controller()
            .run_account(&disabled, now + Duration::seconds(30))
            .await
            .unwrap();
        assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
        assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
        assert_eq!(
            harness
                .mock
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| {
                    call.method == Method::POST
                        && call.path.ends_with("/schedulable")
                        && call.body["schedulable"] == true
                })
                .count(),
            0
        );
    }

    let harness = Harness::new(false).await;
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_test_status(id, 401);
    let gate = Arc::new(Notify::new());
    harness.mock.set_test_gate(gate);
    let controller = harness.controller.clone();
    let run_row = row.clone();
    let operation = tokio::spawn(async move { controller.run_account(&run_row, now).await });
    harness.mock.wait_for(Method::POST, "/test", 1).await;
    sqlx::query("UPDATE accounts SET status='error',error_message='fixture auth rejection',schedulable=false WHERE id=$1")
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    harness.mock.release_test();
    operation.await.unwrap().unwrap();

    let failed = harness.controller.state(id).unwrap();
    assert_eq!(failed["stage"], "finished");
    assert_eq!(failed["error_code"], "http_401");
    assert_eq!(failed["result_reason"], "http_401");
    assert_eq!(failed["consumed"], true);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 0);

    sqlx::query("UPDATE accounts SET credentials=jsonb_set(credentials,'{access_token}',to_jsonb($1::text),true),status='active',schedulable=false WHERE id=$2")
        .bind(format!("fixture-rotated-token-{id}"))
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    let repaired = reload_account(&harness, id).await;
    harness
        .restart_controller()
        .run_account(&repaired, now + Duration::seconds(30))
        .await
        .unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 0);
    assert_eq!(
        harness
            .mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| {
                call.method == Method::POST
                    && call.path.ends_with("/schedulable")
                    && call.body["schedulable"] == true
            })
            .count(),
        0
    );
}

#[tokio::test]
async fn detection_version_hold_and_failed_readback_are_fail_closed() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, evidence) = harness.seed_depletion(&harness.controller).await;
    let initial = harness.controller.state(id).unwrap();
    let task = harness
        .controller
        .begin_episode(id, &initial, &evidence)
        .await
        .unwrap();
    harness.controller.policy(id, &task, now).await.unwrap();
    harness.set_detection_version(json!("2".repeat(64)));
    run_automatic(&harness, &row, now).await;
    let invalidated = harness.controller.state(id).unwrap();
    assert_eq!(invalidated["stage"], "finished");
    assert_eq!(invalidated["result_reason"], "manual_intervention");
    assert_eq!(harness.mock.total(), 0);

    let second_id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(second_id, "connection"));
    let (second_row, second_evidence) = harness.seed_depletion(&harness.controller).await;
    let second_id = integer(&second_row["id"]).unwrap();
    let second_task = harness.controller.state(second_id).unwrap();
    harness
        .controller
        .begin_episode(second_id, &second_task, &second_evidence)
        .await
        .unwrap();
    harness.fail_detection_version(true);
    let result = harness.controller.run_account(&second_row, now).await;
    assert!(result.is_err());
    let waiting = harness.controller.state(second_id).unwrap();
    assert_eq!(waiting["stage"], "waiting");
    assert!(waiting["policy_version"].is_null());
    assert_eq!(harness.mock.total(), 0);

    harness.fail_detection_version(false);
    let third_id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(third_id, "connection"));
    let (third_row, third_evidence) = harness.seed_depletion(&harness.controller).await;
    let third_id = integer(&third_row["id"]).unwrap();
    let old = harness.controller.state(third_id).unwrap();
    let mut third_task = harness
        .controller
        .begin_episode(third_id, &old, &third_evidence)
        .await
        .unwrap();
    third_task = harness
        .controller
        .policy(third_id, &third_task, now)
        .await
        .unwrap();
    third_task = harness
        .controller
        .save(
            third_id,
            &third_task,
            json!({"model_test_job_id":"fixture-job"}),
        )
        .unwrap();
    harness.hold_detection(true);
    run_automatic(&harness, &third_row, now).await;
    let finished = harness.controller.state(third_id).unwrap();
    assert_eq!(finished["stage"], "finished");
    assert_eq!(finished["result_reason"], "detection_held");
    let merged = harness
        .controller
        .merge_disposition(
            third_id,
            "fixture-job",
            &json!({
                "marked":true,
                "schedule_verified":true,
                "model_id":"fixture-model",
                "prediction":"gpt-5.6-luna",
                "probability":0.97
            }),
            &third_row,
        )
        .await
        .unwrap();
    assert!(merged);
    let notice = &harness.controller.store.snapshot().unwrap()["scheduler"][third_id.to_string()]
        ["credit_result_notifications"][third_task["episode"].as_str().unwrap()];
    assert_eq!(notice["degradation_confirmed"], true);
    assert_eq!(notice["model_id"], "fixture-model");
    assert_eq!(notice["prediction"], "gpt-5.6-luna");
    assert_eq!(notice["probability"], 0.97);
    assert_eq!(harness.mock.total(), 0);
}

#[tokio::test]
async fn confirmed_degradation_merges_into_one_credit_notice_and_bark_delivery() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    let gate = Arc::new(Notify::new());
    harness.mock.set_test_gate(gate);
    let controller = harness.controller.clone();
    let scheduled_row = row.clone();
    let operation = tokio::spawn(async move { controller.run_account(&scheduled_row, now).await });
    harness.mock.wait_for(Method::POST, "/test", 1).await;

    let pending = harness.controller.state(id).unwrap();
    assert_eq!(pending["stage"], "testing");
    assert_eq!(pending["consumed"], true);
    let job_id = "fixture-degradation-job";
    let pending = harness
        .controller
        .save(id, &pending, json!({"model_test_job_id":job_id}))
        .unwrap();
    assert_eq!(pending["model_test_job_id"], job_id);
    let merged = harness
        .controller
        .merge_disposition(
            id,
            job_id,
            &json!({
                "marked":true,
                "schedule_verified":true,
                "model_id":"fixture-model",
                "prediction":"gpt-5.6-luna",
                "probability":0.97
            }),
            &row,
        )
        .await
        .unwrap();
    assert!(merged);
    let finished = harness.controller.state(id).unwrap();
    assert_eq!(finished["stage"], "finished");
    assert_eq!(finished["result_reason"], "detection_held");
    assert_eq!(finished["consumed"], true);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);

    let state = harness.controller.store.snapshot().unwrap();
    let notices = state["scheduler"][id.to_string()]["credit_result_notifications"]
        .as_object()
        .unwrap();
    assert_eq!(notices.len(), 1);
    let episode = finished["episode"].as_str().unwrap();
    let notice = &notices[episode];
    assert_eq!(notice["degradation_confirmed"], true);
    assert_eq!(notice["model_id"], "fixture-model");
    assert_eq!(notice["prediction"], "gpt-5.6-luna");
    assert_eq!(notice["probability"], 0.97);
    assert_eq!(notice["outcome"], "unrecovered");

    harness.mock.release_test();
    operation.await.unwrap().unwrap();
    assert_eq!(
        harness.controller.state(id).unwrap()["result_reason"],
        "detection_held"
    );
    let bark = Bark::new(harness.settings.clone()).unwrap();
    harness.controller.deliver_results(&bark).await.unwrap();
    let delivered = &harness.controller.store.snapshot().unwrap()["scheduler"][id.to_string()]
        ["credit_result_notifications"][episode];
    assert_eq!(delivered["status"], "delivered");
    assert_eq!(delivered["attempts"], 1);
    let pushes = bark_calls(&harness);
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].body["group"], "oauth-reset-credit");
    assert!(pushes[0].body["body"]
        .as_str()
        .unwrap()
        .contains("模型验证已判定降智"));
    assert!(pushes[0].body["body"]
        .as_str()
        .unwrap()
        .contains("恢复未完成"));
    assert!(!pushes[0].body.to_string().contains(ADMIN_KEY));

    harness
        .restart_controller()
        .run_account(&row, now + Duration::hours(1))
        .await
        .unwrap();
    harness.controller.deliver_results(&bark).await.unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/push"), 1);
    assert_eq!(
        harness.controller.state(id).unwrap()["result_reason"],
        "detection_held"
    );
}

#[tokio::test]
async fn manual_reset_uses_the_shared_receipt_and_two_controllers_dispatch_once() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let row = harness.seed_account(true, true).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    let gate = Arc::new(Notify::new());
    harness.mock.set_reset_gate(gate);
    let first_controller = harness.controller.clone();
    let first_row = row.clone();
    let first = tokio::spawn(async move {
        first_controller
            .manual(&first_row, "reset", ADMIN_KEY)
            .await
    });
    harness.mock.wait_for(Method::POST, "/reset-quota", 1).await;
    let second_controller = harness.restart_controller();
    let second = second_controller.manual(&row, "reset", ADMIN_KEY).await;
    assert!(
        second.is_err(),
        "second controller must lose the disk lease"
    );
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    harness.mock.release_reset();
    let result = first.await.unwrap().unwrap();
    assert_eq!(result["consumed"], true);
    let manual = harness.controller.state(id).unwrap();
    assert_eq!(manual["stage"], "manual");
    assert_eq!(manual["consumption_outcome"], "consumed");
    assert!(manual["reset_completed_at"].is_string());
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 0);

    let repeated = harness
        .restart_controller()
        .manual(&row, "reset", ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(repeated["error_code"], "result_uncertain");
    run_automatic(&harness, &row, now + Duration::seconds(2)).await;
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 0);
}

#[tokio::test]
async fn two_automatic_controllers_share_the_disk_lease_and_only_one_consumes() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse(id, successful_sse());

    let gate = Arc::new(Notify::new());
    harness.mock.set_schedule_gate(false, gate);
    let first_controller = harness.controller.clone();
    let first_row = row.clone();
    let first = tokio::spawn(async move { first_controller.run_account(&first_row, now).await });
    harness.mock.wait_for(Method::POST, "/schedulable", 1).await;

    let second_controller = harness.restart_controller();
    second_controller.run_account(&row, now).await.unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    harness.mock.release_schedule();
    first.await.unwrap().unwrap();

    assert_eq!(harness.controller.state(id).unwrap()["stage"], "recovered");
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 2);

    harness
        .restart_controller()
        .run_account(&row, now + Duration::seconds(2))
        .await
        .unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/recover-state"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 2);
}

#[tokio::test]
async fn legacy_unknown_episodes_remain_quarantined_after_restarts_without_any_io() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    for (source, has_evidence) in [(Value::Null, false), (json!("manual"), true)] {
        let row = harness.seed_account(true, false).await;
        let id = integer(&row["id"]).unwrap();
        let episode = format!("fixture-legacy-unknown-{id}");
        let mut task = json!({
            "episode":episode,
            "stage":"uncertain",
            "attempt_at":datetime(now),
            "consumed":false,
            "consumption_outcome":"unknown",
            "error_code":"result_uncertain",
            "owns_pause":false,
            "next_at":null,
            "receipt":{"http_status":502,"business_code":"unrecognized"}
        });
        if !source.is_null() {
            task["source"] = source;
        }
        if has_evidence {
            task["evidence_at"] = json!(datetime(now - Duration::seconds(30)));
        }
        harness
            .controller
            .store
            .transaction(|state| {
                state["scheduler"][id.to_string()]["auto_reset_credit"] = task.clone();
                Ok(())
            })
            .unwrap();
        harness.set_config(recovery_config(id, "connection"));
        run_automatic(&harness, &row, now).await;
        assert_eq!(harness.controller.state(id).unwrap(), task);
        harness
            .restart_controller()
            .run_account(&row, now + Duration::hours(2))
            .await
            .unwrap();
        assert_eq!(harness.controller.state(id).unwrap(), task);
        assert_eq!(harness.mock.total(), 0);
        let state = harness.controller.store.snapshot().unwrap();
        assert!(state["recovery_history"].as_object().unwrap().is_empty());
        assert!(
            state["scheduler"][id.to_string()]["credit_result_notifications"]
                .as_object()
                .is_none_or(serde_json::Map::is_empty)
        );
        assert!(
            state["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"]
                .as_array()
                .is_none_or(Vec::is_empty)
        );
    }
}

#[tokio::test]
async fn manual_http_502_receipt_is_not_replayed_and_requires_a_fresh_quota_confirmation() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let row = harness.seed_account(true, false).await;
    let id = integer(&row["id"]).unwrap();
    harness.mock.set_reset(
        id,
        502,
        json!({"code":1,"message":"fixture ambiguous reset"}),
    );
    let first = harness
        .controller
        .manual(&row, "reset", ADMIN_KEY)
        .await
        .unwrap();
    assert_ne!(first["success"], true);
    assert_eq!(harness.controller.state(id).unwrap()["stage"], "uncertain");
    assert_eq!(
        harness.controller.state(id).unwrap()["consumption_outcome"],
        "unknown"
    );
    let owns_pause = harness.controller.state(id).unwrap()["owns_pause"].clone();
    assert!(owns_pause.is_null() || owns_pause == false);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);

    let repeated = harness
        .restart_controller()
        .manual(&row, "reset", ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(repeated["error_code"], "result_uncertain");
    harness.set_config(recovery_config(id, "connection"));
    run_automatic(&harness, &row, now + Duration::seconds(1)).await;
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 0);

    harness.mock.set_usage(
        id,
        200,
        usage_response(
            now + Duration::seconds(2),
            20.0,
            20.0,
            Some(snapshot(
                1,
                now + Duration::days(2),
                now + Duration::seconds(2),
            )),
            now + Duration::seconds(2),
            true,
        ),
    );
    let query = harness
        .controller
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Manual,
                reason: "fixture-independent-recovery-confirmation".into(),
                at: now + Duration::seconds(2),
                timeout: 3,
                ..QueryOptions::default()
            },
        )
        .await;
    assert_eq!(query["success"], true, "{query}");
    harness.set_now(now + Duration::seconds(3));
    let live = reload_account(&harness, id).await;
    run_automatic(&harness, &live, now + Duration::seconds(3)).await;
    let recovered = harness.controller.state(id).unwrap();
    assert_eq!(recovered["stage"], "recovered");
    assert_eq!(recovered["consumed"], false);
    assert_eq!(recovered["consumption_outcome"], "unknown");
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 0);
    assert!(
        harness.controller.store.snapshot().unwrap()["recovery_history"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn newer_depletion_after_manual_unknown_recovery_uses_a_new_episode_and_positive_card() {
    let harness = Harness::new(false).await;
    let initial = fixed_now();
    let mut row = harness.seed_account(true, false).await;
    let id = integer(&row["id"]).unwrap();
    let old_episode = "fixture-manual-recovered-old-episode";
    let legacy = json!({
        "episode":old_episode,
        "stage":"uncertain",
        "attempt_at":datetime(initial),
        "consumed":false,
        "consumption_outcome":"unknown",
        "error_code":"result_uncertain",
        "owns_pause":false,
        "receipt":{"http_status":502,"business_code":"unrecognized"}
    });
    harness
        .controller
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]["auto_reset_credit"] = legacy.clone();
            Ok(())
        })
        .unwrap();
    harness.set_config(recovery_config(id, "connection"));

    let recovered_at = initial + Duration::minutes(1);
    let quota = native_quota_data(recovered_at, 10.0, 20.0);
    reset_stored_quota(&harness.controller, &row, recovered_at, quota);
    harness.set_now(recovered_at);
    run_automatic(&harness, &row, recovered_at).await;
    let recovered = harness.controller.state(id).unwrap();
    assert_eq!(recovered["episode"], old_episode);
    assert_eq!(recovered["stage"], "recovered");
    assert_eq!(recovered["consumed"], false);
    assert_eq!(recovered["consumption_outcome"], "unknown");
    assert_eq!(harness.mock.total(), 0);

    let trigger = recovered_at + Duration::minutes(1);
    let reset = trigger + Duration::days(5);
    sqlx::query("UPDATE accounts SET rate_limited_at=$1,rate_limit_reset_at=$2,schedulable=true WHERE id=$3")
        .bind(trigger)
        .bind(reset)
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ops_error_logs WHERE account_id=$1")
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    insert_error(
        &harness,
        id,
        id + 1_000_000_700,
        trigger,
        429,
        "provider",
        "upstream",
    )
    .await;
    harness.set_now(trigger);
    row = reload_account(&harness, id).await;
    let mut exhausted = native_quota_data(trigger, 10.0, 100.0);
    exhausted["seven_day"]["resets_at"] = json!(datetime(reset));
    reset_stored_quota(&harness.controller, &row, trigger, exhausted);
    row = set_credit_snapshot(&harness, id, snapshot(0, trigger, trigger)).await;
    run_automatic(&harness, &row, trigger).await;
    let no_card = harness.controller.state(id).unwrap();
    assert_eq!(no_card["episode"], old_episode);
    assert_eq!(no_card["stage"], "recovered");
    assert_eq!(
        harness.controller.store.snapshot().unwrap()["scheduler"][id.to_string()]
            ["credit_admission"]["reason"],
        "no_credit"
    );
    assert_eq!(harness.mock.total(), 0);

    row = set_credit_snapshot(
        &harness,
        id,
        snapshot(1, trigger + Duration::days(2), trigger),
    )
    .await;
    let slot_a = harness.controller.slots.acquire().unwrap();
    let slot_b = harness.controller.slots.acquire().unwrap();
    harness.set_now(trigger + Duration::seconds(1));
    run_automatic(&harness, &row, trigger + Duration::seconds(1)).await;
    let fresh = harness.controller.state(id).unwrap();
    assert_ne!(fresh["episode"], old_episode);
    assert!(fresh["source"].is_null());
    assert_eq!(fresh["policy_version"], 1);
    assert_eq!(fresh["stage"], "waiting");
    assert_eq!(fresh["error_code"], "test_slots");
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    drop(slot_a);
    drop(slot_b);
}

#[tokio::test]
async fn bark_delivery_has_an_atomic_reservation_five_attempt_limit_and_no_business_replay() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let first_id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
    harness
        .seed_notice(first_id, "fixture-bark-once", now)
        .await;
    let bark = Bark::new(harness.settings.clone()).unwrap();
    let gate = Arc::new(Notify::new());
    harness.mock.set_push_gate(gate);
    let first_controller = harness.controller.clone();
    let first = tokio::spawn(async move { first_controller.deliver_results(&bark).await });
    harness.mock.wait_for(Method::POST, "/push", 1).await;
    let second_controller = harness.restart_controller();
    let bark2 = Bark::new(harness.settings.clone()).unwrap();
    second_controller.deliver_results(&bark2).await.unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/push"), 1);
    harness.mock.release_push();
    first.await.unwrap().unwrap();
    let delivered = &harness.controller.store.snapshot().unwrap()["scheduler"]
        [first_id.to_string()]["credit_result_notifications"]["fixture-bark-once"];
    assert_eq!(delivered["status"], "delivered");
    assert_eq!(delivered["attempts"], 1);

    let push = bark_calls(&harness);
    assert_eq!(push.len(), 1);
    assert_eq!(
        push[0].headers.get("content-type").unwrap(),
        "application/json"
    );
    assert_eq!(push[0].body["device_key"], "fixture-bark-device-key");
    assert_eq!(push[0].body["level"], "critical");
    assert_eq!(push[0].body["sound"], "alarm");
    assert_eq!(push[0].body["group"], "oauth-reset-credit");
    assert!(!push[0].body.to_string().contains(ADMIN_KEY));

    let retry_id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
    harness
        .seed_notice(retry_id, "fixture-bark-five", now)
        .await;
    harness.mock.set_bark_codes([500, 500, 500, 500, 500]);
    let bark = Bark::new(harness.settings.clone()).unwrap();
    for offset in [0, 5, 35, 155, 755] {
        harness.set_now(now + Duration::seconds(offset));
        harness.controller.deliver_results(&bark).await.unwrap();
    }
    let failed = &harness.controller.store.snapshot().unwrap()["scheduler"][retry_id.to_string()]
        ["credit_result_notifications"]["fixture-bark-five"];
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["attempts"], 5);
    assert!(failed["next_at"].is_null());
    assert_eq!(harness.mock.count(Method::POST, "/push"), 6);
    harness.set_now(now + Duration::seconds(2_000));
    harness.controller.deliver_results(&bark).await.unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/push"), 6);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);

    let disabled_id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
    harness
        .seed_notice(disabled_id, "fixture-bark-disabled", now)
        .await;
    let disabled = Bark::new(harness.settings.clone()).unwrap();
    disabled.configure(
        &json!({
            "enabled":false,
            "device_key":"fixture-bark-device-key",
            "server_url":harness.settings.base_url
        }),
        true,
    );
    harness.controller.deliver_results(&disabled).await.unwrap();
    let suppressed = &harness.controller.store.snapshot().unwrap()["scheduler"]
        [disabled_id.to_string()]["credit_result_notifications"]["fixture-bark-disabled"];
    assert_eq!(suppressed["status"], "suppressed");
    assert_eq!(suppressed["attempts"], 0);
    assert!(suppressed["next_at"].is_null());
    assert_eq!(harness.mock.count(Method::POST, "/push"), 6);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
}

#[tokio::test]
async fn terminal_credit_outcomes_and_bark_receipts_survive_restart_once() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let bark = Bark::new(harness.settings.clone()).unwrap();
    for expected_outcome in [
        "restored",
        "not_executed",
        "consume_failed",
        "unrecovered",
        "unknown",
    ] {
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        harness.set_config(recovery_config(id, "connection"));
        let before_reset = harness.mock.count(Method::POST, "/reset-quota");
        let before_test = harness.mock.count(Method::POST, "/test");
        let before_push = harness.mock.count(Method::POST, "/push");
        match expected_outcome {
            "restored" => {
                harness.mock.set_reset(
                    id,
                    200,
                    reset_response(now, 20.0, 20.0, "success", json!(1)),
                );
                harness.mock.set_sse(id, successful_sse());
                run_automatic(&harness, &row, now).await;
            }
            "not_executed" => {
                let slot_a = harness.controller.slots.acquire().unwrap();
                let slot_b = harness.controller.slots.acquire().unwrap();
                run_automatic(&harness, &row, now).await;
                assert_eq!(harness.controller.state(id).unwrap()["stage"], "waiting");
                assert_eq!(
                    harness.mock.count(Method::POST, "/reset-quota"),
                    before_reset
                );
                let no_credit =
                    set_credit_snapshot(&harness, id, snapshot(0, now + Duration::days(2), now))
                        .await;
                drop(slot_a);
                drop(slot_b);
                harness.set_now(now + Duration::seconds(2));
                run_automatic(&harness, &no_credit, now + Duration::seconds(2)).await;
            }
            "consume_failed" => {
                harness.mock.set_reset(
                    id,
                    200,
                    json!({"code":0,"data":{"code":"nothing_to_reset","windows_reset":0}}),
                );
                run_automatic(&harness, &row, now).await;
            }
            "unrecovered" => {
                harness.mock.set_reset(
                    id,
                    200,
                    reset_response(now, 20.0, 20.0, "success", json!(1)),
                );
                harness.mock.set_sse_sequence(
                    id,
                    [
                        explicit_sse_error("http_502"),
                        explicit_sse_error("http_502"),
                    ],
                );
                run_automatic(&harness, &row, now).await;
                harness.set_now(now + Duration::seconds(60));
                run_automatic(&harness, &row, now + Duration::seconds(60)).await;
            }
            "unknown" => {
                harness
                    .mock
                    .set_reset(id, 502, json!({"code":1,"message":"fixture HTTP failure"}));
                run_automatic(&harness, &row, now).await;
            }
            _ => unreachable!(),
        }
        let task = harness.controller.state(id).unwrap();
        assert_eq!(
            task["outcome"], expected_outcome,
            "{expected_outcome}: {task}"
        );
        let notices = &harness.controller.store.snapshot().unwrap()["scheduler"][id.to_string()]
            ["credit_result_notifications"];
        assert_eq!(notices.as_object().unwrap().len(), 1, "{expected_outcome}");
        let episode = task["episode"].as_str().unwrap();
        let notice = &notices[episode];
        assert_eq!(notice["status"], "pending", "{expected_outcome}");
        assert_eq!(notice["outcome"], expected_outcome, "{expected_outcome}");

        harness.controller.deliver_results(&bark).await.unwrap();
        let delivered = &harness.controller.store.snapshot().unwrap()["scheduler"][id.to_string()]
            ["credit_result_notifications"][episode];
        assert_eq!(delivered["status"], "delivered", "{expected_outcome}");
        assert_eq!(delivered["attempts"], 1, "{expected_outcome}");
        assert_eq!(harness.mock.count(Method::POST, "/push"), before_push + 1);

        harness
            .restart_controller()
            .run_account(&row, now + Duration::hours(1))
            .await
            .unwrap();
        harness.controller.deliver_results(&bark).await.unwrap();
        assert_eq!(
            harness.controller.state(id).unwrap()["outcome"],
            expected_outcome
        );
        assert_eq!(
            harness.mock.count(Method::POST, "/reset-quota"),
            before_reset
                + match expected_outcome {
                    "not_executed" => 0,
                    "unknown" => 1,
                    _ => 1,
                }
        );
        assert_eq!(harness.mock.count(Method::POST, "/push"), before_push + 1);
        let after = &harness.controller.store.snapshot().unwrap()["scheduler"][id.to_string()]
            ["credit_result_notifications"][episode];
        assert_eq!(after["status"], "delivered");
        assert_eq!(after["attempts"], 1);
        if expected_outcome == "unrecovered" {
            assert_eq!(harness.mock.count(Method::POST, "/test"), before_test + 2);
        } else if expected_outcome == "restored" {
            assert_eq!(harness.mock.count(Method::POST, "/test"), before_test + 1);
        } else {
            assert_eq!(harness.mock.count(Method::POST, "/test"), before_test);
        }
        harness.set_now(now);
    }
}

#[tokio::test]
async fn coordinator_observation_requires_fresh_explicit_matching_database_and_state_receipts() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let good = snapshot(1, now + Duration::days(2), now);
    let cases = [
        (
            "fresh_match",
            Some(good.clone()),
            now,
            true,
            true,
            false,
            true,
        ),
        ("no_card", None, now, true, true, false, false),
        (
            "cache_unconfirmed",
            Some(good.clone()),
            now,
            false,
            true,
            false,
            false,
        ),
        (
            "db_mismatch",
            Some(snapshot(1, now + Duration::days(3), now)),
            now,
            true,
            false,
            false,
            false,
        ),
        (
            "credentials_changed",
            Some(good.clone()),
            now,
            true,
            true,
            true,
            false,
        ),
        (
            "stale_fetched_at",
            Some(good.clone()),
            now - Duration::seconds(2),
            true,
            true,
            false,
            false,
        ),
        (
            "future_fetched_at",
            Some(good.clone()),
            now + Duration::seconds(2),
            true,
            true,
            false,
            false,
        ),
    ];
    for (
        name,
        card,
        fetched,
        cache_persisted,
        writeback,
        change_credentials,
        expected_observation,
    ) in cases
    {
        let row = harness.seed_account(true, false).await;
        let id = integer(&row["id"]).unwrap();
        harness.mock.set_usage(
            id,
            200,
            usage_response(now, 20.0, 20.0, card, fetched, cache_persisted),
        );
        harness.mock.set_usage_writeback(id, writeback);
        harness
            .mock
            .set_usage_credential_change(id, change_credentials);
        let before = harness.mock.count(Method::GET, "/usage");
        let result = harness
            .controller
            .queries
            .query(
                id,
                ADMIN_KEY,
                QueryOptions {
                    source: Source::Manual,
                    reason: format!("fixture-observation-{name}"),
                    at: now,
                    timeout: 3,
                    ..QueryOptions::default()
                },
            )
            .await;
        assert_eq!(result["success"], true, "{name}: {result}");
        assert_eq!(harness.mock.count(Method::GET, "/usage"), before + 1);
        let state = harness.controller.store.snapshot().unwrap();
        let observation = &state["scheduler"][id.to_string()]["reset_credit_observation"];
        assert_eq!(observation.is_object(), expected_observation, "{name}");
        if expected_observation {
            assert_eq!(observation["available_count"], 1);
            assert_eq!(observation["observed_at"], datetime(now));
            assert_eq!(
                observation["credential_fingerprint"],
                oauth_admission::credential_fingerprint(&row)
            );
            assert_eq!(observation["window_minutes"], 10080);
            assert_eq!(observation.as_object().unwrap().len(), 9);
            for field in [
                "snapshot_sha256",
                "credential_fingerprint",
                "limit_fingerprint",
            ] {
                assert_eq!(observation[field].as_str().unwrap().len(), 64);
            }
            assert!(!observation.to_string().contains(ADMIN_KEY));
        } else {
            assert_eq!(state["oauth_results"][id.to_string()]["success"], true);
            assert!(
                state["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"]
                    .as_array()
                    .is_none_or(|attempts| attempts.is_empty())
            );
        }
    }

    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness
        .controller
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]
                .as_object_mut()
                .unwrap()
                .remove("reset_credit_observation");
            Ok(())
        })
        .unwrap();
    let card = snapshot(1, now + Duration::days(2), now);
    let mut undated_card = card.clone();
    undated_card.as_object_mut().unwrap().remove("fetched_at");
    let row = set_credit_snapshot(&harness, id, undated_card).await;
    harness.mock.set_usage(
        id,
        200,
        usage_response(now, 100.0, 100.0, Some(card), now, true),
    );
    harness.mock.set_usage_writeback(id, false);
    harness.mock.trip_lock_after(Method::GET, "/usage");
    let failed = harness
        .controller
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Manual,
                reason: "fixture-observation-write-failure".into(),
                at: now,
                timeout: 3,
                ..QueryOptions::default()
            },
        )
        .await;
    assert_eq!(failed["error_code"], "query_state_unavailable");
    let lock_path = PathBuf::from(format!("{}.lock", harness.settings.oauth_path.display()));
    fs::remove_dir(&lock_path).expect("remove only this fixture lock fault");
    let after_failed_write = harness.controller.store.snapshot().unwrap();
    assert!(after_failed_write["scheduler"][id.to_string()]["reset_credit_observation"].is_null());
    harness.mock.set_usage(
        id,
        503,
        json!({"code":1,"message":"fixture fresh read unavailable"}),
    );
    harness.set_config(recovery_config(id, "connection"));
    run_automatic(&harness, &row, now).await;
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 0);
}

#[tokio::test]
async fn usage_refresh_http_429_is_status_only_and_cannot_create_an_observation() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness
        .controller
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]
                .as_object_mut()
                .unwrap()
                .remove("reset_credit_observation");
            Ok(())
        })
        .unwrap();
    harness.mock.set_usage(
        id,
        429,
        json!({
            "code":"fixture-card-secret",
            "message":"fixture-access-token",
            "card_id":"fixture-card-id"
        }),
    );
    let before_reset = harness.mock.count(Method::POST, "/reset-quota");
    let before_schedule = harness.mock.count(Method::POST, "/schedulable");
    let result = harness
        .controller
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Manual,
                reason: "fixture-refresh-http-429".into(),
                at: now,
                timeout: 3,
                ..QueryOptions::default()
            },
        )
        .await;
    assert_eq!(result["success"], false);
    assert_eq!(result["error_code"], "http_429");
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 1);
    assert_eq!(
        harness.mock.count(Method::POST, "/reset-quota"),
        before_reset
    );
    assert_eq!(
        harness.mock.count(Method::POST, "/schedulable"),
        before_schedule
    );
    let state = harness.controller.store.snapshot().unwrap();
    assert!(state["scheduler"][id.to_string()]["reset_credit_observation"].is_null());
    assert_eq!(
        state["scheduler"][id.to_string()]["last_error_code"],
        "http_429"
    );
    for private_value in [
        "fixture-card-secret",
        "fixture-access-token",
        "fixture-card-id",
    ] {
        assert!(!result.to_string().contains(private_value));
        assert!(!state.to_string().contains(private_value));
    }
    let audit = fs::read_to_string(&harness.settings.audit_path).unwrap_or_default();
    assert!(audit.contains("http_429"));
    for private_value in [
        "fixture-card-secret",
        "fixture-access-token",
        "fixture-card-id",
    ] {
        assert!(!audit.contains(private_value));
    }
}

#[tokio::test]
async fn corrupt_native_observation_cannot_fall_back_to_a_legacy_fetched_timestamp() {
    let now = fixed_now();
    for (field, bad_value) in [
        ("snapshot_sha256", json!("invalid")),
        ("observed_at", json!("invalid-time")),
    ] {
        let harness = Harness::new(false).await;
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        let mut legacy_snapshot = row["extra"]["codex_reset_credit_snapshot"].clone();
        legacy_snapshot["fetched_at"] = json!(datetime(now - Duration::hours(2)));
        let row = set_credit_snapshot(&harness, id, legacy_snapshot).await;
        let mut state = harness.controller.store.snapshot().unwrap();
        state["scheduler"][id.to_string()]["reset_credit_observation"][field] = bad_value.clone();
        fs::write(
            &harness.settings.oauth_path,
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        harness.set_config(recovery_config(id, "connection"));

        let result = harness.controller.run_account(&row, now).await;
        assert!(result.is_err(), "corrupt {field} was accepted");
        assert_eq!(harness.mock.total(), 0, "corrupt {field} authorized I/O");
        assert_eq!(harness.mock.count(Method::GET, "/usage"), 0);
        assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
        assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
        assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 0);
        let stored =
            serde_json::from_slice::<Value>(&fs::read(&harness.settings.oauth_path).unwrap())
                .unwrap();
        assert_eq!(
            stored["scheduler"][id.to_string()]["auto_reset_credit"],
            Value::Null
        );
        assert_eq!(
            stored["scheduler"][id.to_string()]["reset_credit_observation"][field],
            bad_value
        );
    }
}

#[tokio::test]
async fn usage_auth_failure_is_scoped_to_credentials_and_never_schedules_card_replay() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness
        .controller
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]
                .as_object_mut()
                .unwrap()
                .remove("reset_credit_observation");
            Ok(())
        })
        .unwrap();
    harness.mock.set_usage(
        id,
        401,
        json!({
            "code":"fixture-auth-secret",
            "message":"fixture-access-token",
            "card_id":"fixture-card-id"
        }),
    );
    let result = harness
        .controller
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Manual,
                reason: "fixture-usage-http-401".into(),
                at: now,
                timeout: 3,
                ..QueryOptions::default()
            },
        )
        .await;
    assert_eq!(result["error_code"], "http_401");
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 1);
    let state = harness.controller.store.snapshot().unwrap();
    let query = &state["scheduler"][id.to_string()]["quota_query"];
    assert_eq!(
        query["auth_fingerprint"],
        oauth_admission::credential_fingerprint(&row)
    );
    assert_eq!(
        state["scheduler"][id.to_string()]["last_error_code"],
        "http_401"
    );
    assert!(state["scheduler"][id.to_string()]["auto_reset_credit"].is_null());

    harness.set_config(recovery_config(id, "connection"));
    run_automatic(&harness, &row, now + Duration::seconds(30)).await;
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 0);

    sqlx::query("UPDATE accounts SET credentials=jsonb_set(credentials,'{access_token}',to_jsonb($1::text),true) WHERE id=$2")
        .bind("fixture-rotated-auth-token")
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    let changed = reload_account(&harness, id).await;
    run_automatic(&harness, &changed, now + Duration::seconds(60)).await;
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 0);
    for private_value in [
        "fixture-auth-secret",
        "fixture-access-token",
        "fixture-card-id",
    ] {
        assert!(!result.to_string().contains(private_value));
        assert!(!harness
            .controller
            .store
            .snapshot()
            .unwrap()
            .to_string()
            .contains(private_value));
    }
}

#[tokio::test]
async fn confirmed_card_use_tests_immediately_despite_cache_warning_and_stops_on_bad_quota() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    for (name, malformed_quota) in [("cache_warning", false), ("malformed_quota", true)] {
        let expected_reset = harness.mock.count(Method::POST, "/reset-quota") + 1;
        let expected_test = harness.mock.count(Method::POST, "/test") + 1;
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        harness.set_config(recovery_config(id, "connection"));
        let quota = if malformed_quota {
            json!({
                "fetched_at":datetime(now + Duration::seconds(2)),
                "rate_limit":{
                    "primary_window":{"limit_window_seconds":"bad","used_percent":20,"reset_at":datetime(now + Duration::hours(6))},
                    "secondary_window":{}
                }
            })
        } else {
            let mut quota = native_quota_data(now, 20.0, 20.0);
            quota["fetched_at"] = json!(datetime(now + Duration::seconds(2)));
            quota
        };
        harness.mock.set_reset(
            id,
            200,
            json!({
                "code":0,
                "data":{
                    "code":"reset",
                    "windows_reset":1,
                    "quota":quota,
                    "cache_refreshed":false,
                    "cache_persisted":false
                }
            }),
        );
        harness.mock.set_sse(id, successful_sse());
        run_automatic(&harness, &row, now).await;
        let task = harness.controller.state(id).unwrap();
        assert_eq!(task["consumed"], true, "{name}");
        assert_eq!(task["receipt"]["cache_refreshed"], false, "{name}");
        assert_eq!(task["receipt"]["cache_persisted"], false, "{name}");
        assert_eq!(
            harness.mock.count(Method::POST, "/reset-quota"),
            expected_reset
        );
        assert_eq!(harness.mock.count(Method::POST, "/test"), expected_test);
        assert_eq!(harness.mock.count(Method::GET, "/usage"), 0);
        if malformed_quota {
            assert_eq!(task["stage"], "finished", "{name}");
            assert_eq!(task["result_reason"], "incomplete_quota", "{name}");
        }
        harness.set_now(now + Duration::hours(1));
        harness
            .restart_controller()
            .run_account(&row, now + Duration::hours(1))
            .await
            .unwrap();
        assert_eq!(
            harness.mock.count(Method::POST, "/reset-quota"),
            expected_reset
        );
        assert_eq!(harness.mock.count(Method::POST, "/test"), expected_test);
        assert_eq!(harness.mock.count(Method::GET, "/usage"), 0);
        harness.set_now(now);
    }
}

#[tokio::test]
async fn read_only_credit_runtime_dispatches_nothing_and_leaves_no_state_or_lock() {
    let harness = Harness::new(true).await;
    let now = fixed_now();
    let row = harness.seed_account(true, true).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));

    run_automatic(&harness, &row, now).await;
    let query = harness
        .controller
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Manual,
                reason: "fixture-read-only".into(),
                at: now,
                timeout: 3,
                ..QueryOptions::default()
            },
        )
        .await;
    assert_eq!(query["error_code"], "read_only");
    let bark = Bark::new(harness.settings.clone()).unwrap();
    harness.controller.deliver_results(&bark).await.unwrap();
    let state_path = harness.settings.oauth_path.clone();
    let lock_path = PathBuf::from(format!("{}.lock", state_path.display()));
    assert!(!state_path.exists());
    assert!(!lock_path.exists());
    assert!(!harness.settings.audit_path.exists());
    assert_eq!(harness.mock.total(), 0);
}

#[tokio::test]
async fn explicit_loopback_card_observation_authorizes_one_real_reset_and_immediate_test() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    let card = snapshot(2, now + Duration::days(2), now);
    harness.mock.set_usage(
        id,
        200,
        usage_response(now, 100.0, 100.0, Some(card), now, true),
    );
    let query = harness
        .controller
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Manual,
                reason: "fixture-observe-before-consume".into(),
                at: now,
                timeout: 3,
                ..QueryOptions::default()
            },
        )
        .await;
    assert_eq!(query["success"], true, "{query}");
    let observed_state = harness.controller.store.snapshot().unwrap();
    let observation = &observed_state["scheduler"][id.to_string()]["reset_credit_observation"];
    assert_eq!(observation["available_count"], 2);
    assert_eq!(observation["observed_at"], datetime(now));
    assert_eq!(
        observation["window_reset_at"],
        datetime(now + Duration::days(5))
    );
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 1);

    let live = reload_account(&harness, id).await;
    harness.set_config(recovery_config(id, "connection"));
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.set_sse(id, successful_sse());
    harness
        .restart_controller()
        .run_account(&live, now)
        .await
        .unwrap();
    let task = harness.controller.state(id).unwrap();
    assert_eq!(task["stage"], "recovered");
    assert_eq!(task["consumption_outcome"], "consumed");
    assert_eq!(task["receipt"]["business_code"], "success");
    let automatic_attempts = harness.controller.store.snapshot().unwrap()["scheduler"]
        [id.to_string()]["quota_query"]["automatic_attempts"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(automatic_attempts, 1);
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
}

#[tokio::test]
async fn waiting_episode_rejects_changed_snapshot_credentials_window_and_limit_without_supplement()
{
    let harness = Harness::new(false).await;
    let now = fixed_now();
    for change in ["snapshot", "credentials", "window", "limit"] {
        let (row, _) = harness.seed_depletion(&harness.controller).await;
        let id = integer(&row["id"]).unwrap();
        harness.set_config(recovery_config(id, "connection"));
        let slot_a = harness.controller.slots.acquire().unwrap();
        let slot_b = harness.controller.slots.acquire().unwrap();
        run_automatic(&harness, &row, now).await;
        let waiting = harness.controller.state(id).unwrap();
        assert_eq!(waiting["stage"], "waiting", "{change}");
        assert_eq!(
            harness.mock.total(),
            0,
            "{change} dispatched before its test slot was available"
        );

        match change {
            "snapshot" => {
                set_credit_snapshot(&harness, id, snapshot(1, now + Duration::days(4), now)).await;
            }
            "credentials" => {
                sqlx::query(
                    "UPDATE accounts SET credentials=jsonb_set(credentials,'{access_token}',to_jsonb($1::text),true) WHERE id=$2",
                )
                .bind(format!("fixture-changed-token-{id}"))
                .bind(id)
                .execute(&harness.db.pool)
                .await
                .unwrap();
            }
            "window" => {
                let mut quota = native_quota_data(now, 100.0, 100.0);
                quota["seven_day"]["resets_at"] = json!(datetime(now + Duration::days(6)));
                reset_stored_quota(&harness.controller, &row, now, quota);
            }
            "limit" => {
                let new_limited = now - Duration::seconds(30);
                sqlx::query("UPDATE accounts SET rate_limited_at=$1 WHERE id=$2")
                    .bind(new_limited)
                    .bind(id)
                    .execute(&harness.db.pool)
                    .await
                    .unwrap();
                insert_error(
                    &harness,
                    id,
                    id + 1_000_000_500,
                    new_limited,
                    429,
                    "provider",
                    "upstream",
                )
                .await;
            }
            _ => unreachable!(),
        }
        let changed = reload_account(&harness, id).await;
        drop(slot_a);
        drop(slot_b);
        harness.set_now(now + Duration::seconds(2));
        run_automatic(&harness, &changed, now + Duration::seconds(2)).await;
        let terminal = harness.controller.state(id).unwrap();
        assert_ne!(terminal["stage"], "recovered", "{change}");
        assert_ne!(terminal["consumed"], true, "{change}");
        assert_eq!(harness.mock.count(Method::GET, "/usage"), 0, "{change}");
        assert_eq!(
            harness.mock.count(Method::POST, "/reset-quota"),
            0,
            "{change}"
        );
        assert_eq!(harness.mock.count(Method::POST, "/test"), 0, "{change}");
        harness.set_now(now);
    }
}

#[tokio::test]
async fn automatic_usage_cooldown_and_six_per_day_budget_survive_restart() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let row = harness.seed_account(true, false).await;
    let id = integer(&row["id"]).unwrap();
    let query = |at| QueryOptions {
        source: Source::Automatic,
        reason: "fixture-budget".into(),
        at,
        timeout: 3,
        ..QueryOptions::default()
    };
    let first = harness
        .controller
        .queries
        .query(id, ADMIN_KEY, query(now))
        .await;
    assert_eq!(first["success"], true);
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 1);
    let cooldown = harness
        .controller
        .queries
        .query(id, ADMIN_KEY, query(now + Duration::seconds(3_599)))
        .await;
    assert_eq!(cooldown["error_code"], "query_cooldown");
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 1);
    for hour in 1..6 {
        let at = now + Duration::hours(hour);
        harness.set_now(at);
        let result = harness
            .controller
            .queries
            .query(id, ADMIN_KEY, query(at))
            .await;
        assert_eq!(result["success"], true, "hour {hour}: {result}");
    }
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 6);
    let saved = harness.controller.store.snapshot().unwrap();
    assert_eq!(
        saved["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    let over_budget_at = now + Duration::hours(6);
    harness.set_now(over_budget_at);
    let blocked = harness
        .controller
        .queries
        .query(id, ADMIN_KEY, query(over_budget_at))
        .await;
    assert_eq!(blocked["error_code"], "query_budget");
    let restarted = harness.restart_controller();
    let before_expiry = now + Duration::hours(24) - Duration::microseconds(1);
    harness.set_now(before_expiry);
    let still_blocked = restarted
        .queries
        .query(id, ADMIN_KEY, query(before_expiry))
        .await;
    assert_eq!(still_blocked["error_code"], "query_budget");
    let expires = now + Duration::hours(24);
    harness.set_now(expires);
    let allowed = restarted.queries.query(id, ADMIN_KEY, query(expires)).await;
    assert_eq!(allowed["success"], true, "{allowed}");
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 7);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
}

#[test]
fn recovery019_free_plan_preserves_the_full_primary_thirty_day_quota_result() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../tests/contracts/credit-lifecycle/fixtures.json"
    ))
    .expect("fixed-SHA Recovery019 oracle fixture must parse");
    let oracle = fixtures["oracle_outputs"]["recovery019_free_primary_thirty_day"].clone();
    let input = &oracle["input"];
    let at = parse_datetime(&input["at"]).expect("fixed legacy observation time");
    let actual = credit_rules::quota_result(&input["data"], &input["row"], at, None);
    assert_eq!(actual, oracle["expected"]);
    assert_eq!(actual["success"], true);
    assert_eq!(actual["oauth_quota"]["plan_type"], "free");
    let windows = actual["oauth_quota"]["ui_windows"].as_array().unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["key"], "codex_7d");
    assert_eq!(windows[0]["reset_at"], input["expected_reset_at"]);
    assert!(windows[0]["used_percent"].as_f64().unwrap() < 100.0);
}

#[tokio::test]
async fn admission_pause_and_consume_receipt_write_failures_never_dispatch_again() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let lock_path = PathBuf::from(format!("{}.lock", harness.settings.oauth_path.display()));

    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    fs::remove_file(&lock_path).unwrap();
    fs::create_dir(&lock_path).unwrap();
    assert!(harness.controller.run_account(&row, now).await.is_err());
    assert_eq!(harness.mock.total(), 0);
    fs::remove_dir(&lock_path).unwrap();
    assert!(harness
        .controller
        .state(id)
        .unwrap()
        .as_object()
        .unwrap()
        .is_empty());

    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    harness.mock.trip_lock_after(Method::POST, "/schedulable");
    assert!(harness.controller.run_account(&row, now).await.is_err());
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    fs::remove_dir(&lock_path).unwrap();
    harness
        .restart_controller()
        .run_account(&row, now + Duration::seconds(2))
        .await
        .unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
    assert_ne!(harness.controller.state(id).unwrap()["stage"], "recovered");
    assert_eq!(reload_account(&harness, id).await["schedulable"], false);
    let paused_final = harness.controller.state(id).unwrap();
    assert_eq!(paused_final["stage"], "finished");
    assert_eq!(paused_final["result_reason"], "pause_uncertain");
    assert_eq!(paused_final["outcome"], "not_executed");
    let paused_state = harness.controller.store.snapshot().unwrap();
    let paused_notices = paused_state["scheduler"][id.to_string()]["credit_result_notifications"]
        .as_object()
        .unwrap();
    assert_eq!(paused_notices.len(), 1);
    let paused_notice = &paused_notices[paused_final["episode"].as_str().unwrap()];
    assert_eq!(paused_notice["outcome"], "not_executed");
    assert_eq!(paused_notice["reason_code"], "pause_uncertain");

    let (row, _) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "connection"));
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    harness.mock.trip_lock_after(Method::POST, "/reset-quota");
    assert!(harness.controller.run_account(&row, now).await.is_err());
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    fs::remove_dir(&lock_path).unwrap();
    let reserved = harness.controller.state(id).unwrap();
    assert!(reserved["attempt_at"].is_string());
    assert_ne!(reserved["consumed"], true);
    harness
        .restart_controller()
        .run_account(&row, now + Duration::minutes(6))
        .await
        .unwrap();
    let final_task = harness.controller.state(id).unwrap();
    assert_eq!(final_task["stage"], "finished");
    assert_eq!(final_task["outcome"], "unknown");
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 2);
}

#[tokio::test]
async fn confirmed_receipt_survives_a_later_state_write_failure_without_reset_replay() {
    let harness = Harness::new(false).await;
    let now = fixed_now();
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, _) = harness.seed_depletion(&harness.controller).await;
    harness.mock.set_reset(
        id,
        200,
        reset_response(now, 20.0, 20.0, "success", json!(1)),
    );
    let state_path = harness.settings.oauth_path.clone();
    let watcher = std::thread::spawn(move || {
        let lock_path = PathBuf::from(format!("{}.lock", state_path.display()));
        let deadline = StdInstant::now() + StdDuration::from_secs(5);
        loop {
            if let Ok(bytes) = fs::read(&state_path) {
                if serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .is_some_and(|state| {
                        state["scheduler"][id.to_string()]["auto_reset_credit"]["consumed"] == true
                    })
                {
                    if lock_path.is_file() {
                        fs::remove_file(&lock_path).unwrap();
                    }
                    fs::create_dir(&lock_path).unwrap();
                    return true;
                }
            }
            if StdInstant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }
    });
    let result = harness.controller.run_account(&row, now).await;
    assert!(
        watcher.join().unwrap(),
        "consumption receipt was not persisted"
    );
    assert!(result.is_err());
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    let lock_path = PathBuf::from(format!("{}.lock", harness.settings.oauth_path.display()));
    fs::remove_dir(&lock_path).unwrap();
    let consumed = harness.controller.state(id).unwrap();
    assert_eq!(consumed["consumed"], true);
    assert_eq!(consumed["receipt"]["business_code"], "success");
    let receipt = consumed["receipt"].clone();
    harness
        .restart_controller()
        .run_account(&row, now + Duration::seconds(2))
        .await
        .unwrap();
    let after_restart = harness.controller.state(id).unwrap();
    assert_eq!(after_restart["consumed"], true);
    assert_eq!(after_restart["receipt"], receipt);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
}
