//! Runtime model recovery tests replace only the final HTTP destination with loopback.
use crate::{
    compat::{account_version, datetime, integer, parse_datetime},
    credit_rules,
    db::Database,
    model_target, modeltrace, oauth_admission, quota, recovery_events, recovery_rules,
    reset_credit_observation,
    runtime::Runtime,
    settings::Settings,
};
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
    collections::BTreeMap,
    fs,
    sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration as StdDuration,
};
use tokio::{
    net::TcpListener,
    sync::{Notify, Semaphore},
    task::JoinHandle,
};

const ADMIN_KEY: &str = "fixture-admin-automation-contract";
const SUCCESS_TEST_SSE: &str = "data: {\"type\":\"test_start\",\"model\":\"fixture-model\"}\n\ndata: {\"type\":\"test_complete\",\"success\":true}\n\n";
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(19_400_000);

fn reference_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2031, 4, 5, 6, 7, 8).unwrap()
}

fn sample(name: &str) -> String {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/contracts/modeltrace-runtime/fixtures.json"
    ))
    .unwrap();
    fixture["modeltrace_runtime"][name]["outputs"][0]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn global_detection_model_has_its_own_default() {
    let temp = tempfile::tempdir().unwrap();
    let oauth_path = temp.path().join("oauth-config.json");
    let bark_path = temp.path().join("bark-config.json");
    fs::write(&oauth_path, "{}").unwrap();
    fs::write(&bark_path, "{}").unwrap();
    let mut env = BTreeMap::new();
    env.insert(
        "DATABASE_URL".into(),
        "postgres://example.invalid/sub2ops".into(),
    );
    env.insert(
        "OAUTH_CONFIG_PATH".into(),
        oauth_path.to_string_lossy().into_owned(),
    );
    env.insert(
        "BARK_CONFIG_PATH".into(),
        bark_path.to_string_lossy().into_owned(),
    );

    let settings = Settings::from_env(&env, false).unwrap();
    assert_eq!(settings.oauth["model_detection_model_id"], "gpt-6-luna");
    assert_eq!(
        settings.oauth["oauth_recovery_test_model_id"],
        crate::model_downgrade::DEFAULT_TEST_MODEL
    );
    assert_ne!(
        settings.oauth["model_detection_model_id"],
        settings.oauth["oauth_recovery_test_model_id"]
    );
}

fn method_name(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::DELETE => "DELETE",
        Method::PUT => "PUT",
        _ => "OTHER",
    }
}

#[derive(Clone)]
struct Call {
    method: String,
    path: String,
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
struct BulkGate {
    entered: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

impl BulkGate {
    fn new() -> Self {
        Self {
            entered: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

#[derive(Clone)]
struct MockUpstream {
    pool: PgPool,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    calls: Arc<StdMutex<Vec<Call>>>,
    unexpected: Arc<StdMutex<Vec<String>>>,
    test_sse: Arc<StdMutex<String>>,
    model_gate: Arc<StdMutex<Option<Gate>>>,
    recovery_gate: Arc<StdMutex<Option<Gate>>>,
    usage_gate: Arc<StdMutex<Option<Gate>>>,
    usage_cards: Arc<StdMutex<Option<Value>>>,
    model_status: Arc<StdMutex<u16>>,
    model_output: Arc<StdMutex<String>>,
    expected_responses_model: Arc<StdMutex<String>>,
    reset_uncertain: Arc<StdMutex<bool>>,
    temp_delete: Arc<StdMutex<TempDelete>>,
    temp_delete_gate: Arc<StdMutex<Option<Gate>>>,
    bulk_delete_gate: Arc<StdMutex<Option<BulkGate>>>,
    delete_started: Arc<Notify>,
}

#[derive(Clone, Copy)]
struct TempDelete {
    status: u16,
    clear_database: bool,
}

impl MockUpstream {
    fn new(pool: PgPool, clock: Arc<StdMutex<DateTime<Utc>>>) -> Self {
        Self {
            pool,
            clock,
            calls: Arc::new(StdMutex::new(Vec::new())),
            unexpected: Arc::new(StdMutex::new(Vec::new())),
            test_sse: Arc::new(StdMutex::new(SUCCESS_TEST_SSE.into())),
            model_gate: Arc::new(StdMutex::new(None)),
            recovery_gate: Arc::new(StdMutex::new(None)),
            usage_gate: Arc::new(StdMutex::new(None)),
            usage_cards: Arc::new(StdMutex::new(None)),
            model_status: Arc::new(StdMutex::new(200)),
            model_output: Arc::new(StdMutex::new(sample("above_99"))),
            expected_responses_model: Arc::new(StdMutex::new("gpt-6-luna".into())),
            reset_uncertain: Arc::new(StdMutex::new(false)),
            temp_delete: Arc::new(StdMutex::new(TempDelete {
                status: 200,
                clear_database: true,
            })),
            temp_delete_gate: Arc::new(StdMutex::new(None)),
            bulk_delete_gate: Arc::new(StdMutex::new(None)),
            delete_started: Arc::new(Notify::new()),
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

    fn unexpected(&self) -> Vec<String> {
        self.unexpected.lock().unwrap().clone()
    }

    fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }

    fn set_test_sse(&self, value: impl Into<String>) {
        *self.test_sse.lock().unwrap() = value.into();
    }

    fn set_model_gate(&self, gate: Gate) {
        *self.model_gate.lock().unwrap() = Some(gate);
    }

    fn set_expected_responses_model(&self, model: impl Into<String>) {
        *self.expected_responses_model.lock().unwrap() = model.into();
    }

    fn set_temp_delete(&self, status: u16, clear_database: bool) {
        *self.temp_delete.lock().unwrap() = TempDelete {
            status,
            clear_database,
        };
    }

    fn set_temp_delete_gate(&self, gate: Option<Gate>) {
        *self.temp_delete_gate.lock().unwrap() = gate;
    }

    fn set_bulk_delete_gate(&self, gate: Option<BulkGate>) {
        *self.bulk_delete_gate.lock().unwrap() = gate;
    }

    fn temp_delete_ids(&self) -> Vec<i64> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.method == "DELETE")
            .filter_map(|call| path_id(&call.path, "/temp-unschedulable"))
            .collect()
    }
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

async fn mock_upstream(State(state): State<MockUpstream>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let method = parts.method;
    let path = parts.uri.path().to_owned();
    let bytes = to_bytes(body, 2_000_000).await.unwrap_or_default();
    let payload = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    state.record(&method, &path);

    if method == Method::GET && path == "/api/v1/admin/groups/all" {
        assert_eq!(parts.headers.get("x-api-key").unwrap(), ADMIN_KEY);
        return json_response(json!({"code":0,"data":[]}));
    }

    if method == Method::GET && path.starts_with("/fixture-models/") {
        assert!(!parts.headers.contains_key("x-api-key"));
        assert!(parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("Bearer fixture-")));
        return json_response(json!({
            "data": [
                {"id":"gpt-6-astra","name":"fixture requested model"},
                {"id":"gpt-6-sol","name":"fixture lower tier"},
                {"id":"gpt-6-luna","name":"fixture model"},
                {"id":"gpt-5.6-luna","name":"fixture reference"},
                {"id":"fixture-model","name":"fixture recovery model"}
            ]
        }));
    }

    if method == Method::DELETE && path.ends_with("/temp-unschedulable") {
        let Some(id) = path_id(&path, "/temp-unschedulable") else {
            return (StatusCode::NOT_FOUND, Json(json!({"code":1}))).into_response();
        };
        assert_eq!(parts.headers.get("x-api-key").unwrap(), ADMIN_KEY);
        state.delete_started.notify_one();
        let bulk_delete_gate = state.bulk_delete_gate.lock().unwrap().clone();
        let delete_gate = state.temp_delete_gate.lock().unwrap().clone();
        if let Some(gate) = bulk_delete_gate {
            gate.entered.fetch_add(1, Ordering::SeqCst);
            gate.release
                .acquire()
                .await
                .expect("bulk DELETE gate remains open")
                .forget();
        } else if let Some(gate) = delete_gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let mode = *state.temp_delete.lock().unwrap();
        let status = StatusCode::from_u16(mode.status).expect("fixture HTTP status");
        if status.is_success() && mode.clear_database {
            sqlx::query("UPDATE accounts SET temp_unschedulable_until=NULL,temp_unschedulable_reason=NULL,extra=coalesce(extra,'{}'::jsonb)-'model_rate_limits',updated_at=$1 WHERE id=$2")
                .bind(state.now())
                .bind(id)
                .execute(&state.pool)
                .await
                .expect("simulate native temp pause and model-limit cleanup");
        }
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(
                "{{\"code\":0,\"data\":{{\"id\":{id}}}}}"
            )))
            .expect("fixture temp-unschedulable response");
    }

    if method == Method::GET && path.ends_with("/models") {
        return json_response(json!({
            "code": 0,
            "data": [
                {"id":"gpt-6-astra","name":"fixture requested model"},
                {"id":"gpt-6-sol","name":"fixture lower tier"},
                {"id":"gpt-6-luna","name":"fixture model"},
                {"id":"gpt-5.6-luna","name":"fixture reference"},
                {"id":"fixture-model","name":"fixture recovery model"}
            ]
        }));
    }

    if method == Method::GET && path.ends_with("/usage") {
        let gate = state.usage_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let now = state.now();
        let mut data = json!({
            "five_hour": {
                "utilization": 15,
                "resets_at": datetime(now + Duration::hours(5)),
                "remaining_seconds": 18000,
                "window_minutes": 300
            },
            "seven_day": {
                "utilization": 25,
                "resets_at": datetime(now + Duration::days(7)),
                "remaining_seconds": 604800,
                "window_minutes": 10080
            }
        });
        let cards = state.usage_cards.lock().unwrap().clone();
        if let Some(cards) = cards {
            let id = path_id(&path, "/usage").unwrap();
            sqlx::query("UPDATE accounts SET extra=jsonb_set(coalesce(extra,'{}'::jsonb),'{codex_reset_credit_snapshot}',$1) WHERE id=$2")
                .bind(SqlJson(cards.clone())).bind(id)
                .execute(&state.pool).await.unwrap();
            data["rate_limit_reset_credits"] = cards;
            data["fetched_at"] = datetime(now).into();
            data["cache_persisted"] = true.into();
        }
        return json_response(json!({
            "code": 0,
            "data": data
        }));
    }

    if method == Method::POST && path.ends_with("/schedulable") {
        let Some(id) = path_id(&path, "/schedulable") else {
            return (StatusCode::NOT_FOUND, Json(json!({"code": 1}))).into_response();
        };
        let enabled = payload["schedulable"].as_bool().unwrap_or(false);
        sqlx::query("UPDATE accounts SET schedulable=$1,updated_at=$2 WHERE id=$3")
            .bind(enabled)
            .bind(state.now())
            .bind(id)
            .execute(&state.pool)
            .await
            .expect("apply fixture schedulable write to temporary PostgreSQL");
        return json_response(json!({"code":0,"data":{"id":id,"schedulable":enabled}}));
    }

    if method == Method::POST && path.ends_with("/test") {
        let sse = state.test_sse.lock().unwrap().clone();
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(sse))
            .expect("fixture test SSE response");
    }

    if method == Method::POST && path.ends_with("/reset-quota") {
        let id = path
            .strip_prefix("/api/v1/admin/openai/accounts/")
            .and_then(|rest| rest.strip_suffix("/reset-quota"))
            .and_then(|id| id.parse::<i64>().ok())
            .unwrap();
        assert_eq!(parts.headers.get("x-api-key").unwrap(), ADMIN_KEY);
        if *state.reset_uncertain.lock().unwrap() {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error":"fixture receipt lost"})),
            )
                .into_response();
        }
        sqlx::query("UPDATE accounts SET extra=jsonb_set(extra,'{codex_reset_credit_snapshot}',$1) WHERE id=$2")
            .bind(SqlJson(json!({"available_count":0,"credits":[]})))
            .bind(id).execute(&state.pool).await.unwrap();
        return json_response(json!({"code":0,"data":{
            "code":"success","credit":{"status":"redeemed"},"windows_reset":1,
            "quota":native_quota(state.now(),20),"cache_refreshed":true,"cache_persisted":true
        }}));
    }

    if method == Method::POST && path.ends_with("/recover-state") {
        let Some(id) = path_id(&path, "/recover-state") else {
            return (StatusCode::NOT_FOUND, Json(json!({"code": 1}))).into_response();
        };
        sqlx::query("UPDATE accounts SET rate_limited_at=NULL,rate_limit_reset_at=NULL,overload_until=NULL,temp_unschedulable_until=NULL,temp_unschedulable_reason=NULL,updated_at=$1 WHERE id=$2")
            .bind(state.now())
            .bind(id)
            .execute(&state.pool)
            .await
            .expect("apply synthetic recovery write to temporary PostgreSQL");
        let gate = state.recovery_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        return json_response(json!({"code":0,"data":{"id":id}}));
    }

