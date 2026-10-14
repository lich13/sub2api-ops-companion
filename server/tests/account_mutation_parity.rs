use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, types::Json as SqlJson, Connection, PgConnection, PgPool};
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use sub2ops_server::{
    account_templates::Templates,
    api,
    compat::{account_version, operation_versions},
    operations::{OperationStore, Operations},
    runtime::Runtime,
    settings::Settings,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

const FIXTURES: &str = include_str!("../../tests/contracts/account-mutation/fixtures.json");
const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";
const ADMIN_KEY: &str = "fixture-account-mutation-admin-040";
const CLIENT_ID: &str = "fixture-account-mutation-client-0001";
const PROXY_ID: i64 = 1_520_000_001;
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(820_000_000);

fn fixtures() -> Value {
    serde_json::from_str(FIXTURES).expect("account-mutation fixture JSON must parse")
}

#[derive(Clone, Debug)]
struct Call {
    method: Method,
    path: String,
    uri: String,
    body: Value,
    api_key: String,
}

#[derive(Default)]
struct CallGate {
    entered: Notify,
    release: Notify,
}

#[derive(Clone, Copy, Debug, Default)]
enum BulkMode {
    #[default]
    Confirmed,
    PartialSecond,
    UnknownAppliedGatewayTimeout,
    UnknownNotAppliedGatewayTimeout,
    MalformedResultIds,
    MalformedTotal,
    ExplicitUnsupported,
    GenericNotFound,
}

#[derive(Clone, Copy, Default)]
enum ManualMode {
    #[default]
    Confirmed,
    Rejected,
    Unchanged,
    DisconnectedApplied,
}

#[derive(Clone)]
struct MockUpstream {
    pool: PgPool,
    calls: Arc<Mutex<Vec<Call>>>,
    gates: Arc<Mutex<BTreeMap<String, Arc<CallGate>>>>,
    apply_groups: Arc<AtomicBool>,
    apply_updates: Arc<AtomicBool>,
    bulk_mode: Arc<Mutex<BulkMode>>,
    proxy_active: Arc<AtomicBool>,
    diagnostic_status: Arc<AtomicI64>,
    group_status: Arc<AtomicI64>,
    update_status: Arc<AtomicI64>,
    diagnostic_results: Arc<Mutex<BTreeMap<String, Value>>>,
    grok_billing: Arc<Mutex<BTreeMap<i64, Value>>>,
    sse_chunks: Arc<Vec<String>>,
    manual_mode: Arc<Mutex<ManualMode>>,
    connection_streams: Arc<Mutex<BTreeMap<i64, bool>>>,
}

impl MockUpstream {
    fn new(pool: PgPool) -> Self {
        Self {
            pool,
            calls: Arc::new(Mutex::new(Vec::new())),
            gates: Arc::new(Mutex::new(BTreeMap::new())),
            apply_groups: Arc::new(AtomicBool::new(true)),
            apply_updates: Arc::new(AtomicBool::new(true)),
            bulk_mode: Arc::new(Mutex::new(BulkMode::Confirmed)),
            proxy_active: Arc::new(AtomicBool::new(true)),
            diagnostic_status: Arc::new(AtomicI64::new(200)),
            group_status: Arc::new(AtomicI64::new(200)),
            update_status: Arc::new(AtomicI64::new(200)),
            diagnostic_results: Arc::new(Mutex::new(BTreeMap::new())),
            grok_billing: Arc::new(Mutex::new(BTreeMap::new())),
            sse_chunks: Arc::new(
                fixtures()["python_oracle"]["desktop_actions"]["sse_chunks"]
                    .as_array()
                    .expect("pinned Python SSE chunks")
                    .iter()
                    .map(|value| value.as_str().expect("SSE chunk string").to_owned())
                    .collect(),
            ),
            manual_mode: Arc::new(Mutex::new(ManualMode::Confirmed)),
            connection_streams: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn matching(&self, method: Method, path: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| call.method == method && call.path == path)
            .collect()
    }

    fn gate(&self, path: &str) -> Arc<CallGate> {
        let gate = Arc::new(CallGate::default());
        self.gates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_owned(), gate.clone());
        gate
    }

    fn ungate(&self, path: &str) {
        self.gates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(path);
    }

    fn set_bulk_mode(&self, mode: BulkMode) {
        *self.bulk_mode.lock().unwrap_or_else(|e| e.into_inner()) = mode;
    }

    fn set_diagnostic_status(&self, status: i64) {
        self.diagnostic_status.store(status, Ordering::Release);
    }

    fn set_group_status(&self, status: i64) {
        self.group_status.store(status, Ordering::Release);
    }

    fn set_update_status(&self, status: i64) {
        self.update_status.store(status, Ordering::Release);
    }

    fn set_diagnostic_result(&self, action: &str, result: Value) {
        self.diagnostic_results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(action.to_owned(), result);
    }

    fn set_grok_billing(&self, account_id: i64, billing: Value) {
        self.grok_billing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account_id, billing);
    }

    fn set_proxy_active(&self, active: bool) {
        self.proxy_active.store(active, Ordering::Release);
    }
}

fn response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

