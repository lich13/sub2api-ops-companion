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
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration as StdDuration,
};
use sub2ops_server::{
    account_locks::AccountLocks,
    account_templates::Templates,
    actions::Actions,
    bark::Bark,
    compat::{datetime, integer, operation_versions, parse_datetime},
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
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(930_000);

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

#[test]
fn pure_quota_projection_and_reason_tables_match_fixed_python_output() {
    let reference = fixture();
    assert_eq!(reference["reference"]["sha"], REFERENCE_SHA);

    let quota_case = &reference["quota_result"]["input"];
    let now = parse_datetime(&quota_case["now"]).unwrap();
    let requested_at = parse_datetime(&quota_case["requested_at"]).unwrap();
    let actual = credit_rules::quota_result(
        &quota_case["data"],
        &quota_case["row"],
        now,
        Some(requested_at),
    );
    assert_eq!(actual, reference["quota_result"]["expected"]);

    let project_case = &reference["project"]["input"];
    assert_eq!(
        credit_rules::project(&project_case["task"], &project_case["notices"]),
        reference["project"]["expected"]
    );
    for (code, expected) in reference["reason_codes"]["expected"].as_object().unwrap() {
        assert_eq!(
            credit_rules::reason(code),
            expected.as_str().unwrap(),
            "reason {code}"
        );
    }
    for (code, expected) in reference["evidence_errors"]["expected"]
        .as_object()
        .unwrap()
    {
        assert_eq!(
            credit_rules::evidence_error(code),
            expected.as_str().unwrap(),
            "evidence error {code}"
        );
    }
    for (outcome, expected) in reference["labels"]["expected"].as_object().unwrap() {
        assert_eq!(
            credit_rules::label(outcome),
            expected.as_str().unwrap(),
            "label {outcome}"
        );
    }
    assert_eq!(
        credit_rules::reason("fixture_unknown_reason"),
        reference["fallbacks"]["reason"]
    );
    assert_eq!(
        credit_rules::evidence_error("fixture_unknown_evidence"),
        reference["fallbacks"]["evidence_error"]
    );
    assert_eq!(
        credit_rules::label("fixture_unknown_outcome"),
        reference["fallbacks"]["label"]
    );
}

#[derive(Clone)]
struct RecordedCall {
    method: Method,
    path: String,
}

#[derive(Clone)]
struct JsonScenario {
    status: u16,
    body: Value,
}

#[derive(Clone, Copy, Default)]
enum ScheduleBehavior {
    #[default]
    Success,
    ApplyThen500,
}

#[derive(Clone)]
struct MockUpstream {
    pool: PgPool,
    calls: Arc<StdMutex<Vec<RecordedCall>>>,
    resets: Arc<StdMutex<BTreeMap<i64, JsonScenario>>>,
    schedules: Arc<StdMutex<BTreeMap<i64, ScheduleBehavior>>>,
    sse: Arc<StdMutex<BTreeMap<i64, String>>>,
    usage_gate: Arc<StdMutex<Option<Arc<Notify>>>>,
    bark_codes: Arc<StdMutex<VecDeque<i64>>>,
    clock: Arc<StdMutex<DateTime<Utc>>>,
    scalar_status: Arc<AtomicI64>,
    scalar_gate: Arc<StdMutex<Option<Arc<Notify>>>>,
    scalar_readonly_parent: Arc<StdMutex<Option<PathBuf>>>,
}

impl MockUpstream {
    fn new(pool: PgPool, clock: Arc<StdMutex<DateTime<Utc>>>) -> Self {
        Self {
            pool,
            calls: Arc::new(StdMutex::new(Vec::new())),
            resets: Arc::new(StdMutex::new(BTreeMap::new())),
            schedules: Arc::new(StdMutex::new(BTreeMap::new())),
            sse: Arc::new(StdMutex::new(BTreeMap::new())),
            usage_gate: Arc::new(StdMutex::new(None)),
            bark_codes: Arc::new(StdMutex::new(VecDeque::new())),
            clock,
            scalar_status: Arc::new(AtomicI64::new(200)),
            scalar_gate: Arc::new(StdMutex::new(None)),
            scalar_readonly_parent: Arc::new(StdMutex::new(None)),
        }
    }

    fn record(&self, method: Method, path: String) {
        self.calls
            .lock()
            .unwrap()
            .push(RecordedCall { method, path });
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

    fn set_schedule_behavior(&self, id: i64, behavior: ScheduleBehavior) {
        self.schedules.lock().unwrap().insert(id, behavior);
    }

    fn set_sse(&self, id: i64, payload: impl Into<String>) {
        self.sse.lock().unwrap().insert(id, payload.into());
    }

    fn set_usage_gate(&self, gate: Arc<Notify>) {
        *self.usage_gate.lock().unwrap() = Some(gate);
    }

    fn release_usage(&self) {
        if let Some(gate) = self.usage_gate.lock().unwrap().clone() {
            gate.notify_one();
        }
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
    state.record(method.clone(), path.clone());

    if method == Method::PUT {
        let Some(id) = path_id(&path, "/api/v1/admin/accounts/", "") else {
            return json_response(404, json!({"code": 1}));
        };
        let status = state.scalar_status.load(Ordering::SeqCst) as u16;
        if status != 200 {
            return json_response(status, json!({"code": 1, "message": "fixture rejection"}));
        }
        let fields = payload.as_object().expect("one scalar update");
        assert_eq!(fields.len(), 1);
        let (field, value) = fields.iter().next().unwrap();
        assert!(["priority", "concurrency", "load_factor"].contains(&field.as_str()));
        sqlx::query(&format!(
            "UPDATE accounts SET {field}=$1,updated_at=$2 WHERE id=$3"
        ))
        .bind(integer(value).map(|v| v as i32))
        .bind(state.now())
        .bind(id)
        .execute(&state.pool)
        .await
        .expect("apply scalar setting in fixture database");
        let parent = { state.scalar_readonly_parent.lock().unwrap().clone() };
        if let Some(parent) = parent {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o500)).unwrap();
        }
        let gate = { state.scalar_gate.lock().unwrap().clone() };
        if let Some(gate) = gate {
            gate.notified().await;
        }
        return json_response(200, json!({"code":0}));
    }

    if method == Method::GET && path.ends_with("/usage") {
        let gate = { state.usage_gate.lock().unwrap().clone() };
        if let Some(gate) = gate {
            gate.notified().await;
        }
        let at = state.now();
        return json_response(200, json!({"code": 0, "data": usage_data(at, 20.0)}));
    }

    if method == Method::POST && path.ends_with("/schedulable") {
        let Some(id) = path_id(&path, "/api/v1/admin/accounts/", "/schedulable") else {
            return json_response(404, json!({"code": 1}));
        };
        let enabled = payload["schedulable"].as_bool().unwrap_or(false);
        let behavior = state
            .schedules
            .lock()
            .unwrap()
            .get(&id)
            .copied()
            .unwrap_or_default();
        let at = state.now();
        sqlx::query("UPDATE accounts SET schedulable=$1,updated_at=$2 WHERE id=$3")
            .bind(enabled)
            .bind(at)
            .bind(id)
            .execute(&state.pool)
            .await
            .expect("apply synthetic schedulable write to fixture PostgreSQL");
        if matches!(behavior, ScheduleBehavior::ApplyThen500) {
            return json_response(500, json!({"code": 1, "message": "fixture-unknown-write"}));
        }
        return json_response(
            200,
            json!({"code": 0, "data": {"id": id, "schedulable": enabled}}),
        );
    }

    if method == Method::POST && path.ends_with("/reset-quota") {
        let Some(id) = path_id(&path, "/api/v1/admin/openai/accounts/", "/reset-quota") else {
            return json_response(404, json!({"code": 1}));
        };
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
        return json_response(scenario.status, scenario.body);
    }

    if method == Method::POST && path.ends_with("/test") {
        let Some(id) = path_id(&path, "/api/v1/admin/accounts/", "/test") else {
            return json_response(404, json!({"code": 1}));
        };
        let sse = state
            .sse
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or_else(|| "data: {\"type\":\"test_complete\",\"success\":true}\n\n".into());
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(sse))
            .expect("fixture SSE response");
    }

    if method == Method::POST && path == "/push" {
        let code = state.bark_codes.lock().unwrap().pop_front().unwrap_or(200);
        return json_response(200, json!({"code": code}));
    }

    json_response(404, json!({"code": 1, "message": "fixture-not-found"}))
}

