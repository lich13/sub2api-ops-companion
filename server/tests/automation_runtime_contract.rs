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
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration as StdDuration,
};
use sub2ops_server::{
    compat::{account_version, datetime, integer, parse_datetime},
    daily_test,
    db::Database,
    model_detection, oauth_admission,
    oauth_state::OAuthState,
    quota, recovery_events, recovery_rules,
    runtime::Runtime,
    settings::Settings,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

const ADMIN_KEY: &str = "fixture-admin-automation-contract";
const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";
const SUCCESS_TEST_SSE: &str = "data: {\"type\":\"test_start\",\"model\":\"fixture-model\"}\n\ndata: {\"type\":\"test_complete\",\"success\":true}\n\n";
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(9_400_000);

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/contracts/automation/automation-fixtures.json"
    ))
    .expect("pinned Python automation fixture must parse")
}

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

    fn set_recovery_gate(&self, gate: Gate) {
        *self.recovery_gate.lock().unwrap() = Some(gate);
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
        .expect("loopback mock did not receive the expected request");
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

    if method == Method::GET && path.starts_with("/fixture-models/") {
        assert!(!parts.headers.contains_key("x-api-key"));
        assert!(parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("Bearer fixture-")));
        return json_response(json!({
            "data": [
                {"id":"gpt-6-luna","name":"fixture model"},
                {"id":"gpt-5.6-luna","name":"fixture reference"},
                {"id":"fixture-model","name":"fixture recovery model"}
            ]
        }));
    }

    if method == Method::GET && path.ends_with("/models") {
        return json_response(json!({
            "code": 0,
            "data": [
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
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{"type":"invalid_request_error","message":"synthetic fixture rejection"}})),
        )
            .into_response();
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
    runtime: Arc<Runtime>,
    mock: MockUpstream,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    base_url: String,
    server: Option<JoinHandle<()>>,
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
            .monitor
            .store
            .save_admin_token(ADMIN_KEY)
            .expect("save synthetic Sub2API token");
        Self {
            _temp: temp,
            db,
            runtime,
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
        self.runtime.close().await;
        self.db.close().await;
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
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
    let row = harness.seed_account("oauth", true, true).await;
    let id = integer(&row["id"]).unwrap();
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
            raw["oauth_recovery_connection_account_ids"] = json!([id]);
            raw["oauth_recovery_model_account_ids"] = json!([]);
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

#[test]
fn pinned_reference_fixture_and_beijing_time_contract_are_fixed() {
    let reference = fixture();
    assert_eq!(reference["reference"]["sha"], REFERENCE_SHA);
    assert_eq!(
        daily_test::next_time(reference_time(), "14:08").unwrap(),
        parse_datetime(&reference["daily_test"]["scheduled_at"]).unwrap()
    );
    assert_eq!(
        reference["detection"]["manual_close"]["generation_delta"],
        1
    );
    assert_eq!(reference["recovery_receipt"]["format"], "legacy-envelope");
    assert_eq!(
        reference["recovery_receipt"]["required_keys"],
        json!(["block_state", "credential_fingerprint", "result"])
    );
    assert_eq!(
        reference["recovery_receipt"]["unknown_verification_error"],
        "verification_uncertain"
    );
}

#[tokio::test]
async fn manual_query_caches_verified_card_receipt_using_the_runtime_clock() {
    use sub2ops_server::oauth_queries::{QueryOptions, Source};
    let harness = Harness::new().await;
    let row = harness.seed_account("oauth", true, false).await;
    let id = integer(&row["id"]).unwrap();
    let cards = json!({"available_count":1,"credits":[{
        "expires_at":datetime(harness.now() + Duration::days(2))
    }]});
    let attempts = json!([datetime(harness.now() - Duration::minutes(1))]);
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()] = json!({
                "quota_query": {"automatic_attempts":attempts}
            });
            Ok(())
        })
        .unwrap();
    *harness.mock.usage_cards.lock().unwrap() = Some(cards);
    let result = harness
        .runtime
        .monitor
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Manual,
                reason: "fixture-card-observation".into(),
                at: harness.now(),
                ..QueryOptions::default()
            },
        )
        .await;
    assert_eq!(result["success"], true);
    let state = harness.runtime.monitor.state().unwrap();
    let metadata = &state["scheduler"][id.to_string()];
    let receipt = &metadata["reset_credit_observation"];
    assert_eq!(receipt["observed_at"], datetime(harness.now()));
    assert_eq!(receipt["available_count"], 1);
    assert_eq!(
        receipt["window_reset_at"],
        datetime(harness.now() + Duration::days(7))
    );
    sub2ops_server::oauth_state::validate_observation(receipt).unwrap();
    assert_eq!(metadata["quota_query"]["automatic_attempts"], attempts);
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 0);
    assert_eq!(harness.mock.count("POST", "/reset-quota"), 0);
    assert!(harness.mock.unexpected().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn daily_worker_runs_once_in_window_and_persists_missed_work_without_replay() {
    let reference = fixture();
    let harness = Harness::new().await;
    let row = harness.seed_account("oauth", true, false).await;
    let id = integer(&row["id"]).unwrap();
    let config = harness
        .runtime
        .config
        .current()
        .expect("read fixture config");
    let startup_at = parse_datetime(&reference["daily_test"]["startup_at"]).unwrap();
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["daily_test"]["batches"]["2031-04-03"] =
                json!({"date":"2031-04-03","status":"running","accounts":{}});
            state["daily_test"]["batches"]["2031-04-04"] =
                json!({"date":"2031-04-04","status":"queued","accounts":{}});
            Ok(())
        })
        .expect("seed stale queued/running daily state");
    harness
        .runtime
        .monitor
        .schedule
        .startup(&config, startup_at)
        .expect("recover stale daily batches and initialize schedule");
    let after_startup = harness.runtime.monitor.state().unwrap()["daily_test"].clone();
    assert_eq!(
        after_startup["batches"]["2031-04-03"]["status"],
        "interrupted"
    );
    assert_eq!(
        after_startup["batches"]["2031-04-04"]["status"],
        "interrupted"
    );
    assert_eq!(
        after_startup["next_run_at"],
        reference["daily_test"]["scheduled_at"]
    );

    let scheduled_at = parse_datetime(&reference["daily_test"]["scheduled_at"]).unwrap();
    harness.set_now(scheduled_at - Duration::seconds(1));
    harness
        .runtime
        .monitor
        .schedule
        .tick(&config, harness.now())
        .unwrap();
    assert!(harness
        .runtime
        .monitor
        .schedule
        .claim(&config, harness.now())
        .unwrap()
        .is_none());

    let on_time_at = parse_datetime(&reference["daily_test"]["on_time_tick_at"]).unwrap();
    harness.set_now(on_time_at);
    harness
        .runtime
        .monitor
        .schedule
        .tick(&config, on_time_at)
        .unwrap();
    let on_time_batch = harness
        .runtime
        .monitor
        .schedule
        .claim(
            &config,
            parse_datetime(&reference["daily_test"]["on_time_claim_at"]).unwrap(),
        )
        .unwrap()
        .expect("scheduled daily run inside the one-minute window should be claimed");
    assert_eq!(
        on_time_batch["status"],
        reference["daily_test"]["claimed_status"]
    );
    harness
        .runtime
        .monitor
        .run_daily(on_time_batch, vec![row])
        .await
        .expect("run real daily quota query and connection test workers");

    let completed = harness.runtime.monitor.state().unwrap()["daily_test"].clone();
    let date = "2031-04-05";
    assert_eq!(completed["batches"][date]["status"], "completed");
    assert_eq!(
        completed["batches"][date]["accounts"][id.to_string()]["status"],
        "success"
    );
    assert_eq!(harness.mock.count("GET", "/usage"), 1);
    assert_eq!(harness.mock.count("POST", "/test"), 1);

    let late_at = parse_datetime(&reference["daily_test"]["late_tick_at"]).unwrap();
    harness.set_now(late_at);
    harness
        .runtime
        .monitor
        .schedule
        .tick(&config, late_at)
        .unwrap();
    let late = harness.runtime.monitor.state().unwrap()["daily_test"].clone();
    assert_eq!(
        late["batches"]["2031-04-06"]["status"],
        reference["daily_test"]["late_status"]
    );
    assert!(harness
        .runtime
        .monitor
        .schedule
        .claim(&config, late_at)
        .unwrap()
        .is_none());
    assert_eq!(
        harness.mock.count("GET", "/usage"),
        1,
        "late work must not requery upstream"
    );
    assert_eq!(
        harness.mock.count("POST", "/test"),
        1,
        "late work must not retest upstream"
    );
    assert!(
        harness.mock.unexpected().is_empty(),
        "unexpected loopback calls: {:?}",
        harness.mock.unexpected()
    );
    harness.close().await;
}

