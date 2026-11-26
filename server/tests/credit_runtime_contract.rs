#[path = "common/harness.rs"]
mod test_harness;
use test_harness::{
    json_status as json_response, operation_settings, path_id_between as path_id,
    seed_credit_account, spawn_loopback, sse_response, write_loopback_config,
};

use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::Method,
    response::Response,
    Router,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use sqlx::PgPool;
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
    credit::{self, CreditController, Hooks},
    db::Database,
    error::Result,
    oauth_admission, oauth_native,
    oauth_queries::{Coordinator, QueryOptions, Source},
    oauth_state::OAuthState,
    operations::TestSlots,
    queries::Queries,
    settings::Settings,
    upstream::Upstream,
};
use tokio::{
    sync::{Notify, Semaphore},
    task::JoinHandle,
};

const ADMIN_KEY: &str = "fixture-admin-credit-contract";
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

    let quota_case = &reference["quota_result"]["input"];
    let now = parse_datetime(&quota_case["now"]).unwrap();
    let requested_at = parse_datetime(&quota_case["requested_at"]).unwrap();
    let actual = credit::rules::quota_result(
        &quota_case["data"],
        &quota_case["row"],
        now,
        Some(requested_at),
    );
    let mut expected = reference["quota_result"]["expected"].clone();
    expected
        .as_object_mut()
        .unwrap()
        .shift_remove("template_type");
    assert_eq!(actual, expected);

    let project_case = &reference["project"]["input"];
    assert_eq!(
        credit::rules::project(&project_case["task"], &project_case["notices"]),
        reference["project"]["expected"]
    );
    for (code, expected) in reference["reason_codes"]["expected"].as_object().unwrap() {
        assert_eq!(
            credit::rules::reason(code),
            expected.as_str().unwrap(),
            "reason {code}"
        );
    }
    for (outcome, expected) in reference["labels"]["expected"].as_object().unwrap() {
        assert_eq!(
            credit::rules::label(outcome),
            expected.as_str().unwrap(),
            "label {outcome}"
        );
    }
    assert_eq!(
        credit::rules::reason("fixture_unknown_reason"),
        reference["fallbacks"]["reason"]
    );
    assert_eq!(
        credit::rules::label("fixture_unknown_outcome"),
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
        return sse_response(sse);
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
    async fn new() -> Self {
        let database_url = std::env::var("DATABASE_URL")
            .expect("contract runner must provide temporary PostgreSQL");
        let db = Database::open(&database_url)
            .await
            .expect("connect to runner-owned PostgreSQL");
        let temp = tempfile::tempdir().expect("create test state directory");
        let clock = Arc::new(StdMutex::new(fixed_now()));
        let mock = MockUpstream::new(db.pool.clone(), clock.clone());
        let (base_url, server) = spawn_loopback(
            Router::new()
                .fallback(mock_upstream)
                .with_state(mock.clone()),
        )
        .await;
        let state_path = temp.path().join("oauth-state.json");
        let connection_path = write_loopback_config(temp.path(), &base_url);
        let settings = Arc::new(Settings {
            oauth_path: state_path.clone(),
            bark_path: temp.path().join("bark-config.json"),
            connection_path,
            bark: json!({
                "enabled": true,
                "device_key": "fixture-bark-device-key",
                "server_url": base_url
            }),
            ..operation_settings(database_url, temp.path(), base_url)
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
        let oauth = OAuthState::new(settings.oauth_path.clone());
        oauth
            .save_admin_token(ADMIN_KEY)
            .expect("save synthetic fixture admin key");
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
            Templates::new(settings.data_dir().join("account-templates.json")),
            settings.clone(),
        )
        .expect("construct fixture actions");
        let coordinator = Coordinator::new(actions.clone(), Arc::new(|| Ok(4)));
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
            TestSlots::new(Arc::new(|| Ok(2))),
            Arc::new(Semaphore::new(2)),
        );
        let now = clock.clone();
        controller.clock = Arc::new(move || *now.lock().unwrap());
        controller
    }

    fn restart_controller(&self) -> CreditController {
        let oauth = OAuthState::new(self.settings.oauth_path.clone());
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
        seed_credit_account(&self.db, id, fixed_now(), schedulable, depleted).await
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
            credit::rules::quota_result(&data, &row, now, Some(now - Duration::seconds(2)));
        result["data"] = data;
        controller
            .store
            .transaction(|state| {
                state["oauth_results"][id.to_string()] = result.clone();
                Ok(())
            })
            .expect("persist synthetic quota result");

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
    let harness = Harness::new().await;
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
    let saved = harness.controller.store.shared_snapshot().unwrap();
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
    let harness = Harness::new().await;
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
    let harness = Harness::new().await;
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
    let harness = Harness::new().await;
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
        .begin_episode(&row, &old, &evidence, fixed_now())
        .expect("admit synthetic credit episode");
    assert!(task["episode"].is_string());

    let (result, _) = harness
        .controller
        .request(
            &row,
            "reset",
            &task,
            fixed_now(),
            Source::Automatic,
            Some(ADMIN_KEY),
            None,
        )
        .await
        .expect("run one automatic pause and consume attempt");
    let current = harness.controller.state(id).unwrap();
    let read_back = credit::account(&harness.db, id).await.unwrap();
    assert_eq!(
        result["consumption_outcome"], "not_consumed",
        "request result: {result}; state: {current}"
    );
    assert_eq!(current["owns_pause"], true);
    assert_eq!(current["consumption_outcome"], "not_consumed");
    assert_eq!(read_back["schedulable"], false);
    assert!(current.get("owned_version").is_none());
    assert!(harness
        .controller
        .owned(&read_back, &current)
        .expect("check pause ownership"));
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 1);
}