struct HookShared {
    config: StdMutex<Value>,
    bump_generation: AtomicBool,
    model_calls: AtomicUsize,
    connection_calls: AtomicUsize,
    recover_readback: AtomicBool,
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
        Ok(false)
    }

    fn detection_version(&self, _id: i64) -> Result<Value> {
        Ok(json!("1".repeat(64)))
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
        Box::pin(async move {
            shared.connection_calls.fetch_add(1, Ordering::SeqCst);
            Ok(oauth_native::test(&actions, id, &model, ADMIN_KEY, seconds).await)
        })
    }

    fn recover(&self, id: i64, _key: String) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            if self.shared.recover_readback.load(Ordering::SeqCst) {
                let at = *self.clock.lock().unwrap();
                sqlx::query("UPDATE accounts SET rate_limited_at=NULL,rate_limit_reset_at=NULL,updated_at=$1 WHERE id=$2")
                    .bind(at).bind(id).execute(&self.actions.queries.db.pool).await.unwrap();
            }
            Ok(json!({"success": true, "receipt": "fixture-recovery"}))
        })
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
            model_calls: AtomicUsize::new(0),
            connection_calls: AtomicUsize::new(0),
            recover_readback: AtomicBool::new(false),
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
        let coordinator = Coordinator::new(actions.clone(), Arc::new(|| 4));
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