    if method == Method::POST && path == "/push" {
        return json_response(json!({"code":200,"data":"fixture-delivered"}));
    }

    if method == Method::POST && path.ends_with("/v1/responses") {
        let gate = state.model_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        assert_eq!(
            parts.headers.get("authorization").unwrap(),
            "Bearer fixture-access-token"
        );
        assert_eq!(
            parts.headers.get("content-type").unwrap(),
            "application/json"
        );
        let expected_model = state.expected_responses_model.lock().unwrap().clone();
        assert_eq!(payload["model"].as_str(), Some(expected_model.as_str()));
        assert_eq!(payload["stream"], true);
        let status = *state.model_status.lock().unwrap();
        if status == 0 {
            return Response::builder().status(StatusCode::OK)
                .header(header::CONTENT_TYPE,"text/event-stream")
                .body(Body::from("data: {\"type\":\"response.output_text.delta\",\"delta\":\"fixture incomplete output\"}\n\n"))
                .unwrap();
        }
        if status != 200 {
            return (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({"error":{"message":"fixture failure"}})),
            )
                .into_response();
        }
        let text = state.model_output.lock().unwrap().clone();
        let completed = json!({"type":"response.completed","response":{
            "status":"completed","model":"fixture-returned-model","object":"response",
            "output_text":text,"output":[{"type":"message","phase":"final","content":[{"type":"output_text","text":text}]}],
            "usage":{"input_tokens":23,"output_tokens":19}
        }});
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(format!("data: {completed}\n\n")))
            .unwrap();
    }

    state
        .unexpected
        .lock()
        .unwrap()
        .push(format!("{} {}", method_name(&method), path));
    (
        StatusCode::NOT_FOUND,
        Json(json!({"code":1,"message":"fixture endpoint not found"})),
    )
        .into_response()
}

struct Harness {
    _temp: tempfile::TempDir,
    db: Database,
    runtime: RuntimeHandle,
    mock: MockUpstream,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    base_url: String,
    server: Option<JoinHandle<()>>,
}

struct RuntimeHandle(Option<Arc<Runtime>>);
impl std::ops::Deref for RuntimeHandle {
    type Target = Arc<Runtime>;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}

impl Harness {
    async fn new() -> Self {
        let database_url = std::env::var("DATABASE_URL")
            .expect("automation contract runner must supply temporary PostgreSQL");
        let db = Database::open(&database_url, false)
            .await
            .expect("open runner-owned PostgreSQL for loopback mock");
        let temp = tempfile::tempdir().expect("create synthetic automation state directory");
        let clock = Arc::new(StdMutex::new(reference_time()));
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback-only Sub2API/Bark fixture");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let mock = MockUpstream::new(db.pool.clone(), clock.clone());
        let router = Router::new()
            .fallback(mock_upstream)
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve loopback automation fixture");
        });

        let connection_path = temp.path().join("sub2api-config.json");
        fs::write(
            &connection_path,
            serde_json::to_vec(&json!({"base_url":base_url,"verify_base_url":""})).unwrap(),
        )
        .expect("write loopback upstream config");
        let oauth = json!({
            "oauth_auto_reset_credit_enabled": false,
            "oauth_daily_test_enabled": true,
            "oauth_daily_test_time": "14:08",
            "oauth_early_probe_batch_size": 4,
            "oauth_recovery_connection_account_ids": [],
            "oauth_recovery_model_account_ids": [],
            "oauth_recovery_monitor_enabled": true,
            "oauth_recovery_test_concurrency": 1,
            "oauth_recovery_test_model_id": "fixture-model",
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
            connection_path,
            model_path: temp.path().join("group-models.json"),
            keepalive_path: temp.path().join("keepalive-state.json"),
            oauth,
            bark: json!({
                "enabled": true,
                "device_key": "fixture-bark-device-key",
                "server_url": base_url.clone()
            }),
            bark_valid: true,
            base_url: base_url.clone(),
            verify_url: String::new(),
            read_only: false,
        };
        let monitor_clock = clock.clone();
        let runtime =
            Runtime::new_with_clock(settings, Arc::new(move || *monitor_clock.lock().unwrap()))
                .await
                .expect("construct real Runtime against synthetic schema and deterministic clock");
        runtime
            .actions
            .set_model_directory_endpoint(&format!("{base_url}/fixture-models"))
            .expect("use the loopback-only provider directory fixture");
        runtime
            .tests
            .set_test_endpoint(&format!("{base_url}/v1/responses"))
            .unwrap();
        runtime
            .monitor
            .store
            .save_admin_token(ADMIN_KEY)
            .expect("save synthetic Sub2API token");
        Self {
            _temp: temp,
            db,
            runtime: RuntimeHandle(Some(runtime)),
            mock,
            clock,
            base_url,
            server: Some(server),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }

    fn set_now(&self, at: DateTime<Utc>) {
        *self.clock.lock().unwrap() = at;
    }

    async fn seed_account(&self, kind: &str, schedulable: bool, rate_limited: bool) -> Value {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        let now = self.now();
        let credentials = if kind == "apikey" {
            json!({
                "api_key": "fixture-model-api-key",
                "base_url": format!("{}/v1", self.base_url)
            })
        } else {
            json!({
                "plan_type": "plus",
                "access_token": "fixture-access-token",
                "refresh_token": "fixture-refresh-token"
            })
        };
        sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,rate_limited_at,rate_limit_reset_at,updated_at) VALUES ($1,$2,'openai',$3,'active',$4,$5,$6,$7,$8,$9)")
            .bind(id)
            .bind(format!("fixture-automation-account-{id}"))
            .bind(kind)
            .bind(schedulable)
            .bind(SqlJson(credentials))
            .bind(SqlJson(json!({})))
            .bind(rate_limited.then_some(now - Duration::hours(1)))
            .bind(rate_limited.then_some(now - Duration::minutes(5)))
            .bind(now - Duration::minutes(2))
            .execute(&self.runtime.db.pool)
            .await
            .expect("insert synthetic account into temporary PostgreSQL");
        self.runtime
            .monitor
            .row(id)
            .await
            .expect("read synthetic account through real query projection")
    }

    async fn close(mut self) {
        assert!(self.mock.unexpected().is_empty());
        self.runtime.close().await;
        self.runtime.0.take();
        self.db.close().await;
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
    }

    async fn restart(&mut self) {
        let runtime = self.runtime.0.take().unwrap();
        let settings = runtime.settings.as_ref().clone();
        self.replace_runtime(runtime, settings).await;
    }

    async fn replace_runtime(&mut self, runtime: Arc<Runtime>, settings: Settings) {
        runtime.close().await;
        drop(runtime);
        let clock = self.clock.clone();
        self.runtime.0 = Some(
            Runtime::new_with_clock(settings, Arc::new(move || *clock.lock().unwrap()))
                .await
                .unwrap(),
        );
        self.runtime
            .actions
            .set_model_directory_endpoint(&format!("{}/fixture-models", self.base_url))
            .expect("retain the loopback-only provider directory fixture after restart");
        self.runtime
            .tests
            .set_test_endpoint(&format!("{}/v1/responses", self.base_url))
            .unwrap();
    }

    async fn restart_with_settings(&mut self, settings: Settings) {
        let runtime = self.runtime.0.take().unwrap();
        self.replace_runtime(runtime, settings).await;
    }
}

#[tokio::test]
async fn shared_model_reads_keep_legacy_jobs_and_owned_results_independent() {
    let h = Harness::new().await;
    let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let job = json!({"id":id,"account_id":7,"status":"completed","target_signature":"fixture-private-signature","report":{"prediction":"fixture-model"}});
    let legacy = json!({"version":1,"accounts":{"7":job}});
    crate::store::write_json(h.runtime.tests.store.path(), &legacy).unwrap();
    let mut read = h.runtime.tests.get(id).unwrap();
    assert!(read.get("target_signature").is_none());
    read["report"]["prediction"] = "changed-by-caller".into();
    assert_eq!(
        h.runtime.tests.latest(7).unwrap()["report"]["prediction"],
        "fixture-model"
    );
    assert_eq!(
        h.runtime.tests.store.read().unwrap(),
        legacy,
        "legacy reads do not migrate on disk"
    );
    h.runtime
        .tests
        .update(id, json!({"error":"fixture-result"}))
        .unwrap();
    let retained = h.runtime.tests.shared_data().unwrap();
    assert!(Arc::ptr_eq(
        &retained,
        &h.runtime.tests.shared_data().unwrap()
    ));
    h.runtime
        .tests
        .update(id, json!({"error":"fixture-new-result"}))
        .unwrap();
    assert_eq!(retained["jobs"][id]["error"], "fixture-result");
    assert_eq!(
        h.runtime.tests.get(id).unwrap()["error"],
        "fixture-new-result"
    );
    assert_eq!(
        h.runtime
            .tests
            .get("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap_err()
            .status
            .as_u16(),
        404
    );
    h.close().await;
}

#[tokio::test]
async fn unchanged_smart_schedule_skips_transactions_and_honors_manual_generation() {
    let h = Harness::new().await;
    let detection = &h.runtime.detection;
    let mut control = crate::model_detection::defaults();
    control["enabled"] = true.into();
    control["recent_request_count"] = 1.into();
    control["schedule_anchor_at"] = datetime(h.now()).into();
    detection.update(7, control).unwrap();
    let control = detection.control(7).unwrap();
    let due = detection.update_smart_schedule(7, &control).unwrap();
    assert_eq!(due, Some(h.now() + Duration::minutes(30)));
    let control = detection.control(7).unwrap();
    let retained = detection.store.read_shared().unwrap();
    assert_eq!(detection.update_smart_schedule(7, &control).unwrap(), due);
    assert!(Arc::ptr_eq(
        &retained,
        &detection.store.read_shared().unwrap()
    ));
    detection
        .update(7, json!({"recent_request_count":0}))
        .unwrap();
    let idle = detection.control(7).unwrap();
    assert_eq!(detection.update_smart_schedule(7, &idle).unwrap(), None);
    assert_eq!(detection.control(7).unwrap()["schedule_idle_paused"], true);
    let idle = detection.control(7).unwrap();
    let retained = detection.store.read_shared().unwrap();
    detection.update_smart_schedule(7, &idle).unwrap();
    assert!(Arc::ptr_eq(
        &retained,
        &detection.store.read_shared().unwrap()
    ));
    detection
        .update(
            7,
            json!({"enabled":false,"generation":1,"status":"disabled"}),
        )
        .unwrap();
    detection.update_smart_schedule(7, &control).unwrap();
    assert_eq!(detection.control(7).unwrap()["status"], "disabled");
    assert_eq!(detection.control(7).unwrap()["generation"], 1);
    h.close().await;
}

async fn close_schedule(harness: &Harness, id: i64) -> Value {
    set_schedulable(harness, id, false).await
}

async fn set_schedulable(harness: &Harness, id: i64, enabled: bool) -> Value {
    let row = harness
        .runtime
        .actions
        .account(id, None)
        .await
        .expect("read API key account before manual scheduling change");
    harness
        .runtime
        .set_schedulable(
            id,
            &json!({
                "schedulable": enabled,
                "expected_version": account_version(&row)
            }),
            ADMIN_KEY,
        )
        .await
        .expect("perform real Runtime manual scheduling close through loopback mock")
}

fn usage_result(row: &Value, at: DateTime<Utc>) -> Value {
    let summary = json!({
        "plan_type": "plus",
        "updated_at": datetime(at),
        "ui_windows": [
            {
                "key": "codex_5h",
                "label": "5h",
                "used_percent": 10,
                "remaining_percent": 90,
                "reset_at": datetime(at + Duration::hours(5)),
                "reset_source": "server_exact",
                "window_minutes": 300
            },
            {
                "key": "codex_7d",
                "label": "7d",
                "used_percent": 20,
                "remaining_percent": 80,
                "reset_at": datetime(at + Duration::days(7)),
                "reset_source": "server_exact",
                "window_minutes": 10080
            }
        ]
    });
    let result = json!({
        "success": true,
        "queried_at": datetime(at),
        "source": "sub2api_admin_usage",
        "oauth_quota": summary
    });
    assert!(oauth_admission::quota_complete(row, &result));
    result
}

async fn seed_recovery(harness: &Harness, uncertain_test: bool) -> (Value, Value, Value) {
    seed_recovery_with_model(harness, uncertain_test, "gpt-6-luna").await
}

async fn seed_recovery_with_model(
    harness: &Harness,
    uncertain_test: bool,
    model_id: &str,
) -> (Value, Value, Value) {
    let row = harness.seed_account("oauth", true, true).await;
    let id = integer(&row["id"]).unwrap();
    harness
        .runtime
        .detection
        .update(id, json!({"model_id":model_id}))
        .unwrap();
    harness
        .mock
        .set_expected_responses_model(model_id.to_owned());
    let observed = harness.now() - Duration::seconds(1);
    let usage = usage_result(&row, observed);
    let summary = quota::summary_from_result(&row, &usage);
    let descriptor = recovery_rules::rate_descriptor(&row, &summary, harness.now());
    assert!(
        descriptor.is_object(),
        "fixture row must derive an exact rate-limit intent"
    );
    let intent = recovery_events::new_intent(&descriptor, "ready", Some(harness.now()));
    harness
        .runtime
        .config
        .oauth
        .transaction(|raw| {
            raw["model_detection_model_id"] = model_id.into();
            raw["oauth_recovery_connection_account_ids"] = json!([]);
            raw["oauth_recovery_model_account_ids"] = json!([id]);
            raw["oauth_recovery_test_model_id"] = "fixture-model".into();
            Ok(())
        })
        .expect("select only the synthetic fixture recovery account");
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["oauth_results"][id.to_string()] = usage.clone();
            state["scheduler"][id.to_string()]["recovery_method_generation"] = 0.into();
            state["scheduler"][id.to_string()]["recovery_intent"] = intent.clone();
            Ok(())
        })
        .expect("persist synthetic fresh quota and recovery intent");
    if uncertain_test {
        harness
            .mock
            .set_test_sse("data: {\"type\":\"test_start\",\"model\":\"fixture-model\"}\n\n");
    }
    (row, usage, intent)
}