#[tokio::test]
async fn five_minute_deadlines_and_unknown_unissued_pause_undo_are_read_back_only() {
    let harness = Harness::new().await;
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let (row, evidence) = harness.seed_depletion(&harness.controller).await;
    let id = integer(&row["id"]).unwrap();
    let now = fixed_now();
    let policy = harness
        .controller
        .begin_episode(&row, &json!({}), &evidence, now)
        .unwrap();
    assert_eq!(policy["policy_version"], 1);
    assert_eq!(
        policy["consume_deadline_at"],
        datetime(
            sub2ops_server::compat::parse_datetime(&evidence["evidence_at"]).unwrap()
                + Duration::seconds(300)
        )
    );
    assert!(policy["verification_deadline_at"].is_null());
    assert!(!credit::rules::expired(
        &policy,
        now + Duration::seconds(239)
    ));
    assert!(credit::rules::expired(
        &policy,
        now + Duration::seconds(240)
    ));
    let consumed = json!({
        "consumed": true,
        "verification_deadline_at": datetime(now + Duration::seconds(310))
    });
    assert!(!credit::rules::expired(
        &consumed,
        now + Duration::seconds(309)
    ));
    assert!(credit::rules::expired(
        &consumed,
        now + Duration::seconds(310)
    ));

    sqlx::query("UPDATE accounts SET schedulable=false,updated_at=$1 WHERE id=$2")
        .bind(now)
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .expect("set owned pause fixture");
    let paused_row = credit::account(&harness.db, id).await.unwrap();
    let pause_task = json!({
        "episode": "fixture-unissued-pause",
        "stage": "finished",
        "revision": 0,
        "owns_pause": true,
        "signature": credit::signature(&paused_row),
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

    let final_row = credit::account(&harness.db, id).await.unwrap();
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
    assert!(final_task["pause_undo_requested_at"].is_null());
    assert!(final_task["pause_undo_confirmed_at"].is_null());
    assert_eq!(harness.mock.count(Method::POST, "/schedulable"), 1);
    assert_eq!(harness.mock.count(Method::POST, "/reset-quota"), 0);
}

#[tokio::test]
async fn a_truncated_native_sse_receipt_finishes_without_a_second_test() {
    let harness = Harness::new().await;
    let id = NEXT_ACCOUNT_ID.load(Ordering::Relaxed);
    harness.set_config(recovery_config(id, "connection"));
    let row = harness.seed_account(false, false).await;
    let id = integer(&row["id"]).unwrap();
    harness.mock.set_sse(
        id,
        "data: {\"type\":\"test_start\",\"model\":\"fixture-model\"}\n\ndata: {\"type\":\"test_complete\",\"success\":",
    );
    let now = fixed_now();
    let state = harness.controller.store.shared_snapshot().unwrap();
    let task = json!({
        "episode": "fixture-sse-episode",
        "stage": "testing",
        "revision": 0,
        "policy_version":1,"policy_control_generation":credit::generation(&state,id),
        "policy_method_generation":0,"policy_method":"connection","policy_detection_version":harness.controller.hooks.detection_version(id).unwrap(),
        "verification_deadline_at":datetime(now + Duration::seconds(298)),
        "owns_pause": true,
        "signature": credit::signature(&row),
        "owned_version": row["updated_at"],
        "control_generation": credit::generation(&state, id),
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
    let harness = Harness::new().await;
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
        .begin_episode(&row, &old, &evidence, fixed_now())
        .expect("admit fixture episode");
    let (consume_result, _) = harness
        .controller
        .request(
            &row,
            "reset",
            &task,
            fixed_now(),
            Source::Automatic,
            Some(ADMIN_KEY),
            None,
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
    let paused_row = credit::account(&harness.db, id).await.unwrap();
    harness
        .controller
        .test(&paused_row, &receipt_task, fixed_now(), false)
        .await
        .expect("dispatch mock model verification across manual generation update");

    let changed = harness.controller.store.shared_snapshot().unwrap();
    let pending = harness.controller.state(id).unwrap();
    let after_row = credit::account(&harness.db, id).await.unwrap();
    assert_eq!(credit::generation(&changed, id), 1);
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
    let final_row = credit::account(&harness.db, id).await.unwrap();
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
    let harness = Harness::new().await;
    let delivered_id = integer(&harness.seed_account(true, false).await["id"]).unwrap();
    harness
        .seed_notice(delivered_id, "fixture-bark-delivered", fixed_now())
        .await;
    let bark = Bark::new(harness.settings.clone()).expect("construct loopback Bark client");
    harness.controller.deliver_results(&bark).await.unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/push"), 1);
    let delivered = harness.controller.store.shared_snapshot().unwrap();
    assert_eq!(
        delivered["scheduler"][delivered_id.to_string()]["credit_result_notifications"]
            ["fixture-bark-delivered"]["status"],
        "delivered"
    );
    harness.controller.deliver_results(&bark).await.unwrap();
    assert_eq!(harness.mock.count(Method::POST, "/push"), 1);

    let failed_id = integer(&harness.seed_account(true, false).await["id"]).unwrap();
    harness
        .seed_notice(failed_id, "fixture-bark-five", fixed_now())
        .await;
    harness.mock.set_bark_codes([500, 500, 500, 500, 500]);
    for offset in [0, 6, 37, 158, 759] {
        harness.set_now(fixed_now() + Duration::seconds(offset));
        harness.controller.deliver_results(&bark).await.unwrap();
    }
    let after_five = harness.controller.store.shared_snapshot().unwrap();
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
    let mut result = credit::rules::quota_result(&data, &row, at, Some(at));
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
        let h = Harness::new().await;
        let (id, _) = seed_parameter_retry(&h).await;
        assert_eq!(
            apply_parameter(&h, id, field, target).await.unwrap()["verified"],
            true
        );
        let held = h.controller.state(id).unwrap();
        let row = h.controller.row(id).await.unwrap();
        assert_eq!(held["owns_pause"], true);
        assert_eq!(held["parameter_edit"], Value::Null);
        assert!(h.controller.owned(&row, &held).unwrap());
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
async fn parameter_recovery_receipt_failure_reports_applied_write() {
    let h = Harness::new().await;
    let (id, original) = seed_parameter_retry(&h).await;
    let bytes = fs::read(&h.settings.oauth_path).unwrap();
    fs::set_permissions(h._temp.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let result = apply_parameter(&h, id, "concurrency", 4).await;
    fs::set_permissions(h._temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(h.mock.count(Method::PUT, &format!("accounts/{id}")), 1);
    assert_eq!(h.controller.row(id).await.unwrap()["concurrency"], 4);
    assert_eq!(fs::read(&h.settings.oauth_path).unwrap(), bytes);
    assert_eq!(h.controller.state(id).unwrap(), original);
}

#[tokio::test]
async fn parameter_recovery_rejected_write_keeps_ownership() {
    for status in [400, 502] {
        let h = Harness::new().await;
        let (id, original) = seed_parameter_retry(&h).await;
        h.mock.scalar_status.store(status, Ordering::SeqCst);
        assert!(apply_parameter(&h, id, "concurrency", 4).await.is_err());
        assert_eq!(h.controller.state(id).unwrap(), original);
        let restored = h.restart_controller();
        restored
            .bounded_step(id, h.controller.now(), true)
            .await
            .unwrap();
        let task = restored.state(id).unwrap();
        assert_eq!(task["owns_pause"], true);
        assert_eq!(task["parameter_edit"], Value::Null);
        assert_eq!(task["signature"], original["signature"]);
        assert_eq!(h.mock.count(Method::PUT, &format!("accounts/{id}")), 1);
        assert_eq!(h.mock.count(Method::POST, "/reset-quota"), 1);
        assert_eq!(h.mock.count(Method::POST, "/test"), 1);
    }
}

#[tokio::test]
async fn parameter_recovery_conflicting_manual_or_account_changes_never_resume_hold() {
    for change in ["generation", "identity", "priority", "concurrency"] {
        let h = Harness::new().await;
        let (id, _) = seed_parameter_retry(&h).await;
        apply_parameter(&h, id, "concurrency", 4).await.unwrap();
        let pending = h.controller.state(id).unwrap();
        h.set_now(fixed_now() + Duration::seconds(2));
        if change == "generation" {
            let row = h.controller.row(id).await.unwrap();
            h.controller
                .store
                .manual_control(&row, true, h.controller.now(), "manual")
                .unwrap();
        } else {
            let clause = match change {
                "identity" => "credentials=credentials || '{\"chatgpt_account_id\":\"fixture-replacement\"}'::jsonb",
                "priority" => "priority=99",
                "concurrency" => "concurrency=99",
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
        h.set_now(fixed_now() + Duration::seconds(61));
        let restored = h.restart_controller();
        let row = restored.row(id).await.unwrap();
        restored
            .sweep(std::slice::from_ref(&row), h.controller.now())
            .await
            .unwrap();
        if restored
            .candidate_ready(&row, h.controller.now())
            .await
            .unwrap()
        {
            restored
                .run_account(&row, h.controller.now())
                .await
                .unwrap();
        }
        let task = restored.state(id).unwrap();
        assert_eq!(task["stage"], "finished", "change={change}; task={task}");
        assert_eq!(task["outcome"], "unrecovered");
        assert_eq!(task["consumed"], true);
        assert!(task["receipt"].is_object());
        assert_eq!(task["owns_pause"], false, "{change}");
        assert_eq!(task["signature"], pending["signature"]);
        assert!(!restored.owned(&row, &task).unwrap());
        assert_eq!(restored.row(id).await.unwrap()["schedulable"], false);
        assert_eq!(
            restored.store.shared_snapshot().unwrap()["recovery_history"],
            json!({})
        );
        assert_eq!(h.mock.count(Method::POST, "/reset-quota"), 1);
        assert_eq!(h.mock.count(Method::POST, "/test"), 1);
        assert_eq!(h.mock.count(Method::POST, "/schedulable"), 1);
    }
}
