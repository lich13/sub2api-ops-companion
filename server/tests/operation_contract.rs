use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use sqlx::{types::Json as SqlJson, PgPool};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use sub2ops_server::{
    account_locks::AccountLocks,
    account_templates::Templates,
    actions::Actions,
    compat::operation_versions,
    db::Database,
    error::{Error, Result},
    oauth_state::OAuthState,
    operations::{Backend, OperationStore, Operations},
    queries::Queries,
    settings::Settings,
    store::Store,
    upstream::Upstream,
};
use tokio::{
    net::TcpListener,
    sync::{Notify, Semaphore},
    task::JoinHandle,
};

const ADMIN_KEY: &str = "fixture-admin-operation-contract";
const CLIENT_ID: &str = "fixture-operation-contract-client";
const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";
const CONTENT_MARKER: &str = "fixture-response-content-operation";
const BEARER_MARKER: &str = "fixture-bearer-operation";
const PROVIDER_MARKER: &str = "fixture-provider-operation";
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(800_000);

fn fixtures() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/contracts/operation-fixtures.json"
    ))
    .expect("operation fixture JSON must parse")
}

#[derive(Clone)]
struct MockCall {
    method: Method,
    path: String,
    body: Value,
    authorized: bool,
}

#[derive(Clone, Copy, Default)]
enum StreamScenario {
    #[default]
    Success,
    ExplicitFailure,
    Truncated,
    NoTerminal,
    Disconnected,
    NoModel,
    NativeError,
    HttpError,
    AuthError,
}

#[derive(Clone, Default)]
enum BulkScenario {
    #[default]
    Unknown,
    Partition {
        success_ids: BTreeSet<i64>,
    },
}

#[derive(Clone)]
struct MockUpstream {
    pool: PgPool,
    calls: Arc<Mutex<Vec<MockCall>>>,
    streams: Arc<Mutex<BTreeMap<i64, StreamScenario>>>,
    test_gates: Arc<Mutex<BTreeMap<i64, Arc<Notify>>>>,
    auth_gate: Arc<Mutex<Option<Arc<Notify>>>>,
    bulk: Arc<Mutex<BulkScenario>>,
    proxy_id: Arc<Mutex<Option<i64>>>,
    active: Arc<Mutex<Vec<i64>>>,
    active_snapshots: Arc<Mutex<Vec<Vec<i64>>>>,
}

impl MockUpstream {
    fn new(pool: PgPool) -> Self {
        Self {
            pool,
            calls: Arc::new(Mutex::new(Vec::new())),
            streams: Arc::new(Mutex::new(BTreeMap::new())),
            test_gates: Arc::new(Mutex::new(BTreeMap::new())),
            auth_gate: Arc::new(Mutex::new(None)),
            bulk: Arc::new(Mutex::new(BulkScenario::Unknown)),
            proxy_id: Arc::new(Mutex::new(None)),
            active: Arc::new(Mutex::new(Vec::new())),
            active_snapshots: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn record(&self, method: Method, path: String, body: Value, authorized: bool) {
        self.calls.lock().unwrap().push(MockCall {
            method,
            path,
            body,
            authorized,
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

    fn test_calls(&self) -> Vec<MockCall> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.method == Method::POST && call.path.ends_with("/test"))
            .cloned()
            .collect()
    }

    fn bulk_calls(&self) -> Vec<MockCall> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| {
                call.method == Method::POST && call.path.ends_with("/accounts/bulk-update")
            })
            .cloned()
            .collect()
    }

    fn set_stream(&self, account_id: i64, scenario: StreamScenario) {
        self.streams.lock().unwrap().insert(account_id, scenario);
    }

    fn gate_test(&self, account_id: i64) -> Arc<Notify> {
        let gate = Arc::new(Notify::new());
        self.test_gates
            .lock()
            .unwrap()
            .insert(account_id, gate.clone());
        gate
    }

    fn release_test(&self, account_id: i64) {
        if let Some(gate) = self.test_gates.lock().unwrap().get(&account_id) {
            gate.notify_one();
        }
    }

    fn release_all(&self) {
        for gate in self.test_gates.lock().unwrap().values() {
            gate.notify_one();
        }
        if let Some(gate) = self.auth_gate.lock().unwrap().as_ref() {
            gate.notify_one();
        }
    }

    fn set_auth_gate(&self, gate: Arc<Notify>) {
        *self.auth_gate.lock().unwrap() = Some(gate);
    }

    fn set_bulk(&self, scenario: BulkScenario) {
        *self.bulk.lock().unwrap() = scenario;
    }

    fn set_proxy_id(&self, id: i64) {
        *self.proxy_id.lock().unwrap() = Some(id);
    }
}

struct ActiveRequest {
    id: i64,
    active: Arc<Mutex<Vec<i64>>>,
}
impl Drop for ActiveRequest {
    fn drop(&mut self) {
        let mut active = self.active.lock().unwrap();
        if let Some(index) = active.iter().position(|id| *id == self.id) {
            active.remove(index);
        }
    }
}

fn json_response(value: Value) -> Response {
    Json(value).into_response()
}

fn status_json(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

fn sse_event(value: Value) -> String {
    format!("data: {value}\n\n")
}

fn account_id_from_test_path(path: &str) -> Option<i64> {
    path.strip_prefix("/api/v1/admin/accounts/")?
        .strip_suffix("/test")?
        .parse()
        .ok()
}

async fn mock_upstream(State(state): State<MockUpstream>, request: Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let authorized = request
        .headers()
        .get("x-api-key")
        .is_some_and(|v| v == ADMIN_KEY);
    let bytes = to_bytes(request.into_body(), 16_000_000)
        .await
        .unwrap_or_default();
    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    state.record(method.clone(), path.clone(), body.clone(), authorized);

    if path == "/api/v1/admin/groups/all" && method == Method::GET {
        let auth_gate = { state.auth_gate.lock().unwrap().clone() };
        if let Some(gate) = auth_gate {
            gate.notified().await;
        }
        return json_response(json!({"code": 0, "data": []}));
    }

    if path == "/api/v1/admin/proxies/all" && method == Method::GET {
        let id = state.proxy_id.lock().unwrap().unwrap_or(9001);
        return json_response(json!({"code": 0, "data": [{
            "id": id, "name": "fixture-proxy", "protocol": "http", "status": "active"
        }]}));
    }

    if method == Method::POST {
        if let Some(account_id) = account_id_from_test_path(&path) {
            {
                let mut active = state.active.lock().unwrap();
                active.push(account_id);
                state.active_snapshots.lock().unwrap().push(active.clone());
            }
            let _active = ActiveRequest {
                id: account_id,
                active: state.active.clone(),
            };
            let test_gate = { state.test_gates.lock().unwrap().get(&account_id).cloned() };
            if let Some(gate) = test_gate {
                gate.notified().await;
            }
            let scenario = state
                .streams
                .lock()
                .unwrap()
                .get(&account_id)
                .copied()
                .unwrap_or_default();
            let requested = body["model_id"].as_str().unwrap_or("");
            let mapped = sqlx::query_scalar::<_, Option<String>>(
                "SELECT credentials->'model_mapping'->>$1 FROM accounts WHERE id=$2",
            )
            .bind(requested)
            .bind(account_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .flatten();
            if matches!(scenario, StreamScenario::Disconnected) {
                return Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(futures_util::stream::iter([Err::<
                        Vec<u8>,
                        _,
                    >(
                        std::io::Error::new(
                            std::io::ErrorKind::ConnectionReset,
                            "fixture disconnect",
                        ),
                    )])))
                    .unwrap();
            }
            if matches!(scenario, StreamScenario::HttpError) {
                return (StatusCode::SERVICE_UNAVAILABLE, CONTENT_MARKER).into_response();
            }
            if matches!(scenario, StreamScenario::AuthError) {
                return (StatusCode::UNAUTHORIZED, "fixture auth expired").into_response();
            }
            let actual = mapped.unwrap_or_else(|| {
                if requested.is_empty() {
                    "fixture-grok-native-model".to_owned()
                } else {
                    requested.to_owned()
                }
            });
            let mut stream = if matches!(scenario, StreamScenario::NoModel) {
                String::new()
            } else {
                sse_event(json!({"type": "test_start", "model": actual}))
            };
            stream.push_str(&sse_event(json!({"type":"content","text":CONTENT_MARKER})));
            stream.push_str(&sse_event(json!({"type":"status","text":CONTENT_MARKER})));
            stream.push_str(&sse_event(json!({"type":"image","image_url":format!("data:image/png;base64,{CONTENT_MARKER}")})));
            match scenario {
                StreamScenario::Success | StreamScenario::NoModel => {
                    stream.push_str(&sse_event(
                        json!({"type": "test_complete", "success": true}),
                    ));
                }
                StreamScenario::ExplicitFailure => {
                    stream.push_str(&sse_event(
                        json!({"type": "test_complete", "success": false}),
                    ));
                }
                StreamScenario::Truncated => {
                    stream.push_str("data: {\"type\":\"test_complete\",\"success\":");
                }
                StreamScenario::NoTerminal => {
                    stream.push_str(&sse_event(
                        json!({"type": "content", "text": "fixture-content"}),
                    ));
                }
                StreamScenario::NativeError => stream.push_str(&sse_event(json!({"type":"error","error":format!("provider rejected: Bearer {BEARER_MARKER}; api_key={PROVIDER_MARKER}")}))),
                StreamScenario::Disconnected | StreamScenario::HttpError | StreamScenario::AuthError => unreachable!(),
            }
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from(stream))
                .expect("fixture SSE response");
        }
    }

    if path == "/api/v1/admin/accounts/bulk-update" && method == Method::POST {
        let expected: BTreeSet<i64> = body["account_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect();
        let bulk_scenario = { state.bulk.lock().unwrap().clone() };
        match bulk_scenario {
            BulkScenario::Unknown => {
                return status_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"code": 1, "message": "fixture-unknown-write-result"}),
                );
            }
            BulkScenario::Partition { success_ids } => {
                let successes: BTreeSet<i64> =
                    success_ids.intersection(&expected).copied().collect();
                let proxy_id = body["proxy_id"].as_i64().unwrap_or_default();
                for account_id in &successes {
                    let _ = sqlx::query("UPDATE accounts SET proxy_id=$1 WHERE id=$2")
                        .bind(proxy_id)
                        .bind(account_id)
                        .execute(&state.pool)
                        .await;
                }
                let failures: BTreeSet<i64> = expected.difference(&successes).copied().collect();
                let results: Vec<Value> = expected
                    .iter()
                    .map(|account_id| {
                        let success = successes.contains(account_id);
                        let mut result = json!({"account_id": account_id, "success": success});
                        if !success {
                            result["error"] = "fixture-sub2api-rejected".into();
                        }
                        result
                    })
                    .collect();
                return json_response(json!({"code": 0, "data": {
                    "success": successes.len(),
                    "failed": failures.len(),
                    "success_ids": successes,
                    "failed_ids": failures,
                    "results": results
                }}));
            }
        }
    }

    status_json(StatusCode::NOT_FOUND, json!({"detail": "Not Found"}))
}

struct FixtureBackend {
    now: Mutex<chrono::DateTime<chrono::Utc>>,
    pool: sqlx::PgPool,
    cycle_db: Database,
    cycle_reads: Mutex<Vec<(i64, bool)>>,
    cycle_gate: Mutex<Option<Arc<Notify>>>,
    cycle_refresh_error: Mutex<Option<String>>,
    recover_calls: Mutex<Vec<i64>>,
    recover_failures: Mutex<BTreeSet<i64>>,
    test_model: Mutex<String>,
    test_concurrency: AtomicI64,
    retry_policy: Mutex<sub2ops_server::connection_retry::Policy>,
}

impl Backend for FixtureBackend {
    fn retry_config(&self) -> Result<sub2ops_server::connection_retry::Policy> {
        Ok(*self.retry_policy.lock().unwrap())
    }
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        *self.now.lock().unwrap()
    }

    fn mark(&self, _id: i64) -> Result<Value> {
        Ok(json!({"marked": false, "version": "0".repeat(64)}))
    }

    fn dispatch<'a>(
        &'a self,
        row: &'a Value,
        job: &'a Value,
        _key: &'a str,
    ) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            if job["action"] != "recover" {
                return Err(Error::invalid());
            }
            let id = row["id"].as_i64().unwrap();
            self.recover_calls.lock().unwrap().push(id);
            if self.recover_failures.lock().unwrap().contains(&id) {
                return Err(Error::new(502, "fixture recovery reply lost"));
            }
            sqlx::query("UPDATE accounts SET status='active',error_message='',rate_limit_reset_at=NULL,overload_until=NULL,temp_unschedulable_until=NULL WHERE id=$1")
                .bind(id).execute(&self.pool).await.unwrap();
            Ok(json!({"verified":true,"account_id":id}))
        })
    }

    fn cycle_observation<'a>(
        &'a self,
        id: i64,
        _key: &'a str,
        refresh: bool,
    ) -> BoxFuture<'a, Result<sub2ops_server::operations::CycleObservation>> {
        Box::pin(async move {
            self.cycle_reads.lock().unwrap().push((id, refresh));
            let gate = if refresh {
                self.cycle_gate.lock().unwrap().take()
            } else {
                None
            };
            if let Some(gate) = gate {
                gate.notified().await;
            }
            if refresh {
                if let Some(message) = self.cycle_refresh_error.lock().unwrap().clone() {
                    return Err(Error::new(502, message));
                }
            }
            let row = sub2ops_server::credit_ownership::account(&self.cycle_db, id).await?;
            let at = self.now();
            let saved = sub2ops_server::quota::latest_result(&row, &Value::Null, at);
            let evidence =
                sub2ops_server::credit_rules::upstream_evidence(&self.cycle_db, &row, at).await?;
            sub2ops_server::operations::observe_cycle(&row, &saved, &evidence, at)
        })
    }

    fn model_get(&self, _id: &str) -> Result<Value> {
        Err(Error::new(404, "fixture model operation absent"))
    }

    fn model_jobs(&self) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }

    fn model_cancel<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { Err(Error::new(404, "fixture model operation absent")) })
    }

    fn test_config(&self) -> Result<(String, usize)> {
        Ok((
            self.test_model.lock().unwrap().clone(),
            self.test_concurrency.load(Ordering::Relaxed) as usize,
        ))
    }
}