#[tokio::test]
async fn concurrent_usage_queries_share_one_dispatch_and_persist_the_budget() {
    let harness = Harness::new(false).await;
    let row = harness.seed_account(true, false).await;
    let id = integer(&row["id"]).unwrap();
    let gate = Arc::new(Notify::new());
    harness.mock.set_usage_gate(gate);
    let options = QueryOptions {
        source: Source::Automatic,
        reason: "fixture-concurrent-query".into(),
        at: fixed_now(),
        timeout: 3,
        ..QueryOptions::default()
    };

    let first_controller = harness.controller.clone();
    let first_options = options.clone();
    let first = tokio::spawn(async move {
        first_controller
            .queries
            .query(id, ADMIN_KEY, first_options)
            .await
    });
    harness.mock.wait_for(Method::GET, "/usage", 1).await;

    let second_controller = harness.controller.clone();
    let second = tokio::spawn(async move {
        second_controller
            .queries
            .query(id, ADMIN_KEY, options)
            .await
    });
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    harness.mock.release_usage();
    let first_result = first.await.expect("first query task");
    let second_result = second.await.expect("coalesced query task");

    assert_eq!(harness.mock.count(Method::GET, "/usage"), 1);
    assert!(first_result["success"] == true, "{first_result}");
    assert!(second_result["coalesced"] == true, "{second_result}");
    let saved = harness.controller.store.snapshot().unwrap();
    assert_eq!(
        saved["scheduler"][id.to_string()]["quota_query"]["automatic_attempts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        saved["scheduler"][id.to_string()]["quota_query"]["last_reason"],
        "fixture-concurrent-query"
    );
}

#[tokio::test]
async fn a_quota_budget_lock_save_failure_never_dispatches_upstream() {
    let harness = Harness::new(false).await;
    let row = harness.seed_account(true, false).await;
    let id = integer(&row["id"]).unwrap();
    let state_path = harness.settings.oauth_path.clone();
    harness
        .controller
        .store
        .transaction(|_| Ok(()))
        .expect("write initial durable state");
    let lock_path = PathBuf::from(format!("{}.lock", state_path.display()));
    fs::remove_file(&lock_path).expect("remove fixture state lock");
    fs::create_dir(&lock_path).expect("replace lock file with an unwritable lock target");

    let result = harness
        .controller
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Automatic,
                reason: "fixture-save-failure".into(),
                at: fixed_now(),
                timeout: 2,
                ..QueryOptions::default()
            },
        )
        .await;

    assert_eq!(result["error_code"], "query_state_unavailable");
    assert_eq!(harness.mock.count(Method::GET, "/usage"), 0);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
}

#[tokio::test]
async fn consume_receipts_survive_restart_and_never_replay() {
    let harness = Harness::new(false).await;
    let cases = [
        (
            200,
            json!({
                "code": 0,
                "data": {"code": "success", "windows_reset": 1, "credit": {"status": "redeemed"}}
            }),
            true,
            "consumed",
        ),
        (
            200,
            json!({
                "code": 0,
                "data": {"code": "no_credit", "windows_reset": 0, "credit": {"status": "available"}}
            }),
            false,
            "not_consumed",
        ),
        (
            500,
            json!({"code": 1, "message": "fixture-native-500"}),
            false,
            "unknown",
        ),
        (
            200,
            json!({
                "code": "0",
                "data": {"code": "success", "windows_reset": 1, "credit": {"status": "redeemed"}}
            }),
            false,
            "unknown",
        ),
    ];

    let mut controller = harness.controller.clone();
    let mut expected_reset_calls = 0;
    for (status, body, consumed, outcome) in cases {
        let row = harness.seed_account(true, false).await;
        let id = integer(&row["id"]).unwrap();
        harness.mock.set_reset(id, status, body);
        let first = controller
            .manual(&row, "reset", ADMIN_KEY)
            .await
            .expect("send one fixture consume request");
        expected_reset_calls += 1;
        assert_eq!(
            harness.mock.count(Method::POST, "/reset-quota"),
            expected_reset_calls
        );
        let persisted = controller.state(id).expect("read persisted receipt");
        assert_eq!(persisted["consumed"], consumed);
        assert_eq!(persisted["consumption_outcome"], outcome);
        assert!(persisted["attempt_at"].is_string());
        assert!(persisted["receipt"].is_object());
        if status == 500 {
            assert_eq!(first["error_code"], "http_500");
        }
        if outcome == "unknown" && status == 200 {
            assert_eq!(first["error_code"], "result_uncertain");
        }

        controller = harness.restart_controller();
        let reopened = controller
            .state(id)
            .expect("reopen durable state in a fresh Controller");
        assert_eq!(reopened["consumed"], consumed);
        assert_eq!(reopened["consumption_outcome"], outcome);
        let retry = controller
            .manual(&row, "reset", ADMIN_KEY)
            .await
            .expect("replayed operation is converted to read-only uncertainty");
        assert_eq!(retry["error_code"], "result_uncertain");
        assert_eq!(
            harness.mock.count(Method::POST, "/reset-quota"),
            expected_reset_calls,
            "a persisted attempt must not reach native reset again"
        );
    }
}

