use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json, Router,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use sqlx::types::Json as SqlJson;
use std::{
    collections::BTreeSet,
    future::pending,
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use sub2ops_server::{
    account_templates::Templates,
    actions::Actions,
    compat::operation_versions,
    db::Database,
    error::{Error, Result},
    model_tests::{ModelHooks, ModelTests},
    oauth_state::OAuthState,
    operations::{Backend, Operations},
    queries::Queries,
    runtime::Runtime,
    settings::Settings,
    store::{valid_object, Store},
    supervision::Supervisor,
};
use tokio::{
    net::TcpListener,
    sync::{Mutex as AsyncMutex, Notify, Semaphore},
    task::JoinHandle,
};

const PANIC_MARKER: &str = "fixture-panic-payload-must-not-appear";
const CHILD_DIR: &str = "SUB2OPS_SUPERVISION_CHILD_DIR";
const CHILD_MODE: &str = "SUB2OPS_SUPERVISION_CHILD_MODE";
static NEXT_ID: AtomicI64 = AtomicI64::new(1);
static UPSTREAM_CALLS: AtomicUsize = AtomicUsize::new(0);

async fn fixture_upstream(request: Request<Body>) -> Response {
    if request.uri().path() == "/api/v1/admin/groups/all" {
        UPSTREAM_CALLS.fetch_add(1, Ordering::AcqRel);
        return Json(json!({"code":0,"data":[]})).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

struct RuntimeFixture {
    runtime: Arc<Runtime>,
    upstream: JoinHandle<()>,
}

fn fixture_settings(
    directory: &Path,
    database_url: String,
    upstream_url: String,
    daily_enabled: bool,
) -> Settings {
    let connection_path = directory.join("sub2api-config.json");
    std::fs::write(
        &connection_path,
        serde_json::to_vec(&json!({"base_url":upstream_url,"verify_base_url":""})).unwrap(),
    )
    .unwrap();
    let settings = Settings {
        database_url,
        bind: "127.0.0.1:0".into(),
        base_path: "/".into(),
        audit_path: directory.join("audit.jsonl"),
        usage_path: directory.join("oauth-state.json"),
        oauth_path: directory.join("oauth-config.json"),
        bark_path: directory.join("bark-config.json"),
        fallback_path: directory.join("key-fallback.json"),
        connection_path,
        model_path: directory.join("group-models.json"),
        oauth: json!({
            "oauth_auto_reset_credit_enabled":false,
            "oauth_daily_test_enabled":daily_enabled,
            "oauth_daily_test_time":"05:00",
            "oauth_early_probe_batch_size":1,
            "oauth_recovery_connection_account_ids":[],
            "oauth_recovery_model_account_ids":[],
            "oauth_recovery_monitor_enabled":false,
            "oauth_recovery_test_concurrency":1,
            "oauth_recovery_test_model_id":"gpt-6-luna",
            "oauth_usage_refresh_concurrency":1
        }),
        bark: json!({"enabled":false}),
        bark_valid: true,
        base_url: upstream_url,
        verify_url: String::new(),
        read_only: false,
    };
    let (bank, mut version) = sub2ops_server::modeltrace::bundled().unwrap();
    let manifest = sub2ops_server::modeltrace::manifest();
    version["core_blob"] = manifest["files"]["core"]["gitBlob"].clone();
    version["challenge_blob"] = manifest["files"]["challenge"]["gitBlob"].clone();
    let raw = serde_json::to_string(&bank).unwrap();
    version["sha256"] = sub2ops_server::compat::sha256(&raw).into();
    let checked_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let state = json!({
        "version":1,
        "active":{"raw_bank":raw,"version":version},
        "source":"bundled",
        "checked_at":checked_at,
        "synced_at":checked_at,
        "cooldown_until":null,
        "rate_limit_until":null,
        "last_error":null,
        "result":"up-to-date"
    });
    Store::new(
        directory.join("modeltrace-bank-state.json"),
        json!({}),
        valid_object,
        false,
    )
    .transaction(|value| {
        *value = state;
        Ok(())
    })
    .unwrap();
    settings
}

async fn runtime_fixture(
    directory: &Path,
    clock: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
    daily_enabled: bool,
) -> RuntimeFixture {
    let database_url = std::env::var("DATABASE_URL").expect("isolated test DATABASE_URL");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_url = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(fixture_upstream);
    let upstream = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let settings = fixture_settings(directory, database_url, upstream_url, daily_enabled);
    let runtime = Runtime::new_with_clock(settings, clock).await.unwrap();
    RuntimeFixture { runtime, upstream }
}

fn fixture_id() -> i64 {
    let process = i64::from(std::process::id());
    1_000_000_000_000 + process * 100 + NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

async fn seed_account(runtime: &Runtime, count: usize) -> Vec<i64> {
    let database = runtime.db.pool.clone();
    let first = fixture_id();
    let group_id = first + 10_000_000;
    sqlx::query("INSERT INTO groups (id,name,platform,model_allowlist) VALUES ($1,$2,'openai',$3)")
        .bind(group_id)
        .bind(format!("fixture-supervision-group-{first}"))
        .bind(SqlJson(json!({"enabled":true,"models":["gpt-6-luna"]})))
        .execute(&database)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra) \
         SELECT $1::bigint+n,'fixture-supervision-account-'||($1::bigint+n)::text,\
                'openai','apikey','active',true,$2::jsonb,'{}'::jsonb \
         FROM generate_series(0,$3::integer-1) AS generated(n)",
    )
    .bind(first)
    .bind(SqlJson(json!({
        "api_key":"fixture-api-key",
        "base_url":"http://127.0.0.1:9/v1"
    })))
    .bind(count as i32)
    .execute(&database)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO account_groups (account_id,group_id) \
         SELECT id,$2 FROM accounts WHERE id >= $1 AND id < $1 + $3",
    )
    .bind(first)
    .bind(group_id)
    .bind(count as i64)
    .execute(&database)
    .await
    .unwrap();
    (0..count).map(|offset| first + offset as i64).collect()
}

async fn wait_for_http(url: &str) {
    let client = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if client
                .get(url)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("runtime health endpoint did not become ready");
}

#[derive(Default)]
struct PanicBackend {
    dispatches: AtomicUsize,
}

impl Backend for PanicBackend {
    fn managed(&self) -> Result<BTreeSet<i64>> {
        Ok(BTreeSet::new())
    }

    fn mark(&self, _id: i64) -> Result<Value> {
        Ok(json!({"marked":false,"version":"0".repeat(64)}))
    }

    fn dispatch<'a>(
        &'a self,
        _row: &'a Value,
        _job: &'a Value,
        _key: &'a str,
    ) -> BoxFuture<'a, Result<Value>> {
        self.dispatches.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { panic!("{PANIC_MARKER}") })
    }

    fn model_get(&self, _id: &str) -> Result<Value> {
        Err(Error::new(404, "fixture model absent"))
    }

    fn model_jobs(&self) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }

    fn model_cancel<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { Err(Error::new(404, "fixture model absent")) })
    }

    fn delete_lock(&self) -> Arc<AsyncMutex<()>> {
        Arc::new(AsyncMutex::new(()))
    }

    fn detach_deleted<'a>(&'a self, _jobs: &'a mut [Value]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn test_config(&self) -> Result<(String, usize)> {
        Ok(("gpt-6-luna".into(), 1))
    }
}