struct Harness {
    _temp: tempfile::TempDir,
    db: Database,
    actions: Actions,
    backend: Arc<FixtureBackend>,
    operations: Arc<Operations>,
    mock: MockUpstream,
    server: Option<JoinHandle<()>>,
}

impl Harness {
    async fn new() -> Self {
        let fixture = fixtures();
        assert_eq!(fixture["reference"]["sha"], REFERENCE_SHA);
        let database_url =
            std::env::var("DATABASE_URL").expect("runner must provide temporary DATABASE_URL");
        let db = Database::open(&database_url, false)
            .await
            .expect("connect to runner-owned PostgreSQL");
        let temp = tempfile::tempdir().expect("create test state directory");
        let mock = MockUpstream::new(db.pool.clone());
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback mock");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .fallback(mock_upstream)
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve loopback fixture");
        });

        let connection_path = temp.path().join("sub2api-config.json");
        fs::write(
            &connection_path,
            serde_json::to_vec(&json!({"base_url": base_url, "verify_base_url": ""})).unwrap(),
        )
        .expect("write loopback upstream config");
        let settings = Arc::new(Settings {
            database_url: database_url.clone(),
            bind: "127.0.0.1:0".into(),
            base_path: String::new(),
            audit_path: temp.path().join("audit.jsonl"),
            usage_path: temp.path().join("usage-state.json"),
            oauth_path: temp.path().join("oauth-state.json"),
            bark_path: temp.path().join("bark-state.json"),
            connection_path,
            model_path: temp.path().join("group-models.json"),
            keepalive_path: temp.path().join("keepalive-state.json"),
            oauth: json!({}),
            bark: json!({"enabled": false}),
            bark_valid: true,
            base_url,
            verify_url: String::new(),
            read_only: false,
        });
        let upstream = Upstream::new(settings.clone()).expect("construct fixture upstream");
        let queries = Queries {
            db: db.clone(),
            oauth: OAuthState::new(temp.path().join("oauth-state.json"), false),
        };
        let actions = Actions::new(
            queries,
            upstream,
            AccountLocks::default(),
            Templates::new(temp.path().join("account-templates.json"), false),
            settings,
        )
        .expect("construct operation actions");
        let backend = Arc::new(FixtureBackend {
            now: Mutex::new(chrono::Utc::now()),
            pool: db.pool.clone(),
            cycle_db: db.clone(),
            cycle_reads: Mutex::new(Vec::new()),
            cycle_gate: Mutex::new(None),
            cycle_refresh_error: Mutex::new(None),
            recover_calls: Mutex::new(Vec::new()),
            recover_failures: Mutex::new(BTreeSet::new()),
            test_model: Mutex::new(
                fixture["connection_test"]["requested_model"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            ),
            test_concurrency: AtomicI64::new(2),
            retry_policy: Mutex::new(Default::default()),
        });
        let operations = Operations::new(
            actions.clone(),
            backend.clone(),
            Arc::new(Semaphore::new(8)),
        )
        .expect("construct durable operation queue");
        Self {
            _temp: temp,
            db,
            actions,
            backend,
            operations,
            mock,
            server: Some(server),
        }
    }

    async fn seed_accounts(&self, count: usize, mapping: Value) -> Vec<i64> {
        assert!(count > 0);
        let first = NEXT_ACCOUNT_ID.fetch_add(count as i64 + 17, Ordering::Relaxed);
        let group_id = first + 100_000_000;
        let model = fixtures()["connection_test"]["unrelated_allowlist_model"]
            .as_str()
            .unwrap()
            .to_owned();
        sqlx::query(
            "INSERT INTO groups (id,name,platform,model_allowlist) VALUES ($1,$2,'openai',$3)",
        )
        .bind(group_id)
        .bind(format!("fixture-group-{first}"))
        .bind(SqlJson(json!({"enabled": true, "models": [model]})))
        .execute(&self.db.pool)
        .await
        .expect("insert synthetic group");
        let credentials = json!({"model_mapping": mapping});
        sqlx::query(
            "INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra) \
             SELECT $1::bigint + n, 'fixture-account-' || ($1::bigint + n)::text, \
                    'openai','oauth','active',true,$2::jsonb,$3::jsonb \
             FROM generate_series(0,$4::integer-1) AS generated(n)",
        )
        .bind(first)
        .bind(SqlJson(credentials))
        .bind(SqlJson(json!({})))
        .bind(count as i32)
        .execute(&self.db.pool)
        .await
        .expect("insert synthetic accounts");
        sqlx::query(
            "INSERT INTO account_groups (account_id,group_id) \
             SELECT id,$2 FROM accounts WHERE id >= $1 AND id < $1 + $3",
        )
        .bind(first)
        .bind(group_id)
        .bind(count as i64)
        .execute(&self.db.pool)
        .await
        .expect("attach synthetic accounts to group");
        (0..count).map(|offset| first + offset as i64).collect()
    }

    async fn expected_versions(&self, ids: &[i64], action: &str) -> BTreeMap<i64, String> {
        self.actions
            .accounts(ids)
            .await
            .expect("read synthetic account rows")
            .into_iter()
            .map(|row| {
                let id = row["id"].as_i64().unwrap();
                let version = operation_versions(&row)[action]
                    .as_str()
                    .unwrap()
                    .to_owned();
                (id, version)
            })
            .collect()
    }

    async fn batch_request(
        &self,
        ids: &[i64],
        action: &str,
        payload: Value,
        request_id: &str,
    ) -> Value {
        let versions = self.expected_versions(ids, action).await;
        json!({
            "action": action,
            "payload": payload,
            "accounts": ids.iter().map(|id| json!({
                "account_id": id,
                "expected_version": versions[id]
            })).collect::<Vec<_>>(),
            "request_id": request_id,
            "client_id": CLIENT_ID
        })
    }

    async fn wait_test_calls(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if self.mock.test_calls().len() >= expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("native SSE request did not arrive");
    }

    async fn wait_status(&self, job_id: &str, expected: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let job = self.operations.get(job_id, 0).unwrap();
                if job["status"] == expected {
                    break job;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("operation did not reach the expected status")
    }

    async fn wait_batch_settled(&self, batch_id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                self.operations.tick().await.unwrap();
                let view = self.operations.listing(Some(batch_id)).unwrap();
                let settled = view["items"].as_array().unwrap().iter().all(|item| {
                    !matches!(
                        item["status"].as_str().unwrap_or(""),
                        "queued" | "running" | "checking"
                    )
                });
                if settled {
                    break view;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("operation batch did not settle")
    }

    async fn restart(&mut self) {
        self.operations.close().await;
        self.operations = Operations::new(
            self.actions.clone(),
            self.backend.clone(),
            Arc::new(Semaphore::new(8)),
        )
        .unwrap();
    }

    async fn finish(&mut self) {
        self.mock.release_all();
        self.operations.close().await;
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
        self.db.close().await;
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.mock.release_all();
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

fn items_by_account(view: &Value) -> BTreeMap<i64, Value> {
    view["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["account_id"].as_i64().unwrap(), item.clone()))
        .collect()
}

fn assert_summary(item: &Value, requested: &str, actual: Option<&str>, success: bool) {
    let result = &item["result"];
    assert_eq!(item["action"], "test");
    assert_eq!(result["requested_model"], requested);
    assert_eq!(result["actual_model"], json!(actual));
    assert_eq!(result["success"], success);
    assert!(result["duration_ms"].as_i64().is_some_and(|v| v >= 0));
    assert!(chrono::DateTime::parse_from_rfc3339(result["completed_at"].as_str().unwrap()).is_ok());
    assert!(result.as_object().unwrap().keys().all(|key| [
        "requested_model",
        "actual_model",
        "success",
        "duration_ms",
        "completed_at",
        "error",
        "error_code",
        "uncertain"
    ]
    .contains(&key.as_str())));
}

fn assert_native_calls(mock: &MockUpstream) {
    let calls = mock.calls.lock().unwrap();
    assert!(calls.iter().all(|call| call.authorized));
    assert!(calls.iter().all(|call| (call.method == Method::GET
        && call.path == "/api/v1/admin/groups/all")
        || (call.method == Method::POST && account_id_from_test_path(&call.path).is_some())));
    for call in calls.iter().filter(|call| call.method == Method::POST) {
        assert!(call.body.get("confirmed").is_none());
        assert!(call.body.get("expected_version").is_none());
    }
}

#[tokio::test]
async fn strict_fields_and_single_test_defaults_match_reference() {
    let mut harness = Harness::new().await;
    let id = harness.seed_accounts(1, json!({})).await[0];
    let expected = harness.expected_versions(&[id], "test").await[&id].clone();
    let mut invalid = json!({
        "action": "test",
        "request_id": "fixture-single-operation-contract-0001",
        "client_id": CLIENT_ID,
        "expected_version": expected,
        "payload": fixtures()["request_defaults"]["input_payload"]
    });
    invalid["unexpected_field"] = true.into();
    let error = harness
        .operations
        .submit(id, &invalid, ADMIN_KEY)
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 422);

    let valid = json!({
        "action": "test",
        "request_id": "fixture-single-operation-contract-0002",
        "client_id": CLIENT_ID,
        "expected_version": harness.expected_versions(&[id], "test").await[&id],
        "payload": fixtures()["request_defaults"]["input_payload"]
    });
    let accepted = harness
        .operations
        .submit(id, &valid, ADMIN_KEY)
        .await
        .expect("valid single test request");
    assert_eq!(accepted["status"], "queued");
    let mut requested = fixtures()["request_defaults"]["public_requested"].clone();
    requested["model_id"] = fixtures()["connection_test"]["requested_model"].clone();
    assert_eq!(accepted["requested"], requested);
    assert!(harness.mock.test_calls().is_empty());

    let mut invalid_batch = harness
        .batch_request(
            &[id],
            "test",
            json!({"confirmed": true}),
            "fixture-strict-batch-contract-0001",
        )
        .await;
    invalid_batch["unexpected_field"] = true.into();
    let error = harness
        .operations
        .submit_batch(&invalid_batch, ADMIN_KEY, false)
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 422);
    invalid_batch
        .as_object_mut()
        .unwrap()
        .remove("unexpected_field");
    let before = harness.operations.store.read().unwrap();
    for payload in [
        json!({}),
        json!({"confirmed":"true"}),
        json!({"confirmed":1}),
        json!({"confirmed":false}),
        json!({"confirmed":true,"model_id":"fixture-untrusted-model"}),
    ] {
        invalid_batch["payload"] = payload;
        assert_eq!(
            harness
                .operations
                .submit_batch(&invalid_batch, ADMIN_KEY, false)
                .await
                .unwrap_err()
                .status
                .as_u16(),
            422
        );
        assert_eq!(harness.operations.store.read().unwrap(), before);
    }
    assert!(
        sub2ops_server::validation::operation_payload("test", &json!({"confirmed": "true"}))
            .is_err()
    );
    assert!(sub2ops_server::validation::operation_payload(
        "test",
        &json!({"confirmed": true, "unknown": true})
    )
    .is_err());
    harness.finish().await;
}

#[tokio::test]
async fn batch_acceptance_is_idempotent_conflicts_are_rejected_and_dispatch_is_deferred() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(2, json!({})).await;
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed": true}),
            "fixture-batch-idempotency-contract-0001",
        )
        .await;
    let (first, replay) = tokio::join!(
        harness.operations.submit_batch(&request, ADMIN_KEY, false),
        harness.operations.submit_batch(&request, ADMIN_KEY, false)
    );
    let first = first.expect("accept complete batch");
    let replay = replay.expect("idempotent concurrent replay");
    let mut reversed = request.clone();
    reversed["accounts"].as_array_mut().unwrap().reverse();
    let reordered = harness
        .operations
        .submit_batch(&reversed, ADMIN_KEY, false)
        .await
        .unwrap();
    assert_eq!(first["batch_id"], reordered["batch_id"]);
    assert_eq!(first["batch_id"], replay["batch_id"]);
    assert_eq!(first["pending"], 2);
    assert_eq!(
        harness.operations.store.read().unwrap()["jobs"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert!(harness.mock.test_calls().is_empty());
    let accepted_state = harness.operations.store.read().unwrap();
    assert_eq!(accepted_state["batches"].as_object().unwrap().len(), 1);
    assert_eq!(
        accepted_state["batch_requests"].as_object().unwrap().len(),
        1
    );
    assert!(accepted_state["jobs"]
        .as_object()
        .unwrap()
        .values()
        .all(|job| job["batch_id"] == first["batch_id"]));
    assert_eq!(
        fs::metadata(harness.operations.store.0.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    for item in first["items"].as_array().unwrap() {
        assert_eq!(
            item["requested"],
            json!({"confirmed":true,"model_id":fixtures()["connection_test"]["requested_model"],"mode":"default"})
        );
    }
    let before = fs::read(harness.operations.store.0.path()).unwrap();
    let original = harness.operations.store.clone();
    let failed_lock = harness._temp.path().join("acceptance-failed-lock");
    fs::create_dir(&failed_lock).unwrap();
    Arc::get_mut(&mut harness.operations).unwrap().store = OperationStore(Store::with_lock(
        original.0.path().to_path_buf(),
        failed_lock,
        json!({"version":1,"jobs":{},"requests":{},"batches":{},"batch_requests":{}}),
        sub2ops_server::operations::validate_store,
        false,
    ));
    let extra = harness.seed_accounts(2, json!({})).await;
    let failed_request = harness
        .batch_request(
            &extra,
            "test",
            json!({"confirmed":true}),
            "fixture-acceptance-save-failure-0001",
        )
        .await;
    assert!(harness
        .operations
        .submit_batch(&failed_request, ADMIN_KEY, false)
        .await
        .is_err());
    assert_eq!(fs::read(original.0.path()).unwrap(), before);
    assert_eq!(harness.operations.keys.lock().unwrap().len(), 2);
    assert!(harness.mock.test_calls().is_empty());
    Arc::get_mut(&mut harness.operations).unwrap().store = original;

    let mut changed = request.clone();
    changed["accounts"].as_array_mut().unwrap().pop();
    let error = harness
        .operations
        .submit_batch(&changed, ADMIN_KEY, false)
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 409);
    for field in ["client_id", "expected_version"] {
        let mut changed = request.clone();
        if field == "client_id" {
            changed[field] = "fixture-other-client".into();
        } else {
            changed["accounts"][0][field] = "f".repeat(64).into();
        }
        assert_eq!(
            harness
                .operations
                .submit_batch(&changed, ADMIN_KEY, false)
                .await
                .unwrap_err()
                .status
                .as_u16(),
            409
        );
    }

    let single = json!({
        "action": "test",
        "request_id": request["request_id"],
        "client_id": CLIENT_ID,
        "expected_version": harness.expected_versions(&[ids[0]], "test").await[&ids[0]],
        "payload": {"confirmed": true}
    });
    let error = harness
        .operations
        .submit(ids[0], &single, ADMIN_KEY)
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 409);

    let duplicate = harness
        .batch_request(
            &[ids[0], ids[0]],
            "test",
            json!({"confirmed": true}),
            "fixture-duplicate-account-contract-0001",
        )
        .await;
    let error = harness
        .operations
        .submit_batch(&duplicate, ADMIN_KEY, false)
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 422);
    assert_eq!(harness.mock.test_calls().len(), 0);
    let single_request = json!({"action":"test","request_id":"fixture-single-first-conflict-0001","client_id":CLIENT_ID,
        "expected_version":harness.expected_versions(&[extra[0]],"test").await[&extra[0]],"payload":{"confirmed":true}});
    harness
        .operations
        .submit(extra[0], &single_request, ADMIN_KEY)
        .await
        .unwrap();
    let mut single_conflict = failed_request;
    single_conflict["request_id"] = single_request["request_id"].clone();
    assert_eq!(
        harness
            .operations
            .submit_batch(&single_conflict, ADMIN_KEY, false)
            .await
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    assert_eq!(
        harness.operations.store.read().unwrap()["jobs"]
            .as_object()
            .unwrap()
            .len(),
        3
    );
    harness.finish().await;
}

#[tokio::test]
async fn one_thousand_account_batch_boundary_and_listing_are_complete() {
    let mut harness = Harness::new().await;
    let max = fixtures()["batch_limit"]["maximum_accounts"]
        .as_u64()
        .unwrap() as usize;
    let ids = harness.seed_accounts(max, json!({})).await;
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed": true}),
            "fixture-thousand-batch-contract-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .expect("accept batch at configured limit");
    assert_eq!(accepted["items"].as_array().unwrap().len(), max);
    assert_eq!(accepted["pending"], max);
    assert_eq!(
        harness.operations.store.read().unwrap()["jobs"]
            .as_object()
            .unwrap()
            .len(),
        max
    );

    let over_limit = json!({
        "action": "test",
        "payload": {"confirmed": true},
        "request_id": "fixture-over-limit-batch-contract-0001",
        "client_id": CLIENT_ID,
        "accounts": (0..=max).map(|offset| json!({
            "account_id": 900_000_000 + offset as i64,
            "expected_version": "a".repeat(64)
        })).collect::<Vec<_>>()
    });
    let error = harness
        .operations
        .submit_batch(&over_limit, ADMIN_KEY, false)
        .await
        .unwrap_err();
    assert_eq!(
        error.status.as_u16(),
        fixtures()["batch_limit"]["over_limit_status"]
            .as_u64()
            .unwrap() as u16
    );
    for accounts in [
        json!([]),
        json!([{"account_id":ids[0].to_string(),"expected_version":"a".repeat(64)}]),
    ] {
        let mut invalid = request.clone();
        invalid["accounts"] = accounts;
        assert_eq!(
            harness
                .operations
                .submit_batch(&invalid, ADMIN_KEY, false)
                .await
                .unwrap_err()
                .status
                .as_u16(),
            422
        );
    }
    let cancelled = harness
        .operations
        .cancel_batch(accepted["batch_id"].as_str().unwrap())
        .await
        .expect("cancel undispatched limit batch");
    assert_eq!(cancelled["pending"], 0);
    assert_eq!(cancelled["items"].as_array().unwrap().len(), max);
    assert!(cancelled["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["status"] == "cancelled"));
    assert!(harness.mock.test_calls().is_empty());
    harness.finish().await;
}

#[tokio::test]
async fn absent_allowlisted_model_does_not_block_batch_and_reports_native_mapping() {
    let mut harness = Harness::new().await;
    let fixture = fixtures()["connection_test"].clone();
    let requested = fixture["requested_model"].as_str().unwrap();
    let actual = fixture["actual_model"].as_str().unwrap();
    let unrelated = fixture["unrelated_allowlist_model"].as_str().unwrap();
    assert_ne!(requested, unrelated);
    let ids = harness.seed_accounts(1, json!({requested: actual})).await;
    let before_rows = harness.actions.accounts(&ids).await.unwrap();
    let before_groups: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(g) FROM groups g WHERE id=$1")
            .bind(ids[0] + 100_000_000)
            .fetch_all(&harness.db.pool)
            .await
            .unwrap();
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed": true}),
            "fixture-mapped-model-contract-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .expect("accept model test despite unrelated allowlist");
    assert_eq!(accepted["items"][0]["requested"]["model_id"], requested);
    assert_eq!(accepted["items"][0]["requested"]["mode"], "default");
    assert!(harness.mock.test_calls().is_empty());

    harness.operations.tick().await.unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    let item = &finished["items"][0];
    assert_eq!(item["status"], "completed");
    assert_eq!(item["result"]["success"], true);
    assert_eq!(item["result"]["requested_model"], requested);
    assert_eq!(item["result"]["actual_model"], actual);
    assert_summary(item, requested, Some(actual), true);
    assert_native_calls(&harness.mock);
    assert_eq!(harness.actions.accounts(&ids).await.unwrap(), before_rows);
    let after_groups: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(g) FROM groups g WHERE id=$1")
            .bind(ids[0] + 100_000_000)
            .fetch_all(&harness.db.pool)
            .await
            .unwrap();
    assert_eq!(after_groups, before_groups);
    let detail = harness
        .operations
        .get(item["id"].as_str().unwrap(), 0)
        .unwrap();
    assert!(detail["events"].as_array().unwrap().is_empty());
    let calls = harness.mock.test_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["model_id"], requested);
    assert_eq!(calls[0].body["mode"], "default");
    assert!(calls[0].body.get("confirmed").is_none());
    assert!(calls[0].body.get("expected_version").is_none());
    harness.operations.tick().await.unwrap();
    assert_eq!(harness.mock.test_calls().len(), 1);
    harness.finish().await;
}

#[tokio::test]
async fn truncated_missing_terminal_and_explicit_failure_never_report_success() {
    let mut harness = Harness::new().await;
    harness.backend.test_concurrency.store(3, Ordering::Relaxed);
    let ids = harness.seed_accounts(3, json!({})).await;
    let fixture = fixtures()["connection_test"]["sse"].clone();
    harness.mock.set_stream(ids[0], StreamScenario::Truncated);
    harness.mock.set_stream(ids[1], StreamScenario::NoTerminal);
    harness
        .mock
        .set_stream(ids[2], StreamScenario::ExplicitFailure);
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed": true}),
            "fixture-sse-boundary-contract-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .expect("accept SSE boundary batch");
    harness.operations.tick().await.unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    let items = items_by_account(&finished);
    assert_eq!(items[&ids[0]]["status"], fixture["truncated_status"]);
    assert_eq!(items[&ids[0]]["result"]["success"], false);
    assert_eq!(items[&ids[0]]["result"]["uncertain"], true);
    assert_eq!(items[&ids[1]]["status"], fixture["missing_terminal_status"]);
    assert_eq!(items[&ids[1]]["result"]["success"], false);
    assert_eq!(items[&ids[1]]["result"]["uncertain"], true);
    assert_eq!(
        items[&ids[2]]["result"]["success"],
        fixture["explicit_failure_success"]
    );
    assert_ne!(items[&ids[2]]["result"]["success"], true);
    assert!(items[&ids[2]]["result"]["error"]
        .as_str()
        .is_some_and(|v| !v.is_empty()));
    harness.operations.tick().await.unwrap();
    assert_eq!(harness.mock.test_calls().len(), 3);
    harness.finish().await;
}

#[tokio::test]
async fn a_busy_account_does_not_block_other_items_and_account_lock_is_respected() {
    let mut harness = Harness::new().await;
    harness.backend.test_concurrency.store(2, Ordering::Relaxed);
    let ids = harness.seed_accounts(2, json!({})).await;
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed": true}),
            "fixture-busy-account-contract-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let busy_id = ids[0];
    let other_id = ids[1];
    let busy_lease = harness.actions.locks.account(busy_id).await;
    let _other_gate = harness.mock.gate_test(other_id);
    harness.operations.tick().await.unwrap();
    harness.wait_test_calls(1).await;
    let running = harness
        .operations
        .listing(Some(accepted["batch_id"].as_str().unwrap()))
        .unwrap();
    let items = items_by_account(&running);
    assert_eq!(items[&busy_id]["status"], "queued");
    assert_eq!(items[&busy_id]["reason"], "等待账号空闲");
    assert_eq!(items[&other_id]["status"], "running");
    harness.mock.release_test(other_id);
    let settled_other = harness
        .wait_status(items[&other_id]["id"].as_str().unwrap(), "completed")
        .await;
    assert_eq!(settled_other["status"], "completed");
    assert_eq!(
        harness
            .operations
            .get(items[&busy_id]["id"].as_str().unwrap(), 0)
            .unwrap()["status"],
        "queued"
    );
    drop(busy_lease);
    harness.operations.tick().await.unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    assert!(items_by_account(&finished)
        .values()
        .all(|item| item["status"] == "completed"));
    assert_eq!(harness.mock.test_calls().len(), 2);
    harness.finish().await;
}

#[tokio::test]
async fn shared_parent_credential_lock_blocks_child_without_blocking_independent_account() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(3, json!({})).await;
    let parent_id = ids[0];
    let child_id = ids[1];
    let independent_id = ids[2];
    sqlx::query(
        "UPDATE accounts SET parent_account_id=$1, extra=jsonb_build_object('parent_account_id',$1) WHERE id=$2",
    )
    .bind(parent_id)
    .bind(child_id)
    .execute(&harness.db.pool)
    .await
    .unwrap();
    let batch_ids = [child_id, independent_id];
    let request = harness
        .batch_request(
            &batch_ids,
            "test",
            json!({"confirmed": true}),
            "fixture-shared-credential-contract-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let parent_lease = harness.actions.locks.account(parent_id).await;
    let _independent_gate = harness.mock.gate_test(independent_id);
    harness.operations.tick().await.unwrap();
    harness.wait_test_calls(1).await;
    let listing = harness
        .operations
        .listing(Some(accepted["batch_id"].as_str().unwrap()))
        .unwrap();
    let items = items_by_account(&listing);
    assert_eq!(items[&child_id]["status"], "queued");
    assert_eq!(items[&independent_id]["status"], "running");
    assert_eq!(
        harness.mock.test_calls()[0].path,
        format!("/api/v1/admin/accounts/{independent_id}/test")
    );
    harness.mock.release_test(independent_id);
    let _ = harness
        .wait_status(items[&independent_id]["id"].as_str().unwrap(), "completed")
        .await;
    assert_eq!(
        harness
            .operations
            .get(items[&child_id]["id"].as_str().unwrap(), 0)
            .unwrap()["status"],
        "queued"
    );
    drop(parent_lease);
    harness.operations.tick().await.unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(
        items_by_account(&finished)[&child_id]["status"],
        "completed"
    );
    assert_eq!(harness.mock.test_calls().len(), 2);
    harness.finish().await;
}

#[tokio::test]
async fn cancelling_batch_stops_only_undispatched_items() {
    let mut harness = Harness::new().await;
    harness.backend.test_concurrency.store(1, Ordering::Relaxed);
    let ids = harness.seed_accounts(3, json!({})).await;
    let mut request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed": true}),
            "fixture-cancel-undispatched-contract-0001",
        )
        .await;
    request["accounts"][2]["expected_version"] = "f".repeat(64).into();
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    for id in &ids {
        let _ = harness.mock.gate_test(*id);
    }
    harness.operations.tick().await.unwrap();
    harness.wait_test_calls(1).await;
    let before = harness
        .operations
        .listing(Some(accepted["batch_id"].as_str().unwrap()))
        .unwrap();
    let before_items = items_by_account(&before);
    assert_eq!(
        before_items
            .values()
            .filter(|item| item["status"] == "running")
            .count(),
        1
    );
    let stopped = harness
        .operations
        .cancel_batch(accepted["batch_id"].as_str().unwrap())
        .await
        .unwrap();
    let stopped_items = items_by_account(&stopped);
    assert_eq!(
        stopped_items
            .values()
            .filter(|item| item["status"] == "running")
            .count(),
        1
    );
    assert_eq!(
        stopped_items
            .values()
            .filter(|item| item["status"] == "cancelled")
            .count(),
        2
    );
    assert_eq!(stopped["pending"], 1);
    harness.mock.release_all();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(
        items_by_account(&finished)
            .values()
            .filter(|item| item["status"] == "completed")
            .count(),
        1
    );
    assert_eq!(harness.mock.test_calls().len(), 1);
    harness.finish().await;
}

#[tokio::test]
async fn running_job_restarts_as_checking_and_never_retests_automatically() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(1, json!({})).await;
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed": true}),
            "fixture-restart-checking-contract-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let job_id = accepted["items"][0]["id"].as_str().unwrap().to_owned();
    harness
        .operations
        .update(
            &job_id,
            json!({"status": "running", "started_at": "fixture-dispatched"}),
        )
        .unwrap();
    harness.operations.close().await;
    let restarted = Operations::new(
        harness.actions.clone(),
        harness.backend.clone(),
        Arc::new(Semaphore::new(8)),
    )
    .unwrap();
    assert_eq!(
        restarted.store.read().unwrap()["jobs"][&job_id]["status"],
        "checking"
    );
    restarted.tick().await.unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let job = restarted.get(&job_id, 0).unwrap();
            if job["status"] == "needs_confirmation" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("checking job should stop for confirmation");
    assert!(harness.mock.test_calls().is_empty());
    assert_eq!(
        fixtures()["native_bulk"]["unknown_result_replays"]
            .as_u64()
            .unwrap(),
        0
    );
    restarted.close().await;
    harness.finish().await;
}

#[tokio::test]
async fn dispatch_state_save_failure_never_sends_the_native_test_request() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(1, json!({})).await;
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed": true}),
            "fixture-dispatch-save-failure-contract-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let job_id = accepted["items"][0]["id"].as_str().unwrap().to_owned();

    let gate = Arc::new(Notify::new());
    harness.mock.set_auth_gate(gate.clone());
    let store_path = harness.operations.store.0.path().to_path_buf();
    let lock_path = harness._temp.path().join("blocked-operation-lock");
    fs::create_dir(&lock_path).expect("make the operation state lock path unwritable");
    Arc::get_mut(&mut harness.operations)
        .expect("no operation task is running")
        .store = OperationStore(Store::with_lock(
        store_path,
        lock_path,
        json!({"version": 1, "jobs": {}, "requests": {}, "batches": {}, "batch_requests": {}}),
        sub2ops_server::operations::validate_store,
        false,
    ));

    harness.operations.tick().await.unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if harness.mock.count(Method::GET, "/api/v1/admin/groups/all") > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dispatch should reach the fixture authentication barrier");
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(8), harness.operations.close())
        .await
        .expect("failed state save should return without dispatch");
    assert!(harness.operations.store.transaction(|_| Ok(())).is_err());
    assert_eq!(
        harness.operations.store.read().unwrap()["jobs"][&job_id]["status"],
        "queued"
    );
    assert!(harness.mock.test_calls().is_empty());
    harness.finish().await;
}