// Keep the normal target resolution, credentials, state guards, semaphore,
// fingerprint bank and transport. Only route its already resolved URL to the
// fixture. Holding the task budget prevents the ordinary worker reaching the
// real OAuth endpoint before this test substitutes the loopback destination.
async fn prepare_loopback_worker(h: &Harness, job: &Value) -> Arc<super::Context> {
    let id = job["id"].as_str().unwrap();
    let handle = h.runtime.tests.tasks.lock().unwrap().remove(id).unwrap();
    handle.abort();
    let _ = handle.await;
    let row = model_target::account(&h.runtime.actions, integer(&job["account_id"]).unwrap())
        .await
        .unwrap();
    let (mut target, owner) = model_target::target(
        &h.runtime.actions,
        &row,
        job["requested_model"].as_str().unwrap(),
    )
    .await
    .unwrap();
    target.url = format!("{}/v1/responses", h.base_url);
    Arc::new(super::Context {
        lm: None,
        samples: StdMutex::new(Default::default()),
        challenges: modeltrace::challenges(),
        snapshot: h.runtime.tests.bank.snapshot(),
        compiled: None,
        row,
        owner,
        target,
    })
}

async fn natural_cycle(h: &Harness, id: i64, original: &Value, usage: &Value) -> Value {
    let row = h.runtime.monitor.row(id).await.unwrap();
    let lease = h.runtime.monitor.lease(&row).unwrap();
    let state = h.runtime.monitor.state().unwrap();
    let meta = &state["scheduler"][id.to_string()];
    h.runtime
        .monitor
        .verify_recovery(
            id,
            original,
            usage,
            integer(&meta["manual_control"]["generation"]).unwrap_or(0),
            integer(&meta["recovery_method_generation"]).unwrap_or(0),
            "model",
            lease,
        )
        .await
        .unwrap()
}

async fn run_natural_model(h: &Harness, id: i64, intent: &Value, usage: &Value) -> Value {
    let budget = h
        .runtime
        .budget
        .clone()
        .acquire_many_owned(24)
        .await
        .unwrap();
    let pending = natural_cycle(h, id, intent, usage).await;
    assert_eq!(pending["deferred"], true, "{pending}");
    let job = h.runtime.tests.latest(id).unwrap();
    assert_eq!(job["status"], "queued");
    assert!(h
        .runtime
        .monitor
        .lease(&h.runtime.monitor.row(id).await.unwrap())
        .is_ok());
    let context = prepare_loopback_worker(h, &job).await;
    let queued = h.runtime.tests.data().unwrap()["jobs"][job["id"].as_str().unwrap()].clone();
    drop(budget);
    h.runtime
        .tests
        .wait_and_run(&queued, Some(context))
        .await
        .unwrap();
    let completed = h.runtime.tests.get(job["id"].as_str().unwrap()).unwrap();
    assert_eq!(
        h.runtime
            .tests
            .contexts
            .lock()
            .unwrap()
            .contains_key(job["id"].as_str().unwrap()),
        completed["can_retry"] == true
    );
    assert_eq!(completed["planned_groups"], 1);
    assert_eq!(completed["groups"].as_array().unwrap().len(), 1);
    assert_eq!(h.mock.count("POST", "/test"), 0);
    assert!(h
        .runtime
        .monitor
        .lease(&h.runtime.monitor.row(id).await.unwrap())
        .is_ok());
    completed
}

#[tokio::test]
async fn recovery_model_one_valid_group_really_recovers_with_durable_history() {
    let h = Harness::new().await;
    h.runtime
        .config
        .oauth
        .transaction(|v| {
            v["model_detection_platforms"] =
                json!({"openai":{"engine":"lm_detector","sample_count":3}});
            Ok(())
        })
        .unwrap();
    let (row, usage, intent) = seed_recovery(&h, false).await;
    let id = integer(&row["id"]).unwrap();
    let job = run_natural_model(&h, id, &intent, &usage).await;
    assert_eq!(job["status"], "completed");
    assert_eq!(job["engine"], "modeltrace");
    assert_eq!(job["completed_groups"], 1);
    assert_eq!(job["valid_groups"], 1);
    assert_eq!(job["attempts"], 1);
    assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
    let result = natural_cycle(&h, id, &intent, &usage).await;
    assert_eq!(result["recovered"], true, "{result}");
    assert_eq!(h.mock.count("POST", "/recover-state"), 1);
    let state = h.runtime.monitor.state().unwrap();
    assert_eq!(
        state["scheduler"][id.to_string()]["recovery_intent"]["status"],
        "recovered"
    );
    assert_eq!(state["recovery_history"].as_object().unwrap().len(), 1);
    assert_eq!(
        state["recovery_history"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()["attempt_count"],
        1
    );
    assert!(h.runtime.monitor.row(id).await.unwrap()["rate_limit_reset_at"].is_null());
    h.close().await;
}

#[tokio::test]
async fn legacy_recovery_without_reliable_test_count_does_not_infer_a_total() {
    let h = Harness::new().await;
    let (row, usage, mut intent) = seed_recovery(&h, false).await;
    let id = integer(&row["id"]).unwrap();
    intent
        .as_object_mut()
        .unwrap()
        .remove("model_attempt_count");
    intent["attempt_count"] = 3.into();
    h.runtime
        .monitor
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]["recovery_intent"] = intent.clone();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        run_natural_model(&h, id, &intent, &usage).await["status"],
        "completed"
    );
    assert_eq!(
        natural_cycle(&h, id, &intent, &usage).await["recovered"],
        true
    );
    let state = h.runtime.monitor.state().unwrap();
    assert!(state["recovery_history"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()["attempt_count"]
        .is_null());
    h.close().await;
}

#[tokio::test]
async fn model_rejected_before_test_execution_does_not_increment_recovery_count() {
    let h = Harness::new().await;
    let (row, usage, intent) = seed_recovery(&h, false).await;
    let id = integer(&row["id"]).unwrap();
    h.runtime
        .config
        .oauth
        .transaction(|raw| {
            raw["model_detection_model_id"] = "unlisted-model".into();
            Ok(())
        })
        .unwrap();

    let result = natural_cycle(&h, id, &intent, &usage).await;
    assert_eq!(result["attempt_count"], 0);
    assert_eq!(h.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(
        h.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"]
            ["model_attempt_count"],
        0
    );
    h.close().await;
}

#[tokio::test]
async fn recovery_model_invalid_auth_or_empty_sample_never_authorizes_recovery() {
    for (status, output, expected) in [
        (400, sample("above_99"), "invalid_model_or_request"),
        (401, sample("above_99"), "http_401"),
        (200, "1, 2, 3".into(), "model_analysis_insufficient"),
    ] {
        let h = Harness::new().await;
        let (row, usage, intent) = seed_recovery(&h, false).await;
        let id = integer(&row["id"]).unwrap();
        *h.mock.model_status.lock().unwrap() = status;
        *h.mock.model_output.lock().unwrap() = output;
        let job = run_natural_model(&h, id, &intent, &usage).await;
        assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
        if status == 401 {
            assert_eq!(job["error_code"], "auth_or_quota");
        }
        if status == 200 {
            assert_eq!(job["valid_groups"], 0);
        }
        natural_cycle(&h, id, &intent, &usage).await;
        let state = h.runtime.monitor.state().unwrap();
        let saved = &state["scheduler"][id.to_string()]["recovery_intent"];
        assert_eq!(
            saved["last_error_code"], expected,
            "status={status}: {saved}"
        );
        assert_eq!(
            saved["status"],
            if status == 401 {
                "auth_failed"
            } else {
                "retry"
            }
        );
        if status == 401 {
            assert!(
                state["scheduler"][id.to_string()]["quota_query"]["auth_fingerprint"].is_string()
            );
        }
        assert_eq!(h.mock.count("POST", "/recover-state"), 0);
        assert!(h.runtime.monitor.row(id).await.unwrap()["rate_limit_reset_at"].is_string());
        h.close().await;
    }
}

#[tokio::test]
async fn recovery_model_degradation_stops_scheduling_without_recovery_write() {
    let h = Harness::new().await;
    let (row, usage, intent) = seed_recovery(&h, false).await;
    let id = integer(&row["id"]).unwrap();
    *h.mock.model_output.lock().unwrap() = sample("automatic_first_group");
    let job = run_natural_model(&h, id, &intent, &usage).await;
    assert_eq!(job["first_group_prediction"], "gpt-5.6-luna");
    assert_eq!(h.runtime.detection.mark(id).unwrap()["marked"], true);
    assert_eq!(
        h.runtime.monitor.row(id).await.unwrap()["schedulable"],
        false
    );
    natural_cycle(&h, id, &intent, &usage).await;
    assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(h.mock.count("POST", "/recover-state"), 0);
    h.close().await;
}

#[tokio::test]
async fn lm_uncertain_group_blocks_disposition_then_lower_family_records_exact_evidence() {
    let h = Harness::new().await;
    h.runtime
        .config
        .oauth
        .transaction(|v| {
            v["model_detection_platforms"] =
                json!({"openai":{"engine":"lm_detector","sample_count":3}});
            v["model_detection_model_id"] = "gpt-6-luna".into();
            Ok(())
        })
        .unwrap();
    let row = h.seed_account("apikey", true, false).await;
    let id = integer(&row["id"]).unwrap();
    h.runtime
        .detection
        .update(id, json!({"enabled":true,"schedule_mode":"fixed"}))
        .unwrap();
    let hold = h
        .runtime
        .budget
        .clone()
        .acquire_many_owned(h.runtime.budget.available_permits() as u32)
        .await
        .unwrap();
    let mut job = h
        .runtime
        .detection
        .trigger(id, &["scheduled".into()])
        .await
        .unwrap();
    let report = |model: &str, count: i64| json!({"decision":if count==1{"partial"}else{"scored"},"prediction":model,"prediction_name":model,"used_outputs":count,"probability":null});
    job["status"] = "completed".into();
    job["report"] = report("gpt-6-luna", 3);
    job["groups"] = json!(["gpt-6-luna","grok-4.5","gemini-3.1-pro-preview"].iter().enumerate().map(|(i,m)|json!({"index":i+1,"status":"completed","transport_complete":true,"report":report(m,1)})).collect::<Vec<_>>());
    assert!(h
        .runtime
        .detection
        .verdict(&job, &job["report"])
        .await
        .unwrap()
        .is_null());
    assert_eq!(h.runtime.detection.mark(id).unwrap()["marked"], false);
    assert_eq!(h.mock.count("POST", "/schedulable"), 0);
    job["groups"][2]["report"] = report("gpt-6-luna", 1);
    job["classification"] = crate::lm_verdict::classify(&job);
    job["disposition_basis"] = job["classification"]["basis"].clone();
    let result = h
        .runtime
        .detection
        .verdict(&job, &job["report"])
        .await
        .unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["basis"], "sample_family");
    assert_eq!(result["group_indexes"], json!([2]));
    assert_eq!(h.runtime.detection.mark(id).unwrap()["marked"], true);
    assert_eq!(
        h.runtime.monitor.row(id).await.unwrap()["schedulable"],
        false
    );
    assert_eq!(h.mock.count("POST", "/schedulable"), 1);
    let notice = h.runtime.detection.store.read().unwrap()["disposition_notifications"]
        [job["id"].as_str().unwrap()]
    .clone();
    let (_, message) = crate::detection_dispatch::notification_message(&notice);
    assert!(message.contains("第 2 组跨家族证据：grok-4.5"));
    assert!(message.contains("LM Detector · 3 组"));
    assert!(notice["probability"].is_null());
    h.runtime
        .tests
        .cancel(job["id"].as_str().unwrap())
        .await
        .unwrap();
    drop(hold);
    h.close().await;
}

