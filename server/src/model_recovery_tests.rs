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
    fs,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration as StdDuration,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

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

fn method_name(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
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
            fallback_path: temp.path().join("key-fallback.json"),
            connection_path,
            model_path: temp.path().join("group-models.json"),
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
        runtime.close().await;
        drop(runtime);
        let clock = self.clock.clone();
        self.runtime.0 = Some(
            Runtime::new_with_clock(settings, Arc::new(move || *clock.lock().unwrap()))
                .await
                .unwrap(),
        );
        self.runtime
            .tests
            .set_test_endpoint(&format!("{}/v1/responses", self.base_url))
            .unwrap();
    }
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
    let descriptor = recovery_rules::rate_descriptor(&row, &summary);
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
        samples: StdMutex::new(Default::default()),
        challenges: modeltrace::challenges(),
        snapshot: h.runtime.tests.bank.snapshot(),
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
    assert_eq!(pending["deferred"], true);
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
    let (row, usage, intent) = seed_recovery(&h, false).await;
    let id = integer(&row["id"]).unwrap();
    let job = run_natural_model(&h, id, &intent, &usage).await;
    assert_eq!(job["status"], "completed");
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
    assert!(h.runtime.monitor.row(id).await.unwrap()["rate_limit_reset_at"].is_null());
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