#[tokio::test]
async fn native_bulk_result_partitions_success_and_failure_after_readback() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(2, json!({})).await;
    let proxy_id = ids[0] + 200_000_000;
    sqlx::query("INSERT INTO proxies (id,protocol,host,port) VALUES ($1,'http','127.0.0.1',9)")
        .bind(proxy_id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    harness.mock.set_proxy_id(proxy_id);
    harness.mock.set_bulk(BulkScenario::Partition {
        success_ids: BTreeSet::from([ids[0]]),
    });
    let request = harness
        .batch_request(
            &ids,
            "proxy",
            json!({"proxy_id": proxy_id}),
            "fixture-native-partition-contract-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .expect("accept native proxy batch");
    assert_eq!(harness.mock.bulk_calls().len(), 0);
    harness.operations.tick().await.unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    let items = items_by_account(&finished);
    assert_eq!(
        items[&ids[0]]["status"],
        fixtures()["native_bulk"]["partitioned_success_status"]
    );
    assert_eq!(items[&ids[0]]["result"]["proxy_id"], proxy_id);
    assert_eq!(
        items[&ids[1]]["status"],
        fixtures()["native_bulk"]["partitioned_failure_status"]
    );
    assert_eq!(harness.mock.bulk_calls().len(), 1);
    let stored = harness.operations.store.read().unwrap();
    let chunk = &stored["batches"][accepted["batch_id"].as_str().unwrap()]["chunks"][0];
    assert_eq!(chunk["status"], "checked");
    assert_eq!(chunk["response_valid"], true);
    harness.finish().await;
}

#[tokio::test]
async fn unknown_native_bulk_result_is_not_replayed() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(1, json!({})).await;
    let proxy_id = ids[0] + 200_000_000;
    sqlx::query("INSERT INTO proxies (id,protocol,host,port) VALUES ($1,'http','127.0.0.1',9)")
        .bind(proxy_id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    harness.mock.set_proxy_id(proxy_id);
    harness.mock.set_bulk(BulkScenario::Unknown);
    let request = harness
        .batch_request(
            &ids,
            "proxy",
            json!({"proxy_id": proxy_id}),
            "fixture-native-unknown-contract-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    harness.operations.tick().await.unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(
        finished["items"][0]["status"],
        fixtures()["native_bulk"]["unknown_result_status"]
    );
    harness.operations.tick().await.unwrap();
    assert_eq!(
        harness.mock.bulk_calls().len(),
        1 + fixtures()["native_bulk"]["unknown_result_replays"]
            .as_u64()
            .unwrap() as usize
    );
    let stored = harness.operations.store.read().unwrap();
    let chunk = &stored["batches"][accepted["batch_id"].as_str().unwrap()]["chunks"][0];
    assert_eq!(chunk["status"], "needs_confirmation");
    assert_eq!(chunk["response_valid"], false);
    harness.finish().await;
}

#[tokio::test]
async fn connection_model_is_frozen_per_batch_and_new_batches_use_current_configuration() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(2, json!({})).await;
    *harness.backend.test_model.lock().unwrap() = "fixture-accepted-model".into();
    let request = harness
        .batch_request(
            &ids[..1],
            "test",
            json!({"confirmed":true}),
            "fixture-frozen-model-0001",
        )
        .await;
    let first = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    *harness.backend.test_model.lock().unwrap() = "fixture-later-model".into();
    let replay = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    assert_eq!(first, replay);
    let final_first = harness
        .wait_batch_settled(first["batch_id"].as_str().unwrap())
        .await;
    assert_summary(
        &final_first["items"][0],
        "fixture-accepted-model",
        Some("fixture-accepted-model"),
        true,
    );
    let request = harness
        .batch_request(
            &ids[1..],
            "test",
            json!({"confirmed":true}),
            "fixture-frozen-model-0002",
        )
        .await;
    let second = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let final_second = harness
        .wait_batch_settled(second["batch_id"].as_str().unwrap())
        .await;
    assert_summary(
        &final_second["items"][0],
        "fixture-later-model",
        Some("fixture-later-model"),
        true,
    );
    assert_eq!(
        harness
            .mock
            .test_calls()
            .iter()
            .map(|call| call.body["model_id"].clone())
            .collect::<Vec<_>>(),
        vec![
            json!("fixture-accepted-model"),
            json!("fixture-later-model")
        ]
    );
    assert_native_calls(&harness.mock);
    harness.finish().await;
}