#[tokio::test]
async fn recovery_model_completed_report_compares_requested_model_before_recovery_write() {
    let h = Harness::new().await;
    let (row, usage, intent) = seed_recovery_with_model(&h, false, "gpt-6-astra").await;
    let id = integer(&row["id"]).unwrap();
    let job = run_natural_model(&h, id, &intent, &usage).await;
    assert_eq!(job["status"], "completed");
    assert_eq!(job["requested_model"], "gpt-6-astra");
    assert_eq!(job["forwarded_model"], "gpt-6-astra");
    assert_eq!(job["first_group_valid"], true);
    assert_ne!(job["first_group_prediction"], "gpt-6-sol");
    assert_eq!(h.runtime.detection.mark(id).unwrap()["marked"], false);

    h.runtime
        .tests
        .update(
            job["id"].as_str().unwrap(),
            json!({
                "forwarded_model":"gpt-6-sol",
                "first_group_valid":true,
                "first_group_prediction":"gpt-6-sol",
                "report":{"prediction":"gpt-6-sol","used_outputs":1,"probability":0.97}
            }),
        )
        .unwrap();
    let result = natural_cycle(&h, id, &intent, &usage).await;
    assert_eq!(result["success"], false);
    assert_eq!(result["error_code"], "detection_held");
    assert_eq!(result["model_id"], "gpt-6-astra");
    assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(h.mock.count("POST", "/recover-state"), 0);
    assert_eq!(h.runtime.detection.mark(id).unwrap()["marked"], false);
    let scheduler = h.runtime.monitor.state().unwrap()["scheduler"][id.to_string()].clone();
    assert_eq!(
        scheduler["recovery_intent"]["last_error_code"],
        "detection_held"
    );
    assert!(h.mock.unexpected().is_empty());
    h.close().await;
}

#[tokio::test]
async fn recovery_model_missing_prediction_never_authorizes_recovery() {
    let h = Harness::new().await;
    let (row, usage, intent) = seed_recovery(&h, false).await;
    let id = integer(&row["id"]).unwrap();
    let job = run_natural_model(&h, id, &intent, &usage).await;
    h.runtime
        .tests
        .update(
            job["id"].as_str().unwrap(),
            json!({"report":{"used_outputs":1,"probability":0.99}}),
        )
        .unwrap();
    natural_cycle(&h, id, &intent, &usage).await;
    assert_eq!(h.mock.count("POST", "/recover-state"), 0);
    assert_ne!(
        h.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"]
            ["status"],
        "recovered"
    );
    h.close().await;
}

#[tokio::test]
async fn recovery_model_method_or_manual_changes_before_dispatch_skip_every_request() {
    for change in ["method", "selection", "manual"] {
        let h = Harness::new().await;
        let (row, usage, intent) = seed_recovery(&h, false).await;
        let id = integer(&row["id"]).unwrap();
        let before = h.runtime.monitor.state().unwrap();
        if change == "method" {
            h.runtime
                .config
                .oauth
                .transaction(|raw| {
                    raw["oauth_recovery_model_account_ids"] = json!([]);
                    raw["oauth_recovery_connection_account_ids"] = json!([id]);
                    Ok(())
                })
                .unwrap();
        } else if change == "selection" {
            h.runtime
                .monitor
                .store
                .transaction(|state| {
                    state["scheduler"][id.to_string()]["recovery_method_generation"] = json!(2);
                    Ok(())
                })
                .unwrap();
        } else {
            close_schedule(&h, id).await;
        }
        let current = h.runtime.monitor.row(id).await.unwrap();
        let lease = h.runtime.monitor.lease(&current).unwrap();
        let result = h
            .runtime
            .monitor
            .verify_recovery(id, &intent, &usage, 0, 0, "model", lease)
            .await
            .unwrap();
        assert_eq!(result["success"], false);
        assert_eq!(result["skipped"], true);
        assert!(h.runtime.tests.latest(id).unwrap().is_null());
        assert_eq!(h.mock.count("POST", "/v1/responses"), 0);
        assert_eq!(h.mock.count("POST", "/recover-state"), 0);
        let state = h.runtime.monitor.state().unwrap();
        assert_eq!(state["recovery_history"], json!({}));
        assert_eq!(
            state["scheduler"][id.to_string()]["quota_query"],
            before["scheduler"][id.to_string()]["quota_query"]
        );
        assert!(h.runtime.monitor.lease(&current).is_ok());
        h.close().await;
    }
}

#[tokio::test]
async fn recovery_model_method_or_manual_changes_reject_completed_success() {
    for change in ["method", "selection", "manual"] {
        let h = Harness::new().await;
        let (row, usage, intent) = seed_recovery(&h, false).await;
        let id = integer(&row["id"]).unwrap();
        let job = run_natural_model(&h, id, &intent, &usage).await;
        assert_eq!(job["status"], "completed");
        if change == "method" {
            h.runtime
                .config
                .oauth
                .transaction(|raw| {
                    raw["oauth_recovery_model_account_ids"] = json!([]);
                    raw["oauth_recovery_connection_account_ids"] = json!([id]);
                    Ok(())
                })
                .unwrap();
        } else if change == "selection" {
            h.runtime
                .monitor
                .store
                .transaction(|state| {
                    state["scheduler"][id.to_string()]["recovery_method_generation"] = json!(2);
                    Ok(())
                })
                .unwrap();
        } else {
            close_schedule(&h, id).await;
        }
        let current = h.runtime.monitor.row(id).await.unwrap();
        let lease = h.runtime.monitor.lease(&current).unwrap();
        let result = h
            .runtime
            .monitor
            .verify_recovery(id, &intent, &usage, 0, 0, "model", lease)
            .await
            .unwrap();
        assert_eq!(result["success"], false);
        assert_eq!(result["skipped"], true);
        assert_eq!(h.mock.count("POST", "/recover-state"), 0);
        assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
        let state = h.runtime.monitor.state().unwrap();
        assert_ne!(
            state["scheduler"][id.to_string()]["recovery_intent"]["status"],
            "recovered"
        );
        if change == "manual" {
            assert_eq!(
                state["scheduler"][id.to_string()]["recovery_intent"]["status"],
                "cancelled"
            );
            assert_eq!(current["schedulable"], false);
        }
        h.close().await;
    }
}

#[tokio::test]
async fn recovery_model_cooldown_opens_at_exact_boundary_without_holding_account() {
    let h = Harness::new().await;
    let (row, usage, intent) = seed_recovery(&h, false).await;
    let id = integer(&row["id"]).unwrap();
    h.runtime
        .detection
        .store
        .transaction(|data| {
            data["automatic_starts"][id.to_string()] =
                json!({"job_id":"fixture-previous-job","started_at":datetime(h.now())});
            Ok(())
        })
        .unwrap();
    let due = h.now() + Duration::seconds(300);
    for at in [h.now(), due - Duration::microseconds(1)] {
        h.set_now(at);
        let result = natural_cycle(&h, id, &intent, &usage).await;
        assert_eq!(result["deferred"], true);
        assert_eq!(result["next_at"], datetime(due));
        assert!(h.runtime.tests.latest(id).unwrap().is_null());
        assert!(h
            .runtime
            .monitor
            .lease(&h.runtime.monitor.row(id).await.unwrap())
            .is_ok());
        assert_eq!(h.mock.count("POST", "/v1/responses"), 0);
    }
    h.set_now(due);
    let completed = run_natural_model(&h, id, &intent, &usage).await;
    assert_eq!(completed["status"], "completed");
    assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(
        h.runtime.detection.next_allowed_at(id).unwrap(),
        datetime(due + Duration::seconds(300))
    );
    h.close().await;
}

#[tokio::test]
async fn recovery_model_waiting_for_slot_releases_account_and_manual_generation_stops_dispatch() {
    for manual in [false, true] {
        let h = Harness::new().await;
        let (row, usage, intent) = seed_recovery(&h, false).await;
        let id = integer(&row["id"]).unwrap();
        let slots = h
            .runtime
            .tests
            .jobs
            .clone()
            .acquire_many_owned(2)
            .await
            .unwrap();
        let pending = natural_cycle(&h, id, &intent, &usage).await;
        assert_eq!(pending["deferred"], true);
        let job = h.runtime.tests.latest(id).unwrap();
        assert_eq!(job["status"], "queued");
        assert!(h.runtime.detection.next_allowed_at(id).unwrap().is_null());
        assert!(h
            .runtime
            .monitor
            .lease(&h.runtime.monitor.row(id).await.unwrap())
            .is_ok());
        assert_eq!(h.mock.count("POST", "/v1/responses"), 0);
        let context = prepare_loopback_worker(&h, &job).await;
        let queued = h.runtime.tests.data().unwrap()["jobs"][job["id"].as_str().unwrap()].clone();
        if manual {
            close_schedule(&h, id).await;
        }
        drop(slots);
        let result = h.runtime.tests.wait_and_run(&queued, Some(context)).await;
        if manual {
            assert_eq!(result.unwrap_err().status.as_u16(), 409);
            assert_eq!(h.mock.count("POST", "/v1/responses"), 0);
            assert!(h.runtime.detection.next_allowed_at(id).unwrap().is_null());
        } else {
            result.unwrap();
            assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
        }
        assert_eq!(h.mock.count("POST", "/recover-state"), 0);
        assert!(h
            .runtime
            .monitor
            .lease(&h.runtime.monitor.row(id).await.unwrap())
            .is_ok());
        h.close().await;
    }
}

fn native_quota(at: DateTime<Utc>, used: i64) -> Value {
    json!({"plan_type":"plus","rate_limit":{
        "primary_window":{"used_percent":used,"reset_at":(at+Duration::hours(5)).timestamp(),"limit_window_seconds":18000},
        "secondary_window":{"used_percent":used,"reset_at":(at+Duration::days(5)).timestamp(),"limit_window_seconds":604800}
    }})
}

async fn seed_credit(h: &Harness) -> (i64, Value) {
    let row = h.seed_account("oauth", true, true).await;
    let id = integer(&row["id"]).unwrap();
    let now = h.now();
    let snapshot =
        json!({"available_count":1,"credits":[{"expires_at":datetime(now+Duration::days(2))}]});
    sqlx::query("UPDATE accounts SET rate_limit_reset_at=$1,extra=$2 WHERE id=$3")
        .bind(now + Duration::days(5))
        .bind(SqlJson(json!({"codex_reset_credit_snapshot":snapshot})))
        .bind(id)
        .execute(&h.db.pool)
        .await
        .unwrap();
    h.runtime
        .config
        .oauth
        .transaction(|raw| {
            raw["oauth_auto_reset_credit_enabled"] = true.into();
            raw["oauth_recovery_model_account_ids"] = json!([id]);
            raw["oauth_recovery_connection_account_ids"] = json!([]);
            Ok(())
        })
        .unwrap();
    let row = h.runtime.monitor.row(id).await.unwrap();
    let data = native_quota(now, 100);
    let result = credit_rules::quota_result(&data, &row, now, Some(now));
    assert!(oauth_admission::quota_complete(&row, &result));
    let reset = parse_datetime(&result["oauth_quota"]["ui_windows"][1]["reset_at"]).unwrap();
    let normalized = crate::oauth_state::normalized_credits(&snapshot).unwrap();
    let observation = reset_credit_observation::create(
        &row,
        &normalized,
        now - Duration::seconds(1),
        &oauth_admission::credential_fingerprint(&row),
        reset,
        &json!(10080),
    )
    .unwrap();
    h.runtime
        .monitor
        .store
        .transaction(|state| {
            state["oauth_results"][id.to_string()] = result.clone();
            state["scheduler"][id.to_string()]["reset_credit_observation"] = observation.clone();
            Ok(())
        })
        .unwrap();
    sqlx::query("INSERT INTO ops_error_logs (id,account_id,created_at,platform,error_owner,error_phase,status_code,upstream_status_code,error_message,upstream_error_message) VALUES ($1,$2,$3,'openai','provider','upstream',429,429,'fixture upstream exhaustion','fixture upstream exhaustion')")
        .bind(id+1_000_000_000).bind(id).bind(now-Duration::seconds(60)).execute(&h.db.pool).await.unwrap();
    assert!(h
        .runtime
        .monitor
        .credits
        .depletion(&row, now, false)
        .await
        .unwrap()
        .is_object());
    (id, row)
}