#[tokio::test]
async fn recovery_unknown_test_is_blocked_and_never_replayed() {
    for error_code in [
        None,
        Some("network_error"),
        Some("timeout"),
        Some("account_test_error"),
    ] {
        let harness = Harness::new().await;
        let (row, usage, intent) = seed_recovery(&harness, true).await;
        if let Some(code) = error_code {
            harness.mock.set_test_sse(format!(
                "data: {}\n\n",
                json!({"type":"error","code":code,"message":"fixture uncertain transport"})
            ));
        }
        let id = integer(&row["id"]).unwrap();
        let generation = harness.runtime.monitor.generation(id).unwrap();
        let lease = harness.runtime.monitor.lease(&row).unwrap();
        let result = harness
            .runtime
            .monitor
            .verify_recovery(id, &intent, &usage, generation, 0, "connection", lease)
            .await
            .expect("run real connection verification against synthetic incomplete SSE");
        assert_eq!(result["error_code"], "verification_uncertain");
        let after = harness.runtime.monitor.state().unwrap();
        let saved = &after["scheduler"][id.to_string()]["recovery_intent"];
        assert_eq!(saved["status"], "blocked");
        assert_eq!(saved["last_error_code"], "verification_uncertain");
        assert_eq!(saved["verification_dispatched"], true);
        assert_eq!(saved["verification_receipt"]["result"]["uncertain"], true);
        assert_eq!(harness.mock.count("POST", "/test"), 1);
        assert_eq!(harness.mock.count("POST", "/recover-state"), 0);

        harness.runtime.monitor.reconcile_unknown(id).await.unwrap();
        harness.runtime.monitor.reconcile_unknown(id).await.unwrap();
        let retry_row = harness.runtime.monitor.row(id).await.unwrap();
        let lease = harness.runtime.monitor.lease(&retry_row).unwrap();
        let retry = harness
            .runtime
            .monitor
            .verify_recovery(id, &intent, &usage, generation, 0, "connection", lease)
            .await
            .expect("re-read uncertain verification without dispatching it again");
        assert_eq!(retry["error_code"], "verification_uncertain");
        assert_eq!(
            harness.mock.count("POST", "/test"),
            1,
            "uncertain test must not be replayed"
        );
        assert_eq!(harness.mock.count("POST", "/recover-state"), 0);
        assert!(
            harness.mock.unexpected().is_empty(),
            "unexpected loopback calls: {:?}",
            harness.mock.unexpected()
        );
        harness.close().await;
    }
}