#[derive(Default)]
struct SafeBackend {
    dispatches: AtomicUsize,
}

impl Backend for SafeBackend {
    fn managed(&self) -> Result<BTreeSet<i64>> {
        Ok(BTreeSet::new())
    }

    fn mark(&self, _id: i64) -> Result<Value> {
        Ok(json!({"marked":false,"version":"0".repeat(64)}))
    }

    fn dispatch<'a>(
        &'a self,
        _row: &'a Value,
        _job: &'a Value,
        _key: &'a str,
    ) -> BoxFuture<'a, Result<Value>> {
        self.dispatches.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Ok(json!({"verified":true})) })
    }

    fn model_get(&self, _id: &str) -> Result<Value> {
        Err(Error::new(404, "fixture model absent"))
    }

    fn model_jobs(&self) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }

    fn model_cancel<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { Err(Error::new(404, "fixture model absent")) })
    }

    fn delete_lock(&self) -> Arc<AsyncMutex<()>> {
        Arc::new(AsyncMutex::new(()))
    }

    fn detach_deleted<'a>(&'a self, _jobs: &'a mut [Value]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn test_config(&self) -> Result<(String, usize)> {
        Ok(("gpt-6-luna".into(), 1))
    }
}

fn operation_actions(runtime: &Runtime, directory: &Path) -> Actions {
    let mut settings = (*runtime.settings).clone();
    settings.usage_path = directory.join("independent").join("oauth-state.json");
    settings.audit_path = directory.join("independent-audit.jsonl");
    settings.oauth_path = directory.join("independent-oauth.json");
    let settings = Arc::new(settings);
    let queries = Queries {
        db: runtime.db.clone(),
        oauth: OAuthState::new(settings.usage_path.clone(), false),
    };
    let mut actions = Actions::new(
        queries,
        runtime.upstream.clone(),
        runtime.actions.locks.clone(),
        Templates::new(directory.join("independent-templates.json"), false),
        settings,
    )
    .unwrap();
    actions.supervisor = runtime.actions.supervisor.clone();
    actions
}