async fn credit_cycle(h: &Harness, id: i64) {
    let row = h.runtime.monitor.row(id).await.unwrap();
    h.runtime
        .monitor
        .credits
        .run_account(&row, h.now())
        .await
        .unwrap();
}

async fn queue_credit(h: &Harness, id: i64) -> (Value, Arc<super::Context>) {
    credit_cycle(h, id).await;
    let job = h.runtime.tests.latest(id).unwrap();
    assert_eq!(job["status"], "queued");
    assert_eq!(h.mock.count("POST", "/reset-quota"), 0);
    assert_eq!(h.mock.count("POST", "/schedulable"), 0);
    assert!(h
        .runtime
        .monitor
        .lease(&h.runtime.monitor.row(id).await.unwrap())
        .is_ok());
    let context = prepare_loopback_worker(h, &job).await;
    let queued = h.runtime.tests.data().unwrap()["jobs"][job["id"].as_str().unwrap()].clone();
    (queued, context)
}

async fn run_credit_model(h: &Harness, id: i64) -> Value {
    let slot = h
        .runtime
        .tests
        .jobs
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let (job, context) = queue_credit(h, id).await;
    drop(slot);
    h.runtime
        .tests
        .wait_and_run(&job, Some(context))
        .await
        .unwrap();
    h.runtime.tests.get(job["id"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn credit_model_consumes_only_with_slot_and_recovers_once_without_hourly_query_delay() {
    for cooldown in [false, true] {
        let h = Harness::new().await;
        h.runtime
            .config
            .oauth
            .transaction(|v| {
                v["model_detection_platforms"] =
                    json!({"openai":{"engine":"lm_detector","sample_count":3}});
                Ok(())
            })
            .unwrap();
        let (id, _) = seed_credit(&h).await;
        if cooldown {
            let at = datetime(h.now() - Duration::seconds(30));
            h.runtime
                .monitor
                .store
                .transaction(|state| {
                    state["scheduler"][id.to_string()]["quota_query"] =
                        json!({"last_query_at":at,"automatic_attempts":[at]});
                    Ok(())
                })
                .unwrap();
        }
        let gate = Gate::new();
        h.mock.set_model_gate(gate.clone());
        let slot = h
            .runtime
            .tests
            .jobs
            .clone()
            .acquire_many_owned(2)
            .await
            .unwrap();
        let (job, context) = queue_credit(&h, id).await;
        assert!(h.runtime.monitor.credits.state(id).unwrap()["attempt_at"].is_null());
        drop(slot);
        let tests = h.runtime.tests.clone();
        let task = tokio::spawn(async move { tests.wait_and_run(&job, Some(context)).await });
        tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
            .await
            .unwrap();
        assert_eq!(h.mock.count("POST", "/reset-quota"), 1);
        assert_eq!(h.mock.count("POST", "/schedulable"), 1);
        assert!(h
            .runtime
            .monitor
            .lease(&h.runtime.monitor.row(id).await.unwrap())
            .is_err());
        assert_eq!(h.runtime.tests.jobs.available_permits(), 1);
        gate.release.notify_one();
        task.await.unwrap().unwrap();
        let job = h.runtime.tests.latest(id).unwrap();
        assert_eq!(job["status"], "completed");
        assert_eq!(job["engine"], "modeltrace");
        assert_eq!(job["planned_groups"], 1);
        assert_eq!(job["groups"].as_array().unwrap().len(), 1);
        assert_eq!(
            h.runtime.monitor.credits.state(id).unwrap()["consumed"],
            true
        );
        h.set_now(h.now() + Duration::seconds(30));
        credit_cycle(&h, id).await;
        let state = h.runtime.monitor.state().unwrap();
        let task = &state["scheduler"][id.to_string()]["auto_reset_credit"];
        assert_eq!(task["stage"], "recovered", "{task}");
        assert_eq!(h.mock.count("POST", "/reset-quota"), 1);
        assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
        assert_eq!(h.mock.count("POST", "/recover-state"), 1);
        assert_eq!(h.mock.count("POST", "/schedulable"), 2);
        assert_eq!(h.mock.count("GET", "/usage"), 0);
        assert_eq!(
            state["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"]
                .as_array()
                .unwrap()
                .len(),
            if cooldown { 2 } else { 1 }
        );
        assert_eq!(state["recovery_history"].as_object().unwrap().len(), 1);
        assert_eq!(
            state["recovery_history"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap()["attempt_count"],
            1
        );
        assert_eq!(state["pending_events"], json!({}));
        assert_eq!(
            state["scheduler"][id.to_string()]["credit_result_notifications"]
                [task["episode"].as_str().unwrap()]["outcome"],
            "restored"
        );
        h.set_now(h.now() + Duration::seconds(30));
        credit_cycle(&h, id).await;
        assert_eq!(h.mock.count("POST", "/reset-quota"), 1);
        assert_eq!(
            h.runtime.monitor.state().unwrap()["recovery_history"],
            state["recovery_history"]
        );
        assert!(h
            .runtime
            .monitor
            .lease(&h.runtime.monitor.row(id).await.unwrap())
            .is_ok());
        h.close().await;
    }
}

#[tokio::test]
async fn credit_reuses_pending_recovery_request_after_global_model_change() {
    let h = Harness::new().await;
    let (id, _) = seed_credit(&h).await;
    h.runtime
        .config
        .oauth
        .transaction(|raw| {
            raw["model_detection_model_id"] = "gpt-6-astra".into();
            Ok(())
        })
        .unwrap();
    h.mock.set_expected_responses_model("gpt-6-astra");
    h.runtime
        .detection
        .update(id, json!({"model_id":"fixture-old-account-model"}))
        .unwrap();

    let slot = h
        .runtime
        .tests
        .jobs
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let (queued, context) = queue_credit(&h, id).await;
    let job_id = queued["id"].as_str().unwrap().to_owned();
    assert_eq!(queued["requested_model"], "gpt-6-astra");
    let requests_before = h.runtime.tests.data().unwrap()["requests"].clone();
    let registrations = requests_before
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, registered_job)| registered_job.as_str() == Some(&job_id))
        .count();
    assert_eq!(
        registrations, 1,
        "the recovery attempt has one registered request"
    );
    assert_eq!(h.mock.count("POST", "/v1/responses"), 0);

    h.runtime
        .config
        .oauth
        .transaction(|raw| {
            raw["model_detection_model_id"] = "gpt-6-luna".into();
            Ok(())
        })
        .unwrap();
    credit_cycle(&h, id).await;
    let reused = h.runtime.tests.latest(id).unwrap();
    assert_eq!(reused["id"], job_id);
    assert_eq!(reused["requested_model"], "gpt-6-astra");
    assert_eq!(
        h.runtime.tests.data().unwrap()["requests"],
        requests_before,
        "a global model change does not register another model test for the same attempt"
    );
    assert_eq!(h.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(h.mock.count("POST", "/reset-quota"), 0);
    assert!(h.runtime.monitor.credits.state(id).unwrap()["attempt_at"].is_null());

    drop(slot);
    h.runtime
        .tests
        .wait_and_run(&queued, Some(context))
        .await
        .unwrap();
    let completed = h.runtime.tests.get(&job_id).unwrap();
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["requested_model"], "gpt-6-astra");
    assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(h.mock.count("POST", "/reset-quota"), 1);
    assert!(h.mock.unexpected().is_empty());
    h.close().await;
}

#[tokio::test]
async fn credit_model_manual_control_keeps_consumed_receipt_and_never_restores_schedule() {
    let h = Harness::new().await;
    let (id, _) = seed_credit(&h).await;
    let gate = Gate::new();
    h.mock.set_model_gate(gate.clone());
    let slots = h
        .runtime
        .tests
        .jobs
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let (job, context) = queue_credit(&h, id).await;
    drop(slots);
    let tests = h.runtime.tests.clone();
    let task = tokio::spawn(async move { tests.wait_and_run(&job, Some(context)).await });
    tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
        .await
        .unwrap();
    close_schedule(&h, id).await;
    let receipt = h.runtime.monitor.credits.state(id).unwrap();
    assert_eq!(receipt["stage"], "manual");
    assert_eq!(receipt["consumed"], true);
    assert!(receipt["reset_completed_at"].is_string());
    assert_eq!(receipt["owns_pause"], false);
    gate.release.notify_one();
    task.await.unwrap().unwrap();
    h.set_now(h.now() + Duration::hours(2));
    credit_cycle(&h, id).await;
    let final_task = h.runtime.monitor.credits.state(id).unwrap();
    assert_eq!(final_task["episode"], receipt["episode"]);
    assert_eq!(final_task["receipt"], receipt["receipt"]);
    assert_eq!(h.mock.count("POST", "/recover-state"), 0);
    assert_eq!(h.mock.count("POST", "/reset-quota"), 1);
    assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(
        h.runtime.monitor.row(id).await.unwrap()["schedulable"],
        false
    );
    assert_eq!(
        h.runtime.monitor.state().unwrap()["recovery_history"],
        json!({})
    );
    h.close().await;
}

#[tokio::test]
async fn credit_model_cooldown_finishes_unissued_episode_without_account_hold() {
    let h = Harness::new().await;
    let (id, _) = seed_credit(&h).await;
    h.runtime
        .detection
        .store
        .transaction(|state| {
            state["automatic_starts"][id.to_string()] =
                json!({"job_id":"fixture-previous","started_at":datetime(h.now())});
            Ok(())
        })
        .unwrap();
    credit_cycle(&h, id).await;
    let task = h.runtime.monitor.credits.state(id).unwrap();
    assert_eq!(task["stage"], "finished");
    assert_eq!(task["result_reason"], "model_cooldown");
    assert_eq!(task["outcome"], "not_executed");
    assert!(task["attempt_at"].is_null());
    assert!(h.runtime.tests.latest(id).unwrap().is_null());
    assert_eq!(h.mock.count("POST", "/reset-quota"), 0);
    assert_eq!(h.mock.count("POST", "/schedulable"), 0);
    assert!(h
        .runtime
        .monitor
        .lease(&h.runtime.monitor.row(id).await.unwrap())
        .is_ok());
    h.set_now(h.now() + Duration::seconds(300));
    credit_cycle(&h, id).await;
    assert_eq!(
        h.runtime.monitor.credits.state(id).unwrap()["stage"],
        "finished"
    );
    assert_eq!(h.mock.count("POST", "/reset-quota"), 0);
    h.close().await;
}

#[tokio::test]
async fn credit_model_failures_obey_terminal_evidence_and_cooldown() {
    for code in [400, 0, 502] {
        let h = Harness::new().await;
        let (id, _) = seed_credit(&h).await;
        *h.mock.model_status.lock().unwrap() = code;
        let job = run_credit_model(&h, id).await;
        assert_eq!(job["status"], "failed");
        assert_eq!(h.mock.count("POST", "/reset-quota"), 1);
        let expected_requests = if code == 502 { 3 } else { 1 };
        assert_eq!(h.mock.count("POST", "/v1/responses"), expected_requests);
        if code == 0 {
            assert_eq!(job["uncertain"], true);
            assert_eq!(job["can_retry"], false);
        }
        h.set_now(h.now() + Duration::seconds(30));
        credit_cycle(&h, id).await;
        h.set_now(h.now() + Duration::seconds(60));
        credit_cycle(&h, id).await;
        let task = h.runtime.monitor.credits.state(id).unwrap();
        assert_eq!(task["stage"], "finished");
        assert_eq!(task["outcome"], "unrecovered");
        if code == 400 {
            assert_eq!(task["result_reason"], "invalid_model_or_request");
        } else if code == 502 {
            assert_eq!(task["result_reason"], "model_cooldown");
        } else {
            assert_eq!(task["result_reason"], "verification_interrupted");
        }
        assert_eq!(h.mock.count("POST", "/reset-quota"), 1);
        assert_eq!(h.mock.count("POST", "/v1/responses"), expected_requests);
        assert_eq!(h.mock.count("POST", "/recover-state"), 0);
        assert_eq!(
            h.runtime.monitor.state().unwrap()["recovery_history"],
            json!({})
        );
        h.close().await;
    }
}

#[tokio::test]
async fn credit_model_unknown_consumption_survives_full_runtime_restart_without_replay() {
    let mut h = Harness::new().await;
    let (id, _) = seed_credit(&h).await;
    *h.mock.reset_uncertain.lock().unwrap() = true;
    let job = run_credit_model(&h, id).await;
    assert_eq!(job["status"], "failed");
    let before = h.runtime.monitor.credits.state(id).unwrap();
    assert!(before["attempt_at"].is_string());
    assert_eq!(h.mock.count("POST", "/v1/responses"), 0);
    h.set_now(h.now() + Duration::seconds(2));
    credit_cycle(&h, id).await;
    h.restart().await;
    for _ in 0..2 {
        h.set_now(h.now() + Duration::hours(2));
        credit_cycle(&h, id).await;
    }
    let state = h.runtime.monitor.state().unwrap();
    let task = &state["scheduler"][id.to_string()]["auto_reset_credit"];
    assert_eq!(task["episode"], before["episode"]);
    assert_eq!(task["attempt_at"], before["attempt_at"]);
    assert_eq!(task["stage"], "finished");
    assert_eq!(task["outcome"], "unknown");
    assert_eq!(h.mock.count("POST", "/reset-quota"), 1);
    assert_eq!(h.mock.count("POST", "/recover-state"), 0);
    assert_eq!(h.mock.count("POST", "/v1/responses"), 0);
    assert_eq!(h.mock.count("GET", "/usage"), 0);
    assert_eq!(h.mock.count("POST", "/schedulable"), 1);
    assert_eq!(
        h.runtime.monitor.row(id).await.unwrap()["schedulable"],
        false
    );
    assert_eq!(state["pending_events"], json!({}));
    assert_eq!(state["recovery_history"], json!({}));
    assert_eq!(
        state["scheduler"][id.to_string()]["credit_result_notifications"]
            [task["episode"].as_str().unwrap()]["status"],
        "pending"
    );
    h.close().await;
}

#[tokio::test]
async fn credit_model_filters_local_limiters_before_selecting_true_upstream_evidence() {
    for variant in ["message", "body", "errors", "owner", "newer"] {
        let h = Harness::new().await;
        let (id, _) = seed_credit(&h).await;
        let evidence_id = id + 1_000_000_000;
        let local = "You have reached the request limit: 10 requests per minute";
        match variant {
            "message" => {
                sqlx::query("UPDATE ops_error_logs SET upstream_error_message=$1 WHERE id=$2")
                    .bind("您已达到请求数限制：1分钟内最多请求 10 次")
                    .bind(evidence_id)
                    .execute(&h.db.pool)
                    .await
                    .unwrap();
            }
            "body" => {
                sqlx::query("UPDATE ops_error_logs SET error_body=$1 WHERE id=$2")
                    .bind(json!({"error":{"message":local}}).to_string())
                    .bind(evidence_id)
                    .execute(&h.db.pool)
                    .await
                    .unwrap();
            }
            "errors" => {
                sqlx::query("UPDATE ops_error_logs SET upstream_errors=$1 WHERE id=$2")
                    .bind(SqlJson(json!([{"message":local}])))
                    .bind(evidence_id)
                    .execute(&h.db.pool)
                    .await
                    .unwrap();
            }
            "owner" => {
                sqlx::query("UPDATE ops_error_logs SET error_owner='client',error_phase='concurrency',upstream_error_message='Too many concurrent requests for user' WHERE id=$1").bind(evidence_id).execute(&h.db.pool).await.unwrap();
            }
            _ => {
                sqlx::query("INSERT INTO ops_error_logs(id,account_id,created_at,platform,error_owner,error_phase,status_code,upstream_status_code,upstream_error_message) VALUES($1,$2,$3,'openai','provider','upstream',429,429,$4)").bind(evidence_id+100_000_000).bind(id).bind(h.now()-Duration::seconds(10)).bind(local).execute(&h.db.pool).await.unwrap();
            }
        }
        if variant == "newer" {
            let job = run_credit_model(&h, id).await;
            assert_eq!(job["status"], "completed");
            assert_eq!(
                h.runtime.monitor.credits.state(id).unwrap()["evidence_id"],
                evidence_id.to_string()
            );
            assert_eq!(h.mock.count("POST", "/reset-quota"), 1);
            assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
        } else {
            credit_cycle(&h, id).await;
            assert!(h.runtime.tests.latest(id).unwrap().is_null());
            assert!(h.runtime.monitor.credits.state(id).unwrap()["attempt_at"].is_null());
            assert_eq!(h.mock.count("POST", "/reset-quota"), 0);
            assert_eq!(h.mock.count("POST", "/schedulable"), 0);
            assert_eq!(
                h.runtime.monitor.row(id).await.unwrap()["schedulable"],
                true
            );
        }
        assert!(h
            .runtime
            .monitor
            .lease(&h.runtime.monitor.row(id).await.unwrap())
            .is_ok());
        h.close().await;
    }
}

#[tokio::test]
async fn credit_model_failed_evidence_read_never_falls_back_or_consumes() {
    let h = Harness::new().await;
    let (id, row) = seed_credit(&h).await;
    sqlx::query("ALTER TABLE ops_error_logs RENAME TO fixture_unavailable_error_logs")
        .execute(&h.db.pool)
        .await
        .unwrap();
    let result = h.runtime.monitor.credits.run_account(&row, h.now()).await;
    sqlx::query("ALTER TABLE fixture_unavailable_error_logs RENAME TO ops_error_logs")
        .execute(&h.db.pool)
        .await
        .unwrap();
    assert!(result.is_err());
    assert!(h.runtime.tests.latest(id).unwrap().is_null());
    assert!(h.runtime.monitor.credits.state(id).unwrap()["attempt_at"].is_null());
    assert_eq!(h.mock.count("POST", "/reset-quota"), 0);
    assert_eq!(h.mock.count("POST", "/schedulable"), 0);
    assert_eq!(
        h.runtime.monitor.row(id).await.unwrap()["schedulable"],
        true
    );
    assert!(h.runtime.monitor.lease(&row).is_ok());
    h.close().await;
}

fn temp_reason(
    now: DateTime<Utc>,
    until: DateTime<Utc>,
    status_code: i64,
    matched_keyword: &str,
    rule_index: i64,
    error_message: &str,
) -> String {
    serde_json::to_string(&json!({
        "until_unix": until.timestamp(),
        "triggered_at_unix": (now - Duration::seconds(30)).timestamp(),
        "status_code": status_code,
        "matched_keyword": matched_keyword,
        "rule_index": rule_index,
        "error_message": error_message,
    }))
    .unwrap()
}

fn keepalive_row(now: DateTime<Utc>, until: DateTime<Utc>) -> Value {
    json!({
        "id": 7,
        "deleted_at": null,
        "status": "active",
        "schedulable": true,
        "rate_limited_at": null,
        "overload_until": null,
        "temp_unschedulable_until": datetime(until),
        "temp_unschedulable_reason": temp_reason(
            now,
            until,
            500,
            "server_error",
            0,
            "fixture upstream failure"
        ),
    })
}

async fn seed_keepalive_account(
    h: &Harness,
    parent: Option<i64>,
    include_model_limits: bool,
) -> i64 {
    let row = h.seed_account("oauth", true, false).await;
    let id = integer(&row["id"]).unwrap();
    let mut extra = json!({});
    if let Some(parent) = parent {
        extra["parent_account_id"] = parent.into();
    }
    if include_model_limits {
        extra["model_rate_limits"] = json!({
            "fixture-model": {
                "limit": 100,
                "used": 17,
                "rate_limit_reset_at": datetime(h.now() + Duration::hours(2))
            }
        });
    }
    let rate_reset = include_model_limits.then_some(h.now() + Duration::hours(2));
    sqlx::query(
        "UPDATE accounts SET parent_account_id=$1,extra=$2,rate_limit_reset_at=$3 WHERE id=$4",
    )
    .bind(parent)
    .bind(SqlJson(extra))
    .bind(rate_reset)
    .bind(id)
    .execute(&h.runtime.db.pool)
    .await
    .expect("attach synthetic parent and model-limit fixture data");
    id
}

async fn set_keepalive_incident(h: &Harness, id: i64, at: DateTime<Utc>, until: DateTime<Utc>) {
    let reason = temp_reason(
        at,
        until,
        500,
        "server_error",
        0,
        "fixture upstream failure",
    );
    sqlx::query("UPDATE accounts SET temp_unschedulable_until=$1,temp_unschedulable_reason=$2,updated_at=$3 WHERE id=$4")
        .bind(until)
        .bind(reason)
        .bind(at)
        .bind(id)
        .execute(&h.runtime.db.pool)
        .await
        .expect("seed a native-shaped temporary scheduling pause");
}

async fn enable_keepalive(h: &Harness, id: i64) -> Value {
    let view = h.runtime.keepalive.view(id).await.unwrap();
    h.runtime
        .keepalive
        .save(
            id,
            &json!({"enabled":true,"expected_version":view["version"]}),
        )
        .await
        .unwrap()
}

async fn wait_control<F>(h: &Harness, id: i64, mut ready: F) -> crate::keepalive_state::Control
where
    F: FnMut(&crate::keepalive_state::Control) -> bool,
{
    tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            let control = h.runtime.keepalive.control(id).unwrap();
            if ready(&control) {
                return control;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("keepalive worker should persist its terminal state")
}

async fn wait_view_status(h: &Harness, id: i64, status: &str) -> Value {
    tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            let view = h.runtime.keepalive.view(id).await.unwrap();
            if view["status"] == status {
                return view;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("keepalive view should leave the transient running state")
}

async fn wait_delete_started(h: &Harness) {
    tokio::time::timeout(StdDuration::from_secs(5), h.mock.delete_started.notified())
        .await
        .expect("keepalive should send its loopback DELETE request");
}

async fn wait_delete_count(h: &Harness, expected: usize) {
    tokio::time::timeout(StdDuration::from_secs(5), async {
        while h.mock.temp_delete_ids().len() < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("keepalive dispatcher should reach the expected DELETE count");
}

#[test]
fn keepalive_requires_six_native_row_conditions_and_exact_reason_shape() {
    let now = reference_time();
    let until = now + Duration::minutes(5);
    let original = keepalive_row(now, until);
    assert_eq!(crate::keepalive_state::row_reason(&original, now), "");
    assert_eq!(
        crate::keepalive_state::eligible_reason(&original),
        Ok("临时错误策略")
    );

    let mut deleted = original.clone();
    deleted["deleted_at"] = datetime(now).into();
    assert_eq!(
        crate::keepalive_state::row_reason(&deleted, now),
        "账号已删除"
    );

    let mut inactive = original.clone();
    inactive["status"] = "disabled".into();
    assert_eq!(
        crate::keepalive_state::row_reason(&inactive, now),
        "账号停用或认证异常"
    );

    let mut unschedulable = original.clone();
    unschedulable["schedulable"] = false.into();
    assert_eq!(
        crate::keepalive_state::row_reason(&unschedulable, now),
        "调度已关闭"
    );

    let mut rate_limited = original.clone();
    rate_limited["rate_limited_at"] = datetime(now).into();
    assert_eq!(
        crate::keepalive_state::row_reason(&rate_limited, now),
        "账号限流或过载不参与保活"
    );

    let mut overloaded = original.clone();
    overloaded["overload_until"] = datetime(until).into();
    assert_eq!(
        crate::keepalive_state::row_reason(&overloaded, now),
        "账号限流或过载不参与保活"
    );

    let mut expired = original.clone();
    expired["temp_unschedulable_until"] = datetime(now - Duration::seconds(1)).into();
    assert_eq!(
        crate::keepalive_state::row_reason(&expired, now),
        "等待临时不可调度"
    );

    let mut malformed = original.clone();
    malformed["temp_unschedulable_reason"] = "{".into();
    assert_eq!(
        crate::keepalive_state::row_reason(&malformed, now),
        "暂停原因无法识别"
    );

    let mut missing_fields = original.clone();
    missing_fields["temp_unschedulable_reason"] =
        json!({"status_code":500,"rule_index":0}).to_string().into();
    assert_eq!(
        crate::keepalive_state::row_reason(&missing_fields, now),
        "暂停原因无法识别"
    );

    let mut threshold_missing_fields = original.clone();
    threshold_missing_fields["temp_unschedulable_reason"] =
        json!({"source":"account_scheduling_threshold"})
            .to_string()
            .into();
    assert_eq!(
        crate::keepalive_state::row_reason(&threshold_missing_fields, now),
        "额度阈值暂停不参与保活"
    );

    let mut mismatched_until = original.clone();
    let mut reason: Value = serde_json::from_str(
        mismatched_until["temp_unschedulable_reason"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    reason["until_unix"] = (until.timestamp() + 1).into();
    mismatched_until["temp_unschedulable_reason"] = reason.to_string().into();
    assert_eq!(
        crate::keepalive_state::row_reason(&mismatched_until, now),
        "暂停原因无法识别"
    );

    let mut stream_timeout = original;
    stream_timeout["temp_unschedulable_reason"] = temp_reason(
        now,
        until,
        0,
        "stream_timeout",
        -1,
        "upstream stream exceeded its deadline",
    )
    .into();
    assert_eq!(crate::keepalive_state::row_reason(&stream_timeout, now), "");
    assert_eq!(
        crate::keepalive_state::eligible_reason(&stream_timeout),
        Ok("流响应超时")
    );
}

#[tokio::test]
async fn keepalive_defaults_off_persists_privately_and_preserves_corruption() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let mut h = Harness::new().await;
    let id = seed_keepalive_account(&h, None, false).await;
    let path = h.runtime.settings.keepalive_path.clone();
    assert!(
        !path.exists(),
        "default keepalive state must not be created"
    );
    let initial = h.runtime.keepalive.view(id).await.unwrap();
    assert_eq!(initial["enabled"], false);
    assert_eq!(initial["status"], "disabled");
    assert!(!path.exists(), "reading the default must remain read-only");

    let saved = h
        .runtime
        .keepalive
        .save(
            id,
            &json!({"enabled":true,"expected_version":initial["version"]}),
        )
        .await
        .unwrap();
    assert_eq!(saved["enabled"], true);
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let stale = h
        .runtime
        .keepalive
        .save(
            id,
            &json!({"enabled":false,"expected_version":initial["version"]}),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.status.as_u16(), 409);

    h.restart().await;
    h.runtime.keepalive.startup().unwrap();
    assert_eq!(h.runtime.keepalive.view(id).await.unwrap()["enabled"], true);

    fs::write(&path, b"{").unwrap();
    assert!(h.runtime.keepalive.view(id).await.is_err());
    let corrupt = h
        .runtime
        .keepalive
        .save(
            id,
            &json!({"enabled":false,"expected_version":"a".repeat(64)}),
        )
        .await
        .unwrap_err();
    assert_eq!(corrupt.status.as_u16(), 503);
    assert_eq!(fs::read(&path).unwrap(), b"{");
    h.close().await;
}

#[tokio::test]
async fn keepalive_read_only_mode_does_not_create_default_state_or_dispatch() {
    let mut h = Harness::new().await;
    let id = seed_keepalive_account(&h, None, false).await;
    set_keepalive_incident(&h, id, h.now(), h.now() + Duration::minutes(5)).await;
    let mut settings = h.runtime.settings.as_ref().clone();
    settings.read_only = true;
    h.restart_with_settings(settings).await;

    let path = h.runtime.settings.keepalive_path.clone();
    let view = h.runtime.keepalive.view(id).await.unwrap();
    assert_eq!(view["enabled"], false);
    assert_eq!(view["read_only"], true);
    assert!(h
        .runtime
        .keepalive
        .save(
            id,
            &json!({"enabled":true,"expected_version":view["version"]}),
        )
        .await
        .is_err());
    h.runtime.keepalive.tick().await.unwrap();
    assert!(!path.exists());
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 0);
    h.close().await;
}

#[tokio::test]
async fn keepalive_save_failure_leaves_state_unwritten() {
    let mut h = Harness::new().await;
    let id = seed_keepalive_account(&h, None, false).await;
    let blocker = h._temp.path().join("keepalive-parent-blocker");
    let mut settings = h.runtime.settings.as_ref().clone();
    settings.keepalive_path = blocker.join("keepalive-state.json");
    h.restart_with_settings(settings).await;
    fs::write(&blocker, b"fixture file blocks a state directory").unwrap();

    let failed = h
        .runtime
        .keepalive
        .save(
            id,
            &json!({"enabled":true,"expected_version":"b".repeat(64)}),
        )
        .await
        .unwrap_err();
    assert_eq!(failed.status.as_u16(), 500);
    assert_eq!(
        fs::read(&blocker).unwrap(),
        b"fixture file blocks a state directory"
    );
    assert!(!h.runtime.settings.keepalive_path.exists());
    h.close().await;
}

#[tokio::test]
async fn keepalive_delete_confirms_temp_pause_and_preserves_account_scheduling() {
    let h = Harness::new().await;
    let id = seed_keepalive_account(&h, None, true).await;
    let now = h.now();
    set_keepalive_incident(&h, id, now, now + Duration::minutes(5)).await;
    let before = h.runtime.monitor.row(id).await.unwrap();
    enable_keepalive(&h, id).await;

    h.runtime.keepalive.tick().await.unwrap();
    wait_delete_started(&h).await;
    let control = wait_control(&h, id, |c| {
        c.pending.is_none() && c.last_restore_at == Some(now) && c.restores.len() == 1
    })
    .await;
    assert!(control.enabled);
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 1);
    assert_eq!(h.mock.count("POST", "/schedulable"), 0);
    assert_eq!(h.mock.count("POST", "/recover-state"), 0);

    let after = h.runtime.monitor.row(id).await.unwrap();
    assert!(after["temp_unschedulable_until"].is_null());
    assert!(after["temp_unschedulable_reason"].is_null());
    assert_eq!(after["status"], before["status"]);
    assert_eq!(after["schedulable"], true);
    assert_eq!(after["rate_limit_reset_at"], before["rate_limit_reset_at"]);
    assert!(before["extra"]["model_rate_limits"].is_object());
    assert!(after["extra"]["model_rate_limits"].is_null());
    h.close().await;
}

#[tokio::test]
async fn keepalive_unknown_outcome_survives_restart_and_reserves_new_pause_budget() {
    let mut h = Harness::new().await;
    let id = seed_keepalive_account(&h, None, false).await;
    let now = h.now();
    set_keepalive_incident(&h, id, now, now + Duration::minutes(5)).await;
    enable_keepalive(&h, id).await;
    h.mock.set_temp_delete(200, false);

    h.runtime.keepalive.tick().await.unwrap();
    wait_delete_started(&h).await;
    let unknown = wait_view_status(&h, id, "needs_confirmation").await;
    assert_eq!(unknown["enabled"], true);
    assert_eq!(unknown["budget_used"], 1);
    let control = h.runtime.keepalive.control(id).unwrap();
    assert!(control.pending.is_some());
    assert_eq!(control.reservations.len(), 1);
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 1);

    h.restart().await;
    h.runtime.keepalive.startup().unwrap();
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(
        h.runtime.keepalive.view(id).await.unwrap()["status"],
        "needs_confirmation"
    );
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 1);

    let next = h.now();
    set_keepalive_incident(&h, id, next, next + Duration::minutes(6)).await;
    h.runtime.keepalive.tick().await.unwrap();
    let superseded = wait_control(&h, id, |c| c.pending.is_none()).await;
    assert_eq!(superseded.reservations.len(), 1);
    assert_eq!(
        h.runtime.keepalive.view(id).await.unwrap()["budget_used"],
        1
    );
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 1);

    h.set_now(next + Duration::seconds(59));
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 1);

    let restore_at = next + Duration::seconds(60);
    h.set_now(restore_at);
    h.mock.set_temp_delete(200, true);
    h.runtime.keepalive.tick().await.unwrap();
    wait_delete_started(&h).await;
    let confirmed = wait_control(&h, id, |c| {
        c.pending.is_none() && c.last_restore_at == Some(restore_at)
    })
    .await;
    assert_eq!(confirmed.reservations.len(), 1);
    assert_eq!(confirmed.restores.len(), 1);
    assert_eq!(
        h.runtime.keepalive.view(id).await.unwrap()["budget_used"],
        2
    );
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 2);
    h.close().await;
}