#[tokio::test]
async fn successful_recovery_receipt_survives_interrupted_write_and_old_reader_restart() {
    let harness = Harness::new().await;
    let (row, usage, intent) = seed_recovery(&harness, false).await;
    let id = integer(&row["id"]).unwrap();
    let generation = harness.runtime.monitor.generation(id).unwrap();
    let gate = Gate::new();
    harness.mock.set_recovery_gate(gate.clone());
    let monitor = harness.runtime.monitor.clone();
    let original = intent.clone();
    let saved_usage = usage.clone();
    let lease = harness.runtime.monitor.lease(&row).unwrap();
    let task = tokio::spawn(async move {
        monitor
            .verify_recovery(
                id,
                &original,
                &saved_usage,
                generation,
                0,
                "connection",
                lease,
            )
            .await
    });
    harness.mock.wait_for("POST", "/test", 1).await;
    tokio::time::timeout(StdDuration::from_secs(8), gate.entered.notified())
        .await
        .expect("synthetic recovery write did not enter its response gate");

    let state = harness.runtime.monitor.state().unwrap();
    let durable_intent = &state["scheduler"][id.to_string()]["recovery_intent"];
    let receipt = &durable_intent["verification_receipt"];
    let receipt_keys = receipt
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let reference = fixture();
    let required_keys = reference["recovery_receipt"]["required_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let actual_keys = receipt_keys
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert!(required_keys.is_subset(&actual_keys));
    let mut allowed_keys = required_keys.clone();
    allowed_keys.insert("manual_generation".into());
    assert_eq!(actual_keys, allowed_keys);
    assert_eq!(durable_intent["verification_dispatched"], true);
    assert_eq!(durable_intent["recovery_write_dispatched"], true);
    assert_eq!(receipt["manual_generation"], generation);
    assert_eq!(receipt["result"]["success"], true);
    assert_eq!(receipt["result"]["recovery_method"], "connection");
    assert_eq!(receipt["result"]["recovery_method_generation"], 0);
    assert_eq!(
        receipt["credential_fingerprint"].as_str().unwrap().len(),
        64
    );
    assert!(receipt["block_state"].is_object());

    if let Some(export_dir) = std::env::var_os("AUTOMATION_STATE_EXPORT_DIR") {
        let export_dir = PathBuf::from(export_dir);
        fs::create_dir_all(&export_dir).expect("create runner-owned receipt export directory");
        fs::copy(
            &harness.runtime.settings.usage_path,
            export_dir.join("automation-recovery-receipt.json"),
        )
        .expect("export the exact durable state while its successful receipt is pending");
    }
    let reopened = OAuthState::new(harness.runtime.settings.usage_path.clone(), false)
        .snapshot()
        .expect("fresh Rust OAuthState must read the interrupted-write receipt");
    assert_eq!(
        reopened["scheduler"][id.to_string()]["recovery_intent"]["verification_receipt"],
        *receipt
    );
    assert_eq!(harness.mock.count("POST", "/test"), 1);
    assert_eq!(harness.mock.count("POST", "/recover-state"), 1);
    assert_eq!(
        sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "SELECT rate_limited_at FROM accounts WHERE id=$1"
        )
        .bind(id)
        .fetch_one(&harness.runtime.db.pool)
        .await
        .unwrap(),
        None,
        "mock recovery applied before the response was released"
    );

    task.abort();
    assert!(
        task.await.unwrap_err().is_cancelled(),
        "simulate process loss waiting on response"
    );
    gate.release.notify_one();

    harness.runtime.monitor.reconcile_unknown(id).await.unwrap();
    harness.runtime.monitor.reconcile_unknown(id).await.unwrap();
    let recovered = harness.runtime.monitor.state().unwrap();
    assert_eq!(
        recovered["scheduler"][id.to_string()]["recovery_intent"]["status"],
        "recovered"
    );
    let key = format!(
        "recovery:{id}:{}:success",
        intent["fingerprint"].as_str().unwrap()
    );
    assert!(recovered["pending_events"][&key].is_object());
    assert!(recovered["recovery_history"][&key].is_object());
    assert_eq!(recovered["pending_events"].as_object().unwrap().len(), 1);
    assert_eq!(recovered["recovery_history"].as_object().unwrap().len(), 1);
    assert_eq!(recovered["recovery_history"][&key]["dedupe_key"], key);
    assert_eq!(
        harness.mock.count("POST", "/test"),
        1,
        "receipt recovery must not repeat connection test"
    );
    assert_eq!(
        harness.mock.count("POST", "/recover-state"),
        1,
        "unknown write response must not repeat recovery write"
    );
    assert!(
        harness.mock.unexpected().is_empty(),
        "unexpected loopback calls: {:?}",
        harness.mock.unexpected()
    );
    harness.close().await;
}