fn submit_request(
    action: &str,
    request_id: &str,
    expected_version: &Value,
    payload: Value,
) -> Value {
    json!({
        "action":action,
        "request_id":request_id,
        "client_id":"fixture-supervision-client-0001",
        "expected_version":expected_version,
        "payload":payload
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operation_panic_closes_runtime_and_restart_checks_dispatched_work_once() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(chrono::Utc::now);
    let fixture = runtime_fixture(directory.path(), clock, false).await;
    let runtime = fixture.runtime.clone();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, wait_shutdown) = tokio::sync::oneshot::channel::<()>();
    let serving = {
        let runtime = runtime.clone();
        tokio::spawn(async move {
            runtime
                .serve(listener, async {
                    let _ = wait_shutdown.await;
                })
                .await
        })
    };
    wait_for_http(&format!("http://{address}/healthz")).await;

    let accounts = seed_account(&runtime, 2).await;
    let backend = Arc::new(PanicBackend::default());
    let budget = Arc::new(Semaphore::new(1));
    let queue = Operations::new_deferred(
        operation_actions(&runtime, directory.path()),
        backend.clone(),
        budget.clone(),
    )
    .unwrap();
    let mut ids = Vec::new();
    for (index, account_id) in accounts.iter().enumerate() {
        let row = runtime.actions.account(*account_id, None).await.unwrap();
        let request = submit_request(
            "usage",
            &format!("fixture-supervision-operation-{index:04}"),
            &operation_versions(&row, false)["usage"],
            json!({"action":"query_usage"}),
        );
        let accepted = queue
            .submit(*account_id, &request, "fixture-admin-key")
            .await
            .unwrap();
        ids.push(accepted["id"].as_str().unwrap().to_owned());
    }

    let held = budget.clone().try_acquire_owned().unwrap();
    queue.tick().await.unwrap();
    drop(held);
    queue.tick().await.unwrap();

    let failure_signal =
        tokio::time::timeout(Duration::from_secs(10), runtime.actions.supervisor.failed()).await;
    assert!(
        failure_signal.is_ok(),
        "operation did not fail: dispatches={}, upstream_calls={}, stopping={}, jobs={}",
        backend.dispatches.load(Ordering::Acquire),
        UPSTREAM_CALLS.load(Ordering::Acquire),
        runtime.actions.supervisor.is_stopping(),
        queue.store.read().unwrap()["jobs"]
    );
    let serve_result = tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .expect("Runtime::serve did not finish after a task failure")
        .unwrap();
    assert_eq!(serve_result.unwrap_err().status.as_u16(), 500);
    assert_eq!(backend.dispatches.load(Ordering::Acquire), 1);
    assert_eq!(
        budget.available_permits(),
        1,
        "panic releases the operation permit"
    );
    let data = queue.store.read().unwrap();
    let dispatched_id = ids
        .iter()
        .find(|id| data["jobs"][*id]["status"] == "running")
        .expect("one operation was marked dispatched before the backend panic")
        .clone();
    let queued_id = ids
        .iter()
        .find(|id| data["jobs"][*id]["status"] == "queued")
        .expect("the second operation remains undispatched")
        .clone();
    drop(data);
    tokio::time::timeout(Duration::from_secs(3), queue.close())
        .await
        .expect("closing a failed operation queue should not deadlock");

    let mut restarted_actions = queue.actions.clone();
    restarted_actions.supervisor = Supervisor::default();
    restarted_actions.queries.db = Database::open(&runtime.settings.database_url, false)
        .await
        .unwrap();
    restarted_actions
        .queries
        .oauth
        .save_admin_token("fixture-admin-key")
        .unwrap();
    let recovery_backend = Arc::new(SafeBackend::default());
    let restarted = Operations::new(
        restarted_actions,
        recovery_backend.clone(),
        Arc::new(Semaphore::new(1)),
    )
    .unwrap();
    assert_eq!(
        restarted.store.read().unwrap()["jobs"][&dispatched_id]["status"],
        "checking"
    );
    restarted.tick().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            restarted.tick().await.unwrap();
            let data = restarted.store.read().unwrap();
            if data["jobs"][&dispatched_id]["status"] == "needs_confirmation"
                && data["jobs"][&queued_id]["status"] == "completed"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("restart should reconcile dispatched work and retain queued work");
    assert_eq!(backend.dispatches.load(Ordering::Acquire), 1);
    assert_eq!(recovery_backend.dispatches.load(Ordering::Acquire), 1);
    restarted.close().await;
    restarted.actions.queries.db.close().await;
    fixture.upstream.abort();
    let _ = shutdown.send(());
}

struct PanicModelHooks {
    dispatched: Notify,
}

impl ModelHooks for PanicModelHooks {
    fn guard(&self, job: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move { Ok(job) })
    }

    fn next_allowed_at(&self, _id: i64) -> Result<Value> {
        Ok(Value::Null)
    }

    fn register_dispatch(&self, _job: &Value) -> Result<()> {
        self.dispatched.notify_one();
        panic!("{PANIC_MARKER}")
    }

    fn prepare_recovery(&self, _job: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async { Ok(Value::Null) })
    }

    fn verdict(&self, _job: Value, _report: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async { Ok(Value::Null) })
    }
}