#[tokio::test]
async fn automatic_pause_is_read_back_owned_before_consumption_result() {
    let harness = Harness::new(false).await;
    let row_id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(row_id, "connection"));
    let (row, evidence) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    assert_eq!(id, row_id);
    harness
        .mock
        .set_schedule_behavior(id, ScheduleBehavior::Success);
    harness.mock.set_reset(
        id,
        200,
        json!({
            "code": 0,
            "data": {"code": "no_credit", "windows_reset": 0, "credit": {"status": "available"}}
        }),
    );
    let old = harness.controller.state(id).unwrap();
    let task = harness
        .controller
        .begin_episode(id, &old, &evidence)
        .await
        .expect("admit synthetic credit episode");
    assert!(task["episode"].is_string());
    let task = harness
        .controller
        .policy(id, &task, fixed_now())
        .await
        .expect("persist fixture policy before consuming");

    let result = harness
        .controller
        .request(
            &row,
            "reset",
            &task,
            fixed_now(),
            Source::Automatic,
            Some(ADMIN_KEY),
        )
        .await
        .expect("run one automatic pause and consume attempt");
    let current = harness.controller.state(id).unwrap();
    let read_back = credit_ownership::account(&harness.db, id).await.unwrap();
    assert_eq!(
        result["consumption_outcome"], "not_consumed",
        "request result: {result}; state: {current}"
    );
    assert_eq!(current["owns_pause"], true);
    assert_eq!(current["consumption_outcome"], "not_consumed");
    assert_eq!(read_back["schedulable"], false);
    assert_eq!(
        current["owned_version"], read_back["updated_at"],
        "pause ownership is anchored to the server readback version"
    );
    assert!(harness
        .controller
        .owned(&read_back, &current, true)
        .expect("check pause ownership"));
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
}