#[tokio::test]
async fn keepalive_explicit_failure_uses_injected_backoff_without_early_retry() {
    let h = Harness::new().await;
    let id = seed_keepalive_account(&h, None, false).await;
    let now = h.now();
    set_keepalive_incident(&h, id, now, now + Duration::minutes(5)).await;
    enable_keepalive(&h, id).await;
    h.mock.set_temp_delete(503, false);

    h.runtime.keepalive.tick().await.unwrap();
    wait_delete_started(&h).await;
    let failed = wait_control(&h, id, |c| c.failures == 1 && c.pending.is_none()).await;
    assert_eq!(failed.retry_at, Some(now + Duration::seconds(5)));
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 1);

    h.set_now(now + Duration::seconds(4));
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 1);

    h.set_now(now + Duration::seconds(5));
    h.runtime.keepalive.tick().await.unwrap();
    wait_delete_started(&h).await;
    let retried = wait_control(&h, id, |c| c.failures == 2 && c.pending.is_none()).await;
    assert_eq!(retried.retry_at, Some(now + Duration::seconds(35)));
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 2);
    h.close().await;
}

#[tokio::test]
async fn keepalive_spaces_restores_stops_at_five_and_keeps_rolling_budget_after_reenable() {
    let h = Harness::new().await;
    let id = seed_keepalive_account(&h, None, false).await;
    let start = h.now();
    set_keepalive_incident(&h, id, start, start + Duration::minutes(5)).await;
    enable_keepalive(&h, id).await;

    for attempt in 1..=5 {
        let restore_at = start + Duration::seconds((attempt as i64 - 1) * 60);
        if attempt > 1 {
            let preflight = restore_at - Duration::seconds(1);
            h.set_now(preflight);
            set_keepalive_incident(&h, id, preflight, preflight + Duration::minutes(5)).await;
            h.runtime.keepalive.tick().await.unwrap();
            assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), attempt - 1);
            h.set_now(restore_at);
        }
        h.runtime.keepalive.tick().await.unwrap();
        wait_delete_started(&h).await;
        let complete = wait_control(&h, id, |c| {
            c.pending.is_none()
                && c.last_restore_at == Some(restore_at)
                && c.restores.len() == attempt
        })
        .await;
        assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), attempt);
        if attempt < 5 {
            assert!(complete.enabled);
        } else {
            assert!(!complete.enabled);
            assert_eq!(complete.paused_reason, "恢复过于频繁");
        }
    }

    h.runtime.keepalive.deliver().await.unwrap();
    assert_eq!(h.mock.count("POST", "/push"), 1);
    h.runtime.keepalive.deliver().await.unwrap();
    assert_eq!(h.mock.count("POST", "/push"), 1);
    let stopped = h.runtime.keepalive.control(id).unwrap();
    assert_eq!(stopped.notifications.last().unwrap().status, "delivered");

    let reenabled_at = start + Duration::seconds(300);
    h.set_now(reenabled_at);
    enable_keepalive(&h, id).await;
    let reenabled = h.runtime.keepalive.control(id).unwrap();
    assert!(reenabled.enabled);
    assert_eq!(reenabled.restores.len(), 5);
    assert_eq!(
        h.runtime.keepalive.view(id).await.unwrap()["budget_used"],
        5
    );
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 5);

    let almost_one_hour = start + Duration::seconds(3599);
    h.set_now(almost_one_hour);
    set_keepalive_incident(
        &h,
        id,
        almost_one_hour,
        almost_one_hour + Duration::minutes(5),
    )
    .await;
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 5);

    let one_hour = start + Duration::hours(1);
    h.set_now(one_hour);
    h.runtime.keepalive.tick().await.unwrap();
    wait_delete_started(&h).await;
    let rolled = wait_control(&h, id, |c| {
        c.pending.is_none() && c.last_restore_at == Some(one_hour)
    })
    .await;
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 6);
    assert!(
        !rolled.enabled,
        "the fifth restore in the new rolling hour pauses again"
    );
    h.close().await;
}

