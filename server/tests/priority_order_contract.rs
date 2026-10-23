use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, Connection, PgConnection, PgPool};
use std::{
    collections::BTreeMap,
    fs,
    sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use sub2ops_server::{
    api,
    compat::{now, operation_versions},
    runtime::Runtime,
    settings::Settings,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

const FIXTURES: &str = include_str!("../../tests/contracts/priority-order/fixtures.json");
const FIXTURES_V2: &str = include_str!("../../tests/contracts/priority-order/fixtures-v2.json");
const ADMIN_KEY: &str = "fixture-priority-order-admin";
const CLIENT_ID: &str = "fixture-priority-order-client";
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(950_000_000);

fn fixtures() -> Value {
    serde_json::from_str(FIXTURES).expect("priority-order fixture JSON must parse")
}

fn fixtures_v2() -> Value {
    serde_json::from_str(FIXTURES_V2).expect("priority-order v2 fixture JSON must parse")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteMode {
    Confirmed,
    UnknownApplied,
    UnknownNotApplied,
}

#[derive(Default)]
struct WriteGate {
    entered: Notify,
    release: Notify,
}

#[derive(Clone, Debug)]
struct Call {
    method: Method,
    path: String,
    body: Value,
}

#[derive(Clone)]
struct MockUpstream {
    pool: PgPool,
    calls: Arc<Mutex<Vec<Call>>>,
    modes: Arc<Mutex<BTreeMap<i64, WriteMode>>>,
    gates: Arc<Mutex<BTreeMap<i64, Arc<WriteGate>>>>,
    active_writes: Arc<AtomicUsize>,
    max_active_writes: Arc<AtomicUsize>,
}

impl MockUpstream {
    fn new(pool: PgPool) -> Self {
        Self {
            pool,
            calls: Arc::new(Mutex::new(Vec::new())),
            modes: Arc::new(Mutex::new(BTreeMap::new())),
            gates: Arc::new(Mutex::new(BTreeMap::new())),
            active_writes: Arc::new(AtomicUsize::new(0)),
            max_active_writes: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn count_put_for(&self, id: i64) -> usize {
        let suffix = format!("/accounts/{id}");
        self.calls()
            .iter()
            .filter(|call| call.method == Method::PUT && call.path.ends_with(&suffix))
            .count()
    }

    fn put_ids(&self) -> Vec<i64> {
        self.calls()
            .iter()
            .filter(|call| call.method == Method::PUT)
            .filter_map(|call| {
                call.path
                    .strip_prefix("/api/v1/admin/accounts/")
                    .and_then(|id| id.parse().ok())
            })
            .collect()
    }

    fn put_priorities(&self) -> Vec<i64> {
        self.calls()
            .iter()
            .filter(|call| call.method == Method::PUT)
            .filter_map(|call| call.body.get("priority").and_then(Value::as_i64))
            .collect()
    }

    fn set_mode(&self, id: i64, mode: WriteMode) {
        self.modes.lock().unwrap().insert(id, mode);
    }

    fn set_gate(&self, id: i64) -> Arc<WriteGate> {
        let gate = Arc::new(WriteGate::default());
        self.gates.lock().unwrap().insert(id, gate.clone());
        gate
    }

    fn max_active_writes(&self) -> usize {
        self.max_active_writes.load(Ordering::Acquire)
    }

    fn count_method(&self, method: Method) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.method == method)
            .count()
    }
}

fn response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

fn account_id(path: &str) -> Option<i64> {
    path.strip_prefix("/api/v1/admin/accounts/")?.parse().ok()
}

async fn mock_upstream(State(state): State<MockUpstream>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let method = parts.method;
    let path = parts.uri.path().to_owned();
    let bytes = to_bytes(body, 2_000_000).await.unwrap_or_default();
    let payload = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    state.calls.lock().unwrap().push(Call {
        method: method.clone(),
        path: path.clone(),
        body: payload.clone(),
    });

    if method == Method::GET && path == "/api/v1/admin/groups/all" {
        return response(StatusCode::OK, json!({"code":0,"data":[]}));
    }

    if method == Method::PUT {
        if let Some(id) = account_id(&path) {
            let mode = state
                .modes
                .lock()
                .unwrap()
                .get(&id)
                .copied()
                .unwrap_or(WriteMode::Confirmed);
            let gate = state.gates.lock().unwrap().get(&id).cloned();
            let active = state.active_writes.fetch_add(1, Ordering::AcqRel) + 1;
            state.max_active_writes.fetch_max(active, Ordering::AcqRel);
            if let Some(gate) = gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            let should_apply = mode != WriteMode::UnknownNotApplied;
            if should_apply {
                if let Some(priority) = payload.get("priority").and_then(Value::as_i64) {
                    sqlx::query("UPDATE accounts SET priority=$1,updated_at=now() WHERE id=$2")
                        .bind(priority as i32)
                        .bind(id)
                        .execute(&state.pool)
                        .await
                        .expect("fixture priority update");
                }
            }
            state.active_writes.fetch_sub(1, Ordering::AcqRel);
            return match mode {
                WriteMode::Confirmed => response(StatusCode::OK, json!({"code":0,"data":{}})),
                WriteMode::UnknownApplied | WriteMode::UnknownNotApplied => response(
                    StatusCode::GATEWAY_TIMEOUT,
                    json!({"code":1,"message":"fixture write result is unknown"}),
                ),
            };
        }
    }

    response(
        StatusCode::NOT_FOUND,
        json!({"code":404,"message":"fixture route not found"}),
    )
}

struct Harness {
    _temp: tempfile::TempDir,
    admin_connection: Option<PgConnection>,
    database_name: String,
    runtime: Arc<Runtime>,
    upstream: MockUpstream,
    client: reqwest::Client,
    base_url: String,
    app_task: Option<JoinHandle<()>>,
    upstream_task: Option<JoinHandle<()>>,
}

impl Harness {
    async fn new() -> Self {
        assert_eq!(fixtures()["schema_version"], 1);
        let runner_url =
            std::env::var("DATABASE_URL").expect("runner must provide temporary DATABASE_URL");
        let mut database_url =
            reqwest::Url::parse(&runner_url).expect("runner DATABASE_URL must be a PostgreSQL URL");
        assert_eq!(
            database_url.host_str(),
            Some("127.0.0.1"),
            "runner DATABASE_URL must point to loopback PostgreSQL"
        );
        let mut admin_connection = PgConnection::connect(&runner_url)
            .await
            .expect("connect to runner-owned PostgreSQL");
        let database_name = format!("priority_order_{}", uuid::Uuid::new_v4().simple());
        sqlx::raw_sql(&format!("CREATE DATABASE {database_name}"))
            .execute(&mut admin_connection)
            .await
            .expect("create isolated priority-order fixture database");
        database_url.set_path(&format!("/{database_name}"));
        let seed_url = database_url.to_string();
        let mut seed = PgConnection::connect(&seed_url)
            .await
            .expect("connect to isolated priority-order fixture database");
        sqlx::raw_sql(include_str!("../../tests/contracts/http/schema.sql"))
            .execute(&mut seed)
            .await
            .expect("initialize priority-order fixture schema");
        sqlx::raw_sql(
            "DELETE FROM account_groups WHERE account_id=4001;
             DELETE FROM usage_logs WHERE account_id=4001;
             DELETE FROM ops_error_logs WHERE account_id=4001;
             DELETE FROM accounts WHERE id=4001;",
        )
        .execute(&mut seed)
        .await
        .expect("remove unrelated baseline account from priority-order fixture");
        seed.close()
            .await
            .expect("close priority-order schema writer");
        database_url
            .query_pairs_mut()
            .append_pair("application_name", "priority_order_runtime");

        let temp = tempfile::tempdir().expect("create priority-order state directory");
        let (_, mut bank_version) = sub2ops_server::modeltrace::bundled().unwrap();
        let manifest = sub2ops_server::modeltrace::manifest();
        for name in ["core", "challenge"] {
            bank_version[format!("{name}_blob")] = manifest["files"][name]["gitBlob"].clone();
        }
        fs::write(
            temp.path().join("modeltrace-bank-state.json"),
            serde_json::to_vec(&json!({
                "version":1,
                "active":{"raw_bank":include_str!("../resources/modeltrace/unified_bank.json"),"version":bank_version},
                "source":"bundled","checked_at":chrono::Utc::now().timestamp(),
                "synced_at":0,"cooldown_until":null,"last_error":null,"result":null
            }))
            .unwrap(),
        )
        .expect("write modeltrace fixture state");

        let upstream_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind priority-order upstream fixture");
        let upstream_url = format!("http://{}", upstream_listener.local_addr().unwrap());
        let connection_path = temp.path().join("sub2api-config.json");
        fs::write(
            &connection_path,
            serde_json::to_vec(&json!({"base_url":upstream_url,"verify_base_url":""})).unwrap(),
        )
        .expect("write priority-order connection config");
        let settings = Settings {
            database_url: database_url.to_string(),
            bind: "127.0.0.1:0".into(),
            base_path: String::new(),
            audit_path: temp.path().join("audit.jsonl"),
            usage_path: temp.path().join("usage-state.json"),
            oauth_path: temp.path().join("oauth-state.json"),
            bark_path: temp.path().join("bark-state.json"),
            connection_path,
            model_path: temp.path().join("group-models.json"),
            keepalive_path: temp.path().join("keepalive-state.json"),
            oauth: json!({
                "oauth_recovery_monitor_enabled":false,
                "oauth_daily_test_enabled":false,
                "oauth_auto_reset_credit_enabled":false,
                "oauth_daily_test_time":"05:00",
                "oauth_usage_refresh_concurrency":2,
                "oauth_recovery_test_concurrency":2,
                "oauth_early_probe_batch_size":8,
                "oauth_recovery_test_model_id":"fixture-model",
                "oauth_recovery_connection_account_ids":[],
                "oauth_recovery_model_account_ids":[]
            }),
            bark: json!({"enabled":false}),
            bark_valid: true,
            base_url: upstream_url,
            verify_url: String::new(),
            read_only: false,
        };
        let database_pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(database_url.as_str())
            .await
            .expect("open fixture pool for upstream mock");
        let upstream = MockUpstream::new(database_pool);
        let upstream_state = upstream.clone();
        let upstream_task = tokio::spawn(async move {
            axum::serve(
                upstream_listener,
                Router::new()
                    .fallback(any(mock_upstream))
                    .with_state(upstream_state),
            )
            .await
            .expect("serve priority-order upstream fixture");
        });
        let runtime = Runtime::new(settings)
            .await
            .expect("construct priority-order runtime");
        let app_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind priority-order desktop API");
        let base_url = format!("http://{}", app_listener.local_addr().unwrap());
        let api_runtime = runtime.clone();
        let app_task = tokio::spawn(async move {
            axum::serve(app_listener, api::router(api_runtime))
                .await
                .expect("serve priority-order desktop API");
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("construct fixture HTTP client");
        Self {
            _temp: temp,
            admin_connection: Some(admin_connection),
            database_name,
            runtime,
            upstream,
            client,
            base_url,
            app_task: Some(app_task),
            upstream_task: Some(upstream_task),
        }
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value, String) {
        let mut request = self
            .client
            .request(method, format!("{}/api/desktop/v1/{path}", self.base_url))
            .header("x-api-key", ADMIN_KEY);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.expect("call local priority-order API");
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .expect("read priority-order response");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value, text)
    }

    async fn view(&self) -> Value {
        let (status, value, text) = self.request(Method::GET, "account-priorities", None).await;
        assert_eq!(status, 200, "priority view failed: {text}");
        value
    }

    async fn v2_view(&self) -> Value {
        let (status, value, text) = self
            .request(Method::GET, "account-priorities?schema=2", None)
            .await;
        assert_eq!(status, 200, "priority v2 view failed: {text}");
        value
    }

    async fn insert_account(&self, platform: &str, account_type: &str, priority: i64) -> i64 {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        let credentials = if account_type == "apikey" {
            json!({"api_key":"fixture-account-secret"})
        } else {
            json!({"access_token":"fixture-account-secret"})
        };
        sqlx::query(
            "INSERT INTO accounts(id,name,platform,type,status,schedulable,credentials,extra,priority,concurrency,load_factor) VALUES($1,$2,$3,$4,'active',true,$5,$6,$7,2,2)",
        )
        .bind(id)
        .bind(format!("fixture-priority-account-{id}"))
        .bind(platform)
        .bind(account_type)
        .bind(sqlx::types::Json(credentials))
        .bind(sqlx::types::Json(json!({"ownership":"fixture-owner-{id}"})))
        .bind(priority as i32)
        .execute(&self.runtime.db.pool)
        .await
        .expect("insert priority-order account");
        id
    }

    async fn set_parent(&self, child_id: i64, parent_id: i64) {
        sqlx::query("UPDATE accounts SET parent_account_id=$1,updated_at=now() WHERE id=$2")
            .bind(parent_id)
            .bind(child_id)
            .execute(&self.runtime.db.pool)
            .await
            .expect("set fixture parent account");
    }

    async fn delete_account(&self, id: i64) {
        sqlx::query("UPDATE accounts SET deleted_at=now() WHERE id=$1")
            .bind(id)
            .execute(&self.runtime.db.pool)
            .await
            .expect("mark fixture account deleted");
    }

    async fn priority(&self, id: i64) -> i64 {
        sqlx::query_scalar::<_, i32>("SELECT priority FROM accounts WHERE id=$1")
            .bind(id)
            .fetch_one(&self.runtime.db.pool)
            .await
            .expect("read fixture account priority") as i64
    }

    async fn submit(&self, ids: &[i64], version: &str, request_id: &str) -> (u16, Value, String) {
        let platform = sqlx::query_scalar::<_, String>(
            "SELECT platform FROM accounts WHERE id=$1 AND deleted_at IS NULL",
        )
        .bind(
            ids.first()
                .copied()
                .expect("priority submit has an account"),
        )
        .fetch_one(&self.runtime.db.pool)
        .await
        .expect("read priority submit platform");
        self.v2_submit(&platform, ids, version, request_id).await
    }

    async fn v2_submit(
        &self,
        platform: &str,
        ids: &[i64],
        version: &str,
        request_id: &str,
    ) -> (u16, Value, String) {
        self.request(
            Method::POST,
            "account-priority-orders?schema=2",
            Some(json!({
                "platform": platform,
                "ordered_account_ids": ids,
                "expected_version": version,
                "request_id": request_id,
                "client_id": CLIENT_ID
            })),
        )
        .await
    }

    async fn v2_detail(&self, id: &str, details: bool) -> (u16, Value, String) {
        let path = if details {
            format!("account-priority-orders/{id}?schema=2")
        } else {
            format!("account-priority-orders/{id}?schema=2&details=0")
        };
        self.request(Method::GET, &path, None).await
    }

    async fn wait_v2_task(&self, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let (status, value, text) = self.v2_detail(id, true).await;
                assert_eq!(status, 200, "priority v2 task read failed: {text}");
                if value["status"].as_str().is_some_and(|status| {
                    ["completed", "failed", "cancelled", "needs_confirmation"].contains(&status)
                }) {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("priority v2 task should reach a terminal state")
    }

    async fn wait_v2_waiting(&self, id: &str, account_id: i64) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let (status, value, text) = self.v2_detail(id, true).await;
                assert_eq!(status, 200, "priority v2 waiting task read failed: {text}");
                let waiting_ids = value["waiting_account_ids"].as_array();
                if waiting_ids
                    .is_some_and(|ids| ids.iter().any(|id| id.as_i64() == Some(account_id)))
                {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("priority v2 task should expose its waiting account")
    }

    async fn tick(&self) {
        self.runtime
            .priorities
            .tick()
            .await
            .expect("priority-order tick should start");
    }

    async fn wait_task(&self, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let (status, value, text) = self.v2_detail(id, true).await;
                assert_eq!(status, 200, "priority task read failed: {text}");
                if value["status"].as_str().is_some_and(|status| {
                    ["completed", "failed", "cancelled", "needs_confirmation"].contains(&status)
                }) {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("priority-order task should reach a terminal state")
    }

    async fn restart_runtime(&mut self) {
        if let Some(task) = self.app_task.take() {
            task.abort();
            let _ = task.await;
        }
        let mut settings = (*self.runtime.settings).clone();
        // Runtime owns the process disk lease until its Arc is dropped. Copy
        // the durable state into a fresh fixture directory before rebuilding,
        // which models a new process without reaching into private runtime
        // fields or changing production shutdown behavior.
        let restart_dir = self._temp.path().join(format!(
            "priority-restart-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&restart_dir).expect("create restart state directory");
        let old_usage = settings.usage_path.clone();
        let new_usage = restart_dir.join("usage-state.json");
        for (source, target) in [
            (old_usage.clone(), new_usage.clone()),
            (
                old_usage.with_file_name("account-priority-orders.json"),
                new_usage.with_file_name("account-priority-orders.json"),
            ),
            (
                old_usage.with_file_name("account-priority-orders-v2.json"),
                new_usage.with_file_name("account-priority-orders-v2.json"),
            ),
            (
                old_usage.with_file_name("account-operations.json"),
                new_usage.with_file_name("account-operations.json"),
            ),
        ] {
            if source.exists() {
                fs::copy(source, target).expect("copy durable restart fixture state");
            }
        }
        settings.usage_path = new_usage;
        self.runtime.close().await;
        let runtime = Runtime::new(settings)
            .await
            .expect("reconstruct priority-order runtime from persisted state");
        runtime
            .start()
            .await
            .expect("startup recovery should validate persisted priority state");
        let app_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind restarted priority-order desktop API");
        self.base_url = format!("http://{}", app_listener.local_addr().unwrap());
        let api_runtime = runtime.clone();
        self.app_task = Some(tokio::spawn(async move {
            axum::serve(app_listener, api::router(api_runtime))
                .await
                .expect("serve restarted priority-order desktop API");
        }));
        self.runtime = runtime;
    }

    async fn finish(&mut self) {
        if let Some(task) = self.app_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.runtime.close().await;
        if let Some(task) = self.upstream_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.upstream.pool.close().await;
        let admin = self
            .admin_connection
            .as_mut()
            .expect("priority-order admin connection is open");
        sqlx::raw_sql(&format!("DROP DATABASE {}", self.database_name))
            .execute(&mut *admin)
            .await
            .expect("drop isolated priority-order fixture database");
        self.admin_connection
            .take()
            .unwrap()
            .close()
            .await
            .expect("close priority-order admin connection");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(task) = self.app_task.take() {
            task.abort();
        }
        if let Some(task) = self.upstream_task.take() {
            task.abort();
        }
    }
}

fn ids_from_view(view: &Value) -> Vec<i64> {
    view["items"]
        .as_array()
        .expect("priority view items")
        .iter()
        .map(|item| item["id"].as_i64().expect("priority item id"))
        .collect()
}

fn item_for(view: &Value, id: i64) -> Value {
    view["items"]
        .as_array()
        .expect("priority view items")
        .iter()
        .find(|item| item["id"] == id)
        .cloned()
        .expect("priority item exists")
}

#[tokio::test]
async fn priority_view_is_cross_platform_versioned_and_excludes_deleted_accounts() {
    let mut harness = Harness::new().await;
    let openai = harness.insert_account("openai", "oauth", 8).await;
    let grok = harness.insert_account("grok", "apikey", 1).await;
    let anthropic = harness.insert_account("anthropic", "oauth", 4).await;
    let deleted = harness.insert_account("openai", "oauth", 2).await;
    harness.delete_account(deleted).await;

    let view = harness.view().await;
    assert_eq!(ids_from_view(&view), vec![openai, grok, anthropic]);
    assert!(view["version"].as_str().is_some_and(|v| v.len() == 64));
    assert!(view["active_task"].is_null());
    assert!(view["last_task"].is_null());
    for id in [openai, grok, anthropic] {
        let item = item_for(&view, id);
        for key in [
            "id",
            "name",
            "platform",
            "type",
            "priority",
            "version",
            "editable",
            "reason",
            "status",
            "schedulable",
        ] {
            assert!(item.get(key).is_some(), "priority item missing {key}");
        }
        assert!(item["version"].as_str().is_some_and(|v| v.len() == 64));
    }
    harness.finish().await;
}

#[tokio::test]
async fn priority_submit_is_idempotent_and_rejects_busy_or_conflicting_requests() {
    let mut harness = Harness::new().await;
    let a = harness.insert_account("openai", "oauth", 0).await;
    let b = harness.insert_account("openai", "apikey", 0).await;
    let c = harness.insert_account("openai", "oauth", 0).await;
    let _lease = harness.runtime.actions.locks.account(c).await;
    let view = harness.view().await;
    let version = view["version"].as_str().unwrap().to_owned();
    let ordered = vec![c, b, a];
    let (status, first, text) = harness
        .submit(&ordered, &version, "fixture-priority-submit-0001")
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let id = first["id"].as_str().unwrap().to_owned();
    assert_eq!(id.len(), 32);
    assert_eq!(first["status"], "queued");
    assert_eq!(first["total"], 3);
    assert_eq!(first["change_count"], 3);
    assert_eq!(first["verified_count"], 0);

    let (status, replay, text) = harness
        .submit(&ordered, &version, "fixture-priority-submit-0001")
        .await;
    assert_eq!(status, 200, "idempotent replay failed: {text}");
    assert_eq!(replay["id"], id);
    assert_eq!(replay["platform"], first["platform"]);
    assert_eq!(replay["total"], first["total"]);
    assert_eq!(replay["change_count"], first["change_count"]);
    assert_eq!(
        replay["items"][0]["account_id"],
        first["items"][0]["account_id"]
    );
    assert_eq!(
        replay["items"][0]["target_priority"],
        first["items"][0]["target_priority"]
    );

    let mut conflicting = ordered.clone();
    conflicting.swap(0, 2);
    let (status, _, text) = harness
        .submit(&conflicting, &version, "fixture-priority-submit-0001")
        .await;
    assert_eq!(
        status, 409,
        "request-id conflict should be rejected: {text}"
    );

    let (status, _, text) = harness
        .submit(&ordered, &version, "fixture-priority-submit-0002")
        .await;
    assert_eq!(
        status, 409,
        "second active order should be rejected: {text}"
    );
    let state = fs::read_to_string(harness.runtime.priorities.store.path())
        .expect("priority-order state file exists after submit");
    assert!(state.contains("fixture-priority-submit-0001"));
    assert!(!state.contains(ADMIN_KEY));
    harness.finish().await;
}

#[tokio::test]
async fn concurrent_clients_admit_one_order_and_keep_the_other_busy() {
    let mut harness = Harness::new().await;
    let a = harness.insert_account("openai", "oauth", 3).await;
    let b = harness.insert_account("openai", "apikey", 4).await;
    let platform = "openai";
    let v2_version = harness.v2_view().await["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["platform"] == platform)
        .unwrap()["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = |request_id: &str| {
        json!({
            "platform":platform,
            "ordered_account_ids":[b,a],
            "expected_version":v2_version,
            "request_id":request_id,
            "client_id":CLIENT_ID
        })
    };
    let first = harness.request(
        Method::POST,
        "account-priority-orders?schema=2",
        Some(body("fixture-priority-concurrent-a-0001")),
    );
    let second = harness.request(
        Method::POST,
        "account-priority-orders?schema=2",
        Some(body("fixture-priority-concurrent-b-0001")),
    );
    let (first, second) = tokio::join!(first, second);
    let statuses = [first.0, second.0];
    assert!(statuses.contains(&200));
    assert!(statuses.contains(&409));
    let accepted = if first.0 == 200 { first.1 } else { second.1 };
    let order_id = accepted["id"].as_str().unwrap().to_owned();
    harness.tick().await;
    assert_eq!(harness.wait_task(&order_id).await["status"], "completed");
    harness.finish().await;
}

#[tokio::test]
async fn priority_order_writes_changed_items_serially_and_finishes_after_full_readback() {
    let mut harness = Harness::new().await;
    let a = harness.insert_account("openai", "oauth", 7).await;
    let b = harness.insert_account("openai", "apikey", 4).await;
    let c = harness.insert_account("openai", "oauth", 2).await;
    let before = harness.view().await;
    let credentials_before =
        sqlx::query_scalar::<_, Value>("SELECT credentials FROM accounts WHERE id=$1")
            .bind(a)
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap();
    let extra_before = sqlx::query_scalar::<_, Value>("SELECT extra FROM accounts WHERE id=$1")
        .bind(a)
        .fetch_one(&harness.runtime.db.pool)
        .await
        .unwrap();
    let ordered = vec![c, b, a];
    let (status, accepted, text) = harness
        .submit(
            &ordered,
            before["version"].as_str().unwrap(),
            "fixture-priority-serial-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    harness.tick().await;
    let detail = harness.wait_task(accepted["id"].as_str().unwrap()).await;
    assert_eq!(detail["status"], "completed");
    assert_eq!(detail["total"], 3);
    assert_eq!(detail["verified_count"], 3);
    assert!(detail["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["status"] == "verified"));
    assert_eq!(harness.priority(a).await, 3);
    assert_eq!(harness.priority(b).await, 2);
    assert_eq!(harness.priority(c).await, 1);
    assert_eq!(harness.upstream.put_ids().len(), 3);
    assert_eq!(
        harness
            .upstream
            .put_ids()
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    assert_eq!(harness.upstream.put_priorities(), vec![1, 2, 3]);
    assert_eq!(harness.upstream.max_active_writes(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, Value>("SELECT credentials FROM accounts WHERE id=$1")
            .bind(a)
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap(),
        credentials_before
    );
    assert_eq!(
        sqlx::query_scalar::<_, Value>("SELECT extra FROM accounts WHERE id=$1")
            .bind(a)
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap(),
        extra_before
    );
    let after = harness.v2_view().await;
    assert!(after["active_task"].is_null());
    assert_eq!(
        after["platforms"]
            .as_array()
            .unwrap()
            .iter()
            .find(|platform| platform["platform"] == "openai")
            .unwrap()["last_task"]["status"],
        "completed"
    );
    harness.finish().await;
}

#[tokio::test]
async fn external_priority_change_stops_remaining_order_without_dispatch() {
    let mut harness = Harness::new().await;
    let a = harness.insert_account("openai", "oauth", 6).await;
    let b = harness.insert_account("openai", "apikey", 7).await;
    let lease = harness.runtime.actions.locks.account(b).await;
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[b, a],
            view["version"].as_str().unwrap(),
            "fixture-priority-external-change-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    sqlx::query("UPDATE accounts SET priority=55 WHERE id=$1")
        .bind(a)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    drop(lease);
    harness.tick().await;
    let detail = harness.wait_task(accepted["id"].as_str().unwrap()).await;
    assert_eq!(detail["status"], "needs_confirmation");
    assert_eq!(harness.upstream.put_ids().len(), 0);
    assert_eq!(harness.priority(a).await, 55);
    assert_eq!(harness.priority(b).await, 7);
    harness.finish().await;
}

#[tokio::test]
async fn platform_migration_stops_remaining_order_without_cross_platform_dispatch() {
    let mut harness = Harness::new().await;
    let first = harness.insert_account("openai", "oauth", 6).await;
    let second = harness.insert_account("openai", "apikey", 7).await;
    let lease = harness.runtime.actions.locks.account(first).await;
    let view = harness.v2_view().await;
    let openai_version = view["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|platform| platform["platform"] == "openai")
        .unwrap()["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, accepted, text) = harness
        .v2_submit(
            "openai",
            &[first, second],
            &openai_version,
            "fixture-priority-platform-migration-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let order_id = accepted["id"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE accounts SET platform='grok',updated_at=now() WHERE id=$1")
        .bind(second)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("migrate the remaining account to another platform");
    drop(lease);
    for _ in 0..100 {
        harness.tick().await;
        let (_, detail, _) = harness.v2_detail(&order_id, true).await;
        if matches!(
            detail["status"].as_str(),
            Some("needs_confirmation") | Some("failed")
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let detail = harness.wait_v2_task(&order_id).await;
    assert_eq!(detail["status"], "needs_confirmation");
    assert_eq!(harness.upstream.count_method(Method::PUT), 0);
    assert_eq!(harness.priority(first).await, 6);
    assert_eq!(harness.priority(second).await, 7);
    harness.finish().await;
}

#[tokio::test]
async fn account_addition_and_deletion_stop_orders_before_dispatch() {
    let mut harness = Harness::new().await;
    let a = harness.insert_account("openai", "oauth", 6).await;
    let b = harness.insert_account("openai", "apikey", 7).await;
    let lease = harness.runtime.actions.locks.account(b).await;
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[b, a],
            view["version"].as_str().unwrap(),
            "fixture-priority-account-add-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let first_id = accepted["id"].as_str().unwrap().to_owned();
    let added = harness.insert_account("openai", "oauth", 9).await;
    drop(lease);
    harness.tick().await;
    let first = harness.wait_task(&first_id).await;
    assert_eq!(first["status"], "needs_confirmation");
    assert_eq!(harness.upstream.put_ids().len(), 0);
    assert_eq!(harness.priority(added).await, 9);

    let view = harness.view().await;
    let ids = ids_from_view(&view);
    let (status, accepted, text) = harness
        .submit(
            &ids,
            view["version"].as_str().unwrap(),
            "fixture-priority-account-delete-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let second_id = accepted["id"].as_str().unwrap().to_owned();
    harness.delete_account(ids[0]).await;
    harness.tick().await;
    let second = harness.wait_task(&second_id).await;
    assert_eq!(second["status"], "needs_confirmation");
    assert_eq!(harness.upstream.put_ids().len(), 0);
    harness.finish().await;
}

#[tokio::test]
async fn cancel_before_dispatch_keeps_accounts_unchanged_and_client_can_close() {
    let mut harness = Harness::new().await;
    let a = harness.insert_account("openai", "oauth", 6).await;
    let b = harness.insert_account("openai", "apikey", 7).await;
    let lease = harness.runtime.actions.locks.account(b).await;
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[b, a],
            view["version"].as_str().unwrap(),
            "fixture-priority-cancel-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let id = accepted["id"].as_str().unwrap().to_owned();
    let (status, cancelled, text) = harness
        .request(
            Method::POST,
            &format!("account-priority-orders/{id}/cancel?schema=2"),
            None,
        )
        .await;
    assert_eq!(status, 200, "priority cancel failed: {text}");
    assert_eq!(cancelled["cancel_requested"], true);
    drop(cancelled);
    drop(lease);
    harness.tick().await;
    let detail = harness.wait_task(&id).await;
    assert_eq!(detail["status"], "cancelled");
    assert_eq!(harness.upstream.put_ids().len(), 0);
    assert_eq!(harness.priority(a).await, 6);
    assert_eq!(harness.priority(b).await, 7);
    harness.finish().await;
}

#[tokio::test]
async fn cancel_after_dispatch_does_not_replay_or_cancel_the_inflight_item() {
    let mut harness = Harness::new().await;
    let a = harness.insert_account("openai", "oauth", 6).await;
    let b = harness.insert_account("openai", "apikey", 7).await;
    let gate = harness.upstream.set_gate(b);
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[b, a],
            view["version"].as_str().unwrap(),
            "fixture-priority-cancel-dispatched-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let id = accepted["id"].as_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .expect("priority write should reach upstream once");
    let (status, cancelled, text) = harness
        .request(
            Method::POST,
            &format!("account-priority-orders/{id}/cancel?schema=2"),
            None,
        )
        .await;
    assert_eq!(status, 200, "priority cancel failed: {text}");
    assert_eq!(cancelled["cancel_requested"], true);
    gate.release.notify_one();
    let detail = harness.wait_task(&id).await;
    assert_eq!(detail["status"], "cancelled");
    assert_eq!(harness.upstream.count_put_for(b), 1);
    assert_eq!(harness.upstream.count_put_for(a), 0);
    assert_eq!(
        detail["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["status"] == "verified")
            .count(),
        1
    );
    assert_eq!(
        detail["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["status"] == "pending")
            .count(),
        1
    );
    harness.finish().await;
}

#[tokio::test]
async fn unknown_write_is_checked_once_and_never_replayed() {
    let mut harness = Harness::new().await;
    let id = harness.insert_account("openai", "oauth", 6).await;
    harness.upstream.set_mode(id, WriteMode::UnknownNotApplied);
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[id],
            view["version"].as_str().unwrap(),
            "fixture-priority-unknown-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    harness.tick().await;
    let detail = harness.wait_task(accepted["id"].as_str().unwrap()).await;
    assert_eq!(detail["status"], "needs_confirmation");
    assert_eq!(harness.upstream.count_put_for(id), 1);
    assert_eq!(harness.priority(id).await, 6);
    assert_eq!(detail["items"][0]["status"], "dispatched");
    harness.finish().await;
}

#[tokio::test]
async fn unknown_write_that_was_applied_is_verified_without_a_second_put() {
    let mut harness = Harness::new().await;
    let id = harness.insert_account("openai", "oauth", 6).await;
    harness.upstream.set_mode(id, WriteMode::UnknownApplied);
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[id],
            view["version"].as_str().unwrap(),
            "fixture-priority-unknown-applied-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    harness.tick().await;
    let detail = harness.wait_task(accepted["id"].as_str().unwrap()).await;
    assert_eq!(detail["status"], "completed");
    assert_eq!(harness.upstream.count_put_for(id), 1);
    assert_eq!(harness.priority(id).await, 1);
    assert_eq!(detail["items"][0]["status"], "verified");
    harness.finish().await;
}

#[tokio::test]
async fn restart_reconciles_a_dispatched_applied_write_without_replay() {
    let mut harness = Harness::new().await;
    let id = harness.insert_account("openai", "oauth", 6).await;
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[id],
            view["version"].as_str().unwrap(),
            "fixture-priority-restart-applied-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let order_id = accepted["id"].as_str().unwrap().to_owned();
    let target = accepted["items"][0]["target_priority"]
        .as_i64()
        .expect("stored target priority");
    sqlx::query("UPDATE accounts SET priority=$1,updated_at=now() WHERE id=$2")
        .bind(target as i32)
        .bind(id)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("apply persisted fixture write before restart");
    harness
        .runtime
        .priorities
        .store
        .transaction(|state| {
            let job = &mut state["jobs"][&order_id];
            job["status"] = "running".into();
            job["items"][0]["status"] = "dispatched".into();
            job["items"][0]["started_at"] = now().into();
            Ok(())
        })
        .expect("persist dispatched recovery fixture");
    harness.restart_runtime().await;
    let detail = harness.wait_task(&order_id).await;
    assert_eq!(detail["status"], "completed");
    assert_eq!(detail["items"][0]["status"], "verified");
    assert_eq!(detail["items"][0]["observed_priority"], target);
    assert_eq!(harness.upstream.count_put_for(id), 0);
    harness.finish().await;
}

#[tokio::test]
async fn restart_marks_an_unapplied_dispatched_write_for_confirmation_without_replay() {
    let mut harness = Harness::new().await;
    let id = harness.insert_account("openai", "oauth", 6).await;
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[id],
            view["version"].as_str().unwrap(),
            "fixture-priority-restart-unapplied-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let order_id = accepted["id"].as_str().unwrap().to_owned();
    let original = harness.priority(id).await;
    harness
        .runtime
        .priorities
        .store
        .transaction(|state| {
            let job = &mut state["jobs"][&order_id];
            job["status"] = "running".into();
            job["items"][0]["status"] = "dispatched".into();
            job["items"][0]["started_at"] = now().into();
            Ok(())
        })
        .expect("persist dispatched unknown fixture");
    harness.restart_runtime().await;
    let detail = harness.wait_task(&order_id).await;
    assert_eq!(detail["status"], "needs_confirmation");
    assert_eq!(detail["items"][0]["status"], "dispatched");
    assert_eq!(detail["items"][0]["observed_priority"], original);
    assert_eq!(harness.upstream.count_put_for(id), 0);
    harness.finish().await;
}

#[tokio::test]
async fn restart_continues_only_the_never_dispatched_remaining_item() {
    let mut harness = Harness::new().await;
    let first = harness.insert_account("openai", "oauth", 6).await;
    let second = harness.insert_account("openai", "apikey", 7).await;
    let lease = harness.runtime.actions.locks.account(second).await;
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[second, first],
            view["version"].as_str().unwrap(),
            "fixture-priority-restart-remaining-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let order_id = accepted["id"].as_str().unwrap().to_owned();
    let first_target = accepted["items"][0]["target_priority"]
        .as_i64()
        .expect("first target priority");
    sqlx::query("UPDATE accounts SET priority=$1,updated_at=now() WHERE id=$2")
        .bind(first_target as i32)
        .bind(second)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("apply first persisted fixture write");
    let first_row = harness
        .runtime
        .actions
        .account(second, None)
        .await
        .expect("read persisted first item version");
    let verified_version = operation_versions(&first_row)["priority"].clone();
    harness
        .runtime
        .priorities
        .store
        .transaction(|state| {
            let job = &mut state["jobs"][&order_id];
            job["status"] = "running".into();
            job["items"][0]["status"] = "verified".into();
            job["items"][0]["verified_version"] = verified_version.clone();
            job["items"][0]["observed_priority"] = first_target.into();
            Ok(())
        })
        .expect("persist verified first item fixture");
    drop(lease);
    harness.restart_runtime().await;
    let detail = harness.wait_task(&order_id).await;
    assert_eq!(detail["status"], "completed");
    assert_eq!(detail["items"][0]["status"], "verified");
    assert_eq!(detail["items"][1]["status"], "verified");
    assert_eq!(harness.upstream.count_put_for(second), 0);
    assert_eq!(harness.upstream.count_put_for(first), 1);
    harness.finish().await;
}

#[tokio::test]
async fn parent_child_lock_blocks_the_child_then_resumes_without_parallel_writes() {
    let mut harness = Harness::new().await;
    let parent = harness.insert_account("openai", "oauth", 0).await;
    let child = harness.insert_account("openai", "oauth", 1).await;
    harness.set_parent(child, parent).await;
    let parent_lease = harness.runtime.actions.locks.account(parent).await;
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[child, parent],
            view["version"].as_str().unwrap(),
            "fixture-priority-parent-lock-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let order_id = accepted["id"].as_str().unwrap().to_owned();
    harness.tick().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, active, text) = harness
        .request(
            Method::GET,
            &format!("account-priority-orders/{order_id}?schema=2"),
            None,
        )
        .await;
    assert_eq!(status, 200, "priority task read failed: {text}");
    assert!(matches!(
        active["status"].as_str(),
        Some("queued") | Some("running")
    ));
    assert_eq!(harness.upstream.put_ids().len(), 0);
    drop(parent_lease);
    harness.tick().await;
    let detail = harness.wait_task(&order_id).await;
    assert_eq!(detail["status"], "completed");
    assert_eq!(harness.upstream.count_put_for(child), 0);
    assert_eq!(harness.upstream.count_put_for(parent), 1);
    assert_eq!(harness.upstream.max_active_writes(), 1);
    harness.finish().await;
}

#[tokio::test]
async fn credit_pause_ownership_is_reconciled_after_priority_write() {
    let mut harness = Harness::new().await;
    let id = harness.insert_account("openai", "oauth", 6).await;
    sqlx::query("UPDATE accounts SET schedulable=false,updated_at=now() WHERE id=$1")
        .bind(id)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("pause fixture account scheduling");
    let row = sub2ops_server::credit_ownership::account(&harness.runtime.db, id)
        .await
        .expect("read paused fixture account");
    let signature = sub2ops_server::credit_ownership::signature(&row);
    let owned_version = row["updated_at"].clone();
    harness
        .runtime
        .actions
        .queries
        .oauth
        .transaction(|state| {
            state["scheduler"][id.to_string()]["auto_reset_credit"] = json!({
                "stage":"resetting",
                "episode":"fixture-priority-credit-0001",
                "owns_pause":true,
                "signature":signature,
                "owned_version":owned_version,
                "revision":1
            });
            Ok(())
        })
        .expect("persist paused credit ownership fixture");
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[id],
            view["version"].as_str().unwrap(),
            "fixture-priority-credit-write-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    harness.tick().await;
    let detail = harness.wait_task(accepted["id"].as_str().unwrap()).await;
    assert_eq!(detail["status"], "completed");
    assert_eq!(harness.upstream.count_put_for(id), 1);
    let state = harness.runtime.actions.queries.oauth.snapshot().unwrap();
    let task = &state["scheduler"][id.to_string()]["auto_reset_credit"];
    assert_eq!(task["owns_pause"], true);
    assert!(task["parameter_edit"].is_null());
    harness.finish().await;
}

#[tokio::test]
async fn queued_legacy_priority_operation_blocks_a_new_order() {
    let mut harness = Harness::new().await;
    let id = harness.insert_account("openai", "oauth", 6).await;
    let row = harness
        .runtime
        .actions
        .account(id, None)
        .await
        .expect("read legacy priority operation version");
    let expected = operation_versions(&row)["priority"].clone();
    let (status, queued, text) = harness
        .request(
            Method::POST,
            &format!("accounts/{id}/operations"),
            Some(json!({
                "action":"priority",
                "payload":{"priority":1},
                "expected_version":expected,
                "request_id":"fixture-priority-legacy-queue-0001",
                "client_id":CLIENT_ID
            })),
        )
        .await;
    assert_eq!(
        status, 202,
        "legacy priority operation should queue: {text}"
    );
    assert_eq!(queued["status"], "queued");
    let view = harness.view().await;
    let (status, _, text) = harness
        .submit(
            &[id],
            view["version"].as_str().unwrap(),
            "fixture-priority-order-after-legacy-0001",
        )
        .await;
    assert_eq!(
        status, 409,
        "queued legacy priority must block order: {text}"
    );
    assert_eq!(harness.upstream.put_ids().len(), 0);
    harness.finish().await;
}

#[tokio::test]
async fn legacy_priority_write_is_mutually_exclusive_with_active_order() {
    let mut harness = Harness::new().await;
    let a = harness.insert_account("openai", "oauth", 6).await;
    let b = harness.insert_account("openai", "apikey", 7).await;
    let lease = harness.runtime.actions.locks.account(a).await;
    let view = harness.view().await;
    let (status, accepted, text) = harness
        .submit(
            &[b, a],
            view["version"].as_str().unwrap(),
            "fixture-priority-mutex-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let current = harness
        .runtime
        .actions
        .account(a, None)
        .await
        .expect("read full account version for legacy priority API");
    let (status, _, text) = harness
        .request(
            Method::POST,
            &format!("accounts/{a}/priority"),
            Some(json!({
                "expected_version": sub2ops_server::compat::account_version(&current),
                "priority": 10
            })),
        )
        .await;
    assert_eq!(status, 409, "legacy priority write must be blocked: {text}");
    assert_eq!(harness.upstream.put_ids().len(), 0);

    let order_id = accepted["id"].as_str().unwrap();
    harness
        .request(
            Method::POST,
            &format!("account-priority-orders/{order_id}/cancel?schema=2"),
            None,
        )
        .await;
    drop(lease);
    harness.tick().await;
    let _ = harness.wait_task(order_id).await;
    let current = harness
        .runtime
        .actions
        .account(a, None)
        .await
        .expect("read full account version after order cancellation");
    let (status, _, text) = harness
        .request(
            Method::POST,
            &format!("accounts/{a}/priority"),
            Some(json!({
                "expected_version": sub2ops_server::compat::account_version(&current),
                "priority": 10
            })),
        )
        .await;
    assert_eq!(
        status, 200,
        "legacy priority write should resume after cancellation: {text}"
    );
    assert_eq!(harness.priority(a).await, 10);
    assert_eq!(harness.upstream.count_put_for(a), 1);
    harness.finish().await;
}

#[tokio::test]
async fn read_only_startup_only_reads_priority_order_state() {
    let mut harness = Harness::new().await;
    let a = harness.insert_account("openai", "oauth", 6).await;
    let view = harness.view().await;
    let (_, accepted, _) = harness
        .submit(
            &[a],
            view["version"].as_str().unwrap(),
            "fixture-priority-readonly-0001",
        )
        .await;
    let _ = harness.wait_task(accepted["id"].as_str().unwrap()).await;
    let path = harness.runtime.priorities.store.path().to_path_buf();
    let before = fs::read(&path).expect("priority-order state exists");
    let mut settings = (*harness.runtime.settings).clone();
    settings.read_only = true;
    let read_only = Runtime::new(settings)
        .await
        .expect("construct read-only runtime");
    read_only.start().await.expect("read-only startup check");
    let after = fs::read(&path).expect("priority-order state remains");
    assert_eq!(
        before, after,
        "read-only startup must not rewrite priority state"
    );
    read_only.close().await;
    harness.finish().await;
}

#[tokio::test]
async fn corrupt_priority_state_fails_explicitly_and_missing_read_only_state_stays_missing() {
    let mut harness = Harness::new().await;
    let path = harness.runtime.priorities.store.path().to_path_buf();
    assert!(
        !path.exists(),
        "fresh fixture should not create priority state"
    );
    let mut settings = (*harness.runtime.settings).clone();
    settings.read_only = true;
    let read_only = Runtime::new(settings)
        .await
        .expect("construct read-only runtime without priority state");
    read_only
        .start()
        .await
        .expect("read-only startup should be a no-op");
    let view = read_only
        .priorities
        .view(false)
        .await
        .expect("read-only view should use in-memory defaults");
    assert!(view["items"].is_array());
    for item in view["items"].as_array().unwrap() {
        assert_eq!(item["editable"], false);
        assert_eq!(item["reason"], "Companion 处于只读模式");
    }
    let submit = read_only
        .priorities
        .submit(&json!({}))
        .await
        .expect_err("read-only submit must be rejected");
    assert_eq!(submit.status.as_u16(), 409);
    assert!(
        !path.exists(),
        "read-only startup and submit must not write state"
    );
    read_only.close().await;

    fs::write(&path, br#"{"version":1}"#).expect("write corrupt priority state fixture");
    let (status, _, text) = harness
        .request(Method::GET, "account-priorities", None)
        .await;
    assert_eq!(status, 503, "corrupt state must fail explicitly: {text}");
    harness.finish().await;
}

#[tokio::test]
async fn priority_v2_view_is_platform_scoped_and_legacy_list_stays_read_only() {
    assert_eq!(fixtures_v2()["schema_version"], 2);
    assert_eq!(fixtures_v2()["storage"]["version"], 2);
    assert_eq!(
        fixtures_v2()["storage"]["legacy_is_readonly"],
        "read-only after active task retirement"
    );

    let mut harness = Harness::new().await;
    let openai_a = harness.insert_account("openai", "oauth", 2).await;
    let openai_b = harness.insert_account("openai", "oauth", 0).await;
    let grok = harness.insert_account("grok", "apikey", 1).await;
    let deleted = harness.insert_account("openai", "oauth", 4).await;
    harness.delete_account(deleted).await;

    let legacy = harness.view().await;
    let mut legacy_keys = legacy
        .as_object()
        .expect("legacy priority list object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    legacy_keys.sort();
    assert_eq!(
        legacy_keys,
        vec!["active_task", "items", "last_task", "version"]
    );

    let view = harness.v2_view().await;
    let mut keys = view
        .as_object()
        .expect("priority v2 view object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, vec!["active_task", "legacy_last_task", "platforms"]);
    assert!(view["active_task"].is_null());
    assert!(view["legacy_last_task"].is_null());
    let platforms = view["platforms"].as_array().expect("platform list");
    assert_eq!(platforms.len(), 2);
    for platform in platforms {
        assert!(platform["platform"].is_string());
        assert!(platform["version"].as_str().is_some_and(|v| v.len() == 64));
        assert!(platform["items"].is_array());
        assert!(platform.get("last_task").is_some());
        for item in platform["items"].as_array().unwrap() {
            assert_eq!(item["platform"], platform["platform"]);
            assert!(item["version"].as_str().is_some_and(|v| v.len() == 64));
        }
    }
    let openai = platforms
        .iter()
        .find(|platform| platform["platform"] == "openai")
        .expect("openai platform projection");
    assert_eq!(
        openai["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![openai_a, openai_b]
    );
    assert!(!openai["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == deleted));
    assert!(platforms
        .iter()
        .find(|platform| platform["platform"] == "grok")
        .unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == grok));

    let openai_version = platforms
        .iter()
        .find(|platform| platform["platform"] == "openai")
        .unwrap()["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let grok_version = platforms
        .iter()
        .find(|platform| platform["platform"] == "grok")
        .unwrap()["version"]
        .as_str()
        .unwrap()
        .to_owned();
    sqlx::query("UPDATE accounts SET priority=11,updated_at=now() WHERE id=$1")
        .bind(grok)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("change only the grok platform priority");
    let isolated = harness.v2_view().await;
    assert_eq!(
        isolated["platforms"]
            .as_array()
            .unwrap()
            .iter()
            .find(|platform| platform["platform"] == "openai")
            .unwrap()["version"],
        openai_version
    );
    assert_ne!(
        isolated["platforms"]
            .as_array()
            .unwrap()
            .iter()
            .find(|platform| platform["platform"] == "grok")
            .unwrap()["version"],
        grok_version
    );
    for path in [
        "account-priorities?schema=2&schema=2",
        "account-priorities?schema=3",
        "account-priority-orders/00000000000000000000000000000000?schema=2&details=1",
    ] {
        let (status, _, text) = harness.request(Method::GET, path, None).await;
        assert_eq!(
            status, 422,
            "invalid priority query must be rejected: {text}"
        );
    }
    harness.finish().await;
}

#[tokio::test]
async fn priority_v2_rejects_mixed_or_incomplete_sets_and_accepts_unknown_platforms() {
    let mut harness = Harness::new().await;
    let openai_a = harness.insert_account("openai", "oauth", 2).await;
    let _openai_b = harness.insert_account("openai", "oauth", 4).await;
    let grok = harness.insert_account("grok", "apikey", 1).await;
    let unknown = harness
        .insert_account("fixture-unknown-platform", "oauth", 3)
        .await;
    let view = harness.v2_view().await;
    let platform_version = |name: &str| {
        view["platforms"]
            .as_array()
            .unwrap()
            .iter()
            .find(|platform| platform["platform"] == name)
            .unwrap()["version"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let openai_version = platform_version("openai");
    let (status, _, text) = harness
        .v2_submit(
            "openai",
            &[openai_a, grok],
            &openai_version,
            "fixture-priority-v2-mixed-0001",
        )
        .await;
    assert_eq!(status, 409, "mixed-platform order must be rejected: {text}");
    let (status, _, text) = harness
        .v2_submit(
            "openai",
            &[openai_a],
            &openai_version,
            "fixture-priority-v2-incomplete-0001",
        )
        .await;
    assert_eq!(
        status, 409,
        "incomplete platform collection must be rejected: {text}"
    );

    let unknown_version = platform_version("fixture-unknown-platform");
    let (status, accepted, text) = harness
        .v2_submit(
            "fixture-unknown-platform",
            &[unknown],
            &unknown_version,
            "fixture-priority-v2-unknown-platform-0001",
        )
        .await;
    assert_eq!(
        status, 200,
        "unknown platform order should be numbered: {text}"
    );
    assert_eq!(accepted["platform"], "fixture-unknown-platform");
    assert_eq!(accepted["base"], 1);
    assert_eq!(accepted["items"][0]["target_priority"], 1);
    let detail = harness.wait_v2_task(accepted["id"].as_str().unwrap()).await;
    assert_eq!(detail["status"], "completed");
    harness.finish().await;
}

#[tokio::test]
async fn priority_v2_submit_starts_without_tick_and_details_zero_is_summary_only() {
    let mut harness = Harness::new().await;
    let first = harness.insert_account("openai", "oauth", 2).await;
    let second = harness.insert_account("openai", "oauth", 0).await;
    let view = harness.v2_view().await;
    let platform = view["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|platform| platform["platform"] == "openai")
        .unwrap();
    let version = platform["version"].as_str().unwrap();
    let lease = harness.runtime.actions.locks.account(first).await;
    let (status, accepted, text) = harness
        .v2_submit(
            "openai",
            &[first, second],
            version,
            "fixture-priority-v2-submit-0001",
        )
        .await;
    assert_eq!(status, 200, "priority v2 submit failed: {text}");
    for key in ["platform", "base", "waiting_reason", "waiting_account_ids"] {
        assert!(accepted.get(key).is_some(), "task missing {key}");
    }
    assert_eq!(accepted["platform"], "openai");
    assert_eq!(accepted["base"], 1);
    assert!(accepted["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["platform"] == "openai"));
    let id = accepted["id"].as_str().unwrap().to_owned();
    let (status, summary, text) = harness.v2_detail(&id, false).await;
    assert_eq!(status, 200, "priority v2 summary failed: {text}");
    assert!(summary.get("items").is_none(), "details=0 must omit items");
    assert_eq!(summary["platform"], "openai");
    let waiting = harness.wait_v2_waiting(&id, first).await;
    assert!(waiting["waiting_reason"]
        .as_str()
        .is_some_and(|reason| !reason.is_empty()));
    drop(lease);
    harness.tick().await;
    let detail = harness.wait_v2_task(&id).await;
    assert_eq!(detail["status"], "completed");
    assert!(harness.upstream.count_put_for(first) <= 1);
    harness.finish().await;
}

#[tokio::test]
async fn priority_v2_busy_items_wait_and_progress_with_one_dispatched_item() {
    let mut harness = Harness::new().await;
    let first = harness.insert_account("openai", "oauth", 3).await;
    let second = harness.insert_account("openai", "oauth", 4).await;
    let third = harness.insert_account("openai", "oauth", 1).await;
    let view = harness.v2_view().await;
    let platform = view["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|platform| platform["platform"] == "openai")
        .unwrap();
    let lease = harness.runtime.actions.locks.account(first).await;
    let (status, accepted, text) = harness
        .v2_submit(
            "openai",
            &[first, second, third],
            platform["version"].as_str().unwrap(),
            "fixture-priority-v2-busy-0001",
        )
        .await;
    assert_eq!(status, 200, "priority v2 busy submit failed: {text}");
    let id = accepted["id"].as_str().unwrap();
    let waiting = harness.wait_v2_waiting(id, first).await;
    let dispatched = waiting["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["status"] == "dispatched")
        .count();
    assert!(dispatched <= 1, "at most one item may be dispatched");
    assert!(waiting["waiting_account_ids"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_i64() == Some(first)));
    assert_eq!(
        waiting["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["account_id"] == second)
            .unwrap()["status"],
        "verified",
        "a later pending item should complete while the first account is busy"
    );
    drop(lease);
    harness.tick().await;
    let detail = harness.wait_v2_task(id).await;
    assert_eq!(detail["status"], "completed");
    assert_eq!(harness.upstream.count_method(Method::PUT), 3);
    harness.finish().await;
}

#[tokio::test]
async fn priority_v2_missing_worker_auth_stops_before_dispatch() {
    let mut harness = Harness::new().await;
    let id = harness.insert_account("openai", "oauth", 4).await;
    let lease = harness.runtime.actions.locks.account(id).await;
    let view = harness.v2_view().await;
    let version = view["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|platform| platform["platform"] == "openai")
        .unwrap()["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, accepted, text) = harness
        .v2_submit(
            "openai",
            &[id],
            &version,
            "fixture-priority-v2-missing-auth-0001",
        )
        .await;
    assert_eq!(status, 200, "priority submit failed: {text}");
    let order_id = accepted["id"].as_str().unwrap().to_owned();
    let _ = harness.wait_v2_waiting(&order_id, id).await;
    harness
        .runtime
        .actions
        .queries
        .oauth
        .transaction(|state| {
            state["settings"]["sub2api_admin_token"] = Value::Null;
            Ok(())
        })
        .expect("clear the worker admin token");
    drop(lease);
    harness.tick().await;
    let detail = harness.wait_v2_task(&order_id).await;
    assert_eq!(detail["status"], "needs_confirmation");
    assert_eq!(detail["reason"], "管理员授权不可用，请重新连接后核对");
    assert_eq!(harness.upstream.count_put_for(id), 0);
    harness.finish().await;
}

#[tokio::test]
async fn priority_v2_history_is_not_trimmed_and_legacy_store_is_separate() {
    let mut harness = Harness::new().await;
    let first = harness.insert_account("openai", "oauth", 0).await;
    let second = harness.insert_account("openai", "oauth", 1).await;
    let first_request_id = "fixture-priority-v2-history-0000";
    let mut first_version = String::new();
    let mut first_task_id = String::new();
    for index in 0..21 {
        let view = harness.v2_view().await;
        let platform = view["platforms"]
            .as_array()
            .unwrap()
            .iter()
            .find(|platform| platform["platform"] == "openai")
            .unwrap();
        let ordered = if index % 2 == 0 {
            vec![second, first]
        } else {
            vec![first, second]
        };
        let request_id = format!("fixture-priority-v2-history-{index:04}");
        if index == 0 {
            first_version = platform["version"].as_str().unwrap().to_owned();
        }
        let (status, accepted, text) = harness
            .v2_submit(
                "openai",
                &ordered,
                platform["version"].as_str().unwrap(),
                &request_id,
            )
            .await;
        assert_eq!(status, 200, "history submit {index} failed: {text}");
        if index == 0 {
            first_task_id = accepted["id"].as_str().unwrap().to_owned();
        }
        let detail = harness.wait_v2_task(accepted["id"].as_str().unwrap()).await;
        assert_eq!(detail["status"], "completed", "history task {index}");
    }
    let v2_path = harness.runtime.priorities.store.path().to_path_buf();
    let state: Value = serde_json::from_slice(&fs::read(&v2_path).expect("v2 state file exists"))
        .expect("v2 state is JSON");
    assert_eq!(state["version"], 2);
    assert!(state["jobs"].as_object().unwrap().len() >= 21);
    assert!(state["requests"].as_object().unwrap().len() >= 21);
    let legacy_path = v2_path.with_file_name("account-priority-orders.json");
    if legacy_path.exists() {
        let legacy = fs::read_to_string(legacy_path).expect("read legacy state");
        assert!(!legacy.contains("fixture-priority-v2-history-"));
    }
    let (status, replay, text) = harness
        .v2_submit("openai", &[second, first], &first_version, first_request_id)
        .await;
    assert_eq!(
        status, 200,
        "first historical request should replay: {text}"
    );
    assert_eq!(replay["id"], first_task_id);
    harness.finish().await;
}

#[tokio::test]
async fn priority_v2_restart_does_not_replay_a_verified_write() {
    let mut harness = Harness::new().await;
    let id = harness.insert_account("openai", "oauth", 4).await;
    harness.upstream.set_mode(id, WriteMode::UnknownApplied);
    let view = harness.v2_view().await;
    let platform = view["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|platform| platform["platform"] == "openai")
        .unwrap();
    let (status, accepted, text) = harness
        .v2_submit(
            "openai",
            &[id],
            platform["version"].as_str().unwrap(),
            "fixture-priority-v2-restart-0001",
        )
        .await;
    assert_eq!(status, 200, "priority v2 restart submit failed: {text}");
    let order_id = accepted["id"].as_str().unwrap().to_owned();
    let before = harness.wait_v2_task(&order_id).await;
    assert_eq!(before["status"], "completed");
    assert_eq!(harness.upstream.count_put_for(id), 1);
    harness.restart_runtime().await;
    let after = harness.wait_v2_task(&order_id).await;
    assert_eq!(after["status"], "completed");
    assert_eq!(harness.upstream.count_put_for(id), 1);
    harness.finish().await;
}

#[tokio::test]
async fn priority_v2_retires_legacy_active_state_and_rejects_legacy_post() {
    let mut harness = Harness::new().await;
    let legacy_fixture: Value = serde_json::from_str(include_str!(
        "../../tests/contracts/priority-order/legacy-active.json"
    ))
    .expect("legacy active fixture JSON");
    let legacy_id = legacy_fixture["active_id"]
        .as_str()
        .expect("legacy active fixture id")
        .to_owned();
    let legacy_path = harness
        .runtime
        .priorities
        .store
        .path()
        .with_file_name("account-priority-orders.json");
    fs::write(
        &legacy_path,
        include_bytes!("../../tests/contracts/priority-order/legacy-active.json"),
    )
    .expect("write legacy active state fixture");
    harness.restart_runtime().await;
    let view = harness.v2_view().await;
    assert!(view["active_task"].is_null());
    assert!(view["legacy_last_task"].is_object());
    let (status, value, text) = harness
        .request(
            Method::POST,
            "account-priority-orders",
            Some(json!({
                "platform":"openai",
                "ordered_account_ids":[],
                "expected_version":"0000000000000000000000000000000000000000000000000000000000000000",
                "request_id":"fixture-priority-legacy-post-0001",
                "client_id":CLIENT_ID
            })),
        )
        .await;
    assert_eq!(status, 422, "legacy POST must require schema=2: {text}");
    assert_eq!(value["detail"]["code"], "client_upgrade_required");
    let legacy = harness.view().await;
    assert!(legacy["active_task"].is_null());
    assert!(legacy["last_task"].is_object());
    let (status, detail, text) = harness
        .request(
            Method::GET,
            &format!("account-priority-orders/{legacy_id}"),
            None,
        )
        .await;
    assert_eq!(status, 200, "legacy detail should remain readable: {text}");
    let mut detail_keys = detail
        .as_object()
        .expect("legacy task detail object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    detail_keys.sort();
    assert_eq!(
        detail_keys,
        vec![
            "cancel_requested",
            "change_count",
            "completed_at",
            "created_at",
            "id",
            "items",
            "reason",
            "status",
            "total",
            "updated_at",
            "verified_count"
        ]
    );
    assert!(detail["items"][0].get("platform").is_none());
    harness.finish().await;
}

#[tokio::test]
async fn priority_legacy_retirement_reconciles_dispatched_without_replay() {
    for (applied, suffix) in [(true, "applied"), (false, "unapplied")] {
        let mut harness = Harness::new().await;
        let account_id = harness.insert_account("openai", "oauth", 4).await;
        let row = harness
            .runtime
            .actions
            .account(account_id, None)
            .await
            .expect("read legacy retirement account");
        let original_priority = row["priority"].as_i64().expect("legacy original priority");
        let target_priority = 0;
        let expected_version = operation_versions(&row)["priority"].clone();
        let mut identity_row = row.clone();
        identity_row["priority"] = 0.into();
        let identity = operation_versions(&identity_row)["priority"].clone();
        if applied {
            sqlx::query("UPDATE accounts SET priority=$1,updated_at=now() WHERE id=$2")
                .bind(target_priority)
                .bind(account_id)
                .execute(&harness.runtime.db.pool)
                .await
                .expect("apply legacy dispatched fixture before restart");
        }
        let task_id = if applied {
            "0123456789abcdef0123456789abcde1"
        } else {
            "0123456789abcdef0123456789abcde2"
        };
        let request_id = format!("fixture-legacy-dispatched-{suffix}-0001");
        let timestamp = now();
        let state = json!({
            "version": 1,
            "active_id": task_id,
            "last_id": task_id,
            "jobs": {
                task_id: {
                    "id": task_id,
                    "status": "running",
                    "reason": "正在保存",
                    "cancel_requested": false,
                    "request_id": request_id,
                    "client_id": CLIENT_ID,
                    "request_hash": "0000000000000000000000000000000000000000000000000000000000000000",
                    "created_at": timestamp,
                    "updated_at": timestamp,
                    "completed_at": null,
                    "items": [{
                        "account_id": account_id,
                        "account_name": row["name"],
                        "original_priority": original_priority,
                        "target_priority": target_priority,
                        "expected_version": expected_version,
                        "identity": identity,
                        "capability": {"editable": true, "reason": ""},
                        "status": "dispatched",
                        "started_at": timestamp,
                        "observed_priority": null
                    }]
                }
            },
            "requests": {request_id: task_id}
        });
        fs::write(
            harness.runtime.priorities.legacy.path(),
            serde_json::to_vec(&state).expect("serialize legacy dispatched fixture"),
        )
        .expect("write legacy dispatched fixture");
        harness.restart_runtime().await;
        let (status, detail, text) = harness
            .request(
                Method::GET,
                &format!("account-priority-orders/{task_id}"),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "legacy dispatched detail should be readable: {text}"
        );
        assert_eq!(detail["status"], "needs_confirmation");
        assert_eq!(
            detail["items"][0]["status"],
            if applied { "verified" } else { "dispatched" }
        );
        assert_eq!(harness.upstream.count_put_for(account_id), 0);
        assert_eq!(
            harness.priority(account_id).await,
            if applied { 0 } else { 4 }
        );
        harness.finish().await;
    }
}

#[tokio::test]
async fn parallel_slots_keep_priorities_and_validate_platform_capacity() {
    let mut harness = Harness::new().await;
    let a = harness.insert_account("openai", "apikey", 1).await;
    let b = harness.insert_account("openai", "oauth", 3).await;
    let c = harness.insert_account("openai", "apikey", 5).await;
    let view = harness.v2_view().await;
    let (code, changed, _) = harness.request(Method::PUT, "account-priorities?schema=2", Some(json!({"platform":"openai","parallelism":3,"expected_version":view["platforms"][0]["version"]}))).await;
    assert_eq!(code, 200);
    assert_eq!(changed["platforms"][0]["parallelism"], 3);
    let request = json!({"platform":"openai","parallelism":3,"target_priorities":[1,1,5],"ordered_account_ids":[a,b,c],"expected_version":changed["platforms"][0]["version"],"request_id":"fixture-parallel-slots-0001","client_id":CLIENT_ID});
    let (code, job, _) = harness
        .request(
            Method::POST,
            "account-priority-orders?schema=2",
            Some(request.clone()),
        )
        .await;
    assert_eq!(code, 200);
    let finished = harness.wait_v2_task(job["id"].as_str().unwrap()).await;
    assert_eq!(finished["status"], "completed");
    assert_eq!(harness.priority(a).await, 1);
    assert_eq!(harness.priority(b).await, 1);
    assert_eq!(harness.priority(c).await, 5);
    let (code, replay, _) = harness
        .request(
            Method::POST,
            "account-priority-orders?schema=2",
            Some(request),
        )
        .await;
    assert_eq!(code, 200);
    assert_eq!(replay["id"], job["id"]);
    let view = harness.v2_view().await;
    assert_eq!(view["platforms"][0]["parallelism"], 3);
    let invalid = json!({"platform":"openai","parallelism":2,"target_priorities":[1,1,1],"ordered_account_ids":[a,b,c],"expected_version":view["platforms"][0]["version"],"request_id":"fixture-parallel-slots-0002","client_id":CLIENT_ID});
    assert_ne!(
        harness
            .request(
                Method::POST,
                "account-priority-orders?schema=2",
                Some(invalid)
            )
            .await
            .0,
        200
    );
    harness.finish().await;
}