#[tokio::test]
async fn five_minute_deadlines_and_unknown_unissued_pause_undo_are_read_back_only() {
    let harness = Harness::new(false).await;
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let row = harness.seed_account(false, true).await;
    let id = integer(&row["id"]).unwrap();
    let now = fixed_now();
    let task = json!({
        "episode": "fixture-deadline-episode",
        "stage": "waiting",
        "revision": 0,
        "evidence_at": datetime(now),
        "reset_completed_at": datetime(now + Duration::seconds(10)),
        "owns_pause": false
    });
    harness
        .controller
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]["auto_reset_credit"] = task.clone();
            Ok(())
        })
        .expect("persist deadline fixture");
    let policy = harness
        .controller
        .policy(id, &task, now)
        .await
        .expect("derive bounded credit policy");
    assert_eq!(
        policy["consume_deadline_at"],
        datetime(now + Duration::seconds(300))
    );
    assert_eq!(
        policy["verification_deadline_at"],
        datetime(now + Duration::seconds(310))
    );
    assert!(!credit_rules::expired(
        &policy,
        now + Duration::seconds(299)
    ));
    assert!(credit_rules::expired(&policy, now + Duration::seconds(300)));
    let consumed = json!({
        "consumed": true,
        "verification_deadline_at": datetime(now + Duration::seconds(310))
    });
    assert!(!credit_rules::expired(
        &consumed,
        now + Duration::seconds(309)
    ));
    assert!(credit_rules::expired(
        &consumed,
        now + Duration::seconds(310)
    ));

    sqlx::query("UPDATE accounts SET schedulable=false,updated_at=$1 WHERE id=$2")
        .bind(now)
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .expect("set owned pause fixture");
    let paused_row = credit_ownership::account(&harness.db, id).await.unwrap();
    let pause_task = json!({
        "episode": "fixture-unissued-pause",
        "stage": "finished",
        "revision": 0,
        "owns_pause": true,
        "signature": credit_ownership::signature(&paused_row),
        "owned_version": paused_row["updated_at"],
        "control_generation": 0,
        "policy_credential_fingerprint": oauth_admission::credential_fingerprint(&paused_row)
    });
    harness
        .controller
        .store
        .transaction(|state| {
            state["scheduler"][id.to_string()]["auto_reset_credit"] = pause_task.clone();
            Ok(())
        })
        .expect("persist own unissued pause");
    harness
        .mock
        .set_schedule_behavior(id, ScheduleBehavior::ApplyThen500);
    harness
        .controller
        .undo_unissued_pause(id, &paused_row, &pause_task, now)
        .await
        .expect("read back an unknown pause undo result");

    let final_row = credit_ownership::account(&harness.db, id).await.unwrap();
    let final_task = harness.controller.state(id).unwrap();
    assert_eq!(
        final_row["schedulable"],
        true,
        "mock write trace: {:?}",
        harness
            .mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| (call.method.to_string(), call.path.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(final_task["owns_pause"], false);
    assert!(final_task["pause_undo_requested_at"].is_string());
    assert!(final_task["pause_undo_confirmed_at"].is_string());
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
}

#[tokio::test]
async fn a_truncated_native_sse_receipt_finishes_without_a_second_test() {
    let harness = Harness::new(false).await;
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let row = harness.seed_account(false, false).await;
    let id = integer(&row["id"]).unwrap();
    harness.mock.set_sse(
        id,
        "data: {\"type\":\"test_start\",\"model\":\"fixture-model\"}\n\ndata: {\"type\":\"test_complete\",\"success\":",
    );
    let now = fixed_now();
    let state = harness.controller.store.snapshot().unwrap();
    let task = json!({
        "episode": "fixture-sse-episode",
        "stage": "testing",
        "revision": 0,
        "owns_pause": true,
        "signature": credit_ownership::signature(&row),
        "owned_version": row["updated_at"],
        "control_generation": credit_ownership::generation(&state, id),
        "consumed": true,
        "attempt_at": datetime(now - Duration::seconds(5)),
        "reset_completed_at": datetime(now - Duration::seconds(2)),
        "receipt": {"http_status": 200, "business_code": "success"}
    });
    harness
        .controller
        .store
        .transaction(|document| {
            document["scheduler"][id.to_string()]["auto_reset_credit"] = task.clone();
            Ok(())
        })
        .expect("persist consumed receipt before verification");

    harness
        .controller
        .test(&row, &task, now, true)
        .await
        .expect("dispatch one native connection verification");
    let after_test = harness.controller.state(id).unwrap();
    assert_eq!(after_test["stage"], "finished");
    assert_eq!(after_test["result_reason"], "verification_interrupted");
    assert_eq!(after_test["consumed"], true);
    assert_eq!(after_test["receipt"]["business_code"], "success");
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);

    harness
        .controller
        .run_account(&row, now)
        .await
        .expect("reconcile a lost verification receipt without replay");
    assert_eq!(harness.mock.count(Method::POST, "/test"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
}

#[tokio::test]
async fn manual_generation_change_keeps_consumption_receipt_and_never_unpauses() {
    let harness = Harness::new(false).await;
    let account_id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(account_id, "connection"));
    let (row, evidence) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    harness.set_config(recovery_config(id, "model"));
    harness.mock.set_reset(
        id,
        200,
        json!({
            "code": 0,
            "data": {"code": "success", "windows_reset": 1, "credit": {"status": "redeemed"}}
        }),
    );
    let old = harness.controller.state(id).unwrap();
    let task = harness
        .controller
        .begin_episode(id, &old, &evidence)
        .await
        .expect("admit fixture episode");
    let task = harness
        .controller
        .policy(id, &task, fixed_now())
        .await
        .expect("persist fixture policy before consuming");
    let consume_result = harness
        .controller
        .request(
            &row,
            "reset",
            &task,
            fixed_now(),
            Source::Automatic,
            Some(ADMIN_KEY),
        )
        .await
        .expect("record one native consumption receipt");
    let receipt_task = harness.controller.state(id).unwrap();
    assert_eq!(
        receipt_task["consumed"], true,
        "request result: {consume_result}; receipt state: {receipt_task}"
    );
    assert!(receipt_task["receipt"].is_object());
    harness.hooks.bump_generation.store(true, Ordering::SeqCst);
    let paused_row = credit_ownership::account(&harness.db, id).await.unwrap();
    harness
        .controller
        .test(&paused_row, &receipt_task, fixed_now(), false)
        .await
        .expect("dispatch mock model verification across manual generation update");

    let changed = harness.controller.store.snapshot().unwrap();
    let pending = harness.controller.state(id).unwrap();
    let after_row = credit_ownership::account(&harness.db, id).await.unwrap();
    assert_eq!(credit_ownership::generation(&changed, id), 1);
    assert_eq!(pending["consumed"], true);
    assert!(pending["receipt"].is_object());
    assert_eq!(pending["verification_dispatched"], true);
    assert_eq!(after_row["schedulable"], false);
    assert_eq!(harness.hooks.model_calls.load(Ordering::SeqCst), 1);

    harness
        .controller
        .run_account(&paused_row, fixed_now())
        .await
        .expect("stop recovery after manual control generation changes");
    let final_row = credit_ownership::account(&harness.db, id).await.unwrap();
    let final_task = harness.controller.state(id).unwrap();
    assert_eq!(final_row["schedulable"], false);
    assert_eq!(final_task["consumed"], true);
    assert!(final_task["receipt"].is_object());
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/test"), 0);
    assert_eq!(harness.hooks.model_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bark_delivery_is_once_only_and_failure_reservations_stop_after_five() {
    let harness = Harness::new(false).await;
    let delivered_id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
    harness
        .seed_notice(delivered_id, "fixture-bark-delivered", fixed_now())
        .await;
    let bark = Bark::new(harness.settings.clone()).expect("construct loopback Bark client");
    harness.controller.deliver_results(&bark).await.unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/push"), 1);
    let delivered = harness.controller.store.snapshot().unwrap();
    assert_eq!(
        delivered["scheduler"][delivered_id.to_string()]["credit_result_notifications"]
            ["fixture-bark-delivered"]["status"],
        "delivered"
    );
    harness.controller.deliver_results(&bark).await.unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/push"), 1);

    let failed_id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
    harness
        .seed_notice(failed_id, "fixture-bark-five", fixed_now())
        .await;
    harness.mock.set_bark_codes([500, 500, 500, 500, 500]);
    for offset in [0, 6, 37, 158, 759] {
        harness.set_now(fixed_now() + Duration::seconds(offset));
        harness.controller.deliver_results(&bark).await.unwrap();
    }
    let after_five = harness.controller.store.snapshot().unwrap();
    let notice = &after_five["scheduler"][failed_id.to_string()]["credit_result_notifications"]
        ["fixture-bark-five"];
    assert_eq!(notice["attempts"], 5);
    assert_eq!(notice["status"], "failed");
    assert!(notice["next_at"].is_null());
    assert_eq!(harness.mock.count(Method::POST, "/push"), 6);
    harness.set_now(fixed_now() + Duration::seconds(2_000));
    harness.controller.deliver_results(&bark).await.unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/push"), 6);
}

#[tokio::test]
async fn read_only_mode_creates_no_state_and_dispatches_no_query_card_or_bark() {
    let harness = Harness::new(true).await;
    let row = harness.seed_account(true, false).await;
    let id = integer(&row["id"]).unwrap();
    let bark = Bark::new(harness.settings.clone()).expect("construct read-only Bark client");

    let query = harness
        .controller
        .queries
        .query(
            id,
            ADMIN_KEY,
            QueryOptions {
                source: Source::Automatic,
                reason: "fixture-read-only".into(),
                at: fixed_now(),
                timeout: 2,
                ..QueryOptions::default()
            },
        )
        .await;
    assert_eq!(query["error_code"], "read_only");
    let consume = harness
        .controller
        .request(
            &row,
            "reset",
            &json!({"episode": "fixture-read-only", "stage": "waiting", "revision": 0}),
            fixed_now(),
            Source::Manual,
            Some(ADMIN_KEY),
        )
        .await
        .unwrap();
    assert_eq!(consume["error_code"], "read_only");
    assert!(harness
        .controller
        .manual(&row, "reset", ADMIN_KEY)
        .await
        .is_err());
    harness.controller.deliver_results(&bark).await.unwrap();

    let state_path = harness.settings.oauth_path.clone();
    let state_lock = PathBuf::from(format!("{}.lock", state_path.display()));
    assert!(!state_path.exists());
    assert!(!state_lock.exists());
    assert_eq!(harness.mock.total(), 0);
}

async fn seed_parameter_retry(h: &Harness) -> (i64, Value) {
    h.set_config(recovery_config(
        NEXT_ACCOUNT_ID.load(Ordering::Relaxed),
        "connection",
    ));
    let (row, _) = h.seed_depletion(&h.controller).await;
    let id = integer(&row["id"]).unwrap();
    h.set_config(recovery_config(id, "connection"));
    h.mock.set_sse(id, "data: {\"type\":\"error\",\"code\":\"http_502\",\"message\":\"fixture definitive failure\"}\n\n");
    h.controller
        .bounded_step(id, fixed_now(), true)
        .await
        .unwrap();
    let task = h.controller.state(id).unwrap();
    assert_eq!(task["stage"], "retry", "{task}");
    assert_eq!(task["owns_pause"], true);
    assert_eq!(task["consumed"], true);
    assert!(task["receipt"].is_object());
    assert_eq!(h.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(h.mock.count(Method::POST, "/test"), 1);
    assert_eq!(h.mock.count(Method::POST, "/schedulable"), 1);
    h.set_now(fixed_now() + Duration::seconds(1));
    (id, task)
}

async fn apply_parameter(h: &Harness, id: i64, field: &str, target: i64) -> Result<Value> {
    let row = h.controller.actions.account(id, None).await?;
    let versions = operation_versions(&row);
    h.controller
        .actions
        .apply_setting(
            id,
            field,
            &json!({field:target}),
            versions[field].as_str().unwrap(),
            ADMIN_KEY,
        )
        .await
}

async fn write_available_quota(h: &Harness, id: i64, at: DateTime<Utc>) {
    let row = h.controller.row(id).await.unwrap();
    let mut data = pinned_quota_data();
    data["fetched_at"] = json!(datetime(at));
    data["rate_limit"]["primary_window"]["used_percent"] = json!(20);
    data["rate_limit"]["secondary_window"]["used_percent"] = json!(20);
    let mut result = credit_rules::quota_result(&data, &row, at, Some(at));
    result["data"] = data;
    h.controller
        .store
        .transaction(|state| {
            state["oauth_results"][id.to_string()] = result;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn parameter_recovery_scalar_edits_keep_pause_until_one_retry_verifies() {
    for (field, target) in [("priority", 7), ("concurrency", 4), ("load_factor", 9)] {
        let h = Harness::new(false).await;
        let (id, _) = seed_parameter_retry(&h).await;
        assert_eq!(
            apply_parameter(&h, id, field, target).await.unwrap()["verified"],
            true
        );
        let held = h.controller.state(id).unwrap();
        let row = h.controller.row(id).await.unwrap();
        assert_eq!(held["owns_pause"], true);
        assert_eq!(held["parameter_edit"], Value::Null);
        assert_eq!(held["owned_version"], row["updated_at"]);
        assert_eq!(h.mock.count(Method::PUT, &format!("accounts/{id}")), 1);
        let later = fixed_now() + Duration::seconds(61);
        h.set_now(later);
        write_available_quota(&h, id, later).await;
        h.mock.set_sse(
            id,
            "data: {\"type\":\"test_complete\",\"success\":true}\n\n",
        );
        h.hooks.recover_readback.store(true, Ordering::SeqCst);
        h.controller.bounded_step(id, later, true).await.unwrap();
        let task = h.controller.state(id).unwrap();
        assert_eq!(task["stage"], "recovered", "field={field}; task={task}");
        assert_eq!(task["owns_pause"], false);
        assert_eq!(h.mock.count(Method::POST, "/reset-quota"), 1);
        assert_eq!(h.mock.count(Method::POST, "/test"), 2);
        assert_eq!(h.controller.row(id).await.unwrap()["schedulable"], true);
    }
}

#[tokio::test]
async fn parameter_recovery_intent_persistence_failure_stops_before_write() {
    let h = Harness::new(false).await;
    let (id, original) = seed_parameter_retry(&h).await;
    let bytes = fs::read(&h.settings.oauth_path).unwrap();
    fs::set_permissions(h._temp.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let result = apply_parameter(&h, id, "concurrency", 4).await;
    fs::set_permissions(h._temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(h.mock.count(Method::PUT, &format!("accounts/{id}")), 0);
    assert_eq!(fs::read(&h.settings.oauth_path).unwrap(), bytes);
    assert_eq!(h.controller.state(id).unwrap(), original);
}

async fn interrupted_parameter_write(h: &Harness, id: i64) -> Value {
    let gate = Arc::new(Notify::new());
    *h.mock.scalar_gate.lock().unwrap() = Some(gate.clone());
    let actions = h.controller.actions.clone();
    let row = actions.account(id, None).await.unwrap();
    let expected = operation_versions(&row)["concurrency"]
        .as_str()
        .unwrap()
        .to_owned();
    let worker = tokio::spawn(async move {
        actions
            .apply_setting(
                id,
                "concurrency",
                &json!({"concurrency":4}),
                &expected,
                ADMIN_KEY,
            )
            .await
    });
    h.mock
        .wait_for(Method::PUT, &format!("accounts/{id}"), 1)
        .await;
    tokio::time::timeout(StdDuration::from_secs(3), async {
        loop {
            if h.controller.row(id).await.unwrap()["concurrency"] == 4 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    gate.notify_one();
    *h.mock.scalar_gate.lock().unwrap() = None;
    let pending = h.controller.state(id).unwrap();
    assert_eq!(pending["parameter_edit"]["target"], 4);
    assert_ne!(
        pending["owned_version"],
        h.controller.row(id).await.unwrap()["updated_at"]
    );
    pending
}

async fn assert_reconciled_without_replay(h: &Harness, id: i64, old: &Value) {
    let restored = h.restart_controller();
    restored
        .bounded_step(id, h.controller.now(), true)
        .await
        .unwrap();
    let task = restored.state(id).unwrap();
    assert_eq!(task["owns_pause"], true);
    assert_eq!(task["parameter_edit"], Value::Null);
    assert_eq!(
        task["owned_version"],
        restored.row(id).await.unwrap()["updated_at"]
    );
    assert_ne!(task["owned_version"], old["owned_version"]);
    assert_eq!(h.mock.count(Method::PUT, &format!("accounts/{id}")), 1);
    assert_eq!(h.mock.count(Method::POST, "/reset-quota"), 1);
    assert_eq!(h.mock.count(Method::POST, "/test"), 1);
    assert_eq!(restored.row(id).await.unwrap()["schedulable"], false);
}

#[tokio::test]
async fn parameter_recovery_restart_reconciles_applied_value_after_interrupted_request() {
    let h = Harness::new(false).await;
    let (id, _) = seed_parameter_retry(&h).await;
    let pending = interrupted_parameter_write(&h, id).await;
    assert_eq!(pending["owns_pause"], true);
    assert_eq!(pending["consumed"], true);
    assert_reconciled_without_replay(&h, id, &pending).await;
}

#[tokio::test]
async fn parameter_recovery_postwrite_persistence_failure_reads_actual_value_after_restart() {
    let h = Harness::new(false).await;
    let (id, original) = seed_parameter_retry(&h).await;
    *h.mock.scalar_readonly_parent.lock().unwrap() = Some(h._temp.path().into());
    let result = apply_parameter(&h, id, "load_factor", 9).await;
    fs::set_permissions(h._temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(h.controller.row(id).await.unwrap()["load_factor"], 9);
    assert!(h.controller.state(id).unwrap()["parameter_edit"].is_object());
    assert_reconciled_without_replay(&h, id, &original).await;
}

#[tokio::test]
async fn parameter_recovery_rejected_write_clears_intent_and_keeps_owned_version() {
    for status in [400, 502] {
        let h = Harness::new(false).await;
        let (id, original) = seed_parameter_retry(&h).await;
        h.mock.scalar_status.store(status, Ordering::SeqCst);
        assert!(apply_parameter(&h, id, "concurrency", 4).await.is_err());
        assert!(h.controller.state(id).unwrap()["parameter_edit"].is_object());
        let restored = h.restart_controller();
        restored
            .bounded_step(id, h.controller.now(), true)
            .await
            .unwrap();
        let task = restored.state(id).unwrap();
        assert_eq!(task["owns_pause"], true);
        assert_eq!(task["parameter_edit"], Value::Null);
        assert_eq!(task["owned_version"], original["owned_version"]);
        assert_eq!(h.mock.count(Method::PUT, &format!("accounts/{id}")), 1);
        assert_eq!(h.mock.count(Method::POST, "/reset-quota"), 1);
        assert_eq!(h.mock.count(Method::POST, "/test"), 1);
    }
}

#[tokio::test]
async fn parameter_recovery_conflicting_manual_or_account_changes_never_resume_hold() {
    for change in ["generation", "credentials", "mapping", "proxy", "limit"] {
        let h = Harness::new(false).await;
        let (id, _) = seed_parameter_retry(&h).await;
        let pending = interrupted_parameter_write(&h, id).await;
        h.set_now(fixed_now() + Duration::seconds(2));
        if change == "generation" {
            let row = h.controller.row(id).await.unwrap();
            h.controller
                .store
                .manual_control(&row, true, h.controller.now(), "manual")
                .unwrap();
        } else {
            let clause = match change {
                "credentials" => "credentials=credentials || '{\"access_token\":\"fixture-replacement\"}'::jsonb",
                "mapping" => "credentials=credentials || '{\"model_mapping\":{\"fixture-next\":\"fixture-next\"}}'::jsonb",
                "proxy" => "proxy_id=99",
                "limit" => "rate_limited_at=$2,rate_limit_reset_at=$2 + interval '2 hours'",
                _ => unreachable!(),
            };
            sqlx::query(&format!(
                "UPDATE accounts SET {clause},updated_at=$2 WHERE id=$1"
            ))
            .bind(id)
            .bind(h.controller.now())
            .execute(&h.db.pool)
            .await
            .unwrap();
        }
        let restored = h.restart_controller();
        let row = restored.row(id).await.unwrap();
        restored
            .run(std::slice::from_ref(&row), h.controller.now())
            .await
            .unwrap();
        let task = restored.state(id).unwrap();
        assert_eq!(task["stage"], "finished", "change={change}; task={task}");
        assert_eq!(task["outcome"], "unrecovered");
        assert_eq!(task["consumed"], true);
        assert!(task["receipt"].is_object());
        assert_eq!(task["owns_pause"], false);
        assert_eq!(task["owned_version"], pending["owned_version"]);
        assert_ne!(task["owned_version"], row["updated_at"]);
        assert_eq!(restored.row(id).await.unwrap()["schedulable"], false);
        assert_eq!(
            restored.store.snapshot().unwrap()["recovery_history"],
            json!({})
        );
        assert_eq!(h.mock.count(Method::POST, "/reset-quota"), 1);
        assert_eq!(h.mock.count(Method::POST, "/test"), 1);
        assert_eq!(h.mock.count(Method::POST, "/schedulable"), 1);
    }
}