fn id_from_path(path: &str, suffix: &str) -> Option<i64> {
    path.strip_prefix("/api/v1/admin/accounts/")?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

async fn update_account(pool: &PgPool, id: i64, body: &Value) -> Result<(), sqlx::Error> {
    if let Some(value) = body.get("priority").and_then(Value::as_i64) {
        sqlx::query("UPDATE accounts SET priority=$1,updated_at=now() WHERE id=$2")
            .bind(value as i32)
            .bind(id)
            .execute(pool)
            .await?;
    }
    if let Some(value) = body.get("concurrency").and_then(Value::as_i64) {
        sqlx::query("UPDATE accounts SET concurrency=$1,updated_at=now() WHERE id=$2")
            .bind(value as i32)
            .bind(id)
            .execute(pool)
            .await?;
    }
    if let Some(value) = body.get("load_factor") {
        let value = value.as_i64().map(|value| value as i32);
        sqlx::query("UPDATE accounts SET load_factor=$1,updated_at=now() WHERE id=$2")
            .bind(value)
            .bind(id)
            .execute(pool)
            .await?;
    }
    if let Some(value) = body.get("schedulable").and_then(Value::as_bool) {
        sqlx::query("UPDATE accounts SET schedulable=$1,updated_at=now() WHERE id=$2")
            .bind(value)
            .bind(id)
            .execute(pool)
            .await?;
    }
    if let Some(value) = body.get("proxy_id").and_then(Value::as_i64) {
        if value == 0 {
            sqlx::query("UPDATE accounts SET proxy_id=NULL,updated_at=now() WHERE id=$1")
                .bind(id)
                .execute(pool)
                .await?;
        } else {
            sqlx::query("UPDATE accounts SET proxy_id=$1,updated_at=now() WHERE id=$2")
                .bind(value)
                .bind(id)
                .execute(pool)
                .await?;
        }
    }
    if let Some(groups) = body.get("group_ids").and_then(Value::as_array) {
        let values: Vec<i64> = groups.iter().filter_map(Value::as_i64).collect();
        sqlx::query("DELETE FROM account_groups WHERE account_id=$1")
            .bind(id)
            .execute(pool)
            .await?;
        for group_id in values {
            sqlx::query("INSERT INTO account_groups(account_id,group_id) VALUES($1,$2)")
                .bind(id)
                .bind(group_id)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

async fn mutate_bulk(pool: &PgPool, body: &Value) -> Result<Vec<i64>, sqlx::Error> {
    let ids: Vec<i64> = body["account_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .collect();
    if let Some(mapping) = body.pointer("/credentials/model_mapping") {
        for id in &ids {
            sqlx::query(
                "UPDATE accounts SET credentials=jsonb_set(COALESCE(credentials,'{}'::jsonb),'{model_mapping}',$1,true),updated_at=now() WHERE id=$2",
            )
            .bind(SqlJson(mapping.clone()))
            .bind(id)
            .execute(pool)
            .await?;
        }
    }
    if let Some(value) = body.get("proxy_id").and_then(Value::as_i64) {
        for id in &ids {
            update_account(pool, *id, &json!({"proxy_id":value})).await?;
        }
    }
    if body.get("group_ids").is_some() {
        for id in &ids {
            update_account(pool, *id, &json!({"group_ids":body["group_ids"]})).await?;
        }
    }
    Ok(ids)
}

async fn mock_upstream(State(state): State<MockUpstream>, request: Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let uri = request.uri().to_string();
    let api_key = request
        .headers()
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = to_bytes(request.into_body(), 16_000_000)
        .await
        .unwrap_or_default();
    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    state
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Call {
            method: method.clone(),
            path: path.clone(),
            uri,
            body: body.clone(),
            api_key: api_key.clone(),
        });

    let gate = state
        .gates
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&path)
        .cloned();
    if let Some(gate) = gate {
        gate.entered.notify_one();
        gate.release.notified().await;
    }

    if path == "/api/v1/admin/groups/all" && method == Method::GET {
        return if api_key == ADMIN_KEY {
            response(StatusCode::OK, json!({"code":0,"data":[]}))
        } else {
            response(
                StatusCode::UNAUTHORIZED,
                json!({"code":401,"message":"fixture unauthorized"}),
            )
        };
    }
    if path == "/api/v1/admin/proxies/all" && method == Method::GET {
        let status = if state.proxy_active.load(Ordering::Acquire) {
            "active"
        } else {
            "inactive"
        };
        return response(
            StatusCode::OK,
            json!({"code":0,"data":[{
                "id":PROXY_ID,"name":"fixture-private-proxy-name","protocol":"http","status":status,
                "host":"proxy-address.example.invalid","port":8443,
                "username":"fixture-proxy-user","password":"fixture-proxy-password"
            }]}),
        );
    }
    if method == Method::GET && path.ends_with("/models") {
        return response(
            StatusCode::OK,
            json!({"code":0,"data":[
                {"id":"fixture-model-a","name":"fixture-model-a","display_name":"fixture-model-a","type":"text"},
                {"id":"fixture-model-b","name":"fixture-model-b","display_name":"fixture-model-b","type":"text"},
                {"id":"fixture-image-model","name":"fixture-image-model","display_name":"fixture-image-model","type":"image"}
            ]}),
        );
    }

    if let Some(id) = id_from_path(&path, "/usage") {
        if method == Method::GET {
            let billing = state
                .grok_billing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&id)
                .cloned()
                .unwrap_or(Value::Null);
            return response(
                StatusCode::OK,
                json!({"code":0,"data":{"grok_billing":billing}}),
            );
        }
    }

    if let Some(id) = id_from_path(&path, "/models/sync-upstream") {
        if method == Method::POST {
            return response(
                StatusCode::OK,
                json!({"code":0,"data":{"models":["fixture-model-a","bad model","fixture-model-a","fixture-model-b"],"warnings":true}}),
            );
        }
        let _ = id;
    }
    if path.starts_with("/api/v1/admin/proxies/") && method == Method::POST {
        let status = state.diagnostic_status.load(Ordering::Acquire);
        if matches!(status, 404 | 405 | 501) {
            return response(
                StatusCode::from_u16(status as u16).unwrap(),
                json!({"detail":"Not Found"}),
            );
        }
        if !(200..300).contains(&status) {
            return response(
                StatusCode::from_u16(status as u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                json!({"code":status,"message":"fixture diagnostic failure"}),
            );
        }
        let action = path.rsplit('/').next().unwrap_or_default();
        if let Some(data) = state
            .diagnostic_results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(action)
            .cloned()
        {
            return response(StatusCode::OK, json!({"code":0,"data":data}));
        }
        return response(
            StatusCode::OK,
            json!({"code":0,"data":{
                    "success":true,"message":"fixture diagnostic ok","latency_ms":12,
                    "ip_address":"203.0.113.7","city":"Fixture City","region":"Fixture Region",
                    "country":"Fixture Country","country_code":"ZZ",
                    "score":91,"grade":"A","summary":"fixture quality","exit_ip":"203.0.113.7",
                    "base_latency_ms":8,"passed_count":1,"warn_count":0,"failed_count":0,"challenge_count":0,
                    "checked_at":1735689600,"items":[{"target":"https://example.invalid","status":"pass","http_status":200,"latency_ms":12,"message":"fixture pass"}],
                    "address":"secret-address.example.invalid","username":"fixture-private-user","password":"fixture-private-password",
                    "token":"fixture-private-token"
            }}),
        );
    }
    if let Some(id) = id_from_path(&path, "/test") {
        if method == Method::POST {
            let connection = state.connection_streams.lock().unwrap().get(&id).copied();
            if let Some(terminal) = connection {
                let model = body["model_id"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .unwrap_or("fixture-grok-native-model");
                let mut stream = format!(
                    "data: {}\n\ndata: {}\n\n",
                    json!({"type":"test_start","model":model}),
                    json!({"type":"content","text":"fixture-batch-http-private-content"})
                );
                if terminal {
                    stream.push_str(&format!(
                        "data: {}\n\n",
                        json!({"type":"test_complete","success":true})
                    ));
                }
                return Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(Body::from(stream))
                    .unwrap();
            }
            let stream = state
                .sse_chunks
                .iter()
                .map(|text| json!({"type":"content","text":text}))
                .chain(std::iter::once(
                    json!({"type":"test_complete","success":true}),
                ))
                .map(|event| format!("data: {}\n\n", event))
                .collect::<String>();
            let _ = id;
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from(stream))
                .expect("fixture SSE response");
        }
    }
    if path == "/api/v1/admin/accounts/bulk-update" && method == Method::POST {
        let mode = *state.bulk_mode.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(mode, BulkMode::ExplicitUnsupported) {
            return response(StatusCode::NOT_FOUND, json!({"detail":"Not Found"}));
        }
        if matches!(mode, BulkMode::GenericNotFound) {
            return response(
                StatusCode::NOT_FOUND,
                json!({"code":404,"message":"fixture object missing"}),
            );
        }
        let mut ids: Vec<i64> = body["account_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect();
        if matches!(mode, BulkMode::MalformedResultIds) {
            return response(
                StatusCode::OK,
                json!({"code":0,"data":{
                    "success":ids.len(),"failed":0,"success_ids":ids,"failed_ids":[],
                    "total":ids.len(),
                    "results":ids.iter().map(|id|json!({"account_id":id.to_string(),"success":true})).collect::<Vec<_>>(),
                    "errors":[]
                }}),
            );
        }
        if matches!(mode, BulkMode::MalformedTotal) {
            return response(
                StatusCode::OK,
                json!({"code":0,"data":{
                    "success":ids.len(),"failed":0,"success_ids":ids,"failed_ids":[],
                    "total":ids.len()+1,
                    "results":ids.iter().map(|id|json!({"account_id":id,"success":true})).collect::<Vec<_>>(),
                    "errors":[]
                }}),
            );
        }
        if matches!(mode, BulkMode::PartialSecond) && !ids.is_empty() {
            let failed = ids.split_off(1);
            let mut subset = body.clone();
            subset["account_ids"] = json!(ids);
            if mutate_bulk(&state.pool, &subset).await.is_err() {
                return response(StatusCode::INTERNAL_SERVER_ERROR, json!({"code":1}));
            }
            return response(
                StatusCode::OK,
                json!({"code":0,"data":{
                    "success":ids.len(),"failed":failed.len(),"success_ids":ids,"failed_ids":failed,
                    "results":ids.iter().map(|id|json!({"account_id":id,"success":true}))
                        .chain(failed.iter().map(|id|json!({"account_id":id,"success":false,"error":"fixture per-account failure"})))
                        .collect::<Vec<_>>()
                }}),
            );
        }
        let apply = !matches!(mode, BulkMode::UnknownNotAppliedGatewayTimeout);
        let ids = if apply {
            match mutate_bulk(&state.pool, &body).await {
                Ok(ids) => ids,
                Err(_) => return response(StatusCode::INTERNAL_SERVER_ERROR, json!({"code":1})),
            }
        } else {
            ids
        };
        if matches!(
            mode,
            BulkMode::UnknownAppliedGatewayTimeout | BulkMode::UnknownNotAppliedGatewayTimeout
        ) {
            return response(
                StatusCode::GATEWAY_TIMEOUT,
                json!({"code":1,"message":"fixture write result is unknown"}),
            );
        }
        let results: Vec<_> = ids
            .iter()
            .map(|id| json!({"account_id":id,"success":true}))
            .collect();
        return response(
            StatusCode::OK,
            json!({"code":0,"data":{"success":ids.len(),"failed":0,"success_ids":ids,"failed_ids":[],"results":results}}),
        );
    }
    if path == "/api/v1/admin/accounts/batch-delete" && method == Method::POST {
        let ids: Vec<i64> = body["account_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect();
        let _ = sqlx::query("DELETE FROM account_groups WHERE account_id=ANY($1)")
            .bind(&ids)
            .execute(&state.pool)
            .await;
        let _ = sqlx::query("DELETE FROM accounts WHERE id=ANY($1)")
            .bind(&ids)
            .execute(&state.pool)
            .await;
        return response(
            StatusCode::OK,
            json!({"code":0,"data":{"success":ids.len(),"failed":0,"success_ids":ids,"failed_ids":[],"results":ids.iter().map(|id|json!({"account_id":id,"success":true})).collect::<Vec<_>>()}}),
        );
    }
    let manual_id = if method == Method::POST {
        id_from_path(&path, "/schedulable").or_else(|| id_from_path(&path, "/recover-state"))
    } else if method == Method::DELETE {
        id_from_path(&path, "")
    } else {
        None
    };
    if let Some(id) = manual_id {
        let mode = *state.manual_mode.lock().unwrap();
        if matches!(mode, ManualMode::Rejected) {
            return response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"code":500,"message":"fixture-private-upstream-failure"}),
            );
        }
        if !matches!(mode, ManualMode::Unchanged) {
            if method == Method::DELETE {
                sqlx::query("DELETE FROM account_groups WHERE account_id=$1")
                    .bind(id)
                    .execute(&state.pool)
                    .await
                    .unwrap();
                sqlx::query("DELETE FROM accounts WHERE id=$1")
                    .bind(id)
                    .execute(&state.pool)
                    .await
                    .unwrap();
            } else if path.ends_with("/recover-state") {
                sqlx::query("UPDATE accounts SET status='active',rate_limit_reset_at=NULL,temp_unschedulable_until=NULL,overload_until=NULL,error_message=NULL,extra='{}',updated_at=now() WHERE id=$1")
                    .bind(id).execute(&state.pool).await.unwrap();
            } else {
                update_account(&state.pool, id, &body).await.unwrap();
            }
        }
        if matches!(mode, ManualMode::DisconnectedApplied) {
            return Response::builder()
                .status(200)
                .body(Body::from_stream(futures_util::stream::once(async {
                    Err::<Vec<u8>, std::io::Error>(std::io::Error::other("fixture disconnected"))
                })))
                .unwrap();
        }
        return response(
            StatusCode::OK,
            json!({"code":0,"data":{
            "id":id,"schedulable":body["schedulable"],"credentials":"fixture-private-upstream-response"}}),
        );
    }
    if let Some(id) = id_from_path(&path, "") {
        if method == Method::PUT {
            if body.get("group_ids").is_some() {
                if state.apply_groups.load(Ordering::Acquire)
                    && update_account(&state.pool, id, &body).await.is_err()
                {
                    return response(StatusCode::INTERNAL_SERVER_ERROR, json!({"code":1}));
                }
                let status = state.group_status.load(Ordering::Acquire);
                return response(
                    StatusCode::from_u16(status as u16)
                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    json!({"code":if (200..300).contains(&status) {0} else {status},"data":{}}),
                );
            }
            if state.apply_updates.load(Ordering::Acquire)
                && update_account(&state.pool, id, &body).await.is_err()
            {
                return response(StatusCode::INTERNAL_SERVER_ERROR, json!({"code":1}));
            }
            let status = state.update_status.load(Ordering::Acquire);
            return response(
                StatusCode::from_u16(status as u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                json!({"code":if (200..300).contains(&status) {0} else {status},"data":{}}),
            );
        }
    }
    response(
        StatusCode::NOT_FOUND,
        json!({"code":404,"message":"fixture route not found"}),
    )
}

struct RuntimeHandle(Option<Arc<Runtime>>);
impl std::ops::Deref for RuntimeHandle {
    type Target = Arc<Runtime>;
    fn deref(&self) -> &Self::Target {
        self.0
            .as_ref()
            .expect("fixture runtime is present outside restart")
    }
}

struct Harness {
    _temp: tempfile::TempDir,
    admin_connection: Option<PgConnection>,
    database_name: String,
    runtime: RuntimeHandle,
    upstream: MockUpstream,
    client: reqwest::Client,
    base_url: String,
    app_task: Option<JoinHandle<()>>,
    upstream_task: Option<JoinHandle<()>>,
}

impl Harness {
    async fn new() -> Self {
        Self::with_model_setting(Some("fixture-model")).await
    }

    async fn with_model_setting(model: Option<&str>) -> Self {
        let fixture = fixtures();
        assert_eq!(fixture["reference"]["sha"], REFERENCE_SHA);
        let runner_database_url =
            std::env::var("DATABASE_URL").expect("runner must provide temporary DATABASE_URL");
        let mut database_url = reqwest::Url::parse(&runner_database_url)
            .expect("runner DATABASE_URL must be a PostgreSQL URL");
        assert_eq!(
            database_url.host_str(),
            Some("127.0.0.1"),
            "runner DATABASE_URL must point to loopback PostgreSQL"
        );
        let mut admin_connection = PgConnection::connect(&runner_database_url)
            .await
            .expect("connect to runner-owned temporary PostgreSQL");
        let database_name = format!("account_mutation_{}", uuid::Uuid::new_v4().simple());
        sqlx::raw_sql(&format!("CREATE DATABASE {database_name}"))
            .execute(&mut admin_connection)
            .await
            .expect("create isolated account-mutation fixture database");
        database_url.set_path(&format!("/{database_name}"));
        let mut seed_database_url = database_url.clone();
        seed_database_url
            .query_pairs_mut()
            .append_pair("application_name", "account_mutation_seed");
        let mut seed_connection = PgConnection::connect(seed_database_url.as_str())
            .await
            .expect("connect to isolated account-mutation fixture database");
        sqlx::raw_sql(include_str!("../../tests/contracts/http/schema.sql"))
            .execute(&mut seed_connection)
            .await
            .expect("initialize complete HTTP-contract schema and seed rows");
        seed_connection.close().await.expect("close schema writer");
        let mut runtime_database_url = database_url;
        runtime_database_url
            .query_pairs_mut()
            .append_pair("application_name", "account_mutation_runtime");
        let database_url = runtime_database_url.to_string();

        let temp = tempfile::tempdir().expect("create test state directory");
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
            })).unwrap(),
        ).unwrap();
        let upstream_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback upstream");
        let upstream_url = format!("http://{}", upstream_listener.local_addr().unwrap());
        let connection_path = temp.path().join("sub2api-config.json");
        fs::write(
            &connection_path,
            serde_json::to_vec(&json!({"base_url":upstream_url,"verify_base_url":""})).unwrap(),
        )
        .expect("write loopback connection config");
        let runtime = Runtime::new(Settings {
            database_url,
            bind: "127.0.0.1:0".into(),
            base_path: String::new(),
            audit_path: temp.path().join("audit.jsonl"),
            usage_path: temp.path().join("usage-state.json"),
            oauth_path: temp.path().join("oauth-state.json"),
            bark_path: temp.path().join("bark-state.json"),
            fallback_path: temp.path().join("key-fallback.json"),
            connection_path,
            model_path: temp.path().join("group-models.json"),
            oauth: {
                let mut config = json!({
                "oauth_recovery_monitor_enabled":false,"oauth_daily_test_enabled":false,
                "oauth_auto_reset_credit_enabled":false,"oauth_daily_test_time":"05:00",
                "oauth_usage_refresh_concurrency":2,"oauth_recovery_test_concurrency":2,
                "oauth_early_probe_batch_size":8,"oauth_recovery_test_model_id":"fixture-model",
                "oauth_recovery_connection_account_ids":[],"oauth_recovery_model_account_ids":[]
                });
                if let Some(model) = model {
                    config["oauth_recovery_test_model_id"] = model.into();
                } else {
                    config
                        .as_object_mut()
                        .unwrap()
                        .remove("oauth_recovery_test_model_id");
                }
                config
            },
            bark: json!({"enabled":false}),
            bark_valid: true,
            base_url: upstream_url,
            verify_url: String::new(),
            read_only: false,
        })
        .await
        .expect("construct Rust runtime against temporary PostgreSQL");
        let upstream = MockUpstream::new(runtime.db.pool.clone());
        let upstream_state = upstream.clone();
        let upstream_task = tokio::spawn(async move {
            axum::serve(
                upstream_listener,
                Router::new()
                    .fallback(any(mock_upstream))
                    .with_state(upstream_state),
            )
            .await
            .expect("serve loopback upstream fixture");
        });
        let app_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind Rust desktop API");
        let app_url = format!("http://{}", app_listener.local_addr().unwrap());
        let api_runtime = runtime.clone();
        let app_task = tokio::spawn(async move {
            axum::serve(app_listener, api::router(api_runtime))
                .await
                .expect("serve Rust desktop API");
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("construct no-proxy fixture client");
        Self {
            _temp: temp,
            admin_connection: Some(admin_connection),
            database_name,
            runtime: RuntimeHandle(Some(runtime)),
            upstream,
            client,
            base_url: app_url,
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
        self.request_with_key(method, path, body, ADMIN_KEY).await
    }

    async fn request_with_key(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        key: &str,
    ) -> (u16, Value, String) {
        let mut request = self
            .client
            .request(method, format!("{}/api/desktop/v1/{path}", self.base_url))
            .header("x-api-key", key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.expect("call local Rust desktop route");
        let status = response.status().as_u16();
        let bytes = response.bytes().await.expect("read local API response");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value, text)
    }

    async fn insert_group(&self, platform: &str, enabled: bool, models: &[&str]) -> i64 {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed) + 100_000_000;
        sqlx::query("INSERT INTO groups(id,name,platform,model_allowlist) VALUES($1,$2,$3,$4)")
            .bind(id)
            .bind(format!("fixture-group-{id}"))
            .bind(platform)
            .bind(SqlJson(json!({"enabled":enabled,"models":models})))
            .execute(&self.runtime.db.pool)
            .await
            .expect("insert synthetic group");
        id
    }

    async fn insert_account(
        &self,
        platform: &str,
        account_type: &str,
        mapping: Value,
        extra: Value,
        parent: Option<i64>,
        group_ids: &[i64],
    ) -> i64 {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        sqlx::query(
            "INSERT INTO accounts(id,name,platform,type,status,schedulable,credentials,extra,parent_account_id,priority,concurrency,load_factor) VALUES($1,$2,$3,$4,'active',true,$5,$6,$7,3,2,NULL)",
        )
        .bind(id)
        .bind(format!("fixture-account-{id}"))
        .bind(platform)
        .bind(account_type)
        .bind(SqlJson(json!({"access_token":"fixture-account-secret","model_mapping":mapping})))
        .bind(SqlJson(extra))
        .bind(parent)
        .execute(&self.runtime.db.pool)
        .await
        .expect("insert synthetic account");
        for group_id in group_ids {
            sqlx::query("INSERT INTO account_groups(account_id,group_id) VALUES($1,$2)")
                .bind(id)
                .bind(group_id)
                .execute(&self.runtime.db.pool)
                .await
                .expect("link synthetic account group");
        }
        id
    }

    async fn account(&self, id: i64) -> Value {
        self.runtime
            .actions
            .account(id, None)
            .await
            .expect("read synthetic account through real projection")
    }

    async fn linked_groups(&self, id: i64) -> Vec<i64> {
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT group_id FROM account_groups WHERE account_id=$1 ORDER BY group_id",
        )
        .bind(id)
        .fetch_all(&self.runtime.db.pool)
        .await
        .expect("read persisted account group links");
        rows.into_iter().map(|row| row.0).collect()
    }

    async fn wait_job(&self, id: &str, terminal: &[&str]) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let job = self
                    .runtime
                    .operations
                    .get(id, 0)
                    .expect("read operation job");
                if terminal.contains(&job["status"].as_str().unwrap_or("")) {
                    return job;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("operation should reach requested terminal state")
    }

    async fn wait_batch(&self, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let batch = self
                    .runtime
                    .operations
                    .listing(Some(id))
                    .expect("read durable batch listing");
                let items = batch["items"].as_array().expect("batch has items");
                if items.iter().all(|item| {
                    !matches!(
                        item["status"].as_str().unwrap_or(""),
                        "queued" | "running" | "checking"
                    )
                }) {
                    return batch;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            let batch = self.runtime.operations.listing(Some(id)).unwrap();
            let states: Vec<_> = batch["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| json!({"status":item["status"],"reason":item["reason"]}))
                .collect();
            panic!("native batch should reach a terminal state: {states:?}")
        })
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
            .expect("fixture administration connection is open until finish");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let remaining_sessions: Vec<(String, String, i64)> = loop {
            let sessions = sqlx::query_as(
                "SELECT COALESCE(application_name, 'unknown'), COALESCE(state, 'unknown'), COUNT(*) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid() AND backend_type = 'client backend' GROUP BY COALESCE(application_name, 'unknown'), COALESCE(state, 'unknown') ORDER BY 1, 2",
            )
            .bind(&self.database_name)
            .fetch_all(&mut *admin)
            .await
            .expect("inspect isolated fixture database sessions before removal");
            if sessions.is_empty() || tokio::time::Instant::now() >= deadline {
                break sessions;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let remaining_count: i64 = remaining_sessions.iter().map(|(_, _, count)| count).sum();
        assert_eq!(
            remaining_count, 0,
            "isolated fixture database still has {remaining_count} session(s) after pool closure (application/state counts: {remaining_sessions:?})"
        );
        sqlx::raw_sql(&format!("DROP DATABASE {}", self.database_name))
            .execute(&mut *admin)
            .await
            .expect("drop isolated account-mutation fixture database");
        let database_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
                .bind(&self.database_name)
                .fetch_one(&mut *admin)
                .await
                .expect("verify isolated fixture database removal");
        assert!(!database_exists, "fixture database should be removed");
        self.admin_connection
            .take()
            .unwrap()
            .close()
            .await
            .expect("close fixture administration connection");
    }

    async fn restart_api(&mut self) {
        if let Some(task) = self.app_task.take() {
            task.abort();
            let _ = task.await;
        }
        let settings = (*self.runtime.settings).clone();
        self.runtime.close().await;
        drop(self.runtime.0.take());
        self.runtime.0 = Some(Runtime::new(settings).await.unwrap());
        // The old fixture pool closes with the old Runtime; native handlers
        // use the new pool after the restart as the real database survives.
        if let Some(task) = self.upstream_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.upstream.pool = self.runtime.db.pool.clone();
        let native_url = self.runtime.settings.base_url.clone();
        let native_listener = TcpListener::bind(native_url.strip_prefix("http://").unwrap())
            .await
            .unwrap();
        let native = self.upstream.clone();
        self.upstream_task = Some(tokio::spawn(async move {
            axum::serve(
                native_listener,
                Router::new()
                    .fallback(any(mock_upstream))
                    .with_state(native),
            )
            .await
            .unwrap();
        }));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        self.base_url = format!("http://{}", listener.local_addr().unwrap());
        let runtime = self.runtime.clone();
        self.app_task = Some(tokio::spawn(async move {
            axum::serve(listener, api::router(runtime)).await.unwrap();
        }));
    }

    async fn connection_batch(&self, ids: &[i64], request_id: &str) -> Value {
        let mut accounts = Vec::new();
        for id in ids {
            accounts.push(json!({"account_id":id,"expected_version":operation_version(&self.account(*id).await,"test")}));
        }
        native_batch("test", json!({"confirmed":true}), accounts, request_id)
    }

    async fn connection_accounts(&self) -> Vec<i64> {
        let mut ids = Vec::new();
        for (platform, kind) in [
            ("openai", "oauth"),
            ("grok", "apikey"),
            ("openai", "apikey"),
        ] {
            let id = self
                .insert_account(platform, kind, json!({}), json!({}), None, &[])
                .await;
            self.upstream
                .connection_streams
                .lock()
                .unwrap()
                .insert(id, true);
            ids.push(id);
        }
        ids
    }

    async fn batch_listing(&self, batch_id: &str) -> Value {
        let result = self
            .request(
                Method::GET,
                &format!("account-operations?batch_id={batch_id}"),
                None,
            )
            .await;
        assert_eq!(result.0, 200);
        result.1
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

fn operation_version(row: &Value, action: &str) -> String {
    operation_versions(row, false)[action]
        .as_str()
        .expect("field version should be text")
        .to_owned()
}

fn managed_operation_version(row: &Value, action: &str, managed: bool) -> String {
    operation_versions(row, managed)[action]
        .as_str()
        .expect("managed field version should be text")
        .to_owned()
}

fn single_operation(action: &str, payload: Value, version: String, request_id: &str) -> Value {
    json!({
        "action":action,"payload":payload,"expected_version":version,
        "request_id":request_id,"client_id":CLIENT_ID
    })
}

fn native_batch(action: &str, payload: Value, accounts: Vec<Value>, request_id: &str) -> Value {
    json!({
        "action":action,"payload":payload,"accounts":accounts,
        "request_id":request_id,"client_id":CLIENT_ID
    })
}

fn template_batch(
    template_id: &str,
    template_version: &Value,
    accounts: Vec<Value>,
    request_id: &str,
) -> Value {
    json!({
        "template_id":template_id,"template_version":template_version,"accounts":accounts,
        "request_id":request_id,"client_id":"0123456789abcdef0123456789abcdef"
    })
}

fn sse_events(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn profile_in_pg_order(mapping: &Value) -> Value {
    let mut whitelist = Vec::new();
    let mut mappings = Vec::new();
    for (source, target) in mapping
        .as_object()
        .expect("PostgreSQL model mapping object")
    {
        if target.as_str() == Some(source.as_str()) {
            whitelist.push(source.clone());
        } else {
            mappings.push(json!({"source":source,"target":target}));
        }
    }
    json!({"whitelist":whitelist,"mappings":mappings})
}

#[tokio::test]
async fn database_close_waits_for_a_connection_returning_after_release() {
    let mut h = Harness::new().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let callback_entered = Arc::clone(&entered);
    let callback_release = Arc::clone(&release);
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .after_release(move |_connection, _metadata| {
            let entered = Arc::clone(&callback_entered);
            let release = Arc::clone(&callback_release);
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                Ok(true)
            })
        })
        .connect(&h.runtime.settings.database_url)
        .await
        .expect("connect controlled pool to the Harness database");

    let mut database = h.runtime.db.clone();
    database.pool = pool.clone();
    drop(pool.acquire().await.expect("acquire controlled connection"));
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("connection return should enter after_release");

    let close_task = tokio::spawn(async move { database.close().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !pool.is_closed() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Database::close should mark the controlled pool closed");
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), close_task)
        .await
        .expect("Database::close should finish after the connection returns")
        .expect("Database::close task should not panic");

    assert_eq!(
        pool.size(),
        0,
        "Database::close must close a connection returned after the pool closes"
    );
    h.finish().await;
}

fn obstruct_operation_store_lock(
    store: &OperationStore,
    scratch: &std::path::Path,
) -> (std::path::PathBuf, std::path::PathBuf) {
    store
        .transaction(|_| Ok(()))
        .expect("materialize the synthetic operation-store lock file");
    let lock = store.0.path().with_extension("lock");
    let backup = scratch.join(format!("operation-lock-{}", uuid::Uuid::new_v4().simple()));
    fs::rename(&lock, &backup).expect("move only this fixture's operation lock");
    fs::create_dir(&lock).expect("make the operation lock path fail open as a file");
    (lock, backup)
}

fn restore_operation_store_lock(lock: &std::path::Path, backup: &std::path::Path) {
    fs::remove_dir(lock).expect("remove only this fixture's lock obstruction");
    fs::rename(backup, lock).expect("restore this fixture's operation lock");
}

#[tokio::test]
async fn group_management_http_preserves_scope_rejects_stale_targets_and_requires_readback() {
    let mut harness = Harness::new().await;
    let group_a = harness.insert_group("openai", false, &[]).await;
    let group_b = harness.insert_group("openai", false, &[]).await;
    let outside = harness.insert_group("openai", false, &[]).await;
    let wrong_platform = harness.insert_group("grok", false, &[]).await;
    let account_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({}),
            json!({}),
            None,
            &[group_a, outside],
        )
        .await;
    let row = harness.account(account_id).await;
    let path = format!("accounts/{account_id}/groups");
    let (status, result, _) = harness
        .request(
            Method::PUT,
            &path,
            Some(json!({
                "expected_version":account_version(&row),
                "scope_group_ids":[group_a,group_b],"group_ids":[group_b]
            })),
        )
        .await;
    assert_eq!(
        status, 200,
        "valid scoped group update should be verified: {result}"
    );
    assert_eq!(result["group_ids"], json!([group_b, outside]));
    assert_eq!(
        harness.linked_groups(account_id).await,
        vec![group_b, outside]
    );
    let writes = harness
        .upstream
        .matching(Method::PUT, &format!("/api/v1/admin/accounts/{account_id}"));
    assert_eq!(
        writes.len(),
        1,
        "one scoped edit must create one upstream PUT"
    );
    assert_eq!(writes[0].body, json!({"group_ids":[group_b,outside]}));
    assert_eq!(writes[0].api_key, ADMIN_KEY);

    let live = harness.account(account_id).await;
    let fresh = account_version(&live);
    let bad_platform = harness
        .request(
            Method::PUT,
            &path,
            Some(json!({
                "expected_version":fresh,"scope_group_ids":[wrong_platform],"group_ids":[wrong_platform]
            })),
        )
        .await;
    assert_eq!(bad_platform.0, 409);
    let stale = harness
        .request(
            Method::PUT,
            &path,
            Some(json!({
                "expected_version":"0".repeat(64),"scope_group_ids":[group_a],"group_ids":[]
            })),
        )
        .await;
    assert_eq!(stale.0, 409);
    let empty_scope = harness
        .request(
            Method::PUT,
            &path,
            Some(json!({
                "expected_version":fresh,"scope_group_ids":[],"group_ids":[]
            })),
        )
        .await;
    assert_eq!(empty_scope.0, 422);
    assert_eq!(
        harness
            .upstream
            .matching(Method::PUT, &format!("/api/v1/admin/accounts/{account_id}"))
            .len(),
        1,
        "stale versions, foreign groups and empty scopes must be rejected before PUT"
    );

    harness
        .upstream
        .apply_groups
        .store(false, Ordering::Release);
    let unchanged = harness.account(account_id).await;
    let mismatch = harness
        .request(
            Method::PUT,
            &path,
            Some(json!({
                "expected_version":account_version(&unchanged),
                "scope_group_ids":[group_a,group_b],"group_ids":[group_a]
            })),
        )
        .await;
    assert_eq!(
        mismatch.0, 502,
        "2xx without database readback is unconfirmed"
    );
    assert_eq!(
        harness.linked_groups(account_id).await,
        vec![group_b, outside]
    );
    assert_eq!(
        harness
            .upstream
            .matching(Method::PUT, &format!("/api/v1/admin/accounts/{account_id}"))
            .len(),
        2,
        "an unconfirmed group PUT must not be replayed"
    );
    harness.finish().await;
}

#[tokio::test]
async fn settings_http_projects_versions_and_emits_exact_manual_write_payloads() {
    let mut harness = Harness::new().await;
    let account_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let (status, settings, settings_text) = harness
        .request(
            Method::GET,
            &format!("accounts/{account_id}/settings"),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(settings["priority"], 3);
    assert_eq!(settings["concurrency"], 2);
    assert_eq!(settings["effective_load_factor"], 2);
    assert_eq!(settings["capabilities"]["model_config"]["editable"], true);
    assert!(!settings_text.contains("fixture-account-secret"));
    assert!(!settings_text.contains("credentials"));

    let before = harness.account(account_id).await;
    let before_version = operation_version(&before, "priority");
    sqlx::query("UPDATE accounts SET name=name || '-passive',updated_at=now(),extra=extra || '{\"observed\":true}'::jsonb WHERE id=$1")
        .bind(account_id)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("apply passive observation to fixture account");
    let passive = harness.account(account_id).await;
    assert_eq!(operation_version(&passive, "priority"), before_version);
    sqlx::query("UPDATE accounts SET priority=7 WHERE id=$1")
        .bind(account_id)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("apply relevant fixture change");
    let changed = harness.account(account_id).await;
    assert_ne!(operation_version(&changed, "priority"), before_version);

    let expected = account_version(&changed);
    let (status, result, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/priority"),
            Some(json!({"expected_version":expected,"priority":5})),
        )
        .await;
    assert_eq!(
        status, 200,
        "manual priority write should be read back: {result}"
    );
    assert_eq!(result["priority"], 5);
    assert_eq!(result["verified"], true);
    let priority_path = format!("/api/v1/admin/accounts/{account_id}");
    let priority_writes = harness.upstream.matching(Method::PUT, &priority_path);
    assert_eq!(priority_writes.len(), 1);
    assert_eq!(priority_writes[0].body, json!({"priority":5}));

    let (status, proxies, proxy_text) = harness.request(Method::GET, "proxies", None).await;
    assert_eq!(status, 200);
    assert_eq!(proxies[0].as_object().unwrap().len(), 4);
    for private in [
        "host",
        "port",
        "username",
        "password",
        "proxy-address.example.invalid",
        "fixture-proxy-password",
    ] {
        assert!(!proxy_text.contains(private), "proxy DTO leaked {private}");
    }
    assert_eq!(
        harness
            .upstream
            .matching(Method::GET, "/api/v1/admin/proxies/all")
            .len(),
        1
    );
    harness.finish().await;
}

#[tokio::test]
async fn settings_queue_waits_for_parent_lock_then_writes_once_and_supersedes_idempotently() {
    let mut harness = Harness::new().await;
    let parent_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let child_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({}),
            json!({}),
            Some(parent_id),
            &[],
        )
        .await;
    let independent_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let parent_lease = harness.runtime.actions.locks.account(parent_id).await;
    let child_row = harness.account(child_id).await;
    let independent_row = harness.account(independent_id).await;
    let child_request = |version: String, priority: i64, request_id: &str| {
        json!({
            "action":"priority","payload":{"priority":priority},
            "expected_version":version,"request_id":request_id,"client_id":CLIENT_ID
        })
    };
    let child_version = operation_version(&child_row, "priority");
    let request_id = "fixture-account-queue-child-0001";
    let failed_request = child_request(
        operation_version(&independent_row, "priority"),
        4,
        "fixture-account-operation-save-failure-0001",
    );
    let (lock_path, lock_backup) =
        obstruct_operation_store_lock(&harness.runtime.operations.store, harness._temp.path());
    let failed_accept = harness
        .request(
            Method::POST,
            &format!("accounts/{independent_id}/operations"),
            Some(failed_request),
        )
        .await;
    restore_operation_store_lock(&lock_path, &lock_backup);
    assert_eq!(failed_accept.0, 500);
    assert!(harness.runtime.operations.store.read().unwrap()["jobs"]
        .as_object()
        .unwrap()
        .is_empty());
    assert!(harness
        .upstream
        .matching(
            Method::PUT,
            &format!("/api/v1/admin/accounts/{independent_id}")
        )
        .is_empty());

    let (status, first, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{child_id}/operations"),
            Some(child_request(child_version.clone(), 5, request_id)),
        )
        .await;
    assert_eq!(status, 202);
    let (status, duplicate, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{child_id}/operations"),
            Some(child_request(child_version.clone(), 5, request_id)),
        )
        .await;
    assert_eq!(status, 202);
    assert_eq!(first["id"], duplicate["id"]);
    let conflict = harness
        .request(
            Method::POST,
            &format!("accounts/{child_id}/operations"),
            Some(child_request(child_version.clone(), 6, request_id)),
        )
        .await;
    assert_eq!(
        conflict.0, 409,
        "an idempotency key cannot change its payload"
    );

    let reopened = Operations::new(
        harness.runtime.actions.clone(),
        harness.runtime.operations.backend.clone(),
        harness.runtime.operations.budget.clone(),
    )
    .expect("reopen the persisted Rust operation service");
    let recovered_duplicate = reopened
        .submit(
            child_id,
            &child_request(child_version.clone(), 5, request_id),
            ADMIN_KEY,
        )
        .await
        .unwrap();
    assert_eq!(recovered_duplicate["id"], first["id"]);

    let independent_idempotency = "fixture-account-queue-independent-0001";
    let (status, independent, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{independent_id}/operations"),
            Some(child_request(
                operation_version(&independent_row, "priority"),
                4,
                independent_idempotency,
            )),
        )
        .await;
    assert_eq!(status, 202);
    let resumed_independent = reopened
        .submit(
            independent_id,
            &child_request(
                operation_version(&independent_row, "priority"),
                4,
                independent_idempotency,
            ),
            ADMIN_KEY,
        )
        .await
        .unwrap();
    assert_eq!(resumed_independent["id"], independent["id"]);
    assert_eq!(
        harness.runtime.operations.store.read().unwrap()["jobs"]
            .as_object()
            .unwrap()
            .len(),
        2,
        "reopened idempotent submissions must not create duplicate children"
    );
    sqlx::query("UPDATE accounts SET name=name || '-passive',updated_at=now(),extra=extra || '{\"observed\":true}'::jsonb WHERE id=$1")
        .bind(child_id)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("change only passive fields while the queued child waits");
    assert!(harness
        .upstream
        .matching(Method::PUT, &format!("/api/v1/admin/accounts/{child_id}"))
        .is_empty());
    reopened.tick().await.unwrap();
    let independent_done = harness
        .wait_job(independent["id"].as_str().unwrap(), &["completed"])
        .await;
    assert_eq!(independent_done["status"], "completed");
    assert_eq!(
        harness
            .runtime
            .operations
            .get(first["id"].as_str().unwrap(), 0)
            .unwrap()["status"],
        "queued",
        "a child operation must wait while its parent credential lock is held"
    );
    assert_eq!(
        harness
            .upstream
            .matching(
                Method::PUT,
                &format!("/api/v1/admin/accounts/{independent_id}")
            )
            .len(),
        1,
        "an unrelated account should continue while the child waits"
    );
    drop(parent_lease);
    reopened.tick().await.unwrap();
    let child_done = harness
        .wait_job(first["id"].as_str().unwrap(), &["completed"])
        .await;
    assert_eq!(child_done["status"], "completed");
    assert_eq!(
        harness
            .upstream
            .matching(Method::PUT, &format!("/api/v1/admin/accounts/{child_id}"))
            .len(),
        1,
        "queued child write dispatches exactly once after parent unlock"
    );

    let child_live = harness.account(child_id).await;
    let unchanged = harness
        .request(
            Method::POST,
            &format!("accounts/{child_id}/operations"),
            Some(single_operation(
                "priority",
                json!({"priority":5}),
                operation_version(&child_live, "priority"),
                "fixture-priority-already-satisfied-0001",
            )),
        )
        .await;
    assert_eq!(unchanged.0, 202);
    harness.runtime.operations.tick().await.unwrap();
    let unchanged_done = harness
        .wait_job(unchanged.1["id"].as_str().unwrap(), &["completed"])
        .await;
    assert_eq!(unchanged_done["result"]["verified"], true);
    assert_eq!(
        harness
            .upstream
            .matching(Method::PUT, &format!("/api/v1/admin/accounts/{child_id}"))
            .len(),
        1,
        "an already satisfied priority intent is verified without another write"
    );

    let independent_lease = harness.runtime.actions.locks.account(independent_id).await;
    let current_independent = harness.account(independent_id).await;
    let independent_version = operation_version(&current_independent, "priority");
    let original_request_id = "fixture-priority-supersede-a-0001";
    let first_request = single_operation(
        "priority",
        json!({"priority":7}),
        independent_version.clone(),
        original_request_id,
    );
    let (status, queued_first, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{independent_id}/operations"),
            Some(first_request.clone()),
        )
        .await;
    assert_eq!(status, 202);
    let (status, queued_duplicate, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{independent_id}/operations"),
            Some(first_request),
        )
        .await;
    assert_eq!(status, 202);
    assert_eq!(queued_duplicate["id"], queued_first["id"]);
    let changed_payload = harness
        .request(
            Method::POST,
            &format!("accounts/{independent_id}/operations"),
            Some(single_operation(
                "priority",
                json!({"priority":8}),
                independent_version.clone(),
                original_request_id,
            )),
        )
        .await;
    assert_eq!(changed_payload.0, 409);
    let (status, superseding, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{independent_id}/operations"),
            Some(single_operation(
                "priority",
                json!({"priority":9}),
                independent_version,
                "fixture-priority-supersede-b-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    assert_eq!(
        harness
            .runtime
            .operations
            .get(queued_first["id"].as_str().unwrap(), 0)
            .unwrap()["status"],
        "superseded"
    );
    let (status, cancelled, _) = harness
        .request(
            Method::POST,
            &format!(
                "account-operations/{}/cancel",
                superseding["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(cancelled["status"], "cancelled");
    drop(independent_lease);
    harness.runtime.operations.tick().await.unwrap();
    assert_eq!(
        harness
            .upstream
            .matching(
                Method::PUT,
                &format!("/api/v1/admin/accounts/{independent_id}")
            )
            .len(),
        1,
        "superseded and cancelled queued priorities must not dispatch"
    );

    let stale_child_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({}),
            json!({}),
            Some(parent_id),
            &[],
        )
        .await;
    let stale_lease = harness.runtime.actions.locks.account(parent_id).await;
    let stale_row = harness.account(stale_child_id).await;
    let stale_request = single_operation(
        "priority",
        json!({"priority":14}),
        operation_version(&stale_row, "priority"),
        "fixture-priority-changed-under-parent-lease-0001",
    );
    let (status, stale, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{stale_child_id}/operations"),
            Some(stale_request.clone()),
        )
        .await;
    assert_eq!(status, 202);
    sqlx::query("UPDATE accounts SET priority=15 WHERE id=$1")
        .bind(stale_child_id)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("change the versioned priority before releasing its parent lease");
    drop(stale_lease);
    let reopened_stale = reopened
        .submit(stale_child_id, &stale_request, ADMIN_KEY)
        .await
        .unwrap();
    assert_eq!(reopened_stale["id"], stale["id"]);
    reopened.tick().await.unwrap();
    let stale_done = harness
        .wait_job(stale["id"].as_str().unwrap(), &["needs_confirmation"])
        .await;
    assert_eq!(stale_done["status"], "needs_confirmation");
    assert_eq!(
        harness.account(stale_child_id).await["priority"],
        15,
        "a real priority change while queued must preserve the external value"
    );
    assert!(harness
        .upstream
        .matching(
            Method::PUT,
            &format!("/api/v1/admin/accounts/{stale_child_id}")
        )
        .is_empty());
    reopened.close().await;
    harness.finish().await;
}

#[tokio::test]
async fn group_management_auth_schema_deletion_and_error_readback_match_pinned_contract() {
    let mut harness = Harness::new().await;
    let first = harness.insert_group("openai", false, &[]).await;
    let second = harness.insert_group("openai", false, &[]).await;
    let deleted = harness.insert_group("openai", false, &[]).await;
    let account_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[first])
        .await;
    sqlx::query("UPDATE groups SET deleted_at=now() WHERE id=$1")
        .bind(deleted)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("mark a synthetic group deleted");
    let row = harness.account(account_id).await;
    let path = format!("accounts/{account_id}/groups");
    let version = account_version(&row);

    let unauthenticated = harness
        .request_with_key(
            Method::PUT,
            &path,
            Some(json!({
                "expected_version":version,"scope_group_ids":[first,second],"group_ids":[second]
            })),
            "fixture-account-mutation-wrong-key",
        )
        .await;
    assert_eq!(unauthenticated.0, 401);
    assert!(harness
        .upstream
        .matching(Method::PUT, &format!("/api/v1/admin/accounts/{account_id}"))
        .is_empty());

    let extra = harness
        .request(
            Method::PUT,
            &path,
            Some(json!({
                "expected_version":version,"scope_group_ids":[first,second],"group_ids":[second],"future":true
            })),
        )
        .await;
    assert_eq!(extra.0, 422, "unknown group-edit fields are rejected");
    let empty_scope = harness
        .request(
            Method::PUT,
            &path,
            Some(json!({"expected_version":version,"scope_group_ids":[],"group_ids":[]})),
        )
        .await;
    assert_eq!(empty_scope.0, 422);
    let deleted_scope = harness
        .request(
            Method::PUT,
            &path,
            Some(json!({
                "expected_version":version,"scope_group_ids":[deleted],"group_ids":[deleted]
            })),
        )
        .await;
    assert_eq!(deleted_scope.0, 409);
    assert!(harness
        .upstream
        .matching(Method::PUT, &format!("/api/v1/admin/accounts/{account_id}"))
        .is_empty());

    harness.upstream.set_group_status(504);
    let applied_then_error = harness
        .request(
            Method::PUT,
            &path,
            Some(json!({
                "expected_version":version,"scope_group_ids":[first,second],"group_ids":[second]
            })),
        )
        .await;
    assert_eq!(
        applied_then_error.0, 200,
        "readback confirms a write whose upstream request timed out after apply"
    );
    assert_eq!(applied_then_error.1["group_ids"], json!([second]));
    let writes = harness
        .upstream
        .matching(Method::PUT, &format!("/api/v1/admin/accounts/{account_id}"));
    assert_eq!(
        writes.len(),
        1,
        "error after apply must not replay the native PUT"
    );
    assert_eq!(writes[0].body, json!({"group_ids":[second]}));
    assert_eq!(writes[0].api_key, ADMIN_KEY);
    let public = applied_then_error.1.to_string();
    assert!(!public.contains("fixture-account-secret"));
    assert_eq!(harness.linked_groups(account_id).await, vec![second]);

    let wrong_platform_account = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[first])
        .await;
    sqlx::query("UPDATE accounts SET platform='anthropic' WHERE id=$1")
        .bind(wrong_platform_account)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let wrong_row = harness.account(wrong_platform_account).await;
    let wrong_result = harness
        .request(
            Method::PUT,
            &format!("accounts/{wrong_platform_account}/groups"),
            Some(json!({
                "expected_version":account_version(&wrong_row),
                "scope_group_ids":[first],"group_ids":[]
            })),
        )
        .await;
    assert_eq!(
        wrong_result.0, 422,
        "unsupported account platforms cannot edit groups"
    );

    let deleted_account = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[first])
        .await;
    let deleted_row = harness.account(deleted_account).await;
    sqlx::query("UPDATE accounts SET deleted_at=now() WHERE id=$1")
        .bind(deleted_account)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let deleted_result = harness
        .request(
            Method::PUT,
            &format!("accounts/{deleted_account}/groups"),
            Some(json!({
                "expected_version":account_version(&deleted_row),
                "scope_group_ids":[first],"group_ids":[]
            })),
        )
        .await;
    assert_eq!(deleted_result.0, 404);
    assert_eq!(
        harness
            .upstream
            .matching(Method::PUT, &format!("/api/v1/admin/accounts/{account_id}"))
            .len(),
        1,
        "unsupported/deleted account requests must not add an upstream group write"
    );
    harness.finish().await;
}

#[tokio::test]
async fn settings_model_config_proxy_clear_sync_and_numeric_validation_use_exact_contracts() {
    let mut harness = Harness::new().await;
    let account_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-old-model":"fixture-old-model"}),
            json!({"opaque":"fixture-preserved-extra"}),
            None,
            &[],
        )
        .await;

    sqlx::query("UPDATE accounts SET concurrency=8,load_factor=NULL,proxy_id=$1 WHERE id=$2")
        .bind(PROXY_ID)
        .bind(account_id)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("seed a synthetic proxy and automatic load factor");
    let (status, automatic, _) = harness
        .request(
            Method::GET,
            &format!("accounts/{account_id}/settings"),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(automatic["effective_load_factor"], 8);
    sqlx::query("UPDATE accounts SET load_factor=17 WHERE id=$1")
        .bind(account_id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    assert_eq!(
        harness
            .request(
                Method::GET,
                &format!("accounts/{account_id}/settings"),
                None
            )
            .await
            .1["effective_load_factor"],
        17
    );
    sqlx::query("UPDATE accounts SET concurrency=NULL,load_factor=NULL WHERE id=$1")
        .bind(account_id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    assert_eq!(
        harness
            .request(
                Method::GET,
                &format!("accounts/{account_id}/settings"),
                None
            )
            .await
            .1["effective_load_factor"],
        1
    );

    sqlx::query("UPDATE accounts SET load_factor=17 WHERE id=$1")
        .bind(account_id)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("seed a non-null factor before testing an explicit null write");
    let before_null_factor = harness.account(account_id).await;
    let (status, accepted_null_factor, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/operations"),
            Some(single_operation(
                "load_factor",
                json!({"load_factor":null}),
                operation_version(&before_null_factor, "load_factor"),
                "fixture-load-factor-explicit-null-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    harness.runtime.operations.tick().await.unwrap();
    assert_eq!(
        harness
            .wait_job(accepted_null_factor["id"].as_str().unwrap(), &["completed"])
            .await["status"],
        "completed"
    );
    let null_factor_writes = harness
        .upstream
        .matching(Method::PUT, &format!("/api/v1/admin/accounts/{account_id}"));
    assert_eq!(null_factor_writes.len(), 1);
    assert_eq!(null_factor_writes[0].body, json!({"load_factor":null}));
    let persisted_factor: Option<i32> =
        sqlx::query_scalar("SELECT load_factor FROM accounts WHERE id=$1")
            .bind(account_id)
            .fetch_one(&harness.runtime.db.pool)
            .await
            .expect("read explicit nullable load factor from PostgreSQL");
    assert_eq!(persisted_factor, None);

    let before_model = harness.account(account_id).await;
    let mapping = json!({
        "fixture-new-model":"fixture-new-model",
        "fixture-alias-*":"fixture-new-model"
    });
    let (status, accepted_model, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/operations"),
            Some(single_operation(
                "model_config",
                json!({
                    "whitelist":["fixture-new-model"],
                    "mappings":[{"source":"fixture-alias-*","target":"fixture-new-model"}]
                }),
                operation_version(&before_model, "model_config"),
                "fixture-model-config-write-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    assert!(
        harness
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
            .is_empty(),
        "accepting an operation must not send its write"
    );
    harness.runtime.operations.tick().await.unwrap();
    let model_done = harness
        .wait_job(accepted_model["id"].as_str().unwrap(), &["completed"])
        .await;
    assert_eq!(model_done["result"]["verified"], true);
    let model_writes = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(model_writes.len(), 1);
    assert_eq!(model_writes[0].api_key, ADMIN_KEY);
    assert_eq!(
        model_writes[0].body,
        json!({"account_ids":[account_id],"credentials":{"model_mapping":mapping}})
    );
    let model_row = harness.account(account_id).await;
    assert_eq!(model_row["model_mapping"], mapping);
    let persisted_token: Option<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT credentials->>'access_token' FROM accounts WHERE id=$1",
    )
    .bind(account_id)
    .fetch_one(&harness.runtime.db.pool)
    .await
    .expect("read the original secret directly from the synthetic database row");
    assert_eq!(persisted_token.as_deref(), Some("fixture-account-secret"));
    assert_eq!(model_row["extra"]["opaque"], "fixture-preserved-extra");

    let before_proxy = harness.account(account_id).await;
    let (status, accepted_proxy, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/operations"),
            Some(single_operation(
                "proxy",
                json!({"proxy_id":null}),
                operation_version(&before_proxy, "proxy"),
                "fixture-proxy-clear-setting-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    harness.runtime.operations.tick().await.unwrap();
    assert_eq!(
        harness
            .wait_job(accepted_proxy["id"].as_str().unwrap(), &["completed"])
            .await["status"],
        "completed"
    );
    let proxy_path = format!("/api/v1/admin/accounts/{account_id}");
    let proxy_writes = harness.upstream.matching(Method::PUT, &proxy_path);
    assert_eq!(proxy_writes.len(), 2);
    assert_eq!(proxy_writes[0].body, json!({"load_factor":null}));
    assert_eq!(proxy_writes[1].body, json!({"proxy_id":0}));
    assert_eq!(harness.account(account_id).await["proxy_id"], Value::Null);

    let before_sync = harness.account(account_id).await;
    let (status, accepted_sync, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/operations"),
            Some(single_operation(
                "sync_models",
                json!({}),
                operation_version(&before_sync, "sync_models"),
                "fixture-model-sync-setting-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    harness.runtime.operations.tick().await.unwrap();
    let sync_done = harness
        .wait_job(accepted_sync["id"].as_str().unwrap(), &["completed"])
        .await;
    assert_eq!(
        sync_done["result"]["models"],
        json!(["fixture-model-a", "fixture-model-b"])
    );
    assert_eq!(
        sync_done["result"]["warnings"],
        json!(["模型能力信息不完整"])
    );
    let sync_calls = harness.upstream.matching(
        Method::POST,
        &format!("/api/v1/admin/accounts/{account_id}/models/sync-upstream"),
    );
    assert_eq!(sync_calls.len(), 1);
    assert_eq!(sync_calls[0].body, Value::Null);
    assert!(!harness
        .upstream
        .calls()
        .iter()
        .any(|call| { call.path.contains("/test") || call.path.contains("quota") }));

    let current = harness.account(account_id).await;
    for (action, field, value, request_id) in [
        (
            "concurrency",
            "concurrency",
            json!(1),
            "fixture-concurrency-lower-0001",
        ),
        (
            "concurrency",
            "concurrency",
            json!(i32::MAX),
            "fixture-concurrency-upper-0001",
        ),
        (
            "load_factor",
            "load_factor",
            json!(1),
            "fixture-load-factor-lower-0001",
        ),
        (
            "load_factor",
            "load_factor",
            json!(10_000),
            "fixture-load-factor-upper-0001",
        ),
    ] {
        let (status, accepted, _) = harness
            .request(
                Method::POST,
                &format!("accounts/{account_id}/operations"),
                Some(single_operation(
                    action,
                    json!({(field):value}),
                    operation_version(&current, action),
                    request_id,
                )),
            )
            .await;
        assert_eq!(
            status, 202,
            "valid endpoint for {action}.{field} is accepted"
        );
        assert_eq!(
            harness.runtime.operations.store.read().unwrap()["jobs"]
                [accepted["id"].as_str().unwrap()]["payload"][field],
            value
        );
    }
    for (proxy_id, request_id) in [
        (1_i64, "fixture-proxy-id-lower-0001"),
        (i64::MAX, "fixture-proxy-id-upper-0001"),
    ] {
        let (status, accepted_proxy, _) = harness
            .request(
                Method::POST,
                &format!("accounts/{account_id}/operations"),
                Some(single_operation(
                    "proxy",
                    json!({"proxy_id":proxy_id}),
                    operation_version(&current, "proxy"),
                    request_id,
                )),
            )
            .await;
        assert_eq!(status, 202);
        assert_eq!(
            harness.runtime.operations.store.read().unwrap()["jobs"]
                [accepted_proxy["id"].as_str().unwrap()]["payload"]["proxy_id"],
            proxy_id
        );
    }
    for (action, payload, request_id) in [
        (
            "concurrency",
            json!({"concurrency":0}),
            "fixture-concurrency-zero-0001",
        ),
        (
            "concurrency",
            json!({"concurrency":i64::from(i32::MAX)+1}),
            "fixture-concurrency-high-0001",
        ),
        (
            "concurrency",
            json!({"concurrency":true}),
            "fixture-concurrency-boolean-0001",
        ),
        (
            "concurrency",
            json!({"concurrency":1.5}),
            "fixture-concurrency-fraction-0001",
        ),
        (
            "concurrency",
            json!({"concurrency":"7"}),
            "fixture-concurrency-string-0001",
        ),
        (
            "load_factor",
            json!({"load_factor":0}),
            "fixture-load-factor-zero-0001",
        ),
        (
            "load_factor",
            json!({"load_factor":true}),
            "fixture-load-factor-boolean-0001",
        ),
        (
            "load_factor",
            json!({"load_factor":1.5}),
            "fixture-load-factor-fraction-0001",
        ),
        (
            "load_factor",
            json!({"load_factor":10_001}),
            "fixture-load-factor-high-0001",
        ),
        (
            "load_factor",
            json!({"load_factor":"7"}),
            "fixture-load-factor-string-0001",
        ),
        ("proxy", json!({"proxy_id":0}), "fixture-proxy-id-zero-0001"),
        (
            "proxy",
            json!({"proxy_id":true}),
            "fixture-proxy-id-boolean-0001",
        ),
        (
            "proxy",
            json!({"proxy_id":1.5}),
            "fixture-proxy-id-fraction-0001",
        ),
        (
            "proxy",
            json!({"proxy_id":"7"}),
            "fixture-proxy-id-string-0001",
        ),
        (
            "proxy",
            json!({"proxy_id":u64::MAX}),
            "fixture-proxy-id-overflow-0001",
        ),
    ] {
        let current = harness.account(account_id).await;
        assert_eq!(
            harness
                .request(
                    Method::POST,
                    &format!("accounts/{account_id}/operations"),
                    Some(single_operation(
                        action,
                        payload,
                        operation_version(&current, action),
                        request_id,
                    )),
                )
                .await
                .0,
            422,
            "{action} must reject non-integer, out-of-range, or boolean values"
        );
    }
    let invalid_priority = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/priority"),
            Some(json!({"expected_version":account_version(&current),"priority":true})),
        )
        .await;
    assert_eq!(invalid_priority.0, 422);
    let invalid_direct_priority = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/priority"),
            Some(json!({"expected_version":account_version(&current),"priority":-1})),
        )
        .await;
    assert_eq!(invalid_direct_priority.0, 422);
    for priority in [0_i64, i64::from(i32::MAX)] {
        let live = harness.account(account_id).await;
        let (status, result, _) = harness
            .request(
                Method::POST,
                &format!("accounts/{account_id}/priority"),
                Some(json!({"expected_version":account_version(&live),"priority":priority})),
            )
            .await;
        assert_eq!(status, 200, "direct priority accepts boundary {priority}");
        assert_eq!(result["priority"], priority);
        assert_eq!(result["verified"], true);
    }
    let priority_writes: Vec<_> = harness
        .upstream
        .matching(Method::PUT, &format!("/api/v1/admin/accounts/{account_id}"))
        .into_iter()
        .filter(|call| call.body.get("priority").is_some())
        .collect();
    assert_eq!(priority_writes.len(), 2);
    assert_eq!(priority_writes[0].body, json!({"priority":0}));
    assert_eq!(priority_writes[1].body, json!({"priority":i32::MAX}));
    harness.finish().await;
}

#[tokio::test]
async fn settings_unknown_priority_write_is_checked_without_replay() {
    let mut harness = Harness::new().await;
    let account_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let row = harness.account(account_id).await;
    let (status, queued, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/operations"),
            Some(single_operation(
                "priority",
                json!({"priority":12}),
                operation_version(&row, "priority"),
                "fixture-priority-unknown-write-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    harness
        .upstream
        .apply_updates
        .store(false, Ordering::Release);
    harness.upstream.set_update_status(504);
    harness.runtime.operations.tick().await.unwrap();
    let checking = harness
        .wait_job(queued["id"].as_str().unwrap(), &["checking"])
        .await;
    assert_eq!(checking["status"], "checking");
    let path = format!("/api/v1/admin/accounts/{account_id}");
    let writes = harness.upstream.matching(Method::PUT, &path);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].body, json!({"priority":12}));
    assert_eq!(harness.account(account_id).await["priority"], 3);

    harness.runtime.operations.tick().await.unwrap();
    let final_job = harness
        .wait_job(queued["id"].as_str().unwrap(), &["needs_confirmation"])
        .await;
    assert_eq!(final_job["status"], "needs_confirmation");
    assert_eq!(harness.upstream.matching(Method::PUT, &path).len(), 1);
    assert_eq!(harness.account(account_id).await["priority"], 3);

    let applied_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let applied_row = harness.account(applied_id).await;
    let (status, applied, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{applied_id}/operations"),
            Some(single_operation(
                "priority",
                json!({"priority":18}),
                operation_version(&applied_row, "priority"),
                "fixture-priority-504-after-apply-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    harness
        .upstream
        .apply_updates
        .store(true, Ordering::Release);
    harness.upstream.set_update_status(504);
    harness.runtime.operations.tick().await.unwrap();
    let confirmed = harness
        .wait_job(applied["id"].as_str().unwrap(), &["completed"])
        .await;
    assert_eq!(confirmed["status"], "completed");
    assert_eq!(harness.account(applied_id).await["priority"], 18);
    let applied_path = format!("/api/v1/admin/accounts/{applied_id}");
    let applied_writes = harness.upstream.matching(Method::PUT, &applied_path);
    assert_eq!(applied_writes.len(), 1);
    assert_eq!(applied_writes[0].body, json!({"priority":18}));
    harness.runtime.operations.tick().await.unwrap();
    assert_eq!(
        harness.upstream.matching(Method::PUT, &applied_path).len(),
        1
    );
    harness.finish().await;
}

#[tokio::test]
async fn settings_capabilities_and_human_priority_keep_recovery_intents_independent() {
    let mut harness = Harness::new().await;
    let parent_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let child_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({}),
            json!({}),
            Some(parent_id),
            &[],
        )
        .await;
    let passthrough_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({}),
            json!({"openai_passthrough":true}),
            None,
            &[],
        )
        .await;
    let child_settings = harness
        .request(Method::GET, &format!("accounts/{child_id}/settings"), None)
        .await;
    assert_eq!(child_settings.0, 200);
    assert_eq!(
        child_settings.1["capabilities"]["model_config"]["editable"],
        false
    );
    assert_eq!(child_settings.1["capabilities"]["proxy"]["editable"], false);
    assert_eq!(
        child_settings.1["capabilities"]["sync_models"]["editable"],
        false
    );
    for (action, reason) in [
        ("model_config", "继承母账号凭据，不能独立修改模型配置"),
        ("proxy", "代理继承母账号，不能独立修改"),
        ("sync_models", "请在母账号同步模型"),
    ] {
        assert_eq!(
            child_settings.1["capabilities"][action]["reason"], reason,
            "inherited {action} capability keeps its independent reason"
        );
    }
    let passthrough_settings = harness
        .request(
            Method::GET,
            &format!("accounts/{passthrough_id}/settings"),
            None,
        )
        .await;
    assert_eq!(
        passthrough_settings.1["capabilities"]["model_config"]["editable"],
        false
    );
    assert_eq!(
        passthrough_settings.1["capabilities"]["proxy"]["editable"],
        true
    );
    assert_eq!(
        passthrough_settings.1["capabilities"]["sync_models"]["editable"],
        true
    );
    assert_eq!(
        passthrough_settings.1["capabilities"]["model_config"]["reason"],
        "透传模式会绕过模型配置"
    );
    assert_eq!(
        passthrough_settings.1["capabilities"]["proxy"]["reason"],
        ""
    );
    assert_eq!(
        passthrough_settings.1["capabilities"]["sync_models"]["reason"],
        ""
    );

    let row = harness.account(parent_id).await;
    let priority = harness
        .request(
            Method::POST,
            &format!("accounts/{parent_id}/operations"),
            Some(single_operation(
                "priority",
                json!({"priority":9}),
                operation_version(&row, "priority"),
                "fixture-human-priority-0001",
            )),
        )
        .await;
    assert_eq!(priority.0, 202);
    let recovery = harness
        .request(
            Method::POST,
            &format!("accounts/{parent_id}/operations"),
            Some(single_operation(
                "recover",
                json!({}),
                operation_version(&row, "recover"),
                "fixture-human-recover-0001",
            )),
        )
        .await;
    assert_eq!(recovery.0, 202);
    let reset = harness
        .request(
            Method::POST,
            &format!("accounts/{parent_id}/operations"),
            Some(single_operation(
                "reset_quota",
                json!({"action":"reset_quota","confirmed":true}),
                operation_version(&row, "reset_quota"),
                "fixture-human-reset-0001",
            )),
        )
        .await;
    assert_eq!(reset.0, 202);
    let accepted: Vec<Value> = [priority, recovery, reset]
        .into_iter()
        .map(|(_, value, _)| value)
        .collect();
    assert_eq!(
        accepted
            .iter()
            .map(|value| value["status"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["queued"])
    );
    let persisted = harness.runtime.operations.store.read().unwrap();
    let actions = accepted
        .iter()
        .map(|value| {
            persisted["jobs"][value["id"].as_str().unwrap()]["action"]
                .as_str()
                .unwrap()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actions,
        BTreeSet::from(["priority", "recover", "reset_quota"])
    );
    assert!(
        harness.upstream.calls().iter().all(|call| {
            !matches!(call.method, Method::PUT | Method::POST | Method::DELETE)
                || call.path == "/api/v1/admin/groups/all"
        }),
        "queueing manual priority must leave recovery and reset intents untouched"
    );
    harness.finish().await;
}

#[tokio::test]
async fn desktop_model_options_live_allowlists_and_sse_payload_match_python_oracle() {
    let mut harness = Harness::new().await;
    let group_a = harness
        .insert_group(
            "openai",
            true,
            &[
                "fixture-model-a",
                "fixture-not-in-catalog",
                "fixture-model-*",
                "fixture-image-model",
                "",
            ],
        )
        .await;
    let group_b = harness
        .insert_group("openai", true, &["fixture-model-b", "fixture-model-a"])
        .await;
    let account_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({}),
            json!({}),
            None,
            &[group_a, group_b],
        )
        .await;
    let (status, exact, _) = harness
        .request(
            Method::GET,
            &format!("accounts/{account_id}/models?purpose=model_test"),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        exact,
        fixtures()["python_oracle"]["desktop_actions"]["exact_candidates"]
    );
    assert!(
        harness
            .upstream
            .matching(
                Method::GET,
                &format!("/api/v1/admin/accounts/{account_id}/models")
            )
            .is_empty(),
        "an enabled complete exact allowlist bypasses catalog intersection"
    );
    let row = harness.account(account_id).await;
    for (model, expected) in [
        ("fixture-model-a", true),
        ("fixture-not-in-catalog", true),
        ("fixture-model-*", false),
        ("fixture-image-model", false),
    ] {
        assert_eq!(
            harness
                .runtime
                .actions
                .model_allowed(&row, model, false)
                .await
                .unwrap(),
            expected,
            "model candidate {model} must be rechecked against the exact live group union"
        );
    }
    let (status, normal, _) = harness
        .request(Method::GET, &format!("accounts/{account_id}/models"), None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        normal,
        fixtures()["python_oracle"]["desktop_actions"]["filtered_model_options"]
    );

    // The direct streaming route checks the full account version outside the
    // queued-operation context; queued `test` jobs use the field-scoped version.
    let version = account_version(&row);
    let (status, _, stream) = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/test"),
            Some(json!({"expected_version":version,"model_id":"fixture-model-a","mode":"default"})),
        )
        .await;
    assert_eq!(status, 200);
    let events = sse_events(&stream);
    let content = events
        .iter()
        .filter(|event| event["type"] == "content")
        .map(|event| event["text"].as_str().unwrap())
        .collect::<Vec<_>>();
    let expected_content = fixtures()["python_oracle"]["desktop_actions"]["sse_chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        content,
        expected_content
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        "unexpected synthetic SSE stream: {stream:?}; parsed events: {events:?}"
    );
    assert_eq!(events.last().unwrap()["success"], true);
    let tests = harness.upstream.matching(
        Method::POST,
        &format!("/api/v1/admin/accounts/{account_id}/test"),
    );
    assert_eq!(tests.len(), 1);
    assert_eq!(
        tests[0].body,
        json!({
            "model_id":"fixture-model-a","mode":"default","prompt":"",
            "image_data_url":"","audio_data_url":""
        })
    );
    assert!(tests[0].body.get("confirmed").is_none());

    let (status, _, _) = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/test"),
            Some(json!({"expected_version":version,"model_id":"fixture-model-a","mode":"default","confirmed":true})),
        )
        .await;
    assert_eq!(
        status, 200,
        "the lease must be released after the terminal SSE event"
    );
    assert_eq!(
        harness
            .upstream
            .matching(
                Method::POST,
                &format!("/api/v1/admin/accounts/{account_id}/test")
            )
            .len(),
        2
    );

    let unknown_mode = harness
        .request(
            Method::POST,
            &format!("accounts/{account_id}/test"),
            Some(json!({"expected_version":version,"model_id":"fixture-model-a","mode":"fixture-unknown-mode"})),
        )
        .await;
    assert_eq!(unknown_mode.0, 422);
    assert_eq!(
        harness
            .upstream
            .matching(
                Method::POST,
                &format!("/api/v1/admin/accounts/{account_id}/test")
            )
            .len(),
        2,
        "invalid test modes are rejected before contacting Sub2API"
    );

    let no_group_account = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let no_group_fallback = harness
        .request(
            Method::GET,
            &format!("accounts/{no_group_account}/models?purpose=model_test"),
            None,
        )
        .await;
    assert_eq!(no_group_fallback.0, 200);
    assert_eq!(
        no_group_fallback.1,
        json!([
            {"id":"fixture-model-a","display_name":"fixture-model-a","type":"text"},
            {"id":"fixture-model-b","display_name":"fixture-model-b","type":"text"}
        ])
    );
    let disabled_group = harness
        .insert_group("openai", false, &["fixture-model-a"])
        .await;
    let disabled_account = harness
        .insert_account(
            "openai",
            "oauth",
            json!({}),
            json!({}),
            None,
            &[disabled_group],
        )
        .await;
    let disabled_fallback = harness
        .request(
            Method::GET,
            &format!("accounts/{disabled_account}/models?purpose=model_test"),
            None,
        )
        .await;
    assert_eq!(disabled_fallback.0, 200);
    assert_eq!(disabled_fallback.1, no_group_fallback.1);
    assert_eq!(
        harness
            .upstream
            .matching(
                Method::GET,
                &format!("/api/v1/admin/accounts/{no_group_account}/models"),
            )
            .len(),
        1
    );
    assert_eq!(
        harness
            .upstream
            .matching(
                Method::GET,
                &format!("/api/v1/admin/accounts/{disabled_account}/models"),
            )
            .len(),
        1
    );
    harness.finish().await;
}

#[tokio::test]
async fn desktop_grok_quota_refresh_accepts_only_fresh_complete_billing() {
    let mut harness = Harness::new().await;
    let fixture_seed = sqlx::query(
        "UPDATE accounts SET deleted_at=now() WHERE id=4001 AND platform='openai' AND type='oauth'",
    )
    .execute(&harness.runtime.db.pool)
    .await
    .expect("exclude the temporary PostgreSQL harness seed from this isolated quota fixture");
    assert_eq!(fixture_seed.rows_affected(), 1);
    let fresh_id = harness
        .insert_account("grok", "oauth", json!({}), json!({}), None, &[])
        .await;
    let stale_id = harness
        .insert_account("grok", "oauth", json!({}), json!({}), None, &[])
        .await;
    let partial_id = harness
        .insert_account("grok", "oauth", json!({}), json!({}), None, &[])
        .await;
    let fixture_data = fixtures();
    let oracle = fixture_data["python_oracle"]["desktop_grok_billing"].clone();
    let queried_at = chrono::Utc::now();
    for (name, account_id) in [
        ("fresh", fresh_id),
        ("stale", stale_id),
        ("partial", partial_id),
    ] {
        let case = &oracle["cases"][name];
        let mut billing = case["grok_billing"].clone();
        let age_seconds = case["age_seconds"].as_i64().unwrap();
        // Keep both oracle outcomes clear of the route's whole-second clock boundary.
        let route_age_seconds = 5 + age_seconds * 10;
        billing["fetched_at"] = (queried_at + chrono::Duration::seconds(route_age_seconds))
            .to_rfc3339()
            .into();
        harness.upstream.set_grok_billing(account_id, billing);
    }

    let (status, accepted, _) = harness.request(Method::POST, "quota-refresh", None).await;
    assert_eq!(status, 200);
    assert_eq!(accepted["total"], 3);
    let progress = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let (_, current, _) = harness.request(Method::GET, "quota-refresh", None).await;
            if current["status"] == "completed" {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the local quota refresh should finish");
    let items = progress["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["account_id"].as_i64().unwrap(), item.clone()))
        .collect::<BTreeMap<_, _>>();
    for (name, id) in [
        ("fresh", fresh_id),
        ("stale", stale_id),
        ("partial", partial_id),
    ] {
        let expected_status = if oracle["cases"][name]["accepted"] == true {
            "success"
        } else {
            "partial"
        };
        assert_eq!(items[&id]["status"], expected_status, "oracle case {name}");
        if expected_status == "partial" {
            assert_eq!(items[&id]["error_code"], "partial_billing");
        }
        let calls = harness
            .upstream
            .matching(Method::GET, &format!("/api/v1/admin/accounts/{id}/usage"));
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].uri,
            format!("/api/v1/admin/accounts/{id}/usage?source=active&force=true")
        );
        assert_eq!(calls[0].api_key, ADMIN_KEY);
        assert_eq!(calls[0].body, Value::Null);
    }
    harness.finish().await;
}

#[tokio::test]
async fn template_initialization_uses_only_selected_profiles_once_and_preserves_json_order() {
    let mut harness = Harness::new().await;
    let full_mapping = json!({
        "fixture-full-model":"fixture-full-model",
        "fixture-full-alt":"fixture-full-alt",
        "fixture-full-third":"fixture-full-third"
    });
    let degraded_mapping = json!({
        "fixture-model":"fixture-model",
        "fixture-input-*":"fixture-model",
        "fixture-terra-*":"fixture-model"
    });
    let source_full = harness
        .insert_account("openai", "oauth", full_mapping, json!({}), None, &[])
        .await;
    let source_degraded = harness
        .insert_account("openai", "apikey", degraded_mapping, json!({}), None, &[])
        .await;
    let unrelated = harness
        .insert_account(
            "grok",
            "oauth",
            json!({"fixture-unrelated":"fixture-unrelated"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let templates = &harness.runtime.actions.templates;
    let initial = templates
        .initialize_sources(&harness.runtime.db, None, None)
        .await
        .expect("missing explicit sources should not initialize templates");
    assert_eq!(initial["configured"], false);
    let initialized = templates
        .initialize_sources(
            &harness.runtime.db,
            Some(source_full),
            Some(source_degraded),
        )
        .await
        .expect("copy explicitly selected synthetic accounts");
    assert_eq!(initialized["configured"], true);
    let full_db_mapping: Value = sqlx::query_scalar::<_, SqlJson<Value>>(
        "SELECT credentials->'model_mapping' FROM accounts WHERE id=$1",
    )
    .bind(source_full)
    .fetch_one(&harness.runtime.db.pool)
    .await
    .expect("read the full source mapping in PostgreSQL JSONB order")
    .0;
    let degraded_db_mapping: Value = sqlx::query_scalar::<_, SqlJson<Value>>(
        "SELECT credentials->'model_mapping' FROM accounts WHERE id=$1",
    )
    .bind(source_degraded)
    .fetch_one(&harness.runtime.db.pool)
    .await
    .expect("read the degraded source mapping in PostgreSQL JSONB order")
    .0;
    assert_eq!(
        initialized["templates"]["full"],
        profile_in_pg_order(&full_db_mapping),
        "source whitelist order must follow the real PostgreSQL JSONB result"
    );
    assert_eq!(
        initialized["templates"]["degraded"],
        profile_in_pg_order(&degraded_db_mapping),
        "source mapping order must follow the real PostgreSQL JSONB result"
    );
    assert_eq!(
        initialized["templates"]["takeover"],
        initialized["templates"]["degraded"]
    );
    assert_eq!(initialized["templates"].as_object().unwrap().len(), 3);
    assert!(!initialized.to_string().contains("fixture-account-secret"));
    assert_eq!(
        templates
            .initialize_sources(
                &harness.runtime.db,
                Some(source_full),
                Some(source_degraded),
            )
            .await
            .unwrap()["version"],
        initialized["version"],
        "reinitializing the same explicit pair is idempotent"
    );
    assert_eq!(
        templates
            .initialize_sources(&harness.runtime.db, Some(unrelated), Some(source_degraded),)
            .await
            .unwrap()["version"],
        initialized["version"],
        "configured templates must not be overwritten by a later selection"
    );
    let path = templates.store.path().to_owned();
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    harness.finish().await;
}

#[tokio::test]
async fn template_custom_catalog_caps_crud_and_legacy_empty_state_survive_reload() {
    let root = tempfile::tempdir().expect("template state root");
    let path = root.path().join("account-templates.json");
    let templates = Templates::new(path.clone(), false);
    let mut view = templates.view(None).expect("initial builtins should load");
    for index in 0..29 {
        view = templates
            .edit(
                None,
                &json!({
                    "expected_version":view["version"],"name":format!("fixture custom {index}"),
                    "whitelist":[],"mappings":[]
                }),
                false,
            )
            .expect("custom templates up to the total limit should save");
    }
    assert_eq!(view["templates"].as_object().unwrap().len(), 32);
    assert_eq!(view["custom_templates"].as_array().unwrap().len(), 29);
    let at_limit = fs::read(&path).unwrap();
    let over_limit = templates.edit(
        None,
        &json!({"expected_version":view["version"],"name":"fixture too many","whitelist":[],"mappings":[]}),
        false,
    );
    assert_eq!(over_limit.unwrap_err().status.as_u16(), 409);
    assert_eq!(
        fs::read(&path).unwrap(),
        at_limit,
        "limit rejection must be atomic"
    );

    for id in ["full", "degraded", "takeover"] {
        let before = templates.view(None).unwrap();
        let renamed = templates
            .edit(
                Some(id),
                &json!({
                    "expected_version":before["version"],"name":format!("fixture {id} renamed"),
                    "whitelist":[],"mappings":[]
                }),
                false,
            )
            .expect("legacy template ID remains editable");
        assert_eq!(
            renamed["template_meta"][id]["name"],
            format!("fixture {id} renamed")
        );
        let deleted = templates
            .edit(
                Some(id),
                &json!({"expected_version":renamed["version"]}),
                true,
            )
            .expect("legacy template ID remains deletable");
        assert!(deleted["templates"].get(id).is_none());
    }
    let empty = Templates::new(path, false).view(None).unwrap();
    assert_eq!(empty["templates"].as_object().unwrap().len(), 29);
    for builtin in ["full", "degraded", "takeover"] {
        assert!(
            empty["templates"].get(builtin).is_none(),
            "deleted builtin {builtin} must stay absent after reload"
        );
    }
    assert_eq!(empty["template_meta"].as_object().unwrap().len(), 29);
    assert_eq!(empty["template_versions"].as_object().unwrap().len(), 29);
    assert_eq!(empty["custom_templates"].as_array().unwrap().len(), 29);
}

#[tokio::test]
async fn template_http_crud_legacy_put_names_versions_and_deletion_match_python() {
    let mut harness = Harness::new().await;
    let (status, initial, _) = harness
        .request(Method::GET, "account-templates", None)
        .await;
    assert_eq!(status, 200);
    let original_full_version = initial["template_versions"]["full"].clone();
    let (status, renamed_full, _) = harness
        .request(
            Method::PUT,
            "account-templates/full",
            Some(json!({
                "expected_version":initial["version"],"name":"fixture renamed full",
                "whitelist":initial["templates"]["full"]["whitelist"],
                "mappings":initial["templates"]["full"]["mappings"]
            })),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        renamed_full["template_versions"]["full"], original_full_version,
        "changing only the template name preserves its profile version"
    );
    assert_eq!(
        renamed_full["template_meta"]["full"],
        json!({"name":"fixture renamed full","builtin":false})
    );
    let (status, changed_full, _) = harness
        .request(
            Method::PUT,
            "account-templates/full",
            Some(json!({
                "expected_version":renamed_full["version"],"name":"fixture renamed full",
                "whitelist":["fixture-full-content-changed"],"mappings":[]
            })),
        )
        .await;
    assert_eq!(status, 200);
    assert_ne!(
        changed_full["template_versions"]["full"], original_full_version,
        "changing profile content changes its version"
    );
    assert_eq!(
        changed_full["template_meta"].as_object().unwrap().len(),
        changed_full["templates"].as_object().unwrap().len()
    );
    assert!(changed_full["template_meta"]
        .as_object()
        .unwrap()
        .values()
        .all(
            |item| item["name"].as_str().is_some_and(|name| !name.is_empty())
                && item["builtin"] == false
        ));
    let legacy_put = json!({
        "expected_version":changed_full["version"],
        "full":changed_full["templates"]["full"],
        "degraded":changed_full["templates"]["degraded"],
        "takeover":changed_full["templates"]["takeover"]
    });
    let (status, legacy_saved, _) = harness
        .request(Method::PUT, "account-templates", Some(legacy_put))
        .await;
    assert_eq!(
        status, 200,
        "the deployed legacy PUT path must remain usable"
    );
    assert_eq!(
        legacy_saved["templates"]["full"],
        changed_full["templates"]["full"]
    );

    let (status, created, _) = harness
        .request(
            Method::POST,
            "account-templates/custom",
            Some(json!({
                "expected_version":legacy_saved["version"],"name":"fixture HTTP custom",
                "whitelist":["fixture-http-model"],"mappings":[]
            })),
        )
        .await;
    assert_eq!(status, 200);
    let custom_id = created["custom_templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|template| template["name"] == "fixture HTTP custom")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(custom_id.starts_with("custom-") && custom_id.len() == 31);
    assert!(custom_id[7..].bytes().all(|byte| byte.is_ascii_hexdigit()));
    let created_profile_version = created["template_versions"][&custom_id].clone();

    let (status, renamed, _) = harness
        .request(
            Method::PUT,
            &format!("account-templates/{custom_id}"),
            Some(json!({
                "expected_version":created["version"],"name":"fixture HTTP renamed",
                "whitelist":["fixture-http-model"],"mappings":[]
            })),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        renamed["template_versions"][&custom_id],
        created_profile_version
    );
    assert_eq!(
        renamed["template_meta"][&custom_id]["name"],
        "fixture HTTP renamed"
    );

    let too_long_name = "x".repeat(41);
    for (name, request_id) in [("   ", "blank"), (too_long_name.as_str(), "over-limit")] {
        let (status, _, _) = harness
            .request(
                Method::POST,
                "account-templates/custom",
                Some(json!({
                    "expected_version":renamed["version"],"name":name,
                    "whitelist":[],"mappings":[]
                })),
            )
            .await;
        assert_eq!(
            status, 422,
            "invalid custom template name case {request_id}"
        );
    }
    let unchanged_after_invalid = harness
        .request(Method::GET, "account-templates", None)
        .await;
    assert_eq!(unchanged_after_invalid.0, 200);
    assert_eq!(unchanged_after_invalid.1["version"], renamed["version"]);
    assert_eq!(
        unchanged_after_invalid.1["template_versions"][&custom_id],
        created_profile_version
    );

    let (status, changed_profile, _) = harness
        .request(
            Method::PUT,
            &format!("account-templates/{custom_id}"),
            Some(json!({
                "expected_version":renamed["version"],"name":"fixture HTTP renamed",
                "whitelist":["fixture-http-model","fixture-http-next"],"mappings":[]
            })),
        )
        .await;
    assert_eq!(status, 200);
    assert_ne!(
        changed_profile["template_versions"][&custom_id], created_profile_version,
        "template profile version changes with profile content"
    );

    let stale_delete = harness
        .request(
            Method::DELETE,
            &format!("account-templates/{custom_id}"),
            Some(json!({"expected_version":renamed["version"]})),
        )
        .await;
    assert_eq!(stale_delete.0, 409);
    let (status, deleted, _) = harness
        .request(
            Method::DELETE,
            &format!("account-templates/{custom_id}"),
            Some(json!({"expected_version":changed_profile["version"]})),
        )
        .await;
    assert_eq!(status, 200);
    assert!(deleted["templates"].get(&custom_id).is_none());
    let reloaded = Templates::new(
        harness.runtime.actions.templates.store.path().to_path_buf(),
        false,
    )
    .view(None)
    .unwrap();
    assert!(reloaded["templates"].get(&custom_id).is_none());
    harness.finish().await;
}

#[tokio::test]
async fn native_template_batch_is_atomic_secret_free_idempotent_and_skips_busy_children() {
    let mut harness = Harness::new().await;
    let full_source = harness
        .insert_account(
            "openai",
            "oauth",
            json!({
                "fixture-full-model":"fixture-full-model",
                "fixture-full-alias-*":"fixture-full-model"
            }),
            json!({}),
            None,
            &[],
        )
        .await;
    let degraded_source = harness
        .insert_account(
            "openai",
            "apikey",
            json!({"fixture-degraded-model":"fixture-degraded-model"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let busy_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-old":"fixture-old"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let free_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-old":"fixture-old"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let ineligible_id = harness
        .insert_account("grok", "oauth", json!({}), json!({}), None, &[])
        .await;
    let templates = &harness.runtime.actions.templates;
    let init = templates
        .initialize_sources(
            &harness.runtime.db,
            Some(full_source),
            Some(degraded_source),
        )
        .await
        .unwrap();
    let target = json!({
        "fixture-full-model":"fixture-full-model",
        "fixture-full-alias-*":"fixture-full-model"
    });
    let busy_lease = harness.runtime.actions.locks.account(busy_id).await;
    let full_row = harness.account(full_source).await;
    let busy_row = harness.account(busy_id).await;
    let free_row = harness.account(free_id).await;
    let ineligible_row = harness.account(ineligible_id).await;
    let request = template_batch(
        "full",
        &init["template_versions"]["full"],
        vec![
            json!({"account_id":full_source,"expected_version":operation_version(&full_row,"account_template")}),
            json!({"account_id":busy_id,"expected_version":operation_version(&busy_row,"account_template")}),
            json!({"account_id":free_id,"expected_version":operation_version(&free_row,"account_template")}),
            json!({"account_id":ineligible_id,"expected_version":operation_version(&ineligible_row,"account_template")}),
        ],
        "0123456789abcdef0123456789abcdef",
    );
    let mut failing_request = request.clone();
    failing_request["request_id"] = json!("f123456789abcdef0123456789abcdef");
    let (lock_path, lock_backup) =
        obstruct_operation_store_lock(&harness.runtime.operations.store, harness._temp.path());
    let failed_commit = harness
        .request(
            Method::POST,
            "account-templates/apply",
            Some(failing_request),
        )
        .await;
    restore_operation_store_lock(&lock_path, &lock_backup);
    assert_eq!(failed_commit.0, 500);
    let after_failed_commit = harness.runtime.operations.store.read().unwrap();
    assert!(after_failed_commit["batches"]
        .as_object()
        .unwrap()
        .is_empty());
    assert!(after_failed_commit["jobs"].as_object().unwrap().is_empty());
    assert!(harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .is_empty());

    let (status, accepted, _) = harness
        .request(
            Method::POST,
            "account-templates/apply",
            Some(request.clone()),
        )
        .await;
    assert_eq!(status, 202);
    let (status, duplicate, _) = harness
        .request(
            Method::POST,
            "account-templates/apply",
            Some(request.clone()),
        )
        .await;
    assert_eq!(status, 202);
    assert_eq!(accepted["batch_id"], duplicate["batch_id"]);
    assert_eq!(accepted["items"].as_array().unwrap().len(), 4);
    let existing_store = harness.runtime.operations.store.read().unwrap().clone();
    let mut second_failing_request = request.clone();
    second_failing_request["request_id"] = json!("e123456789abcdef0123456789abcdef");
    let (lock_path, lock_backup) =
        obstruct_operation_store_lock(&harness.runtime.operations.store, harness._temp.path());
    let second_failed_commit = harness
        .request(
            Method::POST,
            "account-templates/apply",
            Some(second_failing_request),
        )
        .await;
    restore_operation_store_lock(&lock_path, &lock_backup);
    assert_eq!(second_failed_commit.0, 500);
    assert_eq!(
        harness.runtime.operations.store.read().unwrap(),
        existing_store,
        "a failed second batch commit preserves the already accepted jobs and indexes"
    );
    assert!(harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .is_empty());
    let reopened = Operations::new(
        harness.runtime.actions.clone(),
        harness.runtime.operations.backend.clone(),
        harness.runtime.operations.budget.clone(),
    )
    .expect("reopen the real queued native template batch");
    let restarted_duplicate = reopened
        .submit_batch(&request, ADMIN_KEY, true)
        .await
        .unwrap();
    assert_eq!(restarted_duplicate["batch_id"], accepted["batch_id"]);
    assert_eq!(restarted_duplicate["items"], accepted["items"]);
    let initial = accepted["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["account_id"].as_i64().unwrap(), item.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(initial[&full_source]["status"], "completed");
    assert_eq!(initial[&full_source]["result"]["unchanged"], true);
    assert_eq!(initial[&busy_id]["status"], "queued");
    assert_eq!(initial[&free_id]["status"], "queued");
    assert_eq!(initial[&ineligible_id]["status"], "failed");
    assert!(harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .is_empty());
    let store = harness.runtime.operations.store.read().unwrap();
    assert_eq!(store["batches"].as_object().unwrap().len(), 1);
    assert_eq!(store["jobs"].as_object().unwrap().len(), 4);
    let persisted = serde_json::to_string(&store).unwrap();
    assert!(!persisted.contains(ADMIN_KEY));
    assert!(!persisted.contains("fixture-account-secret"));

    reopened.tick().await.unwrap();
    let free_job = harness
        .wait_job(initial[&free_id]["id"].as_str().unwrap(), &["completed"])
        .await;
    assert_eq!(free_job["status"], "completed");
    assert_eq!(
        harness
            .runtime
            .operations
            .get(initial[&busy_id]["id"].as_str().unwrap(), 0)
            .unwrap()["status"],
        "queued",
        "a locked account remains queued while its batch peer proceeds"
    );
    let first_write = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(first_write.len(), 1);
    assert_eq!(
        first_write[0].body,
        json!({"account_ids":[free_id],"credentials":{"model_mapping":target}})
    );
    drop(busy_lease);
    reopened.tick().await.unwrap();
    let batch_id = accepted["batch_id"].as_str().unwrap();
    let history = harness.wait_batch(batch_id).await;
    assert_eq!(history["items"].as_array().unwrap().len(), 4);
    assert_eq!(history["pending"], 0);
    let writes = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(writes.len(), 2);
    assert_eq!(
        writes[1].body,
        json!({"account_ids":[busy_id],"credentials":{"model_mapping":target}})
    );
    for id in [busy_id, free_id] {
        let row = harness.account(id).await;
        assert_eq!(row["model_mapping"], target);
        let persisted_token: Option<String> =
            sqlx::query_scalar("SELECT credentials->>'access_token' FROM accounts WHERE id=$1")
                .bind(id)
                .fetch_one(&harness.runtime.db.pool)
                .await
                .expect("read template mutation credentials from synthetic PostgreSQL row");
        assert_eq!(persisted_token.as_deref(), Some("fixture-account-secret"));
    }
    reopened.close().await;
    harness.finish().await;
}

#[tokio::test]
async fn queued_template_batch_rechecks_changed_targets_but_preserves_frozen_dispatched_target() {
    let mut harness = Harness::new().await;
    let full_source = harness
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-full":"fixture-full"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let degraded_source = harness
        .insert_account(
            "openai",
            "apikey",
            json!({"fixture-degraded":"fixture-degraded"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let before_dispatch_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-old":"fixture-old"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let in_flight_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-old":"fixture-old"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let templates = &harness.runtime.actions.templates;
    let initialized = templates
        .initialize_sources(
            &harness.runtime.db,
            Some(full_source),
            Some(degraded_source),
        )
        .await
        .unwrap();
    let before_row = harness.account(before_dispatch_id).await;
    let queued = template_batch(
        "full",
        &initialized["template_versions"]["full"],
        vec![
            json!({"account_id":before_dispatch_id,"expected_version":operation_version(&before_row,"account_template")}),
        ],
        "1123456789abcdef0123456789abcdef",
    );
    let (status, queued, _) = harness
        .request(Method::POST, "account-templates/apply", Some(queued))
        .await;
    assert_eq!(status, 202);
    let current = templates.view(None).unwrap();
    templates
        .edit(
            Some("full"),
            &json!({
                "expected_version":current["version"],"name":"fixture changed full",
                "whitelist":["fixture-changed-target"],"mappings":[]
            }),
            false,
        )
        .unwrap();
    harness.runtime.operations.tick().await.unwrap();
    let queued_batch = harness
        .wait_batch(queued["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(queued_batch["items"][0]["status"], "needs_confirmation");
    assert!(harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .is_empty());
    assert_eq!(
        harness.account(before_dispatch_id).await["model_mapping"],
        json!({"fixture-old":"fixture-old"})
    );

    let in_flight_view = templates.view(None).unwrap();
    let in_flight_row = harness.account(in_flight_id).await;
    let in_flight = template_batch(
        "full",
        &in_flight_view["template_versions"]["full"],
        vec![
            json!({"account_id":in_flight_id,"expected_version":operation_version(&in_flight_row,"account_template")}),
        ],
        "2123456789abcdef0123456789abcdef",
    );
    let path = "/api/v1/admin/accounts/bulk-update";
    let gate = harness.upstream.gate(path);
    let (status, accepted, _) = harness
        .request(Method::POST, "account-templates/apply", Some(in_flight))
        .await;
    assert_eq!(status, 202);
    harness.runtime.operations.tick().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("native template write should reach loopback gate");
    let after_dispatch = templates.view(None).unwrap();
    templates
        .edit(
            Some("full"),
            &json!({
                "expected_version":after_dispatch["version"],"name":"fixture changed after dispatch",
                "whitelist":["fixture-after-dispatch"],"mappings":[]
            }),
            false,
        )
        .unwrap();
    gate.release.notify_one();
    let finished = harness
        .wait_batch(accepted["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(finished["items"][0]["status"], "completed");
    let writes = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(
        writes.len(),
        1,
        "dispatched native target must not be replayed"
    );
    assert_eq!(
        writes[0].body,
        json!({"account_ids":[in_flight_id],"credentials":{"model_mapping":{"fixture-changed-target":"fixture-changed-target"}}})
    );
    assert_eq!(
        harness.account(in_flight_id).await["model_mapping"],
        json!({"fixture-changed-target":"fixture-changed-target"})
    );
    harness.upstream.ungate(path);

    let unrelated_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-old":"fixture-old"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let unrelated_view = templates.view(None).unwrap();
    let full_profile = unrelated_view["templates"]["full"].clone();
    let selected_version = unrelated_view["template_versions"]["full"].clone();
    let unrelated_row = harness.account(unrelated_id).await;
    let unaffected_request = template_batch(
        "full",
        &selected_version,
        vec![
            json!({"account_id":unrelated_id,"expected_version":operation_version(&unrelated_row,"account_template")}),
        ],
        "3123456789abcdef0123456789abcdef",
    );
    let (status, unaffected, _) = harness
        .request(
            Method::POST,
            "account-templates/apply",
            Some(unaffected_request),
        )
        .await;
    assert_eq!(status, 202);
    let renamed = templates
        .edit(
            Some("full"),
            &json!({
                "expected_version":unrelated_view["version"],"name":"fixture renamed full",
                "whitelist":full_profile["whitelist"],"mappings":full_profile["mappings"]
            }),
            false,
        )
        .unwrap();
    assert_eq!(renamed["template_versions"]["full"], selected_version);
    let unrelated_edit = templates
        .edit(
            Some("degraded"),
            &json!({
                "expected_version":renamed["version"],"name":"fixture unrelated degraded",
                "whitelist":["fixture-unrelated-profile"],"mappings":[]
            }),
            false,
        )
        .unwrap();
    assert_eq!(
        unrelated_edit["template_versions"]["full"], selected_version,
        "an unrelated profile edit does not change the selected profile version"
    );
    harness.runtime.operations.tick().await.unwrap();
    let unaffected_batch = harness
        .wait_batch(unaffected["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(unaffected_batch["items"][0]["status"], "completed");
    let writes = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(writes.len(), 2);
    assert_eq!(
        writes[1].body,
        json!({"account_ids":[unrelated_id],"credentials":{"model_mapping":{"fixture-after-dispatch":"fixture-after-dispatch"}}})
    );
    assert_eq!(
        harness.account(unrelated_id).await["model_mapping"],
        json!({"fixture-after-dispatch":"fixture-after-dispatch"})
    );

    let before_delete_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-old":"fixture-old"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let before_delete_view = templates.view(None).unwrap();
    let before_delete_row = harness.account(before_delete_id).await;
    let before_delete_request = template_batch(
        "full",
        &before_delete_view["template_versions"]["full"],
        vec![
            json!({"account_id":before_delete_id,"expected_version":operation_version(&before_delete_row,"account_template")}),
        ],
        "4123456789abcdef0123456789abcdef",
    );
    let (status, before_delete, _) = harness
        .request(
            Method::POST,
            "account-templates/apply",
            Some(before_delete_request),
        )
        .await;
    assert_eq!(status, 202);
    let before_delete_count = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .len();
    let current = templates.view(None).unwrap();
    templates
        .edit(
            Some("full"),
            &json!({"expected_version":current["version"]}),
            true,
        )
        .unwrap();
    harness.runtime.operations.tick().await.unwrap();
    let deleted_before_dispatch = harness
        .wait_batch(before_delete["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(
        deleted_before_dispatch["items"][0]["status"],
        "needs_confirmation"
    );
    assert_eq!(
        harness
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
            .len(),
        before_delete_count,
        "deleting a selected queued profile sends no account write"
    );
    assert_eq!(
        harness.account(before_delete_id).await["model_mapping"],
        json!({"fixture-old":"fixture-old"})
    );

    let delete_after_dispatch_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-old":"fixture-old"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let delete_after_view = templates.view(None).unwrap();
    let delete_after_row = harness.account(delete_after_dispatch_id).await;
    let delete_after_request = template_batch(
        "degraded",
        &delete_after_view["template_versions"]["degraded"],
        vec![
            json!({"account_id":delete_after_dispatch_id,"expected_version":operation_version(&delete_after_row,"account_template")}),
        ],
        "5123456789abcdef0123456789abcdef",
    );
    let path = "/api/v1/admin/accounts/bulk-update";
    let gate = harness.upstream.gate(path);
    let (status, delete_after, _) = harness
        .request(
            Method::POST,
            "account-templates/apply",
            Some(delete_after_request),
        )
        .await;
    assert_eq!(status, 202);
    harness.runtime.operations.tick().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("the frozen template target should reach the loopback upstream");
    let final_view = templates.view(None).unwrap();
    templates
        .edit(
            Some("degraded"),
            &json!({"expected_version":final_view["version"]}),
            true,
        )
        .unwrap();
    harness
        .upstream
        .set_bulk_mode(BulkMode::UnknownAppliedGatewayTimeout);
    gate.release.notify_one();
    let after_delete_dispatch = harness
        .wait_batch(delete_after["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(after_delete_dispatch["items"][0]["status"], "completed");
    let all_writes = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(all_writes.len(), 3);
    assert_eq!(
        all_writes[2].body,
        json!({"account_ids":[delete_after_dispatch_id],"credentials":{"model_mapping":{"fixture-unrelated-profile":"fixture-unrelated-profile"}}})
    );
    assert_eq!(
        harness.account(delete_after_dispatch_id).await["model_mapping"],
        json!({"fixture-unrelated-profile":"fixture-unrelated-profile"})
    );
    assert_eq!(
        harness
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
            .len(),
        3,
        "timeout-after-apply plus selected-profile deletion is read back without replay"
    );
    harness.finish().await;
}

#[test]
fn operation_store_migrates_legacy_single_job_without_losing_jobs_or_request_indexes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("account-operations.json");
    let legacy = json!({
        "version":1,
        "jobs":{"11111111111111111111111111111111":{
            "id":"11111111111111111111111111111111","account_id":73,
            "payload":{"priority":3},"status":"queued","action":"priority",
            "created_at":"fixture-created-at"
        }},
        "requests":{"fixture-request-id-0001":"11111111111111111111111111111111"}
    });
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let store = OperationStore::new(path.clone(), false);
    store.transaction(|_| Ok(())).unwrap();
    let migrated = OperationStore::new(path, false).read().unwrap();
    assert_eq!(migrated["jobs"], legacy["jobs"]);
    assert_eq!(migrated["requests"], legacy["requests"]);
    assert_eq!(migrated["batches"], json!({}));
    assert_eq!(migrated["batch_requests"], json!({}));
}

#[test]
fn template_legacy_save_and_migration_failures_preserve_original_bytes() {
    let root = tempfile::tempdir().unwrap();
    let deleted_path = root.path().join("deleted-template.json");
    let templates = Templates::new(deleted_path.clone(), false);
    let initial = templates.view(None).unwrap();
    let deleted = templates
        .edit(
            Some("degraded"),
            &json!({"expected_version":initial["version"]}),
            true,
        )
        .unwrap();
    let before_deleted_save = fs::read(&deleted_path).unwrap();
    let rejected_save = templates.save(&json!({
        "expected_version":deleted["version"],
        "full":{"whitelist":["fixture-full-updated"],"mappings":[]},
        "degraded":{"whitelist":["fixture-degraded-updated"],"mappings":[]},
        "takeover":{"whitelist":["fixture-takeover-updated"],"mappings":[]}
    }));
    assert_eq!(rejected_save.unwrap_err().status.as_u16(), 409);
    assert_eq!(fs::read(&deleted_path).unwrap(), before_deleted_save);

    let migration_path = root.path().join("legacy-template.json");
    let legacy = json!({
        "version":1,"revision":4,"configured":true,
        "templates":{
            "full":{"whitelist":["fixture-old-full"],"mappings":[]},
            "degraded":{"whitelist":["fixture-old-degraded"],"mappings":[]},
            "takeover":{"whitelist":[],"mappings":[]}
        }
    });
    let original = serde_json::to_vec(&legacy).unwrap();
    fs::write(&migration_path, &original).unwrap();
    fs::create_dir(migration_path.with_extension("lock"))
        .expect("block only this fixture's migration lock");
    let migration = Templates::new(migration_path.clone(), false).data();
    assert!(migration.is_err(), "migration save failures must propagate");
    assert_eq!(fs::read(&migration_path).unwrap(), original);
    assert!(
        serde_json::from_slice::<Value>(&fs::read(&migration_path).unwrap()).unwrap()
            ["custom_templates"]
            .is_null()
    );
}

#[tokio::test]
async fn native_group_and_proxy_batches_send_exact_targets_and_never_replay_unknown_writes() {
    let mut harness = Harness::new().await;
    let group_a = harness.insert_group("openai", false, &[]).await;
    let group_b = harness.insert_group("openai", false, &[]).await;
    let outside = harness.insert_group("openai", false, &[]).await;
    let first = harness
        .insert_account(
            "openai",
            "oauth",
            json!({}),
            json!({}),
            None,
            &[group_a, outside],
        )
        .await;
    let second = harness
        .insert_account(
            "openai",
            "apikey",
            json!({}),
            json!({}),
            None,
            &[group_a, outside],
        )
        .await;
    let first_row = harness.account(first).await;
    let second_row = harness.account(second).await;
    let group_request = native_batch(
        "groups",
        json!({}),
        vec![
            json!({
                "account_id":first,"expected_version":operation_version(&first_row,"groups"),
                "payload":{"scope_group_ids":[group_a,group_b],"group_ids":[group_b]}
            }),
            json!({
                "account_id":second,"expected_version":operation_version(&second_row,"groups"),
                "payload":{"scope_group_ids":[group_a,group_b],"group_ids":[group_b]}
            }),
        ],
        "fixture-native-group-batch-0001",
    );
    let (status, group_accepted, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(group_request.clone()),
        )
        .await;
    assert_eq!(status, 202);
    let (status, group_duplicate, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(group_request),
        )
        .await;
    assert_eq!(status, 202);
    assert_eq!(group_accepted["batch_id"], group_duplicate["batch_id"]);
    assert!(harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .is_empty());
    harness.runtime.operations.tick().await.unwrap();
    let groups = harness
        .wait_batch(group_accepted["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(groups["pending"], 0);
    assert_eq!(harness.linked_groups(first).await, vec![group_b, outside]);
    assert_eq!(harness.linked_groups(second).await, vec![group_b, outside]);
    let group_writes = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(group_writes.len(), 1);
    assert_eq!(
        group_writes[0].body,
        json!({"account_ids":[first,second],"group_ids":[group_b,outside]})
    );
    assert_eq!(group_writes[0].api_key, ADMIN_KEY);

    sqlx::query(
        "UPDATE accounts SET proxy_id=CASE WHEN id=$1 THEN 71 ELSE 72 END WHERE id=ANY($2)",
    )
    .bind(first)
    .bind(vec![first, second])
    .execute(&harness.runtime.db.pool)
    .await
    .expect("seed proxy batch values");
    let first_row = harness.account(first).await;
    let second_row = harness.account(second).await;
    let clear_request = native_batch(
        "proxy",
        json!({"proxy_id":null}),
        vec![
            json!({"account_id":first,"expected_version":operation_version(&first_row,"proxy")}),
            json!({"account_id":second,"expected_version":operation_version(&second_row,"proxy")}),
        ],
        "fixture-native-proxy-clear-0001",
    );
    let (status, clear, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(clear_request),
        )
        .await;
    assert_eq!(status, 202);
    harness.runtime.operations.tick().await.unwrap();
    assert_eq!(
        harness
            .wait_batch(clear["batch_id"].as_str().unwrap())
            .await["pending"],
        0
    );
    let proxy_writes = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(proxy_writes.len(), 2);
    assert_eq!(
        proxy_writes[1].body,
        json!({"account_ids":[first,second],"proxy_id":0})
    );
    assert_eq!(harness.account(first).await["proxy_id"], Value::Null);
    assert_eq!(harness.account(second).await["proxy_id"], Value::Null);

    let not_applied_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    sqlx::query("UPDATE accounts SET proxy_id=81 WHERE id=$1")
        .bind(not_applied_id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let not_applied_row = harness.account(not_applied_id).await;
    let not_applied_request = native_batch(
        "proxy",
        json!({"proxy_id":null}),
        vec![json!({
            "account_id":not_applied_id,"expected_version":operation_version(&not_applied_row,"proxy")
        })],
        "fixture-native-proxy-unknown-noapply-0001",
    );
    let (status, not_applied, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(not_applied_request),
        )
        .await;
    assert_eq!(status, 202);
    harness
        .upstream
        .set_bulk_mode(BulkMode::UnknownNotAppliedGatewayTimeout);
    harness.runtime.operations.tick().await.unwrap();
    let unknown = harness
        .wait_batch(not_applied["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(unknown["items"][0]["status"], "needs_confirmation");
    assert_eq!(harness.account(not_applied_id).await["proxy_id"], 81);
    harness.runtime.operations.tick().await.unwrap();
    assert_eq!(
        harness
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
            .len(),
        3,
        "an unknown write outcome is checked and never replayed"
    );

    let applied_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    sqlx::query("UPDATE accounts SET proxy_id=82 WHERE id=$1")
        .bind(applied_id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let applied_row = harness.account(applied_id).await;
    let applied_request = native_batch(
        "proxy",
        json!({"proxy_id":null}),
        vec![
            json!({"account_id":applied_id,"expected_version":operation_version(&applied_row,"proxy")}),
        ],
        "fixture-native-proxy-unknown-applied-0001",
    );
    let (status, applied, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(applied_request),
        )
        .await;
    assert_eq!(status, 202);
    harness
        .upstream
        .set_bulk_mode(BulkMode::UnknownAppliedGatewayTimeout);
    harness.runtime.operations.tick().await.unwrap();
    let applied_result = harness
        .wait_batch(applied["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(applied_result["items"][0]["status"], "completed");
    assert_eq!(harness.account(applied_id).await["proxy_id"], Value::Null);
    harness.runtime.operations.tick().await.unwrap();
    assert_eq!(
        harness
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
            .len(),
        4
    );

    let mut malformed_accounts = Vec::new();
    for index in 0..4 {
        let id = harness
            .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
            .await;
        let proxy_id: i64 = 83 + index;
        sqlx::query("UPDATE accounts SET proxy_id=$1 WHERE id=$2")
            .bind(proxy_id)
            .bind(id)
            .execute(&harness.runtime.db.pool)
            .await
            .unwrap();
        malformed_accounts.push((id, proxy_id));
    }
    for (scenario, mode, ids) in [
        (
            "fixture-native-proxy-malformed-result-0001",
            BulkMode::MalformedResultIds,
            vec![malformed_accounts[0], malformed_accounts[1]],
        ),
        (
            "fixture-native-proxy-malformed-total-0001",
            BulkMode::MalformedTotal,
            vec![malformed_accounts[2], malformed_accounts[3]],
        ),
    ] {
        let mut accounts = Vec::new();
        for (id, _) in &ids {
            let row = harness.account(*id).await;
            accounts.push(json!({
                "account_id":id,"expected_version":operation_version(&row,"proxy")
            }));
        }
        let (status, malformed, _) = harness
            .request(
                Method::POST,
                "account-operations/batch",
                Some(native_batch(
                    "proxy",
                    json!({"proxy_id":null}),
                    accounts,
                    scenario,
                )),
            )
            .await;
        assert_eq!(status, 202);
        let before_malformed = harness
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
            .len();
        harness.upstream.set_bulk_mode(mode);
        harness.runtime.operations.tick().await.unwrap();
        let malformed_result = harness
            .wait_batch(malformed["batch_id"].as_str().unwrap())
            .await;
        assert!(malformed_result["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["status"] == "needs_confirmation"));
        for (id, original_proxy_id) in &ids {
            assert_eq!(harness.account(*id).await["proxy_id"], *original_proxy_id);
        }
        harness.runtime.operations.tick().await.unwrap();
        assert_eq!(
            harness
                .upstream
                .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
                .len(),
            before_malformed + 1,
            "malformed result IDs/totals cannot be accepted as success or replayed"
        );
    }

    let fallback_first = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let fallback_second = harness
        .insert_account("grok", "oauth", json!({}), json!({}), None, &[])
        .await;
    sqlx::query(
        "UPDATE accounts SET proxy_id=CASE WHEN id=$1 THEN 84 ELSE 85 END WHERE id=ANY($2)",
    )
    .bind(fallback_first)
    .bind(vec![fallback_first, fallback_second])
    .execute(&harness.runtime.db.pool)
    .await
    .unwrap();
    let fallback_first_row = harness.account(fallback_first).await;
    let fallback_second_row = harness.account(fallback_second).await;
    let fallback_request = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        vec![
            json!({"account_id":fallback_first,"expected_version":operation_version(&fallback_first_row,"proxy")}),
            json!({"account_id":fallback_second,"expected_version":operation_version(&fallback_second_row,"proxy")}),
        ],
        "fixture-native-proxy-explicit-unsupported-0001",
    );
    let (status, fallback, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(fallback_request),
        )
        .await;
    assert_eq!(status, 202);
    let before_fallback = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .len();
    harness
        .upstream
        .set_bulk_mode(BulkMode::ExplicitUnsupported);
    harness.runtime.operations.tick().await.unwrap();
    assert_eq!(
        harness
            .wait_batch(fallback["batch_id"].as_str().unwrap())
            .await["pending"],
        0
    );
    assert_eq!(
        harness
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
            .len(),
        before_fallback + 1
    );
    for id in [fallback_first, fallback_second] {
        let path = format!("/api/v1/admin/accounts/{id}");
        let writes = harness.upstream.matching(Method::PUT, &path);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].body, json!({"proxy_id":PROXY_ID}));
        assert_eq!(harness.account(id).await["proxy_id"], PROXY_ID);
    }

    let generic_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    sqlx::query("UPDATE accounts SET proxy_id=86 WHERE id=$1")
        .bind(generic_id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let generic_row = harness.account(generic_id).await;
    let generic_request = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        vec![
            json!({"account_id":generic_id,"expected_version":operation_version(&generic_row,"proxy")}),
        ],
        "fixture-native-proxy-generic-404-0001",
    );
    let (status, generic, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(generic_request),
        )
        .await;
    assert_eq!(status, 202);
    let before_generic = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .len();
    harness.upstream.set_bulk_mode(BulkMode::GenericNotFound);
    harness.runtime.operations.tick().await.unwrap();
    let generic_result = harness
        .wait_batch(generic["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(generic_result["items"][0]["status"], "needs_confirmation");
    assert!(harness
        .upstream
        .matching(Method::PUT, &format!("/api/v1/admin/accounts/{generic_id}"))
        .is_empty());
    harness.runtime.operations.tick().await.unwrap();
    assert_eq!(
        harness
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
            .len(),
        before_generic + 1,
        "generic 404 must not trigger compatibility writes or replay"
    );
    harness.upstream.set_bulk_mode(BulkMode::Confirmed);
    harness.finish().await;
}

#[tokio::test]
async fn proxy_batch_acceptance_and_dispatch_persistence_failures_never_send_early() {
    let mut harness = Harness::new().await;
    let account_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    sqlx::query("UPDATE accounts SET proxy_id=77 WHERE id=$1")
        .bind(account_id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let row = harness.account(account_id).await;
    let request = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        vec![json!({
            "account_id":account_id,"expected_version":operation_version(&row,"proxy")
        })],
        "fixture-proxy-store-atomicity-0001",
    );

    let (lock_path, lock_backup) =
        obstruct_operation_store_lock(&harness.runtime.operations.store, harness._temp.path());
    let failed_accept = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(request.clone()),
        )
        .await;
    restore_operation_store_lock(&lock_path, &lock_backup);
    assert_eq!(failed_accept.0, 500);
    let empty = harness.runtime.operations.store.read().unwrap();
    assert!(empty["jobs"].as_object().unwrap().is_empty());
    assert!(empty["batches"].as_object().unwrap().is_empty());
    assert!(harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .is_empty());
    assert!(harness
        .upstream
        .matching(Method::PUT, &format!("/api/v1/admin/accounts/{account_id}"))
        .is_empty());

    let (status, accepted, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(request.clone()),
        )
        .await;
    assert_eq!(
        status, 202,
        "a failed commit must not reserve its request ID"
    );
    let (lock_path, lock_backup) =
        obstruct_operation_store_lock(&harness.runtime.operations.store, harness._temp.path());
    harness.runtime.operations.tick().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        harness
            .runtime
            .operations
            .get(accepted["items"][0]["id"].as_str().unwrap(), 0)
            .unwrap()["status"],
        "queued",
        "native dispatch state must be durable before the request leaves the process"
    );
    assert!(harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .is_empty());
    restore_operation_store_lock(&lock_path, &lock_backup);
    harness.runtime.operations.tick().await.unwrap();
    let completed = harness
        .wait_batch(accepted["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(completed["items"][0]["status"], "completed");
    let writes = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].body,
        json!({"account_ids":[account_id],"proxy_id":PROXY_ID})
    );
    assert_eq!(harness.account(account_id).await["proxy_id"], PROXY_ID);
    harness.finish().await;
}

#[tokio::test]
async fn proxy_batch_schema_template_and_target_preflight_are_per_item_and_atomic() {
    let mut harness = Harness::new().await;
    let source_full = harness
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-template-model":"fixture-template-model"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let source_degraded = harness
        .insert_account(
            "openai",
            "apikey",
            json!({"fixture-degraded-model":"fixture-degraded-model"}),
            json!({}),
            None,
            &[],
        )
        .await;
    let parent_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let inherited_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({}),
            json!({}),
            Some(parent_id),
            &[],
        )
        .await;
    let deleted_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let deleted_row = harness.account(deleted_id).await;
    sqlx::query("UPDATE accounts SET deleted_at=now() WHERE id=$1")
        .bind(deleted_id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let live_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let initialized = harness
        .runtime
        .actions
        .templates
        .initialize_sources(
            &harness.runtime.db,
            Some(source_full),
            Some(source_degraded),
        )
        .await
        .unwrap();
    let inherited_row = harness.account(inherited_id).await;
    let good_proxy_item = json!({
        "account_id":inherited_id,
        "expected_version":operation_version(&inherited_row,"proxy")
    });
    let mut string_id = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        vec![good_proxy_item.clone()],
        "fixture-proxy-invalid-string-id-0001",
    );
    string_id["accounts"][0]["account_id"] = json!(inherited_id.to_string());
    assert_eq!(
        harness
            .request(Method::POST, "account-operations/batch", Some(string_id))
            .await
            .0,
        422
    );
    let mut duplicate_ids = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        vec![good_proxy_item.clone(), good_proxy_item.clone()],
        "fixture-proxy-duplicate-id-0001",
    );
    duplicate_ids["client_id"] = json!("fixture-proxy-schema-client-0001");
    assert_eq!(
        harness
            .request(
                Method::POST,
                "account-operations/batch",
                Some(duplicate_ids),
            )
            .await
            .0,
        422
    );
    let mut extra_field = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        vec![good_proxy_item.clone()],
        "fixture-proxy-extra-shape-0001",
    );
    extra_field["unexpected"] = json!(true);
    assert_eq!(
        harness
            .request(Method::POST, "account-operations/batch", Some(extra_field),)
            .await
            .0,
        422
    );
    assert_eq!(
        harness
            .request(
                Method::POST,
                "account-operations/batch",
                Some(native_batch(
                    "proxy",
                    json!({"proxy_id":PROXY_ID}),
                    Vec::new(),
                    "fixture-proxy-empty-targets-0001",
                )),
            )
            .await
            .0,
        422,
        "an empty native proxy batch is rejected before persistence"
    );
    assert_eq!(
        harness
            .request(
                Method::POST,
                "account-operations/batch",
                Some(native_batch(
                    "proxy",
                    json!({"proxy_id":0}),
                    vec![good_proxy_item.clone()],
                    "fixture-proxy-zero-target-0001",
                )),
            )
            .await
            .0,
        422,
        "proxy zero is not accepted as an explicit target"
    );
    assert_eq!(
        harness
            .request(
                Method::POST,
                "account-operations/batch",
                Some(native_batch(
                    "proxy",
                    json!({"proxy_id":PROXY_ID,"unexpected":true}),
                    vec![good_proxy_item.clone()],
                    "fixture-proxy-extra-payload-0001",
                )),
            )
            .await
            .0,
        422,
        "unknown proxy payload fields are rejected"
    );
    let too_many_accounts = (0..1001)
        .map(|index| {
            json!({
                "account_id":8_000_000_000_i64+index,
                "expected_version":"a".repeat(64)
            })
        })
        .collect();
    assert_eq!(
        harness
            .request(
                Method::POST,
                "account-operations/batch",
                Some(native_batch(
                    "proxy",
                    json!({"proxy_id":PROXY_ID}),
                    too_many_accounts,
                    "fixture-proxy-over-limit-accounts-0001",
                )),
            )
            .await
            .0,
        422,
        "the native batch request enforces the 1,000 account limit"
    );
    assert!(harness.runtime.operations.store.read().unwrap()["batches"]
        .as_object()
        .unwrap()
        .is_empty());
    assert!(harness.runtime.operations.store.read().unwrap()["jobs"]
        .as_object()
        .unwrap()
        .is_empty());

    let bad_template_version = json!("f".repeat(64));
    let live_row = harness.account(live_id).await;
    let stale_template = template_batch(
        "full",
        &bad_template_version,
        vec![
            json!({"account_id":live_id,"expected_version":operation_version(&live_row,"account_template")}),
        ],
        "6123456789abcdef0123456789abcdef",
    );
    assert_eq!(
        harness
            .request(
                Method::POST,
                "account-templates/apply",
                Some(stale_template),
            )
            .await
            .0,
        409,
        "a stale selected-template version is rejected before a batch is recorded"
    );
    assert!(harness.runtime.operations.store.read().unwrap()["batches"]
        .as_object()
        .unwrap()
        .is_empty());

    let (status, inherited_proxy, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(native_batch(
                "proxy",
                json!({"proxy_id":PROXY_ID}),
                vec![good_proxy_item],
                "fixture-proxy-inherited-child-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    assert_eq!(inherited_proxy["items"][0]["status"], "failed");
    assert!(!inherited_proxy["items"][0]["reason"]
        .as_str()
        .unwrap()
        .is_empty());

    let inherited_template = template_batch(
        "full",
        &initialized["template_versions"]["full"],
        vec![
            json!({"account_id":inherited_id,"expected_version":operation_version(&inherited_row,"account_template")}),
        ],
        "7123456789abcdef0123456789abcdef",
    );
    let (status, inherited_template, _) = harness
        .request(
            Method::POST,
            "account-templates/apply",
            Some(inherited_template),
        )
        .await;
    assert_eq!(status, 202);
    assert_eq!(inherited_template["items"][0]["status"], "failed");

    let missing_id = 9_900_000_001_i64;
    let invalid_target = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID + 1}),
        vec![
            json!({"account_id":inherited_id,"expected_version":operation_version(&inherited_row,"proxy")}),
            json!({"account_id":deleted_id,"expected_version":operation_version(&deleted_row,"proxy")}),
            json!({"account_id":live_id,"expected_version":operation_version(&live_row,"proxy")}),
            json!({"account_id":missing_id,"expected_version":"a".repeat(64)}),
        ],
        "fixture-proxy-invalid-target-per-item-0001",
    );
    let (status, invalid_items, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(invalid_target),
        )
        .await;
    assert_eq!(status, 202);
    let invalid_by_id = invalid_items["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["account_id"].as_i64().unwrap(), item.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(invalid_by_id[&inherited_id]["status"], "failed");
    assert_eq!(invalid_by_id[&deleted_id]["status"], "failed");
    assert_eq!(invalid_by_id[&live_id]["status"], "needs_confirmation");
    assert_eq!(invalid_by_id[&missing_id]["status"], "failed");

    let inactive_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let inactive_row = harness.account(inactive_id).await;
    harness.upstream.set_proxy_active(false);
    let (status, inactive, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(native_batch(
                "proxy",
                json!({"proxy_id":PROXY_ID}),
                vec![json!({"account_id":inactive_id,"expected_version":operation_version(&inactive_row,"proxy")})],
                "fixture-proxy-inactive-target-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    assert_eq!(inactive["items"][0]["status"], "needs_confirmation");
    assert!(harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .is_empty());
    harness.finish().await;
}

#[tokio::test]
async fn native_delete_confirmation_detaches_managed_accounts_and_rechecks_children() {
    let mut harness = Harness::new().await;
    let managed_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let ordinary_id = harness
        .insert_account("grok", "apikey", json!({}), json!({}), None, &[])
        .await;
    harness
        .runtime
        .config
        .fallback
        .write_locked(
            true,
            true,
            &[managed_id],
            None,
            1,
            "fixture-account-mutation",
        )
        .unwrap();
    let managed_row = harness.account(managed_id).await;
    let ordinary_row = harness.account(ordinary_id).await;
    let delete_accounts = vec![
        json!({
            "account_id":managed_id,
            "expected_version":managed_operation_version(&managed_row,"delete",true),
            "payload":{"detach_managed":true}
        }),
        json!({
            "account_id":ordinary_id,
            "expected_version":managed_operation_version(&ordinary_row,"delete",false),
            "payload":{"detach_managed":false}
        }),
    ];
    let unconfirmed = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(native_batch(
                "delete",
                json!({"confirmed":false}),
                delete_accounts.clone(),
                "fixture-delete-unconfirmed-0001",
            )),
        )
        .await;
    assert_eq!(unconfirmed.0, 422);
    assert!(harness.runtime.operations.store.read().unwrap()["batches"]
        .as_object()
        .unwrap()
        .is_empty());

    let (status, accepted, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(native_batch(
                "delete",
                json!({"confirmed":true}),
                delete_accounts,
                "fixture-delete-detach-managed-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    harness.runtime.operations.tick().await.unwrap();
    let deleted = harness
        .wait_batch(accepted["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(deleted["pending"], 0);
    assert!(deleted["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["status"] == "completed"));
    let delete_calls = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/batch-delete");
    assert_eq!(delete_calls.len(), 1);
    assert_eq!(
        delete_calls[0].body,
        json!({"account_ids":[managed_id,ordinary_id]})
    );
    assert_eq!(delete_calls[0].api_key, ADMIN_KEY);
    assert!(delete_calls[0].body.get("detach_managed").is_none());
    assert!(!harness
        .runtime
        .config
        .fallback
        .managed()
        .unwrap()
        .contains(&managed_id));
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE id=ANY($1)")
        .bind(vec![managed_id, ordinary_id])
        .fetch_one(&harness.runtime.db.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);

    let parent_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let parent_row = harness.account(parent_id).await;
    let changed_children = native_batch(
        "delete",
        json!({"confirmed":true}),
        vec![json!({
            "account_id":parent_id,
            "expected_version":managed_operation_version(&parent_row,"delete",false),
            "payload":{"detach_managed":false}
        })],
        "fixture-delete-child-set-change-0001",
    );
    let (status, queued_parent, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(changed_children),
        )
        .await;
    assert_eq!(status, 202);
    let child_id = harness
        .insert_account(
            "openai",
            "oauth",
            json!({}),
            json!({}),
            Some(parent_id),
            &[],
        )
        .await;
    harness.runtime.operations.tick().await.unwrap();
    let changed_child_result = harness
        .wait_batch(queued_parent["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(
        changed_child_result["items"][0]["status"],
        "needs_confirmation"
    );
    assert!(changed_child_result["items"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("关联账号"));
    assert_eq!(
        harness
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts/batch-delete")
            .len(),
        1
    );
    assert!(harness.account(child_id).await.is_object());

    let managed_parent = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    harness
        .runtime
        .config
        .fallback
        .write_locked(
            true,
            true,
            &[managed_parent],
            None,
            2,
            "fixture-account-mutation",
        )
        .unwrap();
    let managed_parent_row = harness.account(managed_parent).await;
    let managed_parent_request = native_batch(
        "delete",
        json!({"confirmed":true}),
        vec![json!({
            "account_id":managed_parent,
            "expected_version":managed_operation_version(&managed_parent_row,"delete",true),
            "payload":{"detach_managed":true}
        })],
        "fixture-delete-managed-child-set-change-0001",
    );
    let (status, queued_managed_parent, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(managed_parent_request),
        )
        .await;
    assert_eq!(status, 202);
    assert_eq!(
        queued_managed_parent["items"][0]["managed_child_ids"],
        json!([])
    );
    let managed_child = harness
        .insert_account(
            "grok",
            "oauth",
            json!({}),
            json!({}),
            Some(managed_parent),
            &[],
        )
        .await;
    harness
        .runtime
        .config
        .fallback
        .write_locked(
            true,
            true,
            &[managed_parent, managed_child],
            None,
            3,
            "fixture-account-mutation",
        )
        .unwrap();
    harness.runtime.operations.tick().await.unwrap();
    let managed_child_change = harness
        .wait_batch(queued_managed_parent["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(
        managed_child_change["items"][0]["status"],
        "needs_confirmation"
    );
    assert!(managed_child_change["items"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("关联账号"));
    assert_eq!(
        harness.runtime.config.fallback.managed().unwrap(),
        BTreeSet::from([managed_parent, managed_child])
    );
    assert!(harness.account(managed_parent).await.is_object());
    harness.finish().await;
}

#[tokio::test]
async fn proxy_batch_restart_checks_a_dispatched_chunk_without_replaying_it() {
    let mut harness = Harness::new().await;
    let account_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    sqlx::query("UPDATE accounts SET proxy_id=81 WHERE id=$1")
        .bind(account_id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let row = harness.account(account_id).await;
    let request = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        vec![json!({
            "account_id":account_id,"expected_version":operation_version(&row,"proxy")
        })],
        "fixture-proxy-restart-dispatched-0001",
    );
    let (status, accepted, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(request.clone()),
        )
        .await;
    assert_eq!(status, 202);
    let batch_id = accepted["batch_id"].as_str().unwrap().to_owned();
    let job_id = accepted["items"][0]["id"].as_str().unwrap().to_owned();
    harness
        .runtime
        .operations
        .store
        .transaction(|data| {
            let job = &mut data["jobs"][&job_id];
            job["status"] = "running".into();
            job["execution_mode"] = "native".into();
            job["native_target"] = json!({"proxy_id":PROXY_ID});
            job["dispatch_id"] = "fixture-proxy-dispatch-already-sent".into();
            data["batches"][&batch_id]["chunks"] = json!([{
                "id":"fixture-proxy-chunk-already-sent","job_ids":[job_id],
                "account_ids":[account_id],"status":"dispatched","execution_mode":"native",
                "dispatched_at":"fixture-dispatched-at"
            }]);
            Ok(())
        })
        .unwrap();

    let reopened = Operations::new(
        harness.runtime.actions.clone(),
        harness.runtime.operations.backend.clone(),
        harness.runtime.operations.budget.clone(),
    )
    .expect("recover the persisted dispatched native proxy chunk");
    let recovered = reopened.store.read().unwrap();
    assert_eq!(recovered["jobs"][&job_id]["status"], "checking");
    assert_eq!(
        recovered["batches"][&batch_id]["chunks"][0]["status"],
        "checking"
    );
    let duplicate = reopened
        .submit_batch(&request, ADMIN_KEY, false)
        .await
        .unwrap();
    assert_eq!(duplicate["batch_id"], batch_id);
    reopened.tick().await.unwrap();
    let checked = harness.wait_job(&job_id, &["needs_confirmation"]).await;
    assert_eq!(checked["status"], "needs_confirmation");
    assert_eq!(harness.account(account_id).await["proxy_id"], 81);
    assert!(harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .is_empty());
    reopened.close().await;
    harness.finish().await;
}

#[tokio::test]
async fn native_proxy_scheduler_sends_a_101_account_batch_as_two_exact_chunks() {
    let mut harness = Harness::new().await;
    let mut account_ids = Vec::new();
    let mut items = Vec::new();
    for _ in 0..101 {
        let id = harness
            .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
            .await;
        let row = harness.account(id).await;
        account_ids.push(id);
        items.push(json!({
            "account_id":id,"expected_version":operation_version(&row,"proxy")
        }));
    }
    harness.runtime.start().await.unwrap();
    let (status, accepted, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(native_batch(
                "proxy",
                json!({"proxy_id":PROXY_ID}),
                items,
                "fixture-proxy-auto-101-chunking-0001",
            )),
        )
        .await;
    assert_eq!(status, 202);
    let batch = harness
        .wait_batch(accepted["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(batch["pending"], 0);
    assert_eq!(batch["items"].as_array().unwrap().len(), 101);
    assert!(batch["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["status"] == "completed"));
    let writes = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(writes.len(), 2);
    let sizes = writes
        .iter()
        .map(|call| call.body["account_ids"].as_array().unwrap().len())
        .collect::<Vec<_>>();
    assert_eq!(
        sizes.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([1, 100])
    );
    let dispatched = writes
        .iter()
        .flat_map(|call| call.body["account_ids"].as_array().unwrap())
        .map(|value| value.as_i64().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(dispatched, account_ids.iter().copied().collect());
    assert_eq!(
        dispatched.len(),
        101,
        "every child is sent in exactly one chunk"
    );
    let updated: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts WHERE id=ANY($1) AND proxy_id=$2")
            .bind(&account_ids)
            .bind(PROXY_ID)
            .fetch_one(&harness.runtime.db.pool)
            .await
            .unwrap();
    assert_eq!(updated, 101);
    harness.finish().await;
}

#[tokio::test]
async fn native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states() {
    let mut harness = Harness::new().await;
    let first = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let second = harness
        .insert_account("grok", "apikey", json!({}), json!({}), None, &[])
        .await;
    let unchanged = harness
        .insert_account("openai", "apikey", json!({}), json!({}), None, &[])
        .await;
    sqlx::query(
        "UPDATE accounts SET proxy_id=CASE WHEN id=$1 THEN 91 ELSE 92 END WHERE id=ANY($2)",
    )
    .bind(first)
    .bind(vec![first, second])
    .execute(&harness.runtime.db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE accounts SET proxy_id=$1 WHERE id=$2")
        .bind(PROXY_ID)
        .bind(unchanged)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let first_row = harness.account(first).await;
    let second_row = harness.account(second).await;
    let unchanged_row = harness.account(unchanged).await;
    let proxy_request = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        vec![
            json!({"account_id":second,"expected_version":operation_version(&second_row,"proxy")}),
            json!({"account_id":first,"expected_version":operation_version(&first_row,"proxy")}),
            json!({"account_id":unchanged,"expected_version":operation_version(&unchanged_row,"proxy")}),
        ],
        "fixture-proxy-partition-order-0001",
    );
    let (status, accepted, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(proxy_request.clone()),
        )
        .await;
    assert_eq!(status, 202);
    assert_eq!(accepted["items"][0]["account_id"], second);
    let unchanged_item = accepted["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["account_id"] == unchanged)
        .unwrap();
    assert_eq!(unchanged_item["status"], "completed");
    assert_eq!(unchanged_item["result"]["unchanged"], true);
    let mut reordered = proxy_request.clone();
    reordered["accounts"] = json!([
        {"account_id":first,"expected_version":operation_version(&first_row,"proxy")},
        {"account_id":second,"expected_version":operation_version(&second_row,"proxy")},
        {"account_id":unchanged,"expected_version":operation_version(&unchanged_row,"proxy")}
    ]);
    let (status, duplicate, _) = harness
        .request(Method::POST, "account-operations/batch", Some(reordered))
        .await;
    assert_eq!(status, 202);
    assert_eq!(duplicate["batch_id"], accepted["batch_id"]);
    assert_eq!(duplicate["items"][0]["account_id"], second);
    let mut changed = proxy_request;
    changed["payload"]["proxy_id"] = json!(PROXY_ID + 1);
    assert_eq!(
        harness
            .request(Method::POST, "account-operations/batch", Some(changed))
            .await
            .0,
        409,
        "a request id cannot be repurposed for a different proxy target"
    );
    harness.upstream.set_bulk_mode(BulkMode::PartialSecond);
    harness.runtime.operations.tick().await.unwrap();
    let partial = harness
        .wait_batch(accepted["batch_id"].as_str().unwrap())
        .await;
    let partial_by_id = partial["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["account_id"].as_i64().unwrap(), item.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(partial_by_id[&first]["status"], "completed");
    assert_eq!(partial_by_id[&second]["status"], "failed");
    assert_eq!(partial_by_id[&unchanged]["status"], "completed");
    assert_eq!(partial_by_id[&unchanged]["result"]["unchanged"], true);
    assert_eq!(harness.account(first).await["proxy_id"], PROXY_ID);
    assert_eq!(harness.account(second).await["proxy_id"], 92);
    let partitioned = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(partitioned.len(), 1);
    assert_eq!(partitioned[0].api_key, ADMIN_KEY);
    assert_eq!(
        partitioned[0].body,
        json!({"account_ids":[first,second],"proxy_id":PROXY_ID})
    );

    let busy_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let free_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    sqlx::query("UPDATE accounts SET proxy_id=93 WHERE id=ANY($1)")
        .bind(vec![busy_id, free_id])
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let busy_row = harness.account(busy_id).await;
    let free_row = harness.account(free_id).await;
    let busy_request = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        vec![
            json!({"account_id":busy_id,"expected_version":operation_version(&busy_row,"proxy")}),
            json!({"account_id":free_id,"expected_version":operation_version(&free_row,"proxy")}),
        ],
        "fixture-proxy-busy-child-0001",
    );
    let (status, busy_batch, _) = harness
        .request(Method::POST, "account-operations/batch", Some(busy_request))
        .await;
    assert_eq!(status, 202);
    let busy_items = busy_batch["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["account_id"].as_i64().unwrap(), item.clone()))
        .collect::<BTreeMap<_, _>>();
    let lease = harness.runtime.actions.locks.account(busy_id).await;
    harness.upstream.set_bulk_mode(BulkMode::Confirmed);
    harness.runtime.operations.tick().await.unwrap();
    harness
        .wait_job(busy_items[&free_id]["id"].as_str().unwrap(), &["completed"])
        .await;
    assert_eq!(
        harness
            .runtime
            .operations
            .get(busy_items[&busy_id]["id"].as_str().unwrap(), 0)
            .unwrap()["status"],
        "queued"
    );
    drop(lease);
    harness.runtime.operations.tick().await.unwrap();
    let busy_finished = harness
        .wait_batch(busy_batch["batch_id"].as_str().unwrap())
        .await;
    assert_eq!(busy_finished["pending"], 0);
    let proxy_writes = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update");
    assert_eq!(proxy_writes.len(), 3);
    assert_eq!(
        proxy_writes[1].body,
        json!({"account_ids":[free_id],"proxy_id":PROXY_ID})
    );
    assert_eq!(
        proxy_writes[2].body,
        json!({"account_ids":[busy_id],"proxy_id":PROXY_ID})
    );

    let cancel_first = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let cancel_middle = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    let cancel_last = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    sqlx::query("UPDATE accounts SET proxy_id=94 WHERE id=ANY($1)")
        .bind(vec![cancel_first, cancel_middle, cancel_last])
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let mut cancel_rows = BTreeMap::new();
    for id in [cancel_first, cancel_middle, cancel_last] {
        cancel_rows.insert(id, harness.account(id).await);
    }
    let cancel_request = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        [cancel_first, cancel_middle, cancel_last]
            .into_iter()
            .map(|id|json!({"account_id":id,"expected_version":operation_version(&cancel_rows[&id],"proxy")}))
            .collect(),
        "fixture-proxy-cancel-child-0001",
    );
    let (status, cancel_batch, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(cancel_request),
        )
        .await;
    assert_eq!(status, 202);
    let cancelled_item = cancel_batch["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["account_id"] == cancel_middle)
        .unwrap();
    let (status, cancelled, _) = harness
        .request(
            Method::POST,
            &format!(
                "account-operations/{}/cancel",
                cancelled_item["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(cancelled["status"], "cancelled");
    let before = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .len();
    harness.runtime.operations.tick().await.unwrap();
    let cancel_done = harness
        .wait_batch(cancel_batch["batch_id"].as_str().unwrap())
        .await;
    let statuses = cancel_done["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["account_id"].as_i64().unwrap(),
                item["status"].as_str().unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(statuses[&cancel_middle], "cancelled");
    assert_eq!(statuses[&cancel_first], "completed");
    assert_eq!(statuses[&cancel_last], "completed");
    let cancel_write = &harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")[before];
    assert_eq!(
        cancel_write.body["account_ids"],
        json!([cancel_first, cancel_last])
    );

    let stale_id = harness
        .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
        .await;
    sqlx::query("UPDATE accounts SET proxy_id=95 WHERE id=$1")
        .bind(stale_id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let stale_row = harness.account(stale_id).await;
    let stale_request = native_batch(
        "proxy",
        json!({"proxy_id":PROXY_ID}),
        vec![
            json!({"account_id":stale_id,"expected_version":operation_version(&stale_row,"proxy")}),
        ],
        "fixture-proxy-field-conflict-0001",
    );
    let (status, stale_batch, _) = harness
        .request(
            Method::POST,
            "account-operations/batch",
            Some(stale_request),
        )
        .await;
    assert_eq!(status, 202);
    let stale_job = stale_batch["items"][0]["id"].as_str().unwrap();
    let writes_before_stale_check = harness
        .upstream
        .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
        .len();
    let lease = harness.runtime.actions.locks.account(stale_id).await;
    sqlx::query("UPDATE accounts SET proxy_id=97 WHERE id=$1")
        .bind(stale_id)
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    drop(lease);
    harness.runtime.operations.tick().await.unwrap();
    let stale_finished = harness
        .wait_batch(stale_batch["batch_id"].as_str().unwrap())
        .await;
    let stale_item = stale_finished["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == stale_job)
        .unwrap();
    assert_eq!(stale_item["status"], "needs_confirmation");
    assert_eq!(harness.account(stale_id).await["proxy_id"], 97);
    assert_eq!(
        harness
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts/bulk-update")
            .len(),
        writes_before_stale_check,
        "a proxy field changed under the account lease after acceptance is confirmed without a native write"
    );
    harness.finish().await;
}

#[tokio::test]
async fn proxy_native_http_routes_headers_and_whitelists_match_python_oracle_at_loopback() {
    let mut harness = Harness::new().await;
    let fixture_data = fixtures();
    let oracle = fixture_data["python_oracle"]["proxy_native_http"].clone();
    harness
        .upstream
        .set_diagnostic_result("test", oracle["test_input"].clone());
    harness
        .upstream
        .set_diagnostic_result("quality-check", oracle["quality_input"].clone());

    let test_path = format!("/api/v1/admin/proxies/{PROXY_ID}/test");
    let (test_status, connection, connection_text) = harness
        .request(Method::POST, &format!("proxies/{PROXY_ID}/test"), None)
        .await;
    assert_eq!(test_status, 200);
    assert_eq!(connection, oracle["test_result"]);
    assert_eq!(connection["latency_ms"], 13);
    assert!(!connection_text.contains("fixture-pass"));
    assert!(!connection_text.contains("fixture-user"));

    let test_calls = harness.upstream.matching(Method::POST, &test_path);
    assert_eq!(test_calls.len(), 1);
    assert_eq!(test_calls[0].path, test_path);
    assert_eq!(test_calls[0].uri, test_path);
    assert_eq!(test_calls[0].body, json!({}));
    assert_eq!(test_calls[0].api_key, ADMIN_KEY);

    let quality_path = format!("/api/v1/admin/proxies/{PROXY_ID}/quality-check");
    let (quality_status, quality, quality_text) = harness
        .request(
            Method::POST,
            &format!("proxies/{PROXY_ID}/quality-check"),
            None,
        )
        .await;
    assert_eq!(quality_status, 200);
    assert_eq!(quality, oracle["quality_result"]);
    assert_eq!(quality["score"], 80);
    assert_eq!(quality["items"][0]["status"], "challenge");
    assert!(!quality_text.contains("fixture-pass"));
    assert!(!quality_text.contains("fixture-user"));

    let quality_calls = harness.upstream.matching(Method::POST, &quality_path);
    assert_eq!(quality_calls.len(), 1);
    assert_eq!(quality_calls[0].path, quality_path);
    assert_eq!(quality_calls[0].uri, quality_path);
    assert_eq!(quality_calls[0].body, json!({}));
    assert_eq!(quality_calls[0].api_key, ADMIN_KEY);
    harness.finish().await;
}

#[tokio::test]
async fn proxy_diagnostics_route_auth_coalescing_sanitization_and_unsupported_status_match_python()
{
    let mut harness = Harness::new().await;
    let test_path = format!("/api/v1/admin/proxies/{PROXY_ID}/test");
    let quality_path = format!("/api/v1/admin/proxies/{PROXY_ID}/quality-check");
    let test_gate = harness.upstream.gate(&test_path);
    let quality_gate = harness.upstream.gate(&quality_path);
    let base = format!(
        "{}/api/desktop/v1/proxies/{PROXY_ID}/test",
        harness.base_url
    );
    let first_client = harness.client.clone();
    let first = tokio::spawn(async move {
        let response = first_client
            .post(base)
            .header("x-api-key", ADMIN_KEY)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        let value = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, value, text)
    });
    tokio::time::timeout(Duration::from_secs(5), test_gate.entered.notified())
        .await
        .expect("the first native diagnostic should enter its loopback gate");

    let duplicate_url = format!(
        "{}/api/desktop/v1/proxies/{PROXY_ID}/test",
        harness.base_url
    );
    let duplicate_client = harness.client.clone();
    let duplicate = tokio::spawn(async move {
        let response = duplicate_client
            .post(duplicate_url)
            .header("x-api-key", ADMIN_KEY)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        let value = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, value, text)
    });

    let quality_url = format!(
        "{}/api/desktop/v1/proxies/{PROXY_ID}/quality-check",
        harness.base_url
    );
    let quality_client = harness.client.clone();
    let quality = tokio::spawn(async move {
        let response = quality_client
            .post(quality_url)
            .header("x-api-key", ADMIN_KEY)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        let value = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, value, text)
    });
    tokio::time::timeout(Duration::from_secs(5), quality_gate.entered.notified())
        .await
        .expect("a different diagnostic action must use an independent in-flight request");

    let test_calls = harness.upstream.matching(Method::POST, &test_path);
    let quality_calls = harness.upstream.matching(Method::POST, &quality_path);
    assert_eq!(
        test_calls.len(),
        1,
        "same proxy/action clicks share one request"
    );
    assert_eq!(
        quality_calls.len(),
        1,
        "quality-check is independent of test"
    );
    assert_eq!(test_calls[0].body, json!({}));
    assert_eq!(quality_calls[0].body, json!({}));
    assert_eq!(test_calls[0].api_key, ADMIN_KEY);
    assert_eq!(quality_calls[0].api_key, ADMIN_KEY);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        harness.upstream.matching(Method::POST, &test_path).len(),
        1,
        "a concurrent duplicate must coalesce before the in-flight request is released"
    );

    test_gate.release.notify_one();
    quality_gate.release.notify_one();
    let first = first.await.unwrap();
    let duplicate = duplicate.await.unwrap();
    let quality = quality.await.unwrap();
    assert_eq!(first.0, 200);
    assert_eq!(duplicate.0, 200);
    assert_eq!(
        first.1,
        fixtures()["python_oracle"]["proxy_diagnostics"]["test_result"]
    );
    assert_eq!(first.1, duplicate.1);
    for private in [
        "secret-address.example.invalid",
        "fixture-private-user",
        "fixture-private-password",
        "fixture-private-token",
    ] {
        assert!(!first.2.contains(private), "test result exposed {private}");
        assert!(
            !quality.2.contains(private),
            "quality result exposed {private}"
        );
    }
    assert_eq!(quality.0, 200);
    assert_eq!(
        quality.1,
        fixtures()["python_oracle"]["proxy_diagnostics"]["quality_result"]
    );
    assert_eq!(quality.1["checked_at"], 1_735_689_600);
    assert_eq!(quality.1["items"].as_array().unwrap().len(), 1);

    harness.upstream.ungate(&test_path);
    harness.upstream.ungate(&quality_path);

    harness.upstream.set_diagnostic_status(404);
    let unsupported = harness
        .request(Method::POST, &format!("proxies/{PROXY_ID}/test"), None)
        .await;
    assert_eq!(unsupported.0, 502);
    assert!(unsupported.1.to_string().contains("不支持"));
    assert_eq!(harness.upstream.matching(Method::POST, &test_path).len(), 2);
    harness.upstream.set_diagnostic_status(503);
    let failed = harness
        .request(
            Method::POST,
            &format!("proxies/{PROXY_ID}/quality-check"),
            None,
        )
        .await;
    assert_eq!(failed.0, 502);
    assert_eq!(
        harness.upstream.matching(Method::POST, &quality_path).len(),
        2
    );
    harness.finish().await;
}

#[tokio::test]
async fn proxy_diagnostic_waits_for_the_real_90_second_timeout_without_replay() {
    let mut harness = Harness::new().await;
    let path = format!("/api/v1/admin/proxies/{PROXY_ID}/test");
    let unauthorized = harness
        .request_with_key(
            Method::POST,
            &format!("proxies/{PROXY_ID}/test"),
            None,
            "fixture-account-mutation-wrong-key",
        )
        .await;
    assert_eq!(unauthorized.0, 401);
    assert!(harness.upstream.matching(Method::POST, &path).is_empty());

    let gate = harness.upstream.gate(&path);
    let url = format!(
        "{}/api/desktop/v1/proxies/{PROXY_ID}/test",
        harness.base_url
    );
    let client = harness.client.clone();
    let started = std::time::Instant::now();
    let request = tokio::spawn(async move {
        let response = client
            .post(url)
            .header("x-api-key", ADMIN_KEY)
            .send()
            .await
            .expect("receive the bounded local diagnostic result");
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (status, text)
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("the one native proxy diagnostic should enter the loopback gate");
    let calls = harness.upstream.matching(Method::POST, &path);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body, json!({}));
    assert_eq!(calls[0].api_key, ADMIN_KEY);

    let result = tokio::time::timeout(Duration::from_secs(100), request)
        .await
        .expect("the configured 90-second request bound should return")
        .expect("the local diagnostic task should not panic");
    let elapsed = started.elapsed();
    assert_eq!(result.0, 504);
    assert!(
        elapsed >= Duration::from_secs(90),
        "the diagnostic returned early after {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(100));
    assert!(result.1.contains("超时"));
    assert!(result.1.contains("未重放"));
    gate.release.notify_one();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        harness.upstream.matching(Method::POST, &path).len(),
        1,
        "the timed-out request must not be replayed"
    );
    harness.finish().await;
}

async fn managed_keys(harness: &Harness, ids: &[i64]) {
    harness
        .runtime
        .config
        .fallback
        .save(
            &json!({"managed_account_ids":ids,"openai_enabled":false,"grok_enabled":true}),
            "fixture",
        )
        .await
        .unwrap();
}

fn manual_writes(harness: &Harness) -> Vec<Call> {
    harness
        .upstream
        .calls()
        .into_iter()
        .filter(|call| call.method != Method::GET)
        .collect()
}

#[tokio::test]
async fn management_auth_cache_refresh_and_invalid_keys_never_expose_key_in_url() {
    let mut h = Harness::new().await;
    h.runtime
        .upstream
        .authenticate(ADMIN_KEY, false)
        .await
        .unwrap();
    h.runtime
        .upstream
        .authenticate(ADMIN_KEY, false)
        .await
        .unwrap();
    assert_eq!(h.upstream.calls().len(), 1);
    h.runtime
        .upstream
        .authenticate(ADMIN_KEY, true)
        .await
        .unwrap();
    assert_eq!(h.upstream.calls().len(), 2);
    for key in ["", "fixture-bad", "fixture\nkey"] {
        assert_eq!(
            h.runtime
                .upstream
                .authenticate(key, false)
                .await
                .err()
                .unwrap()
                .status
                .as_u16(),
            401
        );
    }
    let calls = h.upstream.calls();
    assert_eq!(
        calls.len(),
        3,
        "empty and newline keys never reach upstream"
    );
    assert!(calls
        .iter()
        .all(|c| c.method == Method::GET && c.uri == "/api/v1/admin/groups/all"));
    assert!(calls.iter().all(|c| !c.uri.contains(ADMIN_KEY)));
    assert!(!h.runtime.upstream.status().to_string().contains(ADMIN_KEY));
    assert!(manual_writes(&h).is_empty());
    h.finish().await;
}

#[tokio::test]
async fn management_delete_detaches_before_request_and_preserves_other_keys_and_history() {
    let mut h = Harness::new().await;
    let a = h
        .insert_account("openai", "apikey", json!({}), json!({}), None, &[])
        .await;
    let b = h
        .insert_account("grok", "apikey", json!({}), json!({}), None, &[])
        .await;
    managed_keys(&h, &[a, b]).await;
    h.runtime
        .monitor
        .store
        .0
        .transaction(|s| {
            s["recovery_history"] = json!({"fixture-old":{"id":10,"account_id":a}});
            Ok(())
        })
        .unwrap();
    let expected = account_version(&h.account(a).await);
    let path = format!("/api/v1/admin/accounts/{a}");
    let gate = h.upstream.gate(&path);
    let request = h
        .client
        .delete(format!("{}/api/desktop/v1/accounts/{a}", h.base_url))
        .header("x-api-key", ADMIN_KEY)
        .json(&json!({"expected_version":expected,"detach_managed":true}));
    let pending = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .unwrap();
    let detached = h.runtime.config.fallback.load();
    assert_eq!(detached.ids, vec![b]);
    assert!(!detached.openai);
    assert!(detached.grok);
    gate.release.notify_one();
    let response = pending.await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"account_id":a,"deleted":true,"verified":true,"detached":true})
    );
    assert_eq!(
        h.runtime
            .actions
            .account(a, None)
            .await
            .unwrap_err()
            .status
            .as_u16(),
        404
    );
    assert_eq!(
        h.runtime.monitor.store.snapshot().unwrap()["recovery_history"],
        json!({"fixture-old":{"id":10,"account_id":a}})
    );
    let calls = manual_writes(&h);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, Method::DELETE);
    assert_eq!(calls[0].path, path);
    assert!(calls[0].body.is_null());
    h.finish().await;
}

#[tokio::test]
async fn management_invalid_auth_version_deleted_account_and_failed_detach_never_dispatch() {
    let mut h = Harness::new().await;
    let id = h
        .insert_account("openai", "apikey", json!({}), json!({}), None, &[])
        .await;
    managed_keys(&h, &[id]).await;
    let version = account_version(&h.account(id).await);
    let payload = json!({"expected_version":version,"detach_managed":true});
    let path = format!("accounts/{id}");
    assert_eq!(
        h.request_with_key(Method::DELETE, &path, Some(payload.clone()), "fixture-bad")
            .await
            .0,
        401
    );
    assert_eq!(
        h.request(
            Method::DELETE,
            &path,
            Some(json!({"expected_version":"a".repeat(64),"detach_managed":true}))
        )
        .await
        .0,
        409
    );
    assert_eq!(
        h.request(Method::DELETE, "accounts/9999999999", Some(payload.clone()))
            .await
            .0,
        404
    );
    assert_eq!(
        h.request(
            Method::DELETE,
            &path,
            Some(json!({"expected_version":version}))
        )
        .await
        .0,
        409
    );
    let fallback = h.runtime.settings.fallback_path.clone();
    let before = fs::read(&fallback).unwrap();
    let lock = fallback.with_extension("lock");
    let saved = h._temp.path().join("fixture-fallback-lock");
    fs::rename(&lock, &saved).unwrap();
    fs::create_dir(&lock).unwrap();
    let failed = h.request(Method::DELETE, &path, Some(payload)).await;
    fs::remove_dir(&lock).unwrap();
    fs::rename(saved, &lock).unwrap();
    assert_eq!(failed.0, 503);
    assert!(!failed.2.contains("fixture-fallback-lock"));
    assert_eq!(fs::read(&fallback).unwrap(), before);
    assert_eq!(h.runtime.config.fallback.load().ids, vec![id]);
    assert!(manual_writes(&h).is_empty());
    h.finish().await;
}

#[tokio::test]
async fn management_delete_rejection_keeps_detached_state_and_disconnect_uses_readback_once() {
    let mut h = Harness::new().await;
    let id = h
        .insert_account("openai", "apikey", json!({}), json!({}), None, &[])
        .await;
    managed_keys(&h, &[id]).await;
    let payload =
        json!({"expected_version":account_version(&h.account(id).await),"detach_managed":true});
    *h.upstream.manual_mode.lock().unwrap() = ManualMode::Rejected;
    let failed = h
        .request(
            Method::DELETE,
            &format!("accounts/{id}"),
            Some(payload.clone()),
        )
        .await;
    assert_eq!(failed.0, 502);
    assert_eq!(failed.1["detail"]["detached"], true);
    assert!(failed.2.contains("已解除托管"));
    assert_eq!(failed.1["detail"]["code"], "http_500");
    assert!(!failed.2.contains("fixture-private"));
    assert!(h.runtime.config.fallback.load().ids.is_empty());
    assert!(h.runtime.actions.account(id, None).await.is_ok());
    assert_eq!(manual_writes(&h).len(), 1);
    *h.upstream.manual_mode.lock().unwrap() = ManualMode::DisconnectedApplied;
    let success = h
        .request(Method::DELETE, &format!("accounts/{id}"), Some(payload))
        .await;
    assert_eq!(success.0, 200);
    assert_eq!(success.1["deleted"], true);
    assert_eq!(success.1["verified"], true);
    assert_eq!(manual_writes(&h).len(), 2);
    assert_eq!(
        h.runtime
            .actions
            .account(id, None)
            .await
            .unwrap_err()
            .status
            .as_u16(),
        404
    );
    h.finish().await;
}

#[tokio::test]
async fn management_parallel_deletes_succeed_but_same_account_overlap_rejects_without_queueing() {
    let mut h = Harness::new().await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(
            h.insert_account("openai", "apikey", json!({}), json!({}), None, &[])
                .await,
        );
    }
    managed_keys(&h, &ids[..2]).await;
    let mut requests = Vec::new();
    for id in &ids {
        let request=h.client.delete(format!("{}/api/desktop/v1/accounts/{id}",h.base_url))
            .header("x-api-key",ADMIN_KEY).json(&json!({"expected_version":account_version(&h.account(*id).await),"detach_managed":true}));
        requests.push(tokio::spawn(async move {
            request.send().await.unwrap().status().as_u16()
        }));
    }
    for request in requests {
        assert_eq!(request.await.unwrap(), 200);
    }
    assert!(h.runtime.config.fallback.load().ids.is_empty());
    assert_eq!(manual_writes(&h).len(), 3);
    for id in &ids {
        assert_eq!(
            h.runtime
                .actions
                .account(*id, None)
                .await
                .unwrap_err()
                .status
                .as_u16(),
            404
        );
    }
    let id = h
        .insert_account("openai", "apikey", json!({}), json!({}), None, &[])
        .await;
    let path = format!("/api/v1/admin/accounts/{id}");
    let gate = h.upstream.gate(&path);
    *h.upstream.manual_mode.lock().unwrap() = ManualMode::Rejected;
    let payload = json!({"expected_version":account_version(&h.account(id).await)});
    let request = h
        .client
        .delete(format!("{}/api/desktop/v1/accounts/{id}", h.base_url))
        .header("x-api-key", ADMIN_KEY)
        .json(&payload);
    let pending = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .unwrap();
    let rejected = tokio::time::timeout(
        Duration::from_secs(1),
        h.request(Method::DELETE, &format!("accounts/{id}"), Some(payload)),
    )
    .await
    .unwrap();
    assert_eq!(rejected.0, 409);
    gate.release.notify_one();
    assert_eq!(pending.await.unwrap().status().as_u16(), 502);
    assert_eq!(h.upstream.matching(Method::DELETE, &path).len(), 1);
    h.finish().await;
}

#[tokio::test]
async fn management_recover_exact_request_preserves_switches_history_and_managed_ownership() {
    let mut h = Harness::new().await;
    let id = h
        .insert_account("openai", "apikey", json!({}), json!({}), None, &[])
        .await;
    managed_keys(&h, &[id]).await;
    sqlx::query("UPDATE accounts SET status='error',schedulable=false,rate_limit_reset_at=now()+interval '5 minutes' WHERE id=$1")
        .bind(id).execute(&h.runtime.db.pool).await.unwrap();
    let history = h.runtime.monitor.store.snapshot().unwrap()["recovery_history"].clone();
    let payload = json!({"expected_version":account_version(&h.account(id).await)});
    let response = h
        .request(
            Method::POST,
            &format!("accounts/{id}/recover-state"),
            Some(payload),
        )
        .await;
    assert_eq!(response.0, 200);
    assert_eq!(response.1["verified"], true);
    assert!(!response.2.contains("credentials"));
    let row = h.account(id).await;
    assert_eq!(row["status"], "active");
    assert_eq!(row["schedulable"], false);
    assert!(row["rate_limit_reset_at"].is_null());
    assert_eq!(h.runtime.config.fallback.load().ids, vec![id]);
    assert_eq!(
        h.runtime.monitor.store.snapshot().unwrap()["recovery_history"],
        history
    );
    let calls = manual_writes(&h);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, Method::POST);
    assert_eq!(
        calls[0].path,
        format!("/api/v1/admin/accounts/{id}/recover-state")
    );
    assert_eq!(calls[0].body, json!({}));
    h.finish().await;
}

#[tokio::test]
async fn management_recover_live_model_limit_expired_state_failures_and_disconnect_do_not_retry() {
    let mut h = Harness::new().await;
    let id=h.insert_account("openai","apikey",json!({}),json!({"model_rate_limits":{"fixture-model":{"rate_limit_reset_at":(chrono::Utc::now()+chrono::Duration::minutes(2)).to_rfc3339()}}}),None,&[]).await;
    let path = format!("accounts/{id}/recover-state");
    assert!(sub2ops_server::queries::recoverable(
        &h.account(id).await,
        chrono::Utc::now()
    ));
    let r = h
        .request(
            Method::POST,
            &path,
            Some(json!({"expected_version":account_version(&h.account(id).await)})),
        )
        .await;
    assert_eq!(r.0, 200);
    sqlx::query("UPDATE accounts SET rate_limit_reset_at=now()-interval '1 second' WHERE id=$1")
        .bind(id)
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    assert!(!sub2ops_server::queries::recoverable(
        &h.account(id).await,
        chrono::Utc::now()
    ));
    assert_eq!(
        h.request(
            Method::POST,
            &path,
            Some(json!({"expected_version":account_version(&h.account(id).await)}))
        )
        .await
        .0,
        409
    );
    assert_eq!(manual_writes(&h).len(), 1);
    sqlx::query("UPDATE accounts SET status='error' WHERE id=$1")
        .bind(id)
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    let payload = json!({"expected_version":account_version(&h.account(id).await)});
    *h.upstream.manual_mode.lock().unwrap() = ManualMode::Rejected;
    assert_eq!(
        h.request(Method::POST, &path, Some(payload.clone()))
            .await
            .0,
        502
    );
    assert_eq!(manual_writes(&h).len(), 2);
    *h.upstream.manual_mode.lock().unwrap() = ManualMode::DisconnectedApplied;
    let success = h.request(Method::POST, &path, Some(payload)).await;
    assert_eq!(success.0, 200);
    assert_eq!(success.1["verified"], true);
    assert_eq!(manual_writes(&h).len(), 3);
    h.finish().await;
}

#[tokio::test]
async fn management_schedule_detaches_before_exact_write_and_failed_readback_never_reenrolls() {
    let mut h = Harness::new().await;
    let id = h
        .insert_account("openai", "apikey", json!({}), json!({}), None, &[])
        .await;
    let other = h
        .insert_account("grok", "apikey", json!({}), json!({}), None, &[])
        .await;
    managed_keys(&h, &[id, other]).await;
    let path = format!("/api/v1/admin/accounts/{id}/schedulable");
    let gate = h.upstream.gate(&path);
    let request=h.client.post(format!("{}/api/desktop/v1/accounts/{id}/schedulable",h.base_url))
        .header("x-api-key",ADMIN_KEY).json(&json!({"expected_version":account_version(&h.account(id).await),"schedulable":false,"detach_managed":true}));
    let pending = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .unwrap();
    let config = h.runtime.config.fallback.load();
    assert_eq!(config.ids, vec![other]);
    assert!(!config.openai);
    assert!(config.grok);
    gate.release.notify_one();
    h.upstream.ungate(&path);
    let response = pending.await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["verified"], true);
    assert_eq!(body["detached"], true);
    assert_eq!(h.account(id).await["schedulable"], false);
    let calls = manual_writes(&h);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, Method::POST);
    assert_eq!(calls[0].path, path);
    assert_eq!(calls[0].body, json!({"schedulable":false}));

    managed_keys(&h, &[id, other]).await;
    *h.upstream.manual_mode.lock().unwrap() = ManualMode::Unchanged;
    let failed=h.request(Method::POST,&format!("accounts/{id}/schedulable"),Some(json!({"expected_version":account_version(&h.account(id).await),"schedulable":true,"detach_managed":true}))).await;
    assert_eq!(failed.0, 502);
    assert_eq!(failed.1["detail"]["code"], "readback_mismatch");
    assert_eq!(failed.1["detail"]["detached"], true);
    assert_eq!(h.runtime.config.fallback.load().ids, vec![other]);
    assert_eq!(h.account(id).await["schedulable"], false);
    managed_keys(&h, &[id, other]).await;
    *h.upstream.manual_mode.lock().unwrap() = ManualMode::Rejected;
    let failed=h.request(Method::POST,&format!("accounts/{id}/schedulable"),Some(json!({"expected_version":account_version(&h.account(id).await),"schedulable":true,"detach_managed":true}))).await;
    assert_eq!(failed.0, 502);
    assert_eq!(failed.1["detail"]["detached"], true);
    assert_eq!(h.runtime.config.fallback.load().ids, vec![other]);
    assert_eq!(manual_writes(&h).len(), 3);
    h.finish().await;
}

#[tokio::test]
async fn management_schedule_conflicts_and_failed_detach_stop_before_any_upstream_write() {
    let mut h = Harness::new().await;
    let id = h
        .insert_account("openai", "apikey", json!({}), json!({}), None, &[])
        .await;
    managed_keys(&h, &[id]).await;
    let version = account_version(&h.account(id).await);
    let path = format!("accounts/{id}/schedulable");
    assert_eq!(
        h.request(
            Method::POST,
            &path,
            Some(json!({"expected_version":version,"schedulable":false}))
        )
        .await
        .0,
        409
    );
    let payload = json!({"expected_version":version,"schedulable":false,"detach_managed":true});
    let file = h.runtime.settings.fallback_path.clone();
    let before = fs::read(&file).unwrap();
    let lock = file.with_extension("lock");
    let saved = h._temp.path().join("fixture-fallback-lock");
    fs::rename(&lock, &saved).unwrap();
    fs::create_dir(&lock).unwrap();
    assert_eq!(
        h.request(Method::POST, &path, Some(payload.clone()))
            .await
            .0,
        500
    );
    fs::remove_dir(&lock).unwrap();
    fs::rename(saved, &lock).unwrap();
    assert_eq!(fs::read(&file).unwrap(), before);
    sqlx::query("UPDATE accounts SET type='oauth' WHERE id=$1")
        .bind(id)
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    assert_eq!(
        h.request(Method::POST, &path, Some(payload.clone()))
            .await
            .0,
        409
    );
    assert_eq!(h.runtime.config.fallback.load().ids, vec![id]);
    sqlx::query("DELETE FROM accounts WHERE id=$1")
        .bind(id)
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    assert_eq!(h.request(Method::POST, &path, Some(payload)).await.0, 404);
    assert_eq!(h.runtime.config.fallback.load().ids, vec![id]);
    assert!(manual_writes(&h).is_empty());
    h.finish().await;
}

#[tokio::test]
async fn management_recovery_history_filters_live_accounts_before_cursor_and_redacts_receipts() {
    let mut h = Harness::new().await;
    let mut accounts = Vec::new();
    for _ in 0..3 {
        accounts.push(
            h.insert_account("openai", "apikey", json!({}), json!({}), None, &[])
                .await,
        );
    }
    let history:serde_json::Map<String,Value>=(1..=30).map(|n| {
        let account=match n {1=>accounts[0],3=>accounts[1],8=>accounts[2],_=>9_999_990_000+n};
        (n.to_string(),json!({"id":n,"account_id":account,"account_name":format!("fixture-history-{n}"),"kind":"connection","legacy":false}))
    }).collect();
    h.runtime
        .monitor
        .store
        .transaction(|s| {
            s["recovery_history"] = json!(history);
            Ok(())
        })
        .unwrap();
    let original = h.runtime.monitor.store.snapshot().unwrap()["recovery_history"].clone();
    let queries = &h.runtime.actions.queries;
    let first = queries.recoveries(None, 2).await.unwrap();
    let ids = |value: &Value| {
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_i64().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&first), vec![8, 3]);
    assert_eq!(first["next_cursor"], 3);
    assert_eq!(ids(&queries.recoveries(Some(3), 2).await.unwrap()), vec![1]);
    sqlx::query("UPDATE accounts SET deleted_at=now() WHERE id=$1")
        .bind(accounts[2])
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    assert_eq!(
        ids(&queries.recoveries(None, 20).await.unwrap()),
        vec![3, 1]
    );
    sqlx::query("UPDATE accounts SET deleted_at=now() WHERE id=ANY($1)")
        .bind(&accounts)
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    assert_eq!(
        queries.recoveries(None, 2).await.unwrap(),
        json!({"items":[],"next_cursor":null})
    );
    assert_eq!(
        h.runtime.monitor.store.snapshot().unwrap()["recovery_history"],
        original
    );
    sqlx::query("UPDATE accounts SET deleted_at=NULL WHERE id=$1")
        .bind(accounts[0])
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    let receipt = json!({"consumed":true,"completed_at":"2026-09-30T08:00:00+00:00","verification_method":"model"});
    h.runtime.monitor.store.transaction(|s| {
        s["recovery_history"]=json!({"fixture-reset":{"id":31,"account_id":accounts[0],"model_id":"fixture-model","legacy":false,"kind":"reset_credit",
            "test_completed_at":"2026-09-30T08:00:01+00:00","recovered_at":"2026-09-30T08:00:02+00:00",
            "reset_credit":{"consumed":true,"completed_at":"2026-09-30T08:00:00+00:00","verification_method":"model","card_id":"fixture-private-card","message":"fixture-private-response"}}});Ok(())
    }).unwrap();
    let result = queries.recoveries(None, 20).await.unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["kind"], "reset_credit");
    assert_eq!(result["items"][0]["reset_credit"], receipt);
    assert_eq!(result["items"][0]["model_id"], "fixture-model");
    assert!(!result.to_string().contains("fixture-private"));
    assert!(manual_writes(&h).is_empty());
    h.finish().await;
}

#[tokio::test]
async fn connection_http_missing_model_setting_uses_legacy_default_for_native_request_and_summary()
{
    let mut h = Harness::with_model_setting(None).await;
    let ids = h.connection_accounts().await;
    let request = h
        .connection_batch(&ids[..1], "fixture-http-missing-model-0001")
        .await;
    let (status, accepted, _) = h
        .request(Method::POST, "account-operations/batch", Some(request))
        .await;
    assert_eq!(status, 202);
    assert_eq!(
        accepted["items"][0]["requested"]["model_id"],
        "gpt-5.6-luna"
    );
    h.runtime.operations.tick().await.unwrap();
    let finished = h.wait_batch(accepted["batch_id"].as_str().unwrap()).await;
    assert_eq!(finished["items"][0]["status"], "completed");
    assert_eq!(finished["items"][0]["result"]["success"], true);
    let calls = h.upstream.matching(
        Method::POST,
        &format!("/api/v1/admin/accounts/{}/test", ids[0]),
    );
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["model_id"], "gpt-5.6-luna");
    assert_eq!(
        finished["items"][0]["result"]["requested_model"],
        "gpt-5.6-luna"
    );
    h.finish().await;
}

#[tokio::test]
async fn connection_http_acceptance_is_202_atomic_order_independent_and_survives_runtime_restart() {
    let mut h = Harness::new().await;
    let ids = h.connection_accounts().await;
    let request = h
        .connection_batch(&ids, "fixture-http-batch-idempotency-0001")
        .await;
    let mut reversed = request.clone();
    reversed["accounts"].as_array_mut().unwrap().reverse();
    let (first, duplicate) = tokio::join!(
        h.request(
            Method::POST,
            "account-operations/batch",
            Some(request.clone())
        ),
        h.request(Method::POST, "account-operations/batch", Some(reversed))
    );
    assert_eq!(first.0, 202);
    assert_eq!(duplicate.0, 202);
    assert_eq!(first.1, duplicate.1);
    let batch = first.1;
    let bid = batch["batch_id"].as_str().unwrap().to_owned();
    assert_eq!(batch["pending"], 3);
    let items: BTreeMap<i64, &Value> = batch["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["account_id"].as_i64().unwrap(), v))
        .collect();
    assert!(items
        .values()
        .all(|item| item["action"] == "test" && item["status"] == "queued"));
    assert_eq!(
        items[&ids[0]]["requested"],
        json!({"confirmed":true,"mode":"default","model_id":"fixture-model"})
    );
    assert_eq!(
        items[&ids[1]]["requested"],
        json!({"confirmed":true,"mode":"text","model_id":""})
    );
    assert_eq!(items[&ids[2]]["requested"], items[&ids[0]]["requested"]);
    let state = h.runtime.operations.store.read().unwrap();
    assert_eq!(state["jobs"].as_object().unwrap().len(), 3);
    assert_eq!(state["batches"].as_object().unwrap().len(), 1);
    assert_eq!(
        state["batch_requests"],
        json!({request["request_id"].as_str().unwrap():bid})
    );
    assert_eq!(h.batch_listing(&bid).await, batch);
    h.restart_api().await;
    assert_eq!(h.batch_listing(&bid).await, batch);
    let replay = h
        .request(Method::POST, "account-operations/batch", Some(request))
        .await;
    assert_eq!(replay.0, 202);
    assert_eq!(replay.1, batch);
    assert_eq!(
        h.runtime.operations.store.read().unwrap()["jobs"]
            .as_object()
            .unwrap()
            .len(),
        3
    );
    assert!(manual_writes(&h).is_empty());
    for marker in [ADMIN_KEY, "fixture-account-secret"] {
        assert!(!fs::read_to_string(h.runtime.operations.store.0.path())
            .unwrap()
            .contains(marker));
        assert!(!batch.to_string().contains(marker));
    }
    h.finish().await;
}

#[tokio::test]
async fn connection_http_conflicts_auth_schema_and_missing_batches_preserve_state() {
    let mut h = Harness::new().await;
    let ids = h.connection_accounts().await;
    let request = h
        .connection_batch(&ids[..1], "fixture-http-contract-boundaries-0001")
        .await;
    let accepted = h
        .request(
            Method::POST,
            "account-operations/batch",
            Some(request.clone()),
        )
        .await;
    assert_eq!(accepted.0, 202);
    let batch = accepted.1;
    let bid = batch["batch_id"].as_str().unwrap();
    let jid = batch["items"][0]["id"].as_str().unwrap();
    let before = fs::read(h.runtime.operations.store.0.path()).unwrap();
    let changed = h
        .connection_batch(&ids[1..2], request["request_id"].as_str().unwrap())
        .await;
    let proxy = native_batch(
        "proxy",
        json!({"proxy_id":null}),
        vec![
            json!({"account_id":ids[0],"expected_version":operation_version(&h.account(ids[0]).await,"proxy")}),
        ],
        request["request_id"].as_str().unwrap(),
    );
    for body in [changed, proxy] {
        let result = h
            .request(Method::POST, "account-operations/batch", Some(body))
            .await;
        assert_eq!(result.0, 409);
        assert_eq!(result.1["detail"]["code"], "idempotency_conflict");
        assert_eq!(
            fs::read(h.runtime.operations.store.0.path()).unwrap(),
            before
        );
    }
    let next = h
        .connection_batch(&ids[1..2], "fixture-http-auth-batch-0002")
        .await;
    for key in ["", "fixture-invalid-admin"] {
        for (method, path, body) in [
            (
                Method::POST,
                "account-operations/batch".into(),
                Some(next.clone()),
            ),
            (
                Method::GET,
                format!("account-operations?batch_id={bid}"),
                None,
            ),
            (
                Method::POST,
                format!("account-operations/batches/{bid}/cancel"),
                None,
            ),
            (
                Method::POST,
                format!("account-operations/{jid}/cancel"),
                None,
            ),
        ] {
            let result = h.request_with_key(method, &path, body, key).await;
            assert_eq!(result.0, 401);
            assert_eq!(
                fs::read(h.runtime.operations.store.0.path()).unwrap(),
                before
            );
        }
    }
    let mut invalids = Vec::new();
    for (field, value) in [
        ("model_id", json!("fixture-model")),
        ("mode", json!("image")),
        ("prompt", json!("fixture-request-body-must-not-escape")),
        ("proxy_id", json!(17)),
        ("confirmed", json!("true")),
    ] {
        let mut body = next.clone();
        body["payload"][field] = value;
        invalids.push(body);
    }
    let mut body = next.clone();
    body["upstream_url"] = "https://example.invalid".into();
    invalids.push(body);
    let mut body = next.clone();
    body["accounts"][0]["payload"] = json!({"model_id":"fixture-model"});
    invalids.push(body);
    let mut body = next.clone();
    body["action"] = "proxy".into();
    body["payload"] = json!({"proxy_id":null,"confirmed":true});
    invalids.push(body);
    for body in invalids {
        let result = h
            .request(Method::POST, "account-operations/batch", Some(body))
            .await;
        assert_eq!(result.0, 422);
        assert_eq!(result.1["detail"]["code"], "invalid_request");
        assert!(!result.2.contains("fixture-request-body-must-not-escape"));
        assert_eq!(
            fs::read(h.runtime.operations.store.0.path()).unwrap(),
            before
        );
    }
    for missing in ["f".repeat(32), "fixture-invalid-batch".into()] {
        for (method, path) in [
            (
                Method::GET,
                format!("account-operations?batch_id={missing}"),
            ),
            (
                Method::POST,
                format!("account-operations/batches/{missing}/cancel"),
            ),
        ] {
            assert_eq!(h.request(method, &path, None).await.0, 404);
            assert_eq!(
                fs::read(h.runtime.operations.store.0.path()).unwrap(),
                before
            );
        }
    }
    assert_eq!(h.batch_listing(bid).await, batch);
    assert!(manual_writes(&h).is_empty());
    h.finish().await;
}

#[tokio::test]
async fn connection_http_proxy_union_preserves_existing_action_and_rejects_test_cancel() {
    let mut h = Harness::new().await;
    let ids = h.connection_accounts().await;
    sqlx::query("INSERT INTO proxies(id,protocol,host,port) VALUES($1,'http','example.invalid',8080) ON CONFLICT(id) DO NOTHING").bind(PROXY_ID).execute(&h.runtime.db.pool).await.unwrap();
    sqlx::query("UPDATE accounts SET proxy_id=$1 WHERE id=ANY($2)")
        .bind(PROXY_ID)
        .bind(&ids[1..])
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    let mut accounts = Vec::new();
    for id in &ids {
        accounts.push(json!({"account_id":id,"expected_version":operation_version(&h.account(*id).await,"proxy")}));
    }
    let request = native_batch(
        "proxy",
        json!({"proxy_id":null}),
        accounts,
        "fixture-http-proxy-branch-0001",
    );
    let accepted = h
        .request(Method::POST, "account-operations/batch", Some(request))
        .await;
    assert_eq!(accepted.0, 202);
    let batch = accepted.1;
    let items = &batch["items"];
    assert!(items
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["action"] == "proxy" && item["requested"] == json!({"proxy_id":null})));
    assert_eq!(items[0]["status"], "completed");
    assert_eq!(items[1]["status"], "queued");
    assert_eq!(items[2]["status"], "queued");
    assert_eq!(batch["pending"], 2);
    let bid = batch["batch_id"].as_str().unwrap();
    assert_eq!(h.batch_listing(bid).await, batch);
    let before = fs::read(h.runtime.operations.store.0.path()).unwrap();
    assert_eq!(
        h.request(
            Method::POST,
            &format!("account-operations/batches/{bid}/cancel"),
            None
        )
        .await
        .0,
        404
    );
    assert_eq!(
        fs::read(h.runtime.operations.store.0.path()).unwrap(),
        before
    );
    assert!(manual_writes(&h).is_empty());
    h.finish().await;
}

#[tokio::test]
async fn connection_http_cancel_preserves_running_request_and_generic_cancel_cannot_interrupt_it() {
    let mut h = Harness::new().await;
    h.runtime
        .config
        .oauth
        .transaction(|config| {
            config["oauth_recovery_test_concurrency"] = 1.into();
            Ok(())
        })
        .unwrap();
    let ids = h.connection_accounts().await;
    let mut request = h
        .connection_batch(&ids, "fixture-http-cancel-running-0001")
        .await;
    request["accounts"][2]["expected_version"] = "f".repeat(64).into();
    let accepted = h
        .request(Method::POST, "account-operations/batch", Some(request))
        .await;
    assert_eq!(accepted.0, 202);
    let batch = accepted.1;
    let bid = batch["batch_id"].as_str().unwrap().to_owned();
    let jid = batch["items"][0]["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v1/admin/accounts/{}/test", ids[0]);
    let gate = h.upstream.gate(&path);
    h.runtime.operations.tick().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    let running = h
        .request(Method::GET, &format!("account-operations/{jid}"), None)
        .await;
    assert_eq!(running.0, 200);
    assert_eq!(running.1["status"], "running");
    assert!(running.1["started_at"].is_string());
    let before = fs::read(h.runtime.operations.store.0.path()).unwrap();
    let generic = h
        .request(
            Method::POST,
            &format!("account-operations/{jid}/cancel"),
            None,
        )
        .await;
    assert_eq!(generic.0, 200);
    assert_eq!(generic.1, running.1);
    assert_eq!(
        fs::read(h.runtime.operations.store.0.path()).unwrap(),
        before
    );
    let cancelled = h
        .request(
            Method::POST,
            &format!("account-operations/batches/{bid}/cancel"),
            None,
        )
        .await;
    assert_eq!(cancelled.0, 200);
    assert_eq!(cancelled.1["pending"], 1);
    assert_eq!(cancelled.1["items"][0]["status"], "running");
    for item in cancelled.1["items"].as_array().unwrap().iter().skip(1) {
        assert_eq!(item["status"], "cancelled");
    }
    assert_eq!(h.batch_listing(&bid).await, cancelled.1);
    gate.release.notify_one();
    let detail = h.wait_job(&jid, &["completed"]).await;
    let finished = h.batch_listing(&bid).await;
    assert_eq!(finished["pending"], 0);
    assert_eq!(detail["result"]["success"], true);
    assert_eq!(detail["result"]["requested_model"], "fixture-model");
    assert_eq!(detail["result"]["actual_model"], "fixture-model");
    assert!(detail["result"]["duration_ms"]
        .as_i64()
        .is_some_and(|v| v >= 0));
    assert_eq!(detail["events"], json!([]));
    h.runtime.operations.tick().await.unwrap();
    let writes = manual_writes(&h);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, path);
    assert_eq!(writes[0].api_key, ADMIN_KEY);
    assert_eq!(writes[0].body["model_id"], "fixture-model");
    assert_eq!(
        h.request(
            Method::POST,
            &format!("account-operations/batches/{bid}/cancel"),
            None
        )
        .await
        .1,
        finished
    );
    assert!(!finished
        .to_string()
        .contains("fixture-batch-http-private-content"));
    assert!(!fs::read_to_string(h.runtime.operations.store.0.path())
        .unwrap()
        .contains("fixture-batch-http-private-content"));
    h.restart_api().await;
    assert_eq!(h.batch_listing(&bid).await, finished);
    h.finish().await;
}

#[tokio::test]
async fn connection_http_generic_cancel_and_restart_preserve_dispatched_unknown_result() {
    let mut h = Harness::new().await;
    let ids = h.connection_accounts().await;
    h.upstream
        .connection_streams
        .lock()
        .unwrap()
        .insert(ids[0], false);
    let request = h
        .connection_batch(&ids[..1], "fixture-http-unknown-cancel-0001")
        .await;
    let accepted = h
        .request(Method::POST, "account-operations/batch", Some(request))
        .await;
    assert_eq!(accepted.0, 202);
    let bid = accepted.1["batch_id"].as_str().unwrap().to_owned();
    let jid = accepted.1["items"][0]["id"].as_str().unwrap().to_owned();
    h.runtime.operations.tick().await.unwrap();
    let unknown = h.wait_job(&jid, &["needs_confirmation"]).await;
    assert_eq!(unknown["result"]["success"], false);
    assert_eq!(unknown["result"]["uncertain"], true);
    assert!(unknown["started_at"].is_string());
    let before = fs::read(h.runtime.operations.store.0.path()).unwrap();
    let cancelled = h
        .request(
            Method::POST,
            &format!("account-operations/{jid}/cancel"),
            None,
        )
        .await;
    assert_eq!(cancelled.0, 200);
    assert_eq!(cancelled.1, unknown);
    assert_eq!(
        fs::read(h.runtime.operations.store.0.path()).unwrap(),
        before
    );
    h.restart_api().await;
    assert_eq!(
        h.request(
            Method::POST,
            &format!("account-operations/{jid}/cancel"),
            None
        )
        .await
        .1,
        unknown
    );
    h.runtime.operations.tick().await.unwrap();
    let retained = h.batch_listing(&bid).await;
    assert_eq!(retained["pending"], 1);
    assert_eq!(retained["items"][0]["status"], "needs_confirmation");
    assert_eq!(retained["items"][0]["result"], unknown["result"]);
    assert_eq!(manual_writes(&h).len(), 1);
    h.finish().await;
}

#[tokio::test]
async fn connection_http_cancel_during_blocked_account_read_rejects_late_conflict() {
    let mut h = Harness::new().await;
    let ids = h.connection_accounts().await;
    let request = h
        .connection_batch(&ids[..1], "fixture-http-cancel-stale-read-0001")
        .await;
    let accepted = h
        .request(Method::POST, "account-operations/batch", Some(request))
        .await;
    assert_eq!(accepted.0, 202);
    let bid = accepted.1["batch_id"].as_str().unwrap().to_owned();
    let mut blocker = h.runtime.db.pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("LOCK TABLE accounts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    h.runtime.operations.tick().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(pid)
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
            if blocked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let cancelled = h
        .request(
            Method::POST,
            &format!("account-operations/batches/{bid}/cancel"),
            None,
        )
        .await;
    assert_eq!(cancelled.0, 200);
    assert_eq!(cancelled.1["pending"], 0);
    assert_eq!(cancelled.1["items"][0]["status"], "cancelled");
    sqlx::query("UPDATE accounts SET credentials=credentials||'{\"access_token\":\"fixture-late-changed-token\"}'::jsonb WHERE id=$1").bind(ids[0]).execute(&mut *blocker).await.unwrap();
    blocker.commit().await.unwrap();
    h.runtime.operations.close().await;
    assert_eq!(h.batch_listing(&bid).await, cancelled.1);
    assert!(manual_writes(&h).is_empty());
    h.restart_api().await;
    assert_eq!(h.batch_listing(&bid).await, cancelled.1);
    h.finish().await;
}

#[tokio::test]
async fn connection_http_late_account_busy_requeues_without_dispatch_time_and_can_cancel() {
    let mut h = Harness::new().await;
    let ids = h.connection_accounts().await;
    let request = h
        .connection_batch(&ids[..1], "fixture-http-late-busy-0001")
        .await;
    let accepted = h
        .request(Method::POST, "account-operations/batch", Some(request))
        .await;
    assert_eq!(accepted.0, 202);
    let bid = accepted.1["batch_id"].as_str().unwrap().to_owned();
    let jid = accepted.1["items"][0]["id"].as_str().unwrap().to_owned();
    let auth = h.upstream.gate("/api/v1/admin/groups/all");
    h.runtime.operations.tick().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), auth.entered.notified())
        .await
        .unwrap();
    let lease = h.runtime.actions.locks.account(ids[0]).await;
    h.upstream.ungate("/api/v1/admin/groups/all");
    auth.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = h.runtime.operations.get(&jid, 0).unwrap();
            if job["status"] == "queued" && job["reason"] == "等待账号空闲" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let queued = h.batch_listing(&bid).await;
    assert_eq!(queued["items"][0]["status"], "queued");
    assert!(queued["items"][0].get("started_at").is_some());
    assert_eq!(queued["items"][0]["started_at"], Value::Null);
    assert_eq!(
        h.runtime.operations.store.read().unwrap()["jobs"][&jid]["started_at"],
        Value::Null
    );
    let cancelled = h
        .request(
            Method::POST,
            &format!("account-operations/batches/{bid}/cancel"),
            None,
        )
        .await;
    assert_eq!(cancelled.0, 200);
    assert_eq!(cancelled.1["pending"], 0);
    assert_eq!(cancelled.1["items"][0]["status"], "cancelled");
    drop(lease);
    h.runtime.operations.tick().await.unwrap();
    h.restart_api().await;
    assert_eq!(h.batch_listing(&bid).await, cancelled.1);
    assert!(manual_writes(&h).is_empty());
    h.finish().await;
}

#[tokio::test]
async fn connection_http_runtime_operation_queue_keeps_eight_slots_across_action_types() {
    let mut h = Harness::new().await;
    let mut ids = Vec::new();
    let mut gates = Vec::new();
    let mut singles = Vec::new();
    for index in 0..9 {
        let id = h
            .insert_account("openai", "oauth", json!({}), json!({}), None, &[])
            .await;
        ids.push(id);
        if index < 7 {
            gates.push(h.upstream.gate(&format!("/api/v1/admin/accounts/{id}")));
            let request = single_operation(
                "priority",
                json!({"priority":9}),
                operation_version(&h.account(id).await, "priority"),
                &format!("fixture-global-runtime-priority-{index}"),
            );
            let accepted = h
                .request(
                    Method::POST,
                    &format!("accounts/{id}/operations"),
                    Some(request),
                )
                .await;
            assert_eq!(accepted.0, 202);
            singles.push(accepted.1);
        } else {
            h.upstream
                .connection_streams
                .lock()
                .unwrap()
                .insert(id, true);
            gates.push(
                h.upstream
                    .gate(&format!("/api/v1/admin/accounts/{id}/test")),
            );
        }
    }
    h.runtime.operations.tick().await.unwrap();
    for gate in gates.iter().take(7) {
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .unwrap();
    }
    let request = h
        .connection_batch(&ids[7..], "fixture-global-runtime-batch-0001")
        .await;
    let accepted = h
        .request(Method::POST, "account-operations/batch", Some(request))
        .await;
    assert_eq!(accepted.0, 202);
    h.runtime.operations.tick().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if manual_writes(&h).len() == 8 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for _ in 0..3 {
        h.runtime.operations.tick().await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        manual_writes(&h).len(),
        8,
        "non-test operations consume the same eight queue slots"
    );
    for gate in &gates {
        gate.release.notify_one();
    }
    let bid = accepted.1["batch_id"].as_str().unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            h.runtime.operations.tick().await.unwrap();
            let batch = h.batch_listing(bid).await;
            if batch["pending"] == 0 {
                break batch;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(finished["items"].as_array().unwrap().len(), 2);
    assert_eq!(manual_writes(&h).len(), 9);
    for single in singles {
        assert_eq!(
            h.wait_job(single["id"].as_str().unwrap(), &["completed"])
                .await["status"],
            "completed"
        );
    }
    h.finish().await;
}