#[tokio::test]
async fn keepalive_restart_never_redelivers_an_interrupted_bark_notice() {
    let mut h = Harness::new().await;
    let id = seed_keepalive_account(&h, None, false).await;
    h.runtime
        .keepalive
        .store
        .transaction(|data| {
            let control = crate::keepalive_state::Control {
                enabled: false,
                generation: 3,
                notifications: vec![crate::keepalive_state::Notice {
                    id: "fixture-interrupted-notice".into(),
                    generation: 3,
                    status: "sending".into(),
                    attempts: 1,
                    next_at: None,
                }],
                ..Default::default()
            };
            data["accounts"][id.to_string()] = serde_json::to_value(control).unwrap();
            Ok(())
        })
        .unwrap();

    h.restart().await;
    h.runtime.keepalive.startup().unwrap();
    let control = h.runtime.keepalive.control(id).unwrap();
    assert_eq!(control.notifications[0].status, "needs_confirmation");
    h.runtime.keepalive.deliver().await.unwrap();
    h.runtime.keepalive.deliver().await.unwrap();
    assert_eq!(h.mock.count("POST", "/push"), 0);
    assert_eq!(
        h.runtime.keepalive.control(id).unwrap().notifications[0].status,
        "needs_confirmation"
    );
    h.close().await;
}