#[tokio::test]
async fn connection_mixed_platforms_missing_deleted_unsupported_and_stale_items_are_isolated() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(8, json!({})).await;
    sqlx::query("UPDATE accounts SET platform='grok',type='apikey' WHERE id=$1")
        .bind(ids[1])
        .execute(&harness.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET platform='anthropic' WHERE id=$1")
        .bind(ids[2])
        .execute(&harness.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET type='fixture-unsupported' WHERE id=$1")
        .bind(ids[4])
        .execute(&harness.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET type='apikey' WHERE id=$1")
        .bind(ids[6])
        .execute(&harness.db.pool)
        .await
        .unwrap();
    let mut request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed":true}),
            "fixture-mixed-platforms-0001",
        )
        .await;
    sqlx::query("UPDATE accounts SET deleted_at=now() WHERE id=$1")
        .bind(ids[3])
        .execute(&harness.db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM account_groups WHERE account_id=$1")
        .bind(ids[7])
        .execute(&harness.db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM accounts WHERE id=$1")
        .bind(ids[7])
        .execute(&harness.db.pool)
        .await
        .unwrap();
    request["accounts"][5]["expected_version"] = "f".repeat(64).into();
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    let items = items_by_account(&finished);
    for index in [0, 1, 6] {
        assert_eq!(items[&ids[index]]["status"], "completed");
    }
    for index in [2, 3, 4, 7] {
        assert_eq!(items[&ids[index]]["status"], "failed");
    }
    assert_eq!(items[&ids[5]]["status"], "needs_confirmation");
    assert_eq!(finished["pending"], 1);
    assert_eq!(
        items[&ids[1]]["requested"],
        json!({"confirmed":true,"mode":"text","model_id":""})
    );
    assert_summary(&items[&ids[1]], "", Some("fixture-grok-native-model"), true);
    let calls = harness.mock.test_calls();
    let called: BTreeSet<i64> = calls
        .iter()
        .map(|call| account_id_from_test_path(&call.path).unwrap())
        .collect();
    assert_eq!(called, BTreeSet::from([ids[0], ids[1], ids[6]]));
    assert_eq!(calls.len(), 3);
    let grok = calls
        .iter()
        .find(|call| account_id_from_test_path(&call.path) == Some(ids[1]))
        .unwrap();
    assert_eq!(grok.body["model_id"], "");
    assert_eq!(grok.body["mode"], "text");
    assert_native_calls(&harness.mock);
    harness.finish().await;
}

#[tokio::test]
async fn connection_dispatch_detects_credential_changes_but_ignores_passive_observations() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(2, json!({})).await;
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed":true}),
            "fixture-version-change-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET credentials=credentials||'{\"access_token\":\"fixture-replaced-credential\"}'::jsonb WHERE id=$1").bind(ids[0]).execute(&harness.db.pool).await.unwrap();
    sqlx::query("UPDATE accounts SET extra=extra||'{\"passive_usage\":42}'::jsonb,updated_at=now() WHERE id=$1").bind(ids[1]).execute(&harness.db.pool).await.unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    let items = items_by_account(&finished);
    assert_eq!(items[&ids[0]]["status"], "needs_confirmation");
    assert_eq!(
        items[&ids[0]]["requested"]["model_id"],
        fixtures()["connection_test"]["requested_model"]
    );
    assert_eq!(items[&ids[1]]["status"], "completed");
    let calls = harness.mock.test_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(account_id_from_test_path(&calls[0].path), Some(ids[1]));
    harness.finish().await;
}

#[tokio::test]
async fn connection_concurrency_defaults_to_two_and_clamps_to_one_or_eight() {
    for (configured, expected) in [(None, 2), (Some(0), 1), (Some(99), 8)] {
        let mut harness = Harness::new().await;
        if let Some(value) = configured {
            harness
                .backend
                .test_concurrency
                .store(value, Ordering::Relaxed);
        }
        let ids = harness.seed_accounts(expected + 2, json!({})).await;
        for id in &ids {
            harness.mock.gate_test(*id);
        }
        let request = harness
            .batch_request(
                &ids,
                "test",
                json!({"confirmed":true}),
                "fixture-concurrency-limits-0001",
            )
            .await;
        let accepted = harness
            .operations
            .submit_batch(&request, ADMIN_KEY, false)
            .await
            .unwrap();
        harness.operations.tick().await.unwrap();
        harness.wait_test_calls(expected).await;
        for _ in 0..3 {
            harness.operations.tick().await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(harness.mock.test_calls().len(), expected);
        assert_eq!(harness.mock.active.lock().unwrap().len(), expected);
        let waiting = harness
            .operations
            .listing(Some(accepted["batch_id"].as_str().unwrap()))
            .unwrap();
        assert!(waiting["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|job| job["status"] == "queued")
            .all(|job| job["reason"] == "等待测试执行容量"));
        harness.mock.release_all();
        let finished = harness
            .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
            .await;
        assert!(finished["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["status"] == "completed"));
        assert_eq!(harness.mock.test_calls().len(), ids.len());
        assert!(harness
            .mock
            .active_snapshots
            .lock()
            .unwrap()
            .iter()
            .all(|snapshot| snapshot.len() <= expected));
        harness.finish().await;
    }
}

#[tokio::test]
async fn connection_batch_shares_eight_global_slots_with_existing_single_operations() {
    let mut harness = Harness::new().await;
    harness.backend.test_concurrency.store(8, Ordering::Relaxed);
    let ids = harness.seed_accounts(9, json!({})).await;
    for id in &ids {
        harness.mock.gate_test(*id);
    }
    let versions = harness.expected_versions(&ids, "test").await;
    let mut singles = Vec::new();
    for id in &ids[..7] {
        let request = json!({"action":"test","request_id":format!("fixture-global-single-{id}"),"client_id":CLIENT_ID,
            "expected_version":versions[id],"payload":{"confirmed":true,"model_id":"fixture-single-model"}});
        singles.push(
            harness
                .operations
                .submit(*id, &request, ADMIN_KEY)
                .await
                .unwrap(),
        );
    }
    harness.operations.tick().await.unwrap();
    harness.wait_test_calls(7).await;
    let request = harness
        .batch_request(
            &ids[7..],
            "test",
            json!({"confirmed":true}),
            "fixture-global-batch-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    harness.operations.tick().await.unwrap();
    harness.wait_test_calls(8).await;
    for _ in 0..3 {
        harness.operations.tick().await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(harness.mock.test_calls().len(), 8);
    assert_eq!(harness.mock.active.lock().unwrap().len(), 8);
    harness.mock.release_all();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(finished["pending"], 0);
    for single in &singles {
        assert_eq!(
            harness
                .wait_status(single["id"].as_str().unwrap(), "completed")
                .await["result"]["success"],
            true
        );
    }
    assert_eq!(harness.mock.test_calls().len(), 9);
    assert!(harness
        .mock
        .active_snapshots
        .lock()
        .unwrap()
        .iter()
        .all(|snapshot| snapshot.len() <= 8));
    harness.finish().await;
}

#[tokio::test]
async fn connection_parent_siblings_and_duplicate_account_never_overlap() {
    let mut harness = Harness::new().await;
    harness.backend.test_concurrency.store(8, Ordering::Relaxed);
    let ids = harness.seed_accounts(4, json!({})).await;
    sqlx::query("UPDATE accounts SET parent_account_id=$1,extra=jsonb_build_object('parent_account_id',$1) WHERE id=ANY($2)")
        .bind(ids[0]).bind(&ids[1..3]).execute(&harness.db.pool).await.unwrap();
    for id in [ids[1], ids[3]] {
        harness.mock.gate_test(id);
    }
    let batch_ids = [ids[1], ids[0], ids[2], ids[3]];
    let first_request = harness
        .batch_request(
            &batch_ids,
            "test",
            json!({"confirmed":true}),
            "fixture-parent-siblings-0001",
        )
        .await;
    let first = harness
        .operations
        .submit_batch(&first_request, ADMIN_KEY, false)
        .await
        .unwrap();
    let second_request = harness
        .batch_request(
            &ids[3..],
            "test",
            json!({"confirmed":true}),
            "fixture-parent-siblings-0002",
        )
        .await;
    let second = harness
        .operations
        .submit_batch(&second_request, ADMIN_KEY, false)
        .await
        .unwrap();
    harness.operations.tick().await.unwrap();
    // Only the first child and the independent account may occupy workers,
    // even before either worker reaches authentication or the request lock.
    assert_eq!(harness.operations.budget.available_permits(), 6);
    harness.operations.tick().await.unwrap();
    assert_eq!(harness.operations.budget.available_permits(), 6);
    harness.wait_test_calls(2).await;
    for _ in 0..3 {
        harness.operations.tick().await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        harness
            .mock
            .test_calls()
            .iter()
            .map(|call| account_id_from_test_path(&call.path).unwrap())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([ids[1], ids[3]])
    );
    assert_eq!(harness.mock.test_calls().len(), 2);
    harness.mock.release_all();
    // The repeated independent account has a second request, so remove its
    // per-request gate once the first one has been released.
    harness.mock.test_gates.lock().unwrap().remove(&ids[3]);
    let first = harness
        .wait_batch_settled(first["batch_id"].as_str().unwrap())
        .await;
    let second = harness
        .wait_batch_settled(second["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(first["pending"], 0);
    assert_eq!(second["pending"], 0);
    assert_eq!(
        harness
            .mock
            .test_calls()
            .iter()
            .filter(|call| account_id_from_test_path(&call.path) == Some(ids[3]))
            .count(),
        2
    );
    for snapshot in harness.mock.active_snapshots.lock().unwrap().iter() {
        let mut resources = BTreeSet::new();
        for id in snapshot {
            assert!(resources.insert(*id), "same account overlapped");
            if [ids[1], ids[2]].contains(id) {
                assert!(
                    resources.insert(ids[0]),
                    "shared parent credential overlapped"
                );
            }
        }
    }
    harness.finish().await;
}

#[tokio::test]
async fn connection_restart_resumes_queued_peer_once_and_retains_unknown_dispatched_result() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(2, json!({})).await;
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed":true}),
            "fixture-live-restart-batch-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let lease = harness.actions.locks.account(ids[0]).await;
    harness.mock.gate_test(ids[1]);
    harness.operations.tick().await.unwrap();
    harness.wait_test_calls(1).await;
    assert_eq!(
        account_id_from_test_path(&harness.mock.test_calls()[0].path),
        Some(ids[1])
    );
    harness.restart().await;
    harness.mock.release_all();
    drop(lease);
    let replay = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    assert_eq!(replay["batch_id"], accepted["batch_id"]);
    assert_eq!(
        replay["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].clone())
            .collect::<Vec<_>>(),
        accepted["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].clone())
            .collect::<Vec<_>>()
    );
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    let items = items_by_account(&finished);
    assert_eq!(items[&ids[0]]["status"], "completed");
    assert_eq!(items[&ids[1]]["status"], "needs_confirmation");
    assert_eq!(
        harness
            .mock
            .test_calls()
            .iter()
            .map(|call| account_id_from_test_path(&call.path).unwrap())
            .collect::<Vec<_>>(),
        vec![ids[1], ids[0]]
    );
    let cancelled = harness
        .operations
        .cancel_batch(accepted["batch_id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        items_by_account(&cancelled)[&ids[1]]["status"],
        "needs_confirmation"
    );
    harness.operations.tick().await.unwrap();
    assert_eq!(harness.mock.test_calls().len(), 2);
    harness.finish().await;
}

#[tokio::test]
async fn connection_eof_truncation_and_transport_disconnect_remain_unknown_after_restart() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(3, json!({})).await;
    for (id, scenario) in ids.iter().zip([
        StreamScenario::NoTerminal,
        StreamScenario::Truncated,
        StreamScenario::Disconnected,
    ]) {
        harness.mock.set_stream(*id, scenario);
    }
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed":true}),
            "fixture-unknown-streams-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    for item in finished["items"].as_array().unwrap() {
        assert_eq!(item["status"], "needs_confirmation");
        assert_eq!(item["result"]["success"], false);
        assert_eq!(item["result"]["uncertain"], true);
        assert!(item["result"]["error"]
            .as_str()
            .is_some_and(|v| !v.is_empty()));
    }
    assert_eq!(harness.mock.test_calls().len(), 3);
    harness.restart().await;
    let retained = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(retained, finished);
    assert_eq!(retained["pending"], 3);
    assert_eq!(harness.mock.test_calls().len(), 3);
    harness.finish().await;
}

#[tokio::test]
async fn connection_only_native_success_terminal_passes_and_actual_model_is_never_invented() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(4, json!({})).await;
    for (id, scenario) in ids.iter().zip([
        StreamScenario::ExplicitFailure,
        StreamScenario::NativeError,
        StreamScenario::NoModel,
        StreamScenario::HttpError,
    ]) {
        harness.mock.set_stream(*id, scenario);
    }
    let request = harness
        .batch_request(
            &ids,
            "test",
            json!({"confirmed":true}),
            "fixture-terminal-semantics-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    let items = items_by_account(&finished);
    for index in [0, 1, 3] {
        assert_eq!(items[&ids[index]]["result"]["success"], false);
        assert!(items[&ids[index]]["result"]["error"]
            .as_str()
            .is_some_and(|v| !v.is_empty()));
    }
    assert_summary(
        &items[&ids[2]],
        fixtures()["connection_test"]["requested_model"]
            .as_str()
            .unwrap(),
        None,
        true,
    );
    assert_eq!(harness.mock.test_calls().len(), 4);
    harness.finish().await;
}

#[tokio::test]
async fn connection_batch_keeps_sanitized_summary_and_single_test_keeps_ephemeral_content() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(3, json!({})).await;
    sqlx::query("UPDATE accounts SET credentials=credentials||'{\"access_token\":\"fixture-private-account-token\"}'::jsonb WHERE id=ANY($1)").bind(&ids).execute(&harness.db.pool).await.unwrap();
    harness.mock.set_stream(ids[1], StreamScenario::NativeError);
    let request = harness
        .batch_request(
            &ids[..2],
            "test",
            json!({"confirmed":true}),
            "fixture-summary-redaction-0001",
        )
        .await;
    let accepted = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let finished = harness
        .wait_batch_settled(accepted["batch_id"].as_str().unwrap())
        .await;
    let items = items_by_account(&finished);
    let model = fixtures()["connection_test"]["requested_model"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_summary(&items[&ids[0]], &model, Some(&model), true);
    assert_summary(&items[&ids[1]], &model, Some(&model), false);
    assert!(items[&ids[1]]["result"]["error"]
        .as_str()
        .unwrap()
        .contains("[REDACTED]"));
    let public = serde_json::to_string(&finished).unwrap();
    let saved = fs::read_to_string(harness.operations.store.0.path()).unwrap();
    let audit = fs::read_to_string(&harness.actions.settings.audit_path).unwrap();
    for marker in [
        ADMIN_KEY,
        CONTENT_MARKER,
        BEARER_MARKER,
        PROVIDER_MARKER,
        "fixture-private-account-token",
    ] {
        for surface in [&public, &saved, &audit] {
            assert!(!surface.contains(marker));
        }
    }
    for item in finished["items"].as_array().unwrap() {
        assert!(harness
            .operations
            .get(item["id"].as_str().unwrap(), 0)
            .unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    let single_request = json!({"action":"test","request_id":"fixture-single-content-0001","client_id":CLIENT_ID,
        "expected_version":harness.expected_versions(&ids[2..],"test").await[&ids[2]],"payload":{"confirmed":true,"model_id":"fixture-single-model"}});
    let single = harness
        .operations
        .submit(ids[2], &single_request, ADMIN_KEY)
        .await
        .unwrap();
    harness.operations.tick().await.unwrap();
    let result = harness
        .wait_status(single["id"].as_str().unwrap(), "completed")
        .await;
    assert_eq!(result["result"]["success"], true);
    assert!(result["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["type"] == "content" && event["text"] == CONTENT_MARKER));
    let events = result["events"].as_array().unwrap();
    let saved_before_read = fs::read(harness.operations.store.0.path()).unwrap();
    let next = harness
        .operations
        .get(single["id"].as_str().unwrap(), 1)
        .unwrap();
    assert_eq!(next["events"], json!(&events[1..]));
    assert_eq!(next["next_event"], events.len());
    let end = harness
        .operations
        .get(single["id"].as_str().unwrap(), usize::MAX)
        .unwrap();
    assert_eq!(end["events"], json!([]));
    assert_eq!(end["next_event"], events.len());
    assert_eq!(
        fs::read(harness.operations.store.0.path()).unwrap(),
        saved_before_read
    );
    assert_eq!(
        harness.mock.test_calls().last().unwrap().body["model_id"],
        "fixture-single-model"
    );
    assert!(!fs::read_to_string(harness.operations.store.0.path())
        .unwrap()
        .contains(CONTENT_MARKER));
    harness.finish().await;
}

#[tokio::test]
async fn recover_batch_dispatches_recovery_with_partial_results_and_never_replays_unknown_writes() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(4, json!({})).await;
    sqlx::query("UPDATE accounts SET schedulable=false,status=CASE WHEN id=ANY($1) THEN 'error' ELSE 'active' END WHERE id=ANY($2)")
        .bind(&ids[..3]).bind(&ids).execute(&harness.db.pool).await.unwrap();
    harness
        .backend
        .recover_failures
        .lock()
        .unwrap()
        .insert(ids[1]);
    let mut request = harness
        .batch_request(&ids, "recover", json!({}), "fixture-recover-batch-0001")
        .await;
    request["accounts"][2]["expected_version"] = "f".repeat(64).into();
    let missing = ids[3] + 9;
    request["accounts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"account_id":missing,"expected_version":"0".repeat(64)}));
    let initial = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    let replay = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    assert_eq!(initial["batch_id"], replay["batch_id"]);
    let bid = initial["batch_id"].as_str().unwrap();
    assert_eq!(initial["items"][0]["status"], "queued");
    assert_eq!(initial["items"][1]["status"], "queued");
    assert_eq!(initial["items"][2]["status"], "needs_confirmation");
    assert_eq!(initial["items"][3]["result"]["unchanged"], true);
    assert_eq!(initial["items"][4]["status"], "failed");
    let done = harness.wait_batch_settled(bid).await;
    assert_eq!(done["items"][0]["status"], "completed");
    assert_eq!(done["items"][0]["result"]["verified"], true);
    assert_eq!(done["items"][1]["status"], "needs_confirmation");
    let mut called = harness.backend.recover_calls.lock().unwrap().clone();
    called.sort();
    assert_eq!(called, ids[..2]);
    for id in &ids {
        let account = harness.actions.account(*id, None).await.unwrap();
        assert_eq!(
            account["schedulable"], false,
            "recovery must not enable scheduling"
        );
    }
    assert!(harness.mock.test_calls().is_empty());
    assert!(harness
        .mock
        .calls
        .lock()
        .unwrap()
        .iter()
        .all(|call| call.method == Method::GET));
    harness.restart().await;
    let replay = harness
        .operations
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    assert_eq!(replay["batch_id"], initial["batch_id"]);
    harness.operations.tick().await.unwrap();
    assert_eq!(harness.backend.recover_calls.lock().unwrap().len(), 2);
    for payload in [
        json!({"schedulable":true}),
        json!({"confirmed":true}),
        json!([]),
    ] {
        let mut invalid = request.clone();
        invalid["payload"] = payload;
        assert_eq!(
            harness
                .operations
                .submit_batch(&invalid, ADMIN_KEY, false)
                .await
                .unwrap_err()
                .status
                .as_u16(),
            422
        );
    }
    let mut conflict = request.clone();
    conflict["accounts"][0]["expected_version"] = "e".repeat(64).into();
    assert_eq!(
        harness
            .operations
            .submit_batch(&conflict, ADMIN_KEY, false)
            .await
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    harness.finish().await;
}

#[tokio::test]
async fn scheduled_connection_runs_once_at_the_second_and_keeps_history_across_restart() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(3, json!({})).await;
    let due = chrono::DateTime::parse_from_rfc3339("2030-01-02T09:31:38+08:00")
        .unwrap()
        .with_timezone(&chrono::Utc);
    *harness.backend.now.lock().unwrap() = due - chrono::Duration::seconds(60);
    let versions = harness.expected_versions(&ids, "test").await;
    let request = json!({"action":"test","payload":{"mode":"default","confirmed":true,"model_id":"fixture-model"},"scheduled_at":"2030-01-02T09:31:38+08:00","expected_version":versions[&ids[0]],"request_id":"fixture-scheduled-connection-0001","client_id":CLIENT_ID});
    let job = harness
        .operations
        .submit(ids[0], &request, ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(
        sub2ops_server::operations::scheduled_time(&job["scheduled_at"]).unwrap(),
        due
    );
    *harness.backend.now.lock().unwrap() = due - chrono::Duration::seconds(1);
    harness.operations.tick().await.unwrap();
    assert_eq!(
        harness
            .operations
            .get(job["id"].as_str().unwrap(), 0)
            .unwrap()["reason"],
        "等待预约时间"
    );
    assert!(harness.mock.test_calls().is_empty());
    sqlx::query("UPDATE accounts SET credentials=credentials || '{\"access_token\":\"fixture-refreshed-token\"}'::jsonb WHERE id=$1")
        .bind(ids[0]).execute(&harness.db.pool).await.unwrap();
    harness.restart().await;
    let replay = harness
        .operations
        .submit(ids[0], &request, ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(replay["id"], job["id"]);
    *harness.backend.now.lock().unwrap() = due;
    harness.operations.tick().await.unwrap();
    harness
        .wait_status(job["id"].as_str().unwrap(), "completed")
        .await;
    harness.operations.tick().await.unwrap();
    let history = harness.actions.queries.oauth.snapshot().unwrap()["recovery_history"].clone();
    let entry = &history[format!("connection-test:{}", job["id"].as_str().unwrap())];
    assert_eq!(entry["kind"], "connection_test");
    assert_eq!(entry["result_status"], "completed");
    assert!(entry["recovered_at"].is_null());
    harness.restart().await;
    harness.operations.tick().await.unwrap();
    assert_eq!(harness.mock.test_calls().len(), 1);
    assert_eq!(
        harness.actions.queries.oauth.snapshot().unwrap()["recovery_history"],
        history
    );
    let mut cancelled = request.clone();
    cancelled["expected_version"] = versions[&ids[1]].clone().into();
    cancelled["request_id"] = "fixture-scheduled-connection-0002".into();
    cancelled["scheduled_at"] = "2030-01-02T09:32:38+08:00".into();
    let queued = harness
        .operations
        .submit(ids[1], &cancelled, ADMIN_KEY)
        .await
        .unwrap();
    harness
        .operations
        .cancel(queued["id"].as_str().unwrap())
        .await
        .unwrap();
    *harness.backend.now.lock().unwrap() = due + chrono::Duration::minutes(1);
    harness.operations.tick().await.unwrap();
    assert_eq!(harness.mock.test_calls().len(), 1);
    let state = harness.actions.queries.oauth.snapshot().unwrap();
    assert_eq!(
        state["recovery_history"][format!("connection-test:{}", queued["id"].as_str().unwrap())]
            ["result_status"],
        "cancelled"
    );
    let mut invalid = request.clone();
    invalid["request_id"] = "fixture-scheduled-invalid-0001".into();
    assert!(harness
        .operations
        .submit(ids[0], &invalid, ADMIN_KEY)
        .await
        .is_err());
    invalid["scheduled_at"] = "2031-01-01T01:00:01.001Z".into();
    assert!(harness
        .operations
        .submit(ids[0], &invalid, ADMIN_KEY)
        .await
        .is_err());
    harness.finish().await;
}

#[tokio::test]
async fn single_default_test_model_is_frozen_for_immediate_and_scheduled_requests() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(4, json!({})).await;
    let due = chrono::DateTime::parse_from_rfc3339("2030-01-02T09:31:00+08:00")
        .unwrap()
        .with_timezone(&chrono::Utc);
    *harness.backend.now.lock().unwrap() = due - chrono::Duration::minutes(1);
    *harness.backend.test_model.lock().unwrap() = "fixture-preset-model".into();
    let versions = harness.expected_versions(&ids, "test").await;
    let mut jobs = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let chosen = if index == 2 {
            "fixture-explicit-model"
        } else {
            ""
        };
        let mut request = json!({"action":"test","payload":{"mode":"default","confirmed":true,"model_id":chosen},"expected_version":versions[id],"request_id":format!("fixture-default-model-{index:04}"),"client_id":CLIENT_ID});
        if matches!(index, 1 | 3) {
            request["scheduled_at"] = "2030-01-02T09:31:00+08:00".into();
        }
        let job = harness
            .operations
            .submit(*id, &request, ADMIN_KEY)
            .await
            .unwrap();
        assert_eq!(
            job["requested"]["model_id"],
            if chosen.is_empty() {
                "fixture-preset-model"
            } else {
                chosen
            }
        );
        jobs.push((request, job));
    }
    // Simulate a still-queued appointment accepted by the previous release.
    let legacy_id = jobs[3].1["id"].as_str().unwrap();
    harness
        .operations
        .store
        .transaction(|state| {
            state["jobs"][legacy_id]["payload"]["model_id"] = "".into();
            Ok(())
        })
        .unwrap();
    jobs[3].1["requested"]["model_id"] = "".into();
    *harness.backend.test_model.lock().unwrap() = "fixture-new-preset".into();
    harness.restart().await;
    for (id, (request, job)) in ids.iter().zip(&jobs) {
        let replay = harness
            .operations
            .submit(*id, request, ADMIN_KEY)
            .await
            .unwrap();
        assert_eq!(replay["id"], job["id"]);
        assert_eq!(replay["requested"], job["requested"]);
    }
    *harness.backend.now.lock().unwrap() = due;
    harness.operations.tick().await.unwrap();
    for (_, job) in &jobs {
        harness
            .wait_status(job["id"].as_str().unwrap(), "completed")
            .await;
    }
    let calls = harness.mock.test_calls();
    assert_eq!(calls.len(), 4);
    for id in &ids[..2] {
        assert_eq!(
            calls
                .iter()
                .find(|c| c.path.ends_with(&format!("/{id}/test")))
                .unwrap()
                .body["model_id"],
            "fixture-preset-model"
        );
    }
    assert_eq!(
        calls
            .iter()
            .find(|c| c.path.ends_with(&format!("/{}/test", ids[2])))
            .unwrap()
            .body["model_id"],
        "fixture-explicit-model"
    );
    assert_eq!(
        calls
            .iter()
            .find(|c| c.path.ends_with(&format!("/{}/test", ids[3])))
            .unwrap()
            .body["model_id"],
        "fixture-new-preset"
    );
    harness.operations.tick().await.unwrap();
    assert_eq!(harness.mock.test_calls().len(), 4);
    harness.finish().await;
}

fn cycle_start() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2030-01-02T01:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

async fn cycle_quota(
    harness: &Harness,
    id: i64,
    reset: chrono::DateTime<chrono::Utc>,
    seven: f64,
    limited: bool,
) {
    let at = harness.backend.now();
    let extra = json!({"codex_usage_updated_at":at.to_rfc3339(),"codex_5h_used_percent":100,
        "codex_5h_reset_at":reset.to_rfc3339(),"codex_7d_used_percent":seven,
        "codex_7d_reset_at":(cycle_start()+chrono::Duration::days(6)).to_rfc3339()});
    sqlx::query(
        "UPDATE accounts SET extra=$2,rate_limited_at=$3,rate_limit_reset_at=$4 WHERE id=$1",
    )
    .bind(id)
    .bind(SqlJson(extra))
    .bind(limited.then_some(at - chrono::Duration::seconds(30)))
    .bind(limited.then_some(at + chrono::Duration::hours(1)))
    .execute(&harness.db.pool)
    .await
    .unwrap();
}

async fn cycle_request(harness: &Harness, id: i64, suffix: &str) -> Value {
    json!({"action":"test","payload":{"confirmed":true,"mode":"default","model_id":""},
        "repeat":"quota_cycle","expected_version":harness.expected_versions(&[id], "test").await[&id],
        "request_id":format!("fixture-cycle-{suffix}"),"client_id":CLIENT_ID})
}

#[tokio::test]
async fn continuous_cycle_chains_once_after_reset_and_restart_with_frozen_model() {
    let mut harness = Harness::new().await;
    harness
        .actions
        .queries
        .oauth
        .save_admin_token(ADMIN_KEY)
        .unwrap();
    let id = harness.seed_accounts(1, json!({})).await[0];
    *harness.backend.now.lock().unwrap() = cycle_start();
    *harness.backend.test_model.lock().unwrap() = "fixture-fixed-cycle-model".into();
    let reset = cycle_start() + chrono::Duration::minutes(1) + chrono::Duration::milliseconds(200);
    cycle_quota(&harness, id, reset, 30.0, false).await;
    let request = cycle_request(&harness, id, "first").await;
    let first = harness
        .operations
        .submit(id, &request, ADMIN_KEY)
        .await
        .unwrap();
    let first_id = first["id"].as_str().unwrap();
    let due = cycle_start() + chrono::Duration::seconds(64);
    assert_eq!(
        sub2ops_server::operations::scheduled_time(&first["scheduled_at"]).unwrap(),
        due
    );
    assert_eq!(first["repeat_state"], "active");
    let replay = harness
        .operations
        .submit(id, &request, ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(
        harness
            .backend
            .cycle_reads
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, refresh)| *refresh)
            .count(),
        1
    );
    let duplicate = cycle_request(&harness, id, "duplicate").await;
    assert_eq!(
        harness
            .operations
            .submit(id, &duplicate, ADMIN_KEY)
            .await
            .err()
            .unwrap()
            .status
            .as_u16(),
        409
    );
    *harness.backend.now.lock().unwrap() = due - chrono::Duration::seconds(1);
    harness.operations.tick().await.unwrap();
    assert!(harness.mock.test_calls().is_empty());
    harness.restart().await;
    *harness.backend.now.lock().unwrap() = due;
    harness.operations.tick().await.unwrap();
    harness.wait_status(first_id, "completed").await;
    assert_eq!(
        harness
            .backend
            .cycle_reads
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, refresh)| *refresh)
            .count(),
        2,
        "dispatch refreshes the quota before sending the connection test"
    );
    *harness.backend.test_model.lock().unwrap() = "fixture-changed-model".into();
    let second_reset = due + chrono::Duration::hours(5);
    cycle_quota(&harness, id, second_reset, 40.0, true).await;
    // The completed receipt survives restart before the next appointment is inserted.
    harness.restart().await;
    harness.operations.tick().await.unwrap();
    let previous = harness.operations.get(first_id, 0).unwrap();
    assert_eq!(previous["repeat_state"], "continued");
    let next_id = previous["next_operation_id"].as_str().unwrap();
    let next = harness.operations.get(next_id, 0).unwrap();
    assert_eq!(next["requested"]["model_id"], "fixture-fixed-cycle-model");
    assert_eq!(next["repeat_series_id"], first["id"]);
    assert_eq!(
        sub2ops_server::operations::scheduled_time(&next["scheduled_at"]).unwrap(),
        second_reset + chrono::Duration::seconds(3)
    );
    harness.operations.tick().await.unwrap();
    assert_eq!(
        harness.operations.store.read().unwrap()["jobs"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(harness.operations.listing(None).unwrap()["pending"], 1);
    assert_eq!(harness.mock.test_calls().len(), 1);
    *harness.backend.now.lock().unwrap() = second_reset + chrono::Duration::seconds(3);
    harness.operations.tick().await.unwrap();
    harness.wait_status(next_id, "completed").await;
    let third_reset = second_reset + chrono::Duration::hours(5);
    cycle_quota(&harness, id, third_reset, 50.0, false).await;
    harness.operations.tick().await.unwrap();
    let stopped = harness.operations.cancel(first_id).await.unwrap();
    assert_eq!(stopped["repeat_state"], "stopped");
    assert_eq!(stopped["status"], "cancelled");
    assert_ne!(stopped["id"], first["id"]);
    *harness.backend.now.lock().unwrap() = third_reset + chrono::Duration::seconds(3);
    harness.operations.tick().await.unwrap();
    assert_eq!(harness.mock.test_calls().len(), 2);
    let state = harness.actions.queries.oauth.snapshot().unwrap();
    for jid in [first_id, next_id] {
        assert_eq!(
            state["recovery_history"][format!("connection-test:{jid}")]["result_status"],
            "completed"
        );
    }
    harness.finish().await;
}

#[tokio::test]
async fn continuous_cycle_pauses_when_predispatch_quota_refresh_fails() {
    let mut harness = Harness::new().await;
    harness
        .actions
        .queries
        .oauth
        .save_admin_token(ADMIN_KEY)
        .unwrap();
    let id = harness.seed_accounts(1, json!({})).await[0];
    *harness.backend.now.lock().unwrap() = cycle_start();
    let reset = cycle_start() + chrono::Duration::minutes(1);
    cycle_quota(&harness, id, reset, 20.0, false).await;
    let job = harness
        .operations
        .submit(
            id,
            &cycle_request(&harness, id, "dispatch-refresh-failed").await,
            ADMIN_KEY,
        )
        .await
        .unwrap();
    *harness.backend.cycle_refresh_error.lock().unwrap() =
        Some("fixture quota refresh unavailable".into());
    *harness.backend.now.lock().unwrap() = reset + chrono::Duration::seconds(3);

    harness.operations.tick().await.unwrap();
    let paused = harness
        .wait_status(job["id"].as_str().unwrap(), "needs_confirmation")
        .await;
    assert_eq!(paused["repeat_state"], "needs_confirmation");
    assert!(paused["repeat_reason"]
        .as_str()
        .unwrap()
        .contains("fixture quota refresh unavailable"));
    assert!(harness.mock.test_calls().is_empty());
    harness.restart().await;
    harness.operations.tick().await.unwrap();
    assert!(harness.mock.test_calls().is_empty());
    harness.finish().await;
}

#[tokio::test]
async fn continuous_cycle_requires_both_weekly_depletion_and_current_nonlocal_429() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(6, json!({})).await;
    *harness.backend.now.lock().unwrap() = cycle_start();
    let reset = cycle_start() + chrono::Duration::hours(5);
    let mut jobs = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        cycle_quota(&harness, *id, reset, 20.0, false).await;
        jobs.push(
            harness
                .operations
                .submit(
                    *id,
                    &cycle_request(&harness, *id, &format!("stop-{index}")).await,
                    ADMIN_KEY,
                )
                .await
                .unwrap(),
        );
        cycle_quota(
            &harness,
            *id,
            reset,
            if index == 1 { 40.0 } else { 100.0 },
            index != 0,
        )
        .await;
    }
    // Completed limits and local/client throttles are not a current upstream 429.
    sqlx::query("UPDATE accounts SET rate_limit_reset_at=$2 WHERE id=$1")
        .bind(ids[2])
        .bind(cycle_start() - chrono::Duration::seconds(1))
        .execute(&harness.db.pool)
        .await
        .unwrap();
    for (index, owner, message) in [
        (3, "client", "fixture-client-limit"),
        (
            4,
            "provider",
            "You have reached the request limit: 10 requests per minute",
        ),
    ] {
        sqlx::query("INSERT INTO ops_error_logs (id,account_id,created_at,platform,error_owner,error_phase,status_code,upstream_status_code,error_message,upstream_error_message) VALUES ($1,$2,$3,'openai',$4,'upstream',429,429,$5,$5)")
            .bind(ids[index]+1_000_000_000).bind(ids[index]).bind(cycle_start()).bind(owner).bind(message).execute(&harness.db.pool).await.unwrap();
    }
    harness.operations.tick().await.unwrap();
    for (index, job) in jobs.iter().enumerate() {
        let current = harness
            .operations
            .get(job["id"].as_str().unwrap(), 0)
            .unwrap();
        assert_eq!(
            current["repeat_state"],
            if index == 5 { "stopped" } else { "active" },
            "case {index}"
        );
        assert_eq!(
            current["status"],
            if index == 5 { "cancelled" } else { "queued" }
        );
    }
    assert!(harness.mock.test_calls().is_empty());
    harness.finish().await;
}

#[tokio::test]
async fn continuous_cycle_stops_after_a_confirmed_test_without_losing_its_result() {
    let mut harness = Harness::new().await;
    let id = harness.seed_accounts(1, json!({})).await[0];
    *harness.backend.now.lock().unwrap() = cycle_start();
    let reset = cycle_start() + chrono::Duration::minutes(1);
    cycle_quota(&harness, id, reset, 90.0, false).await;
    let job = harness
        .operations
        .submit(
            id,
            &cycle_request(&harness, id, "after-test").await,
            ADMIN_KEY,
        )
        .await
        .unwrap();
    let jid = job["id"].as_str().unwrap();
    harness.mock.set_stream(id, StreamScenario::ExplicitFailure);
    *harness.backend.now.lock().unwrap() = reset + chrono::Duration::seconds(3);
    harness.operations.tick().await.unwrap();
    wait_attempt(&harness, jid, 1, "queued").await;
    cycle_quota(
        &harness,
        id,
        reset + chrono::Duration::hours(5),
        100.0,
        true,
    )
    .await;
    harness.operations.tick().await.unwrap();
    let stopped = harness.operations.get(jid, 0).unwrap();
    assert_eq!(stopped["repeat_state"], "stopped");
    assert_eq!(stopped["result"]["success"], false);
    assert_eq!(stopped["status"], "completed");
    assert_eq!(
        harness.actions.queries.oauth.snapshot().unwrap()["pending_events"]
            [format!("connection-test:{jid}")]["result_status"],
        "failed"
    );
    assert!(stopped["repeat_reason"].as_str().unwrap().contains("429"));
    assert!(stopped["next_operation_id"].is_null());
    assert_eq!(
        harness.operations.store.read().unwrap()["jobs"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    harness.finish().await;
}

#[tokio::test]
async fn continuous_cycle_pauses_unknown_results_and_unavailable_cycles_without_replay() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(3, json!({})).await;
    *harness.backend.now.lock().unwrap() = cycle_start();
    let missing = harness
        .operations
        .submit(
            ids[0],
            &cycle_request(&harness, ids[0], "missing").await,
            ADMIN_KEY,
        )
        .await
        .unwrap();
    assert_eq!(missing["status"], "needs_confirmation");
    assert!(missing["scheduled_at"].is_null());
    let reset = cycle_start() + chrono::Duration::minutes(1);
    cycle_quota(&harness, ids[1], reset, 20.0, false).await;
    let unknown = harness
        .operations
        .submit(
            ids[1],
            &cycle_request(&harness, ids[1], "unknown").await,
            ADMIN_KEY,
        )
        .await
        .unwrap();
    harness.mock.set_stream(ids[1], StreamScenario::NoTerminal);
    *harness.backend.now.lock().unwrap() = reset + chrono::Duration::seconds(3);
    harness.operations.tick().await.unwrap();
    harness
        .wait_status(unknown["id"].as_str().unwrap(), "needs_confirmation")
        .await;
    harness.restart().await;
    harness.operations.tick().await.unwrap();
    assert_eq!(harness.mock.test_calls().len(), 1);
    assert_eq!(
        harness
            .operations
            .get(unknown["id"].as_str().unwrap(), 0)
            .unwrap()["repeat_state"],
        "needs_confirmation"
    );
    cycle_quota(
        &harness,
        ids[2],
        reset + chrono::Duration::hours(5),
        20.0,
        false,
    )
    .await;
    let changed = harness
        .operations
        .submit(
            ids[2],
            &cycle_request(&harness, ids[2], "changed").await,
            ADMIN_KEY,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET type='apikey' WHERE id=$1")
        .bind(ids[2])
        .execute(&harness.db.pool)
        .await
        .unwrap();
    harness.operations.tick().await.unwrap();
    assert_eq!(
        harness
            .operations
            .get(changed["id"].as_str().unwrap(), 0)
            .unwrap()["repeat_state"],
        "needs_confirmation"
    );
    assert_eq!(harness.mock.test_calls().len(), 1);
    harness.finish().await;
}

#[tokio::test]
async fn continuous_cycle_cancellation_wins_during_dispatch_and_successor_refresh() {
    let mut harness = Harness::new().await;
    let ids = harness.seed_accounts(2, json!({})).await;
    let reset = cycle_start() + chrono::Duration::minutes(1);
    for (index, id) in ids.iter().enumerate() {
        *harness.backend.now.lock().unwrap() = cycle_start();
        cycle_quota(&harness, *id, reset, 20.0, false).await;
        let job = harness
            .operations
            .submit(
                *id,
                &cycle_request(&harness, *id, &format!("cancel-{index}")).await,
                ADMIN_KEY,
            )
            .await
            .unwrap();
        let jid = job["id"].as_str().unwrap();
        if index == 0 {
            harness.mock.gate_test(*id);
        }
        *harness.backend.now.lock().unwrap() = reset + chrono::Duration::seconds(3);
        harness.operations.tick().await.unwrap();
        harness.wait_test_calls(index + 1).await;
        if index == 0 {
            let cancelled = harness.operations.cancel(jid).await.unwrap();
            assert_eq!(cancelled["status"], "running");
            harness.mock.release_test(*id);
            harness.wait_status(jid, "completed").await;
        } else {
            harness.wait_status(jid, "completed").await;
            cycle_quota(
                &harness,
                *id,
                reset + chrono::Duration::hours(5),
                30.0,
                false,
            )
            .await;
            let gate = Arc::new(Notify::new());
            *harness.backend.cycle_gate.lock().unwrap() = Some(gate.clone());
            let queue = harness.operations.clone();
            let tick = tokio::spawn(async move { queue.tick().await });
            tokio::time::timeout(Duration::from_secs(8), async {
                while harness.backend.cycle_gate.lock().unwrap().is_some() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            harness.operations.cancel(jid).await.unwrap();
            gate.notify_one();
            tick.await.unwrap().unwrap();
        }
        harness.operations.tick().await.unwrap();
        let stopped = harness.operations.get(jid, 0).unwrap();
        assert_eq!(stopped["repeat_state"], "stopped");
        assert_eq!(stopped["result"]["success"], true);
        assert!(stopped["next_operation_id"].is_null());
        assert!(
            harness.actions.queries.oauth.snapshot().unwrap()["pending_events"]
                .get(format!("connection-test:{jid}"))
                .is_none()
        );
    }
    assert_eq!(harness.mock.test_calls().len(), 2);
    harness.finish().await;
}

#[tokio::test]
async fn continuous_cycle_validates_mode_account_and_request_identity() {
    let mut harness = Harness::new().await;
    let id = harness.seed_accounts(1, json!({})).await[0];
    *harness.backend.now.lock().unwrap() = cycle_start();
    cycle_quota(
        &harness,
        id,
        cycle_start() + chrono::Duration::hours(5),
        10.0,
        false,
    )
    .await;
    let request = cycle_request(&harness, id, "validation").await;
    for (field, value) in [
        ("repeat", json!("every_second")),
        ("scheduled_at", json!("2030-01-03T00:00:00Z")),
    ] {
        let mut invalid = request.clone();
        invalid[field] = value;
        assert_eq!(
            harness
                .operations
                .submit(id, &invalid, ADMIN_KEY)
                .await
                .err()
                .unwrap()
                .status
                .as_u16(),
            422
        );
    }
    for mode in ["compact", "text"] {
        let mut invalid = request.clone();
        invalid["payload"]["mode"] = mode.into();
        assert_eq!(
            harness
                .operations
                .submit(id, &invalid, ADMIN_KEY)
                .await
                .err()
                .unwrap()
                .status
                .as_u16(),
            422
        );
    }
    let first = harness
        .operations
        .submit(id, &request, ADMIN_KEY)
        .await
        .unwrap();
    let mut invalid = request.clone();
    invalid.as_object_mut().unwrap().remove("repeat");
    assert_eq!(
        harness
            .operations
            .submit(id, &invalid, ADMIN_KEY)
            .await
            .err()
            .unwrap()
            .status
            .as_u16(),
        409
    );
    harness
        .operations
        .cancel(first["id"].as_str().unwrap())
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET type='apikey' WHERE id=$1")
        .bind(id)
        .execute(&harness.db.pool)
        .await
        .unwrap();
    let invalid = cycle_request(&harness, id, "unsupported").await;
    assert_eq!(
        harness
            .operations
            .submit(id, &invalid, ADMIN_KEY)
            .await
            .err()
            .unwrap()
            .status
            .as_u16(),
        422
    );
    harness.finish().await;
}

async fn scheduled_fixture(
    h: &Harness,
    id: i64,
    suffix: &str,
    due: chrono::DateTime<chrono::Utc>,
) -> Value {
    h.actions.queries.oauth.save_admin_token(ADMIN_KEY).unwrap();
    *h.backend.now.lock().unwrap() = due - chrono::Duration::seconds(1);
    h.operations.submit(id,&json!({"action":"test","payload":{"mode":"default","confirmed":true,"model_id":"fixture-model"},
        "scheduled_at":due.to_rfc3339(),"expected_version":h.expected_versions(&[id],"test").await[&id],
        "request_id":format!("fixture-retry-{suffix}"),"client_id":CLIENT_ID}),ADMIN_KEY).await.unwrap()
}
async fn wait_attempt(h: &Harness, jid: &str, count: i64, status: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let job = h.operations.get(jid, 0).unwrap();
            if job["attempt_count"] == count && job["status"] == status {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("connection attempt must complete")
}

#[tokio::test]
async fn scheduled_retry_freezes_policy_survives_restart_and_notifies_only_final_result() {
    let mut h = Harness::new().await;
    let id = h.seed_accounts(1, json!({})).await[0];
    let due = cycle_start();
    let job = scheduled_fixture(&h, id, "restart", due).await;
    let jid = job["id"].as_str().unwrap();
    h.mock.set_stream(id, StreamScenario::ExplicitFailure);
    *h.backend.now.lock().unwrap() = due;
    h.operations.tick().await.unwrap();
    let waiting = wait_attempt(&h, jid, 1, "queued").await;
    assert_eq!(
        waiting["next_retry_at"],
        sub2ops_server::compat::datetime(due + chrono::Duration::seconds(10))
    );
    h.operations.tick().await.unwrap();
    assert!(
        h.actions.queries.oauth.snapshot().unwrap()["pending_events"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.operations.budget.available_permits(), 8);
    h.backend.retry_policy.lock().unwrap().max_retries = 0;
    h.backend.retry_policy.lock().unwrap().interval_seconds = 99;
    h.restart().await;
    *h.backend.now.lock().unwrap() = due + chrono::Duration::seconds(9);
    h.operations.tick().await.unwrap();
    assert_eq!(h.mock.test_calls().len(), 1);
    h.mock.set_stream(id, StreamScenario::Success);
    *h.backend.now.lock().unwrap() = due + chrono::Duration::seconds(10);
    h.operations.tick().await.unwrap();
    wait_attempt(&h, jid, 2, "completed").await;
    h.operations.tick().await.unwrap();
    let key = format!("connection-test:{jid}");
    let state = h.actions.queries.oauth.snapshot().unwrap();
    let event = &state["pending_events"][&key];
    assert_eq!(event["attempt_count"], 2);
    assert_eq!(event["result_status"], "completed");
    assert_eq!(state["pending_events"].as_object().unwrap().len(), 1);
    assert!(h.operations.keys.lock().unwrap().is_empty());
    let (_, body) = sub2ops_server::bark::oauth_message(event);
    assert!(body.contains("2"));
    h.actions
        .queries
        .oauth
        .mark_delivered(std::slice::from_ref(event), false)
        .unwrap();
    h.restart().await;
    h.operations.tick().await.unwrap();
    assert!(
        h.actions.queries.oauth.snapshot().unwrap()["pending_events"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.mock.test_calls().len(), 2);
    h.finish().await;
}

#[tokio::test]
async fn scheduled_retry_limit_zero_custom_interval_and_cancellation_are_bounded() {
    for (retries, interval, cancel) in
        [(3, 10, false), (0, 10, false), (1, 7, false), (3, 10, true)]
    {
        let mut h = Harness::new().await;
        let id = h.seed_accounts(1, json!({})).await[0];
        *h.backend.retry_policy.lock().unwrap() = sub2ops_server::connection_retry::Policy {
            interval_seconds: interval,
            max_retries: retries,
        };
        let due = cycle_start();
        let job = scheduled_fixture(&h, id, "bounded", due).await;
        let jid = job["id"].as_str().unwrap();
        h.mock.set_stream(id, StreamScenario::ExplicitFailure);
        for attempt in 1..=retries + 1 {
            *h.backend.now.lock().unwrap() =
                due + chrono::Duration::seconds(interval * (attempt - 1));
            h.operations.tick().await.unwrap();
            let status = if attempt <= retries {
                "queued"
            } else {
                "completed"
            };
            let job = wait_attempt(&h, jid, attempt, status).await;
            if cancel {
                h.operations.cancel(jid).await.unwrap();
                *h.backend.now.lock().unwrap() = due + chrono::Duration::hours(1);
                h.operations.tick().await.unwrap();
                assert_eq!(h.operations.get(jid, 0).unwrap()["status"], "cancelled");
                assert_eq!(h.mock.test_calls().len(), 1);
                assert!(
                    h.actions.queries.oauth.snapshot().unwrap()["pending_events"]
                        .as_object()
                        .unwrap()
                        .is_empty()
                );
                break;
            }
            if attempt <= retries {
                assert_eq!(
                    job["next_retry_at"],
                    sub2ops_server::compat::datetime(
                        due + chrono::Duration::seconds(interval * attempt)
                    )
                );
            }
        }
        if !cancel {
            h.operations.tick().await.unwrap();
            let state = h.actions.queries.oauth.snapshot().unwrap();
            let event = &state["pending_events"][format!("connection-test:{jid}")];
            assert_eq!(event["result_status"], "failed");
            assert_eq!(event["attempt_count"], retries + 1);
            assert_eq!(h.mock.test_calls().len(), (retries + 1) as usize);
            assert!(h.operations.keys.lock().unwrap().is_empty());
        }
        h.finish().await;
    }
}

#[tokio::test]
async fn continuous_retry_finishes_the_round_before_handing_authorization_to_successor() {
    let mut h = Harness::new().await;
    let id = h.seed_accounts(1, json!({})).await[0];
    *h.backend.now.lock().unwrap() = cycle_start();
    let reset = cycle_start() + chrono::Duration::minutes(1);
    cycle_quota(&h, id, reset, 20.0, false).await;
    let job = h
        .operations
        .submit(id, &cycle_request(&h, id, "retry-handoff").await, ADMIN_KEY)
        .await
        .unwrap();
    let jid = job["id"].as_str().unwrap();
    h.mock.set_stream(id, StreamScenario::ExplicitFailure);
    *h.backend.now.lock().unwrap() = reset + chrono::Duration::seconds(3);
    h.operations.tick().await.unwrap();
    wait_attempt(&h, jid, 1, "queued").await;
    h.operations.tick().await.unwrap();
    assert!(h.operations.get(jid, 0).unwrap()["next_operation_id"].is_null());
    assert!(h.operations.keys.lock().unwrap().contains_key(jid));
    h.mock.set_stream(id, StreamScenario::Success);
    *h.backend.now.lock().unwrap() = reset + chrono::Duration::seconds(13);
    h.operations.tick().await.unwrap();
    wait_attempt(&h, jid, 2, "completed").await;
    cycle_quota(&h, id, reset + chrono::Duration::hours(5), 30.0, false).await;
    h.operations.tick().await.unwrap();
    let old = h.operations.get(jid, 0).unwrap();
    let next = old["next_operation_id"].as_str().unwrap();
    assert!(!h.operations.keys.lock().unwrap().contains_key(jid));
    assert_eq!(h.operations.keys.lock().unwrap()[next], ADMIN_KEY);
    assert_eq!(
        h.operations.get(next, 0).unwrap()["attempt_count"],
        Value::Null
    );
    assert_eq!(
        h.actions.queries.oauth.snapshot().unwrap()["pending_events"]
            [format!("connection-test:{jid}")]["attempt_count"],
        2
    );
    h.finish().await;
}

#[tokio::test]
async fn scheduled_auth_failure_and_unknown_result_are_final_without_automatic_replay() {
    for (scenario, status) in [
        (StreamScenario::AuthError, "completed"),
        (StreamScenario::NoTerminal, "needs_confirmation"),
    ] {
        let mut h = Harness::new().await;
        let id = h.seed_accounts(1, json!({})).await[0];
        let due = cycle_start();
        let job = scheduled_fixture(&h, id, "no-replay", due).await;
        let jid = job["id"].as_str().unwrap();
        h.mock.set_stream(id, scenario);
        *h.backend.now.lock().unwrap() = due;
        h.operations.tick().await.unwrap();
        wait_attempt(&h, jid, 1, status).await;
        h.restart().await;
        *h.backend.now.lock().unwrap() = due + chrono::Duration::hours(1);
        h.operations.tick().await.unwrap();
        assert_eq!(h.mock.test_calls().len(), 1);
        assert_eq!(
            h.actions.queries.oauth.snapshot().unwrap()["pending_events"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert!(h.operations.keys.lock().unwrap().is_empty());
        h.finish().await;
    }
}

#[tokio::test]
async fn batched_cycle_observations_match_live_reads_and_keep_per_account_failures() {
    let mut h = Harness::new().await;
    let ids = h.seed_accounts(6, json!({})).await;
    let at = cycle_start();
    *h.backend.now.lock().unwrap() = at;
    for (index, id) in ids.iter().enumerate() {
        cycle_quota(
            &h,
            *id,
            at + chrono::Duration::hours(5),
            if index == 0 { 30.0 } else { 100.0 },
            index > 0,
        )
        .await;
    }
    for (index, owner, message) in [
        (1, "upstream", "too many requests"),
        (
            2,
            "provider",
            "You have reached the request limit: 10 requests per minute",
        ),
        (3, "client", "too many requests"),
    ] {
        sqlx::query("INSERT INTO ops_error_logs(id,account_id,created_at,status_code,upstream_status_code,error_owner,error_message) VALUES($1,$2,$3,429,429,$4,$5)")
            .bind(ids[index]+1_000_000_000).bind(ids[index]).bind(at).bind(owner).bind(message).execute(&h.db.pool).await.unwrap();
    }
    let runtime = sub2ops_server::runtime::Runtime::new_with_clock(
        (*h.actions.settings).clone(),
        Arc::new(move || at),
    )
    .await
    .unwrap();
    runtime
        .monitor
        .store
        .transaction(|state| {
            state["scheduler"][ids[4].to_string()] = json!({"last_error_code":"http_401"});
            Ok(())
        })
        .unwrap();
    sqlx::query("UPDATE accounts SET deleted_at=$1 WHERE id=$2")
        .bind(at)
        .bind(ids[5])
        .execute(&h.db.pool)
        .await
        .unwrap();
    let batch = runtime
        .operations
        .backend
        .cycle_observations(&ids)
        .await
        .unwrap();
    for id in &ids[..5] {
        match (
            runtime
                .operations
                .backend
                .cycle_observation(*id, ADMIN_KEY, false)
                .await,
            &batch[id],
        ) {
            (Ok(single), Ok(all)) => {
                assert_eq!(single.windows, all.windows);
                assert_eq!(single.stopped, all.stopped);
            }
            (Err(single), Err(all)) => assert_eq!(single.detail, all.detail),
            _ => panic!("batch and individual cycle outcomes differ"),
        }
    }
    assert!(!batch[&ids[0]].as_ref().unwrap().stopped);
    assert!(batch[&ids[1]].as_ref().unwrap().stopped);
    assert!(!batch[&ids[2]].as_ref().unwrap().stopped);
    assert!(!batch[&ids[3]].as_ref().unwrap().stopped);
    assert!(batch[&ids[4]].is_err());
    assert!(!batch.contains_key(&ids[5]));
    runtime.close().await;
    h.finish().await;
}