async fn prepare_detection_job(harness: &Harness) -> (Value, String, i64) {
    let row = harness.seed_account("apikey", true, false).await;
    let id = integer(&row["id"]).unwrap();
    let control = harness.runtime.detection.control(id).unwrap();
    harness
        .runtime
        .detection
        .save(
            id,
            &json!({
                "enabled": true,
                "interval_minutes": 1,
                "schedule_mode": "fixed",
                "model_id": "gpt-6-luna",
                "expected_version": model_detection::version(&control)
            }),
        )
        .await
        .expect("enable real automatic ModelTests schedule with loopback catalog");
    harness
        .runtime
        .detection
        .update(
            id,
            json!({"next_at":datetime(harness.now()-Duration::seconds(1))}),
        )
        .unwrap();
    let before = harness.runtime.detection.control(id).unwrap();
    let generation = integer(&before["generation"]).unwrap();
    harness.runtime.detection.tick().await.unwrap();
    let after = harness.runtime.detection.control(id).unwrap();
    let job_id = after["job_id"].as_str().unwrap().to_owned();
    (row, job_id, generation)
}

#[tokio::test]
async fn manual_close_during_active_detection_cancels_without_rewriting_generation_or_timer() {
    let reference = fixture();
    let harness = Harness::new().await;
    let gate = Gate::new();
    harness.mock.set_model_gate(gate.clone());
    let (row, job_id, generation) = prepare_detection_job(&harness).await;
    let id = integer(&row["id"]).unwrap();
    harness.mock.wait_for("POST", "/v1/responses", 1).await;
    let closed = close_schedule(&harness, id).await;
    assert_eq!(closed["verified"], true);
    let control = harness.runtime.detection.control(id).unwrap();
    assert_eq!(control["generation"], generation + 1);
    assert!(control["next_at"].is_null());

    harness.runtime.detection.tick().await.unwrap();
    let interim = harness.runtime.detection.control(id).unwrap();
    assert!(
        reference["detection"]["manual_close"]["active_interim_statuses"]
            .as_array()
            .unwrap()
            .contains(&interim["status"]),
        "active worker snapshot was not an allowed legacy-compatible state: {interim}"
    );
    assert_eq!(interim["generation"], generation + 1);
    assert!(interim["next_at"].is_null());
    assert_eq!(
        harness.runtime.tests.get(&job_id).unwrap()["status"],
        "cancelled"
    );

    gate.release.notify_one();
    harness.runtime.detection.tick().await.unwrap();
    let final_control = harness.runtime.detection.control(id).unwrap();
    assert_eq!(final_control["generation"], generation + 1);
    assert_eq!(
        final_control["status"],
        reference["detection"]["manual_close"]["terminal_status"]
    );
    assert_eq!(
        final_control["next_at"],
        reference["detection"]["manual_close"]["terminal_next_at"]
    );
    assert_eq!(
        harness.mock.count("POST", "/v1/responses"),
        1,
        "cancelled worker must not dispatch another model request"
    );
    let schedulable = sqlx::query_scalar::<_, bool>("SELECT schedulable FROM accounts WHERE id=$1")
        .bind(id)
        .fetch_one(&harness.runtime.db.pool)
        .await
        .unwrap();
    assert!(!schedulable);
    assert!(
        harness.mock.unexpected().is_empty(),
        "unexpected loopback calls: {:?}",
        harness.mock.unexpected()
    );
    harness.close().await;
}