async fn model_hook_panic_child(directory: &Path) -> ! {
    sub2ops_server::error::install_panic_redaction();
    let fixed = chrono::DateTime::parse_from_rfc3339("2099-03-01T20:59:30Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let fixture = runtime_fixture(directory, Arc::new(move || fixed), false).await;
    let runtime = fixture.runtime.clone();
    let account = seed_account(&runtime, 1).await[0];
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.serve(listener, pending::<()>()).await })
    };
    wait_for_http(&format!("http://{address}/healthz")).await;

    let hooks = Arc::new(PanicModelHooks {
        dispatched: Notify::new(),
    });
    let budget = Arc::new(Semaphore::new(1));
    let tests = ModelTests::new(
        runtime.actions.clone(),
        runtime.tests.bank.clone(),
        hooks.clone(),
        budget.clone(),
    );
    let row = runtime.actions.account(account, None).await.unwrap();
    let started = tests
        .start(
            account,
            &json!({
                "model_id":"gpt-6-luna",
                "expected_version":sub2ops_server::compat::account_version(&row),
                "request_id":"fixture-supervision-model-hook-panic-0001",
                "concurrency":1
            }),
            Some(&json!({
                "planned_groups":1,
                "triggers":["fixture"],
                "degradation_recheck":false
            })),
        )
        .await
        .unwrap();
    let job_id = started["id"].as_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(10), runtime.actions.supervisor.failed())
        .await
        .expect("model hook panic did not notify the service supervisor");
    let serve_result = tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .expect("Runtime::serve did not finish after a model hook panic")
        .unwrap();
    let serve_error = serve_result.unwrap_err();
    assert_eq!(serve_error.status.as_u16(), 500);
    assert!(runtime.actions.supervisor.has_failed());
    assert!(runtime.writable().is_err());
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if tests.get(&job_id).unwrap()["status"] == "interrupted" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("model worker should persist its uncertain interrupted state");
    assert_eq!(tests.get(&job_id).unwrap()["uncertain"], true);
    assert_eq!(
        budget.available_permits(),
        1,
        "panic releases the model budget"
    );
    let lease = runtime.actions.locks.try_ids([account]).unwrap();
    drop(lease);
    assert_eq!(
        tests
            .start(account, &json!({}), None)
            .await
            .unwrap_err()
            .status
            .as_u16(),
        503,
        "failed supervision rejects another model dispatch"
    );
    tokio::time::timeout(Duration::from_secs(3), tests.close())
        .await
        .expect("closing model tests after a hook panic should not deadlock");
    fixture.upstream.abort();
    eprintln!(
        "Sub2Ops startup or shutdown failed ({})",
        serve_error.status.as_u16()
    );
    std::process::exit(1)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_hook_panic_is_redacted_and_exits_through_runtime_serve() {
    if std::env::var_os(CHILD_MODE).is_some() {
        let directory = std::path::PathBuf::from(std::env::var_os(CHILD_DIR).unwrap());
        model_hook_panic_child(&directory).await;
    }

    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "model_hook_panic_is_redacted_and_exits_through_runtime_serve",
            "--nocapture",
        ])
        .env(CHILD_MODE, "1")
        .env(CHILD_DIR, directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "child exit: {output:?}");
    let combined = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(combined.contains("Sub2Ops internal task failed"));
    assert!(combined.contains("Sub2Ops startup or shutdown failed (500)"));
    assert!(!combined.contains(PANIC_MARKER));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daily_runtime_cycle_uses_the_injected_clock() {
    let directory = tempfile::tempdir().unwrap();
    let now = Arc::new(AtomicI64::new(
        chrono::DateTime::parse_from_rfc3339("2099-03-01T20:59:30Z")
            .unwrap()
            .timestamp(),
    ));
    let clock_now = now.clone();
    let clock = Arc::new(move || {
        chrono::DateTime::from_timestamp(clock_now.load(Ordering::Acquire), 0).unwrap()
    });
    let fixture = runtime_fixture(directory.path(), clock, true).await;
    let runtime = fixture.runtime.clone();
    runtime.start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    now.store(
        chrono::DateTime::parse_from_rfc3339("2099-03-01T21:00:10Z")
            .unwrap()
            .timestamp(),
        Ordering::Release,
    );
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let state = runtime.actions.queries.oauth.snapshot().unwrap();
            if state["daily_test"]["batches"]["2099-03-02"]["status"] == "queued" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("daily cycle did not schedule at the injected local time");
    assert_eq!(
        runtime.actions.queries.oauth.snapshot().unwrap()["daily_test"]["batches"]["2099-03-02"]
            ["scheduled_at"],
        "2099-03-01T21:00:00+00:00"
    );
    runtime.close().await;
    fixture.upstream.abort();
}