#[tokio::test]
async fn keepalive_blocks_parent_manual_model_and_degradation_recovery_work() {
    let h = Harness::new().await;
    let parent = seed_keepalive_account(&h, None, false).await;
    let child = seed_keepalive_account(&h, Some(parent), false).await;
    let now = h.now();
    set_keepalive_incident(&h, child, now, now + Duration::minutes(5)).await;
    enable_keepalive(&h, child).await;

    let operation_id = "fixture-parent-manual-operation";
    h.runtime
        .operations
        .store
        .0
        .transaction(|state| {
            state["jobs"][operation_id] = json!({
                "id":operation_id,
                "account_id":parent,
                "payload":{},
                "status":"queued"
            });
            Ok(())
        })
        .unwrap();
    assert_eq!(
        h.runtime.keepalive.view(child).await.unwrap()["reason"],
        "等待人工操作完成或核对"
    );
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 0);

    h.runtime
        .operations
        .store
        .0
        .transaction(|state| {
            state["jobs"][operation_id]["status"] = "completed".into();
            Ok(())
        })
        .unwrap();
    let model_job = "fixture-parent-model-test";
    h.runtime
        .tests
        .store
        .transaction(|state| {
            state["jobs"][model_job] = json!({
                "id":model_job,
                "account_id":parent,
                "status":"queued"
            });
            Ok(())
        })
        .unwrap();
    assert_eq!(
        h.runtime.keepalive.view(child).await.unwrap()["reason"],
        "模型测试或复查正在排队或执行"
    );
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 0);

    h.runtime
        .tests
        .store
        .transaction(|state| {
            state["jobs"][model_job]["status"] = "completed".into();
            Ok(())
        })
        .unwrap();
    h.runtime
        .detection
        .update(
            child,
            json!({
                "hold":true,
                "recheck_recovery":{"status":"pending","job_id":"fixture-recheck"}
            }),
        )
        .unwrap();
    assert_eq!(
        h.runtime.keepalive.view(child).await.unwrap()["reason"],
        "检测处置未释放，保活暂停"
    );
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 0);

    h.runtime
        .detection
        .update(child, json!({"hold":false}))
        .unwrap();
    h.runtime
        .detection
        .capacity
        .store
        .transaction(|state| {
            state["marks"][child.to_string()] = json!({
                "marked":true,
                "marked_at":datetime(now),
                "changed_at":datetime(now)
            });
            Ok(())
        })
        .unwrap();
    assert_eq!(
        h.runtime.keepalive.view(child).await.unwrap()["reason"],
        "已判定降智，保活暂停"
    );
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 0);

    h.runtime
        .detection
        .capacity
        .store
        .transaction(|state| {
            state["marks"][child.to_string()] = json!({
                "marked":false,
                "marked_at":null,
                "changed_at":datetime(now)
            });
            Ok(())
        })
        .unwrap();
    h.runtime
        .monitor
        .store
        .transaction(|state| {
            state["scheduler"][parent.to_string()]["recovery_intent"] = json!({"status":"running"});
            Ok(())
        })
        .unwrap();
    assert_eq!(
        h.runtime.keepalive.view(child).await.unwrap()["reason"],
        "等待恢复任务完成"
    );
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 0);

    h.runtime
        .monitor
        .store
        .transaction(|state| {
            state["scheduler"][parent.to_string()]["recovery_intent"]["status"] =
                "recovered".into();
            Ok(())
        })
        .unwrap();
    h.runtime.keepalive.tick().await.unwrap();
    wait_delete_started(&h).await;
    let restored = wait_control(&h, child, |c| {
        c.pending.is_none() && c.last_restore_at == Some(now)
    })
    .await;
    assert!(restored.enabled);
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 1);
    h.close().await;
}

#[tokio::test]
async fn keepalive_sibling_dispatches_share_the_parent_account_lock() {
    let h = Harness::new().await;
    let parent = seed_keepalive_account(&h, None, false).await;
    let first = seed_keepalive_account(&h, Some(parent), false).await;
    let second = seed_keepalive_account(&h, Some(parent), false).await;
    let now = h.now();
    for id in [first, second] {
        set_keepalive_incident(&h, id, now, now + Duration::minutes(5)).await;
        enable_keepalive(&h, id).await;
    }
    let gate = Gate::new();
    h.mock.set_temp_delete_gate(Some(gate.clone()));
    h.runtime.keepalive.tick().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(5), gate.entered.notified())
        .await
        .expect("first sibling should hold the shared parent lock while dispatching");
    let dispatched = h.mock.temp_delete_ids();
    assert_eq!(dispatched.len(), 1);
    let first_id = dispatched[0];
    let blocked_id = if first_id == first { second } else { first };
    let blocked = wait_view_status(&h, blocked_id, "waiting").await;
    assert_eq!(blocked["enabled"], true);
    assert_eq!(
        h.runtime.keepalive.view(first_id).await.unwrap()["status"],
        "running"
    );
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 1);

    gate.release.notify_one();
    let completed = wait_control(&h, first_id, |c| {
        c.pending.is_none() && c.last_restore_at == Some(now)
    })
    .await;
    assert_eq!(completed.restores.len(), 1);
    h.mock.set_temp_delete_gate(None);
    h.runtime.keepalive.tick().await.unwrap();
    wait_delete_started(&h).await;
    let sibling_completed = wait_control(&h, blocked_id, |c| {
        c.pending.is_none() && c.last_restore_at == Some(now)
    })
    .await;
    assert_eq!(sibling_completed.restores.len(), 1);
    assert_eq!(h.mock.count("DELETE", "/temp-unschedulable"), 2);
    assert_eq!(h.mock.count("POST", "/schedulable"), 0);
    h.close().await;
}

#[tokio::test]
async fn keepalive_dispatch_is_limited_to_eight_and_releases_capacity_and_locks() {
    let h = Harness::new().await;
    let mut ids = Vec::new();
    for _ in 0..9 {
        let id = seed_keepalive_account(&h, None, false).await;
        let now = h.now();
        set_keepalive_incident(&h, id, now, now + Duration::minutes(5)).await;
        enable_keepalive(&h, id).await;
        ids.push(id);
    }
    let gate = BulkGate::new();
    h.mock.set_bulk_delete_gate(Some(gate.clone()));

    h.runtime.keepalive.tick().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(5), async {
        while gate.entered.load(Ordering::SeqCst) < 8 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first tick should dispatch eight independent account restores");
    assert_eq!(gate.entered.load(Ordering::SeqCst), 8);
    assert_eq!(h.mock.temp_delete_ids().len(), 8);

    h.runtime.keepalive.tick().await.unwrap();
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let mut in_flight = 0;
    for id in &ids {
        if h.runtime.keepalive.view(*id).await.unwrap()["status"] == "running" {
            in_flight += 1;
        }
    }
    assert_eq!(in_flight, 8);
    assert_eq!(gate.entered.load(Ordering::SeqCst), 8);
    assert_eq!(h.mock.temp_delete_ids().len(), 8);

    gate.release.add_permits(8);
    let now = h.now();
    for id in &ids[..8] {
        let completed = wait_control(&h, *id, |c| {
            c.pending.is_none() && c.last_restore_at == Some(now)
        })
        .await;
        assert_eq!(completed.restores.len(), 1);
        wait_view_status(&h, *id, "waiting").await;
    }

    h.mock.set_bulk_delete_gate(None);
    h.runtime.keepalive.tick().await.unwrap();
    wait_delete_count(&h, 9).await;
    let completed = wait_control(&h, ids[8], |c| {
        c.pending.is_none() && c.last_restore_at == Some(now)
    })
    .await;
    assert_eq!(completed.restores.len(), 1);
    for id in &ids {
        wait_view_status(&h, *id, "waiting").await;
    }
    assert_eq!(h.mock.temp_delete_ids().len(), 9);

    for id in &ids {
        let view = h.runtime.keepalive.view(*id).await.unwrap();
        let saved = tokio::time::timeout(
            StdDuration::from_secs(5),
            h.runtime.keepalive.save(
                *id,
                &json!({"enabled":false,"expected_version":view["version"]}),
            ),
        )
        .await
        .expect("released keepalive control lock should be reusable")
        .unwrap();
        assert_eq!(saved["enabled"], false);
    }
    h.runtime.keepalive.tick().await.unwrap();
    assert_eq!(h.mock.temp_delete_ids().len(), 9);
    h.close().await;
}

#[tokio::test]
async fn batched_waiting_accounts_handle_deleted_and_changed_configuration() {
    let h = Harness::new().await;
    let first = h.seed_account("apikey", true, false).await;
    let second = h.seed_account("apikey", true, false).await;
    let first_id = integer(&first["id"]).unwrap();
    let second_id = integer(&second["id"]).unwrap();
    for id in [first_id, second_id] {
        h.runtime.detection.update(id,json!({"enabled":true,"schedule_mode":"fixed","interval_minutes":15,"status":"waiting","next_at":datetime(h.now()+Duration::minutes(15))})).unwrap();
    }
    h.runtime.detection.tick().await.unwrap();
    assert_eq!(
        h.runtime.detection.control(first_id).unwrap()["status"],
        "waiting"
    );
    assert_eq!(
        h.runtime.detection.control(second_id).unwrap()["status"],
        "waiting"
    );
    sqlx::query("UPDATE accounts SET deleted_at=now() WHERE id=$1")
        .bind(second_id)
        .execute(&h.db.pool)
        .await
        .unwrap();
    h.runtime
        .detection
        .update(
            first_id,
            json!({"enabled":false,"status":"disabled","generation":1}),
        )
        .unwrap();
    h.runtime.detection.tick().await.unwrap();
    assert_eq!(
        h.runtime.detection.control(first_id).unwrap()["status"],
        "disabled"
    );
    assert_eq!(
        h.runtime.detection.control(second_id).unwrap()["status"],
        "paused"
    );
    assert_eq!(
        h.runtime.detection.control(second_id).unwrap()["reason"],
        "账号不存在"
    );
    assert_eq!(h.mock.count("POST", "/v1/responses"), 0);
    h.close().await;
}

#[tokio::test]
async fn early_reset_model_receipt_finishes_after_disable_and_restart_with_regular_monitor_off() {
    let mut h = Harness::new().await;
    let (row, mut usage, mut intent) = seed_recovery(&h, false).await;
    let id = integer(&row["id"]).unwrap();
    usage["account_id"] = id.into();
    intent["kind"] = "early_reset".into();
    intent["plan_type"] = "plus".into();
    intent["credential_fingerprint"] = oauth_admission::credential_fingerprint(&row).into();
    intent["block_state"] = crate::recovery_verification::state_block(&row);
    intent["manual_generation"] = 0.into();
    intent["observed_at"] = usage["queried_at"].clone();
    h.runtime
        .config
        .oauth
        .transaction(|c| {
            c["oauth_early_reset_enabled"] = true.into();
            c["oauth_recovery_monitor_enabled"] = false.into();
            Ok(())
        })
        .unwrap();
    h.runtime
        .monitor
        .store
        .transaction(|s| {
            s["oauth_results"][id.to_string()] = usage.clone();
            s["scheduler"][id.to_string()]["recovery_intent"] = intent.clone();
            Ok(())
        })
        .unwrap();
    let job = run_natural_model(&h, id, &intent, &usage).await;
    assert_eq!(job["status"], "completed");
    h.runtime
        .config
        .oauth
        .transaction(|c| {
            c["oauth_early_reset_enabled"] = false.into();
            Ok(())
        })
        .unwrap();
    h.restart().await;
    h.set_now(h.now() + Duration::seconds(5));
    tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            h.runtime.monitor.tick().await.unwrap();
            if h.runtime.monitor.state().unwrap()["scheduler"][id.to_string()]["recovery_intent"]
                ["status"]
                == "recovered"
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(h.mock.count("POST", "/v1/responses"), 1);
    assert_eq!(h.mock.count("POST", "/recover-state"), 1);
    assert_eq!(h.mock.count("POST", "/test"), 0);
    h.close().await;
}