#[tokio::test]
async fn manual_close_after_detection_worker_terminal_does_not_rearm_or_redispatch() {
    let reference = fixture();
    let harness = Harness::new().await;
    let (row, job_id, generation) = prepare_detection_job(&harness).await;
    let id = integer(&row["id"]).unwrap();
    harness.mock.wait_for("POST", "/v1/responses", 1).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let job = harness.runtime.tests.get(&job_id).unwrap();
            if !matches!(
                job["status"].as_str(),
                Some("queued" | "running" | "retrying")
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("synthetic model worker did not reach a terminal result");
    let closed = close_schedule(&harness, id).await;
    assert_eq!(closed["verified"], true);
    harness.runtime.detection.tick().await.unwrap();
    let final_control = harness.runtime.detection.control(id).unwrap();
    assert_eq!(final_control["generation"], generation + 1);
    assert_eq!(
        final_control["status"],
        reference["detection"]["manual_close"]["terminal_status"]
    );
    assert_eq!(
        final_control["next_at"],
        reference["detection"]["manual_close"]["terminal_next_at"]
    );
    assert_eq!(
        harness.mock.count("POST", "/v1/responses"),
        1,
        "terminal worker must not be replayed by tick"
    );
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT schedulable FROM accounts WHERE id=$1")
            .bind(id)
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap()
    );
    assert!(
        harness.mock.unexpected().is_empty(),
        "unexpected loopback calls: {:?}",
        harness.mock.unexpected()
    );
    harness.close().await;
}