#[tokio::test]
async fn supervisor_preserves_business_errors_and_cancellation_but_publishes_panics() {
    let supervisor = Supervisor::default();
    let result = supervisor
        .spawn_result("business_error", async {
            Err::<(), _>(Error::new(422, "fixture business rejection"))
        })
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(result.status.as_u16(), 422);
    assert!(!supervisor.has_failed());
    assert!(!supervisor.is_stopping());

    let started = Arc::new(Notify::new());
    let notify = started.clone();
    let cancelled = supervisor.spawn("cancelled", async move {
        notify.notify_one();
        pending::<()>().await;
    });
    started.notified().await;
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    assert!(
        !supervisor.has_failed(),
        "normal task cancellation is not a panic"
    );

    let early_waiter = {
        let supervisor = supervisor.clone();
        tokio::spawn(async move { supervisor.failed().await })
    };
    tokio::task::yield_now().await;
    let guarded = supervisor
        .guard::<()>("panic", async { panic!("fixture supervision panic") })
        .await;
    assert!(guarded.is_none());
    tokio::time::timeout(Duration::from_secs(1), early_waiter)
        .await
        .expect("the existing failure subscriber should wake")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), supervisor.failed())
        .await
        .expect("subscribers created after a failure should observe it");
    assert!(supervisor.has_failed());
    assert!(supervisor.is_stopping());

    let tasks = sub2ops_server::tasks::Tasks::supervised(supervisor.clone());
    assert!(
        tasks.spawn(async {}).is_err(),
        "failed supervisor rejects dispatch"
    );
    tokio::time::timeout(Duration::from_secs(1), tasks.close(0))
        .await
        .expect("closing a failed supervised task set should complete");

    let stopped = Supervisor::default();
    stopped.stop();
    assert!(stopped.is_stopping());
    assert!(
        !stopped.has_failed(),
        "an ordinary stop is not a task failure"
    );
}