#[tokio::test]
async fn manual_reopen_rearms_detection_from_saved_interval_after_terminal_worker() {
    let harness = Harness::new().await;
    let (row, job_id, generation) = prepare_detection_job(&harness).await;
    let id = integer(&row["id"]).unwrap();
    harness.mock.wait_for("POST", "/v1/responses", 1).await;
    tokio::time::timeout(StdDuration::from_secs(8), async {
        loop {
            let job = harness.runtime.tests.get(&job_id).unwrap();
            if !matches!(
                job["status"].as_str(),
                Some("queued" | "running" | "retrying")
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("synthetic model worker did not reach a terminal result");

    close_schedule(&harness, id).await;
    harness.runtime.detection.tick().await.unwrap();
    let paused = harness.runtime.detection.control(id).unwrap();
    assert_eq!(paused["generation"], generation + 1);
    assert_eq!(paused["status"], "paused");
    assert!(paused["next_at"].is_null());

    set_schedulable(&harness, id, true).await;
    harness.runtime.detection.tick().await.unwrap();
    let rearmed = harness.runtime.detection.control(id).unwrap();
    assert_eq!(rearmed["generation"], generation + 2);
    assert_eq!(rearmed["status"], "waiting");
    assert_eq!(
        rearmed["interval_minutes"],
        fixture()["detection"]["manual_reopen"]["saved_interval_minutes"]
    );
    assert_eq!(
        rearmed["next_at"],
        fixture()["detection"]["manual_reopen"]["next_at"]
    );
    assert_eq!(
        fixture()["detection"]["manual_reopen"]["next_allowed_at"],
        datetime(harness.now() + Duration::seconds(300))
    );
    assert_eq!(harness.mock.count("POST", "/v1/responses"), 1);
    assert!(
        harness.mock.unexpected().is_empty(),
        "unexpected loopback calls: {:?}",
        harness.mock.unexpected()
    );
    harness.close().await;
}

#[tokio::test]
async fn manual_revoke_filters_daily_event_and_bark_delivery_is_deduplicated() {
    let reference = fixture();
    let harness = Harness::new().await;
    let row = harness.seed_account("oauth", true, false).await;
    let id = integer(&row["id"]).unwrap();
    let removed_key = format!("daily:2031-04-05:{id}");
    let removed_recovery_key = format!("recovery:{id}:fixture:failure:synthetic_error");
    let retained_key = format!("auth:{id}:active_usage:fixture");
    let event = |stage: &str, key: &str| {
        json!({
            "account_id": id,
            "account_name": format!("fixture-automation-account-{id}"),
            "plan_type": "plus",
            "window_labels": ["fixture window"],
            "status": "auth_failed",
            "stage": stage,
            "dedupe_key": key,
            "error_code": "http_401",
            "error": "synthetic fixture event"
        })
    };
    harness
        .runtime
        .monitor
        .store
        .transaction(|state| {
            state["pending_events"][&removed_key] = event("daily_test", &removed_key);
            state["pending_events"][&removed_recovery_key] =
                event("recovery", &removed_recovery_key);
            state["pending_events"][&retained_key] = event("active_usage", &retained_key);
            Ok(())
        })
        .expect("seed three synthetic pending event stages");
    close_schedule(&harness, id).await;
    let after_revoke = harness.runtime.monitor.state().unwrap();
    assert!(after_revoke["pending_events"].get(&removed_key).is_none());
    assert!(after_revoke["pending_events"]
        .get(&removed_recovery_key)
        .is_none());
    assert!(after_revoke["pending_events"][&retained_key].is_object());
    assert_eq!(
        after_revoke["scheduler"][id.to_string()]["manual_control"]["enabled"],
        false
    );

    harness.runtime.monitor.deliver().await.unwrap();
    harness.runtime.monitor.deliver().await.unwrap();
    let after_delivery = harness.runtime.monitor.state().unwrap();
    assert!(after_delivery["pending_events"]
        .get(&retained_key)
        .is_none());
    assert_eq!(
        harness.mock.count("POST", "/push"),
        reference["manual_control"]["bark_posts_after_two_deliver_ticks"]
    );
    assert!(after_delivery["scheduler"][id.to_string()]["notified_keys"]
        .as_array()
        .unwrap()
        .contains(&json!(retained_key)));
    assert!(
        !after_delivery["scheduler"][id.to_string()]["notified_keys"]
            .as_array()
            .unwrap()
            .contains(&json!(removed_key))
    );
    assert!(
        harness.mock.unexpected().is_empty(),
        "unexpected loopback calls: {:?}",
        harness.mock.unexpected()
    );
    harness.close().await;
}
