use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, post},
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use futures_util::FutureExt;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex as StdMutex},
    time::Duration as StdDuration,
};
use sub2ops_server::{
    account_locks::AccountLocks, account_templates, actions::Actions, capacity_alerts,
    capacity_alerts::CapacityAlerts, compat, db::Database, error_evidence, model_target,
    oauth_state::OAuthState, queries::Queries, quota, recovery_rules, runtime::Runtime,
    settings::Settings, slow_ttft, upstream::Upstream, usage,
};

const FIXTURES: &str = include_str!("../../tests/contracts/projection/fixtures.json");
static POSTGRES_PROJECTION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
struct TemplateUpstreamState {
    pool: sqlx::PgPool,
    calls: Arc<StdMutex<Vec<(String, String, Value)>>>,
}

#[derive(Clone, Debug)]
struct ProjectionHttpCall {
    method: String,
    path_and_query: String,
    key: String,
}

#[derive(Clone)]
struct ProjectionHttpState {
    calls: Arc<StdMutex<Vec<ProjectionHttpCall>>>,
    responses: Arc<StdMutex<BTreeMap<String, (u16, Value)>>>,
    block_path: Arc<StdMutex<Option<String>>>,
    entered: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
    pool: Option<sqlx::PgPool>,
}

impl ProjectionHttpState {
    fn new(pool: Option<sqlx::PgPool>) -> Self {
        Self {
            calls: Arc::new(StdMutex::new(Vec::new())),
            responses: Arc::new(StdMutex::new(BTreeMap::new())),
            block_path: Arc::new(StdMutex::new(None)),
            entered: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
            pool,
        }
    }

    fn set_response(&self, path: &str, status: u16, body: Value) {
        self.responses
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(path.into(), (status, body));
    }

    fn clear_response(&self, path: &str) {
        self.responses
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(path);
    }

    fn recorded_calls(&self) -> Vec<ProjectionHttpCall> {
        self.calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

fn projection_quota_data() -> Value {
    let now = Utc::now();
    json!({
        "five_hour": {
            "utilization": 10,
            "resets_at": compat::datetime(now + Duration::hours(5)),
            "window_minutes": 300
        },
        "seven_day": {
            "utilization": 20,
            "resets_at": compat::datetime(now + Duration::days(7)),
            "window_minutes": 10080
        },
        "fetched_at": compat::datetime(now)
    })
}

async fn projection_http_mock(
    State(state): State<ProjectionHttpState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str().to_owned())
        .unwrap_or_else(|| path.clone());
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let bearer = authorization
        .strip_prefix("Bearer ")
        .or_else(|| authorization.strip_prefix("bearer "))
        .unwrap_or("");
    let key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or(bearer)
        .to_owned();
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    state
        .calls
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(ProjectionHttpCall {
            method: method.to_string(),
            path_and_query,
            key: key.clone(),
        });

    let should_block = state
        .block_path
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_deref()
        == Some(path.as_str());
    if should_block {
        state.entered.add_permits(1);
        if let Ok(permit) = state.release.clone().acquire_owned().await {
            drop(permit);
        }
    }

    let override_response = state
        .responses
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&path)
        .cloned();
    let (status, response) = if let Some(response) = override_response {
        response
    } else if path == "/api/v1/admin/groups/all" {
        if key.is_empty() || key == "invalid-fixture-key" || key == "bad-fixture-key" {
            (401, json!({"code":401,"message":"fixture auth rejected"}))
        } else {
            (200, json!({"code":0,"data":[]}))
        }
    } else if path.ends_with("/quota/refresh") {
        let mut data = projection_quota_data();
        let id = path
            .split('/')
            .nth_back(2)
            .and_then(|value| value.parse::<i64>().ok());
        let observed = Utc::now();
        // Mirror Sub2API: the stored card snapshot has no timestamp and the
        // response reports the collection time as Unix seconds.
        let credit_snapshot = json!({
            "available_count":1,
            "credits":[{"expires_at":"2099-12-31T00:00:00+00:00"}]
        });
        if let (Some(pool), Some(id)) = (state.pool.as_ref(), id) {
            let _ = sqlx::query(
                "UPDATE accounts SET extra=jsonb_set(extra,'{codex_reset_credit_snapshot}',$2,true) WHERE id=$1",
            )
            .bind(id)
            .bind(sqlx::types::Json(credit_snapshot.clone()))
            .execute(pool)
            .await;
        }
        data["fetched_at"] = observed.timestamp().into();
        data["rate_limit_reset_credits"] = credit_snapshot;
        data["cache_persisted"] = true.into();
        data["credits_cache_persisted"] = true.into();
        (200, json!({"code":0,"data":data}))
    } else if path.ends_with("/reset-quota") {
        (
            200,
            json!({"code":0,"data":{"code":"success","windows_reset":1,"cache_refreshed":true,"cache_persisted":true}}),
        )
    } else if path.ends_with("/usage") && method == axum::http::Method::GET {
        let mut data = projection_quota_data();
        data["credentials"] = json!({"api_key":"secret-marker"});
        data["points"] = 99.into();
        data["referral"] = "private-invite".into();
        (200, json!({"code":0,"data":data}))
    } else if path.ends_with("/quota") && path.contains("/grok/accounts/") {
        (
            200,
            json!({"code":0,"data":{"requests":{"limit":10,"remaining":1},"tokens":{"limit":100,"remaining":20}}}),
        )
    } else if method == axum::http::Method::PUT
        && path.starts_with("/api/v1/admin/groups/")
        && path != "/api/v1/admin/groups/all"
    {
        let result = async {
            let id = path.rsplit('/').next()?.parse::<i64>().ok()?;
            let pool = state.pool.as_ref()?;
            sqlx::query("UPDATE groups SET model_allowlist=$2,updated_at=now() WHERE id=$1")
                .bind(id)
                .bind(sqlx::types::Json(body["model_allowlist"].clone()))
                .execute(pool)
                .await
                .ok()
        }
        .await;
        if result.is_some_and(|result| result.rows_affected() == 1) {
            (200, json!({"code":0,"data":{"updated":1}}))
        } else {
            (
                500,
                json!({"code":500,"message":"fixture group update failed"}),
            )
        }
    } else {
        (200, json!({"code":0,"data":{}}))
    };
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(response)).into_response()
}

async fn start_projection_http_mock(
    pool: Option<sqlx::PgPool>,
) -> (String, ProjectionHttpState, tokio::task::JoinHandle<()>) {
    let state = ProjectionHttpState::new(pool);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback-only projection mock");
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/{*path}", any(projection_http_mock))
        .route("/", any(projection_http_mock))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve loopback-only projection mock");
    });
    (format!("http://{address}"), state, task)
}

async fn start_projection_api(
    runtime: Arc<Runtime>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a local Rust API listener");
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, sub2ops_server::api::router(runtime))
            .await
            .expect("serve the actual Rust API router");
    });
    (address, task)
}

async fn insert_runtime_account(
    pool: &sqlx::PgPool,
    id: i64,
    platform: &str,
    kind: &str,
    credentials: Value,
    extra: Value,
    deleted: bool,
) {
    sqlx::query(
        "INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,deleted_at) \
         VALUES ($1,$2,$3,$4,'active',true,$5,$6,CASE WHEN $7 THEN now() ELSE NULL END)",
    )
    .bind(id)
    .bind(format!("fixture-runtime-account-{id}"))
    .bind(platform)
    .bind(kind)
    .bind(sqlx::types::Json(credentials))
    .bind(sqlx::types::Json(extra))
    .bind(deleted)
    .execute(pool)
    .await
    .expect("insert a synthetic account for a Runtime projection contract");
}

fn projection_credit_extra() -> Value {
    json!({
        "codex_reset_credit_snapshot": {
            "available_count":1,
            "credits":[{"expires_at":"2099-12-31T00:00:00+00:00"}],
            "fetched_at":compat::datetime(Utc::now())
        }
    })
}

async fn usage_action_payload(
    runtime: &Runtime,
    id: i64,
    action: &str,
    confirmed: Option<bool>,
) -> Value {
    let row = runtime
        .actions
        .account(id, None)
        .await
        .expect("read the synthetic account before building its guarded action payload");
    let mut payload = json!({
        "action":action,
        "expected_version":compat::account_version(&row)
    });
    if let Some(confirmed) = confirmed {
        payload["confirmed"] = confirmed.into();
    }
    payload
}

fn read_oauth_state(path: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(path).expect("read the temporary OAuth state produced by the Runtime"),
    )
    .expect("temporary OAuth state is JSON")
}

struct ProjectionRuntimeFixture {
    _directory: tempfile::TempDir,
    seed: Database,
    runtime: Arc<Runtime>,
    http: ProjectionHttpState,
    base_url: String,
    http_task: tokio::task::JoinHandle<()>,
}

impl ProjectionRuntimeFixture {
    async fn close(self) {
        self.runtime.close().await;
        self.http_task.abort();
        self.seed.close().await;
    }
}

async fn start_projection_runtime_fixture(application_name: &str) -> ProjectionRuntimeFixture {
    start_projection_runtime_fixture_with_oauth(application_name, json!({})).await
}

async fn start_projection_runtime_fixture_with_oauth(
    application_name: &str,
    oauth_overrides: Value,
) -> ProjectionRuntimeFixture {
    let seed = open_test_database(&format!("{application_name}-seed")).await;
    let (base_url, http, http_task) = start_projection_http_mock(Some(seed.pool.clone())).await;
    let directory = tempfile::tempdir().expect("create an owned Runtime contract directory");
    let mut settings = test_settings(
        directory.path(),
        database_url_for(application_name),
        &base_url,
    )
    .as_ref()
    .clone();
    settings
        .oauth
        .as_object_mut()
        .expect("default fixture OAuth configuration is an object")
        .extend(
            oauth_overrides
                .as_object()
                .expect("OAuth fixture overrides are an object")
                .clone(),
        );
    let runtime = Runtime::new(settings)
        .await
        .expect("create Runtime against temporary PostgreSQL and the loopback mock");
    ProjectionRuntimeFixture {
        _directory: directory,
        seed,
        runtime,
        http,
        base_url,
        http_task,
    }
}

async fn projection_json_response(request: reqwest::RequestBuilder) -> (StatusCode, Value) {
    let response = request
        .send()
        .await
        .expect("send a request to the loopback Rust API");
    let status = response.status();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

fn fixtures() -> Value {
    serde_json::from_str(FIXTURES).expect("pinned projection fixture JSON")
}

fn json_numbers_equal(actual: &Value, expected: &Value) -> bool {
    if actual == expected {
        return true;
    }
    match (actual, expected) {
        (Value::Number(left), Value::Number(right)) => left
            .as_f64()
            .zip(right.as_f64())
            .is_some_and(|(left, right)| {
                (left - right).abs() <= 1e-12 * left.abs().max(right.abs()).max(1.0)
            }),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| json_numbers_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|expected| json_numbers_equal(value, expected))
                })
        }
        _ => false,
    }
}

fn assert_json_numbers_equal(actual: &Value, expected: &Value, context: &str) {
    assert!(
        json_numbers_equal(actual, expected),
        "{context}: expected {expected}, got {actual}"
    );
}

fn at(value: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value.as_str().expect("fixture timestamp must be a string"))
        .expect("fixture timestamp must include an offset")
        .with_timezone(&Utc)
}

fn stats_from_entries(entries: &Value) -> usage::Stats {
    entries
        .as_array()
        .expect("stats entries must be an array")
        .iter()
        .map(|entry| {
            (
                (
                    entry["account_id"].as_i64().expect("stats account ID"),
                    entry["key"].as_str().expect("stats key").to_owned(),
                ),
                entry["stats"].clone(),
            )
        })
        .collect()
}

fn database_url_for(application_name: &str) -> String {
    let raw = std::env::var("DATABASE_URL")
        .expect("projection test runner must provide temporary PostgreSQL");
    let mut url = reqwest::Url::parse(&raw).expect("temporary PostgreSQL URL must parse");
    url.query_pairs_mut()
        .append_pair("application_name", application_name);
    url.to_string()
}

fn test_settings(root: &Path, database_url: String, base_url: &str) -> Arc<Settings> {
    std::fs::create_dir_all(root).expect("create owned projection settings directory");
    let connection_path = root.join("sub2api-config.json");
    std::fs::write(
        &connection_path,
        serde_json::to_vec(&json!({
            "base_url": base_url,
            "verify_base_url": base_url,
        }))
        .expect("serialize local fixture upstream URLs"),
    )
    .expect("write loopback-only fixture connection");
    Arc::new(Settings {
        database_url,
        bind: "127.0.0.1:0".into(),
        base_path: String::new(),
        audit_path: root.join("audit.jsonl"),
        usage_path: root.join("oauth-state.json"),
        oauth_path: root.join("oauth-config.json"),
        bark_path: root.join("bark-config.json"),
        connection_path,
        keepalive_path: root.join("keepalive-state.json"),
        oauth: json!({
            "oauth_auto_reset_credit_enabled": false,
            "oauth_daily_test_enabled": false,
            "oauth_daily_test_time": "23:59",
            "oauth_recovery_connection_account_ids": [],
            "oauth_recovery_model_account_ids": [],
            "oauth_recovery_monitor_enabled": false,
            "oauth_recovery_test_concurrency": 1,
            "oauth_recovery_test_model_id": "fixture-model",
            "oauth_usage_refresh_concurrency": 1,
        }),
        bark: json!({"enabled":false,"device_key":"","server_url":""}),
        bark_valid: true,
        base_url: base_url.into(),
        verify_url: base_url.into(),
    })
}

async fn open_test_database(application_name: &str) -> Database {
    Database::open(&database_url_for(application_name))
        .await
        .expect("connect only to the test runner's temporary PostgreSQL")
}

async fn insert_account(
    pool: &sqlx::PgPool,
    id: i64,
    platform: &str,
    kind: &str,
    extra: Value,
    deleted: bool,
) {
    sqlx::query(
        "INSERT INTO accounts (id,name,platform,type,status,credentials,extra,deleted_at) \
         VALUES ($1,$2,$3,$4,'active','{}'::jsonb,$5,CASE WHEN $6 THEN now() ELSE NULL END)",
    )
    .bind(id)
    .bind(format!("fixture-account-{id}"))
    .bind(platform)
    .bind(kind)
    .bind(sqlx::types::Json(extra))
    .bind(deleted)
    .execute(pool)
    .await
    .expect("insert synthetic projection account row");
}

async fn projection_template_bulk_update(
    State(state): State<TemplateUpstreamState>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    state
        .calls
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push((uri.path().to_owned(), key, body.clone()));
    let Some(id) = body["account_ids"][0].as_i64() else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    let mapping = body["credentials"]["model_mapping"].clone();
    match sqlx::query(
        "UPDATE accounts SET credentials=jsonb_set(credentials,'{model_mapping}',$2,true) WHERE id=$1",
    )
    .bind(id)
    .bind(sqlx::types::Json(mapping))
    .execute(&state.pool)
    .await
    {
        Ok(result) if result.rows_affected() == 1 => {
            Json(json!({"code":0,"data":{"updated":result.rows_affected()}})).into_response()
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn insert_capacity_error(
    pool: &sqlx::PgPool,
    id: i64,
    account_id: i64,
    message: &str,
    created_at: DateTime<Utc>,
) {
    sqlx::query(
        "INSERT INTO ops_error_logs (id,account_id,created_at,platform,error_owner,error_phase,error_source,stream,status_code,upstream_status_code,upstream_error_message) \
         VALUES ($1,$2,$3,'openai','provider','upstream','upstream',true,503,503,$4)",
    )
    .bind(id)
    .bind(account_id)
    .bind(created_at)
    .bind(message)
    .execute(pool)
    .await
    .expect("insert synthetic provider capacity error");
}

#[tokio::test]
async fn apikey_capacity_watermark_clues_mark_suppression_and_restart_match_pinned_python() {
    let _database_serial = POSTGRES_PROJECTION_LOCK.lock().await;
    const ACCOUNT_ID: i64 = 967_321;
    const FIRST_ERROR_ID: i64 = 967_321_001;
    let db = open_test_database("projection-apikey-capacity").await;
    insert_account(&db.pool, ACCOUNT_ID, "openai", "apikey", json!({}), false).await;
    let directory = tempfile::tempdir().expect("create owned capacity alert state directory");
    let settings = test_settings(
        directory.path(),
        database_url_for("projection-apikey-capacity"),
        "http://127.0.0.1:9",
    );
    let alerts = CapacityAlerts::new(db.clone(), settings.as_ref());
    alerts.initialize(Utc::now()).unwrap();
    alerts
        .poll()
        .await
        .expect("initialize capacity and API key watermarks without replay");
    let initial = alerts
        .store
        .snapshot()
        .expect("read initialized watermarks");
    let old_watermark = at(&initial["apikey_since"]);
    alerts
        .store
        .transaction(|state| {
            state.as_object_mut().unwrap().remove("apikey_since");
            Ok(())
        })
        .expect("simulate an OAuth-only persisted state upgraded to include API keys");

    let stale_created_at = old_watermark + Duration::milliseconds(1);
    insert_capacity_error(
        &db.pool,
        FIRST_ERROR_ID,
        ACCOUNT_ID,
        fixtures()["key_support"]["capacity_messages"][0]
            .as_str()
            .unwrap(),
        stale_created_at,
    )
    .await;
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    alerts.initialize(Utc::now()).unwrap();
    alerts
        .poll()
        .await
        .expect("advance the API key watermark without classifying older evidence");
    let after_stale = alerts
        .store
        .snapshot()
        .expect("read advanced API key watermark");
    assert!(at(&after_stale["apikey_since"]) > stale_created_at);
    assert!(after_stale["notifications"]
        .get(FIRST_ERROR_ID.to_string())
        .is_none());
    assert!(after_stale["detection_events"]
        .get(format!("error:{FIRST_ERROR_ID}"))
        .is_none());

    let messages = fixtures()["key_support"]["capacity_messages"]
        .as_array()
        .expect("pinned Python capacity clue messages")
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 3);
    let after_watermark = at(&after_stale["apikey_since"]);
    let capacity_ids = (FIRST_ERROR_ID + 1..=FIRST_ERROR_ID + 3).collect::<Vec<_>>();
    for (index, (id, message)) in capacity_ids.iter().zip(&messages).enumerate() {
        insert_capacity_error(
            &db.pool,
            *id,
            ACCOUNT_ID,
            message,
            after_watermark + Duration::milliseconds(index as i64 + 1),
        )
        .await;
    }
    let local_throttle = fixtures()["slow"]["throttle_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["expected"] == true && case["row"]["message"].is_string())
        .expect("pinned local throttle fixture")["row"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    let local_id = FIRST_ERROR_ID + 4;
    insert_capacity_error(
        &db.pool,
        local_id,
        ACCOUNT_ID,
        &local_throttle,
        after_watermark + Duration::milliseconds(4),
    )
    .await;
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    alerts
        .poll()
        .await
        .expect("record new provider capacity clues without a notification transport");
    let clues = alerts.store.snapshot().expect("read first clue state");
    let expected_events = capacity_ids
        .iter()
        .map(|id| format!("error:{id}"))
        .collect::<BTreeSet<_>>();
    let observed_events = clues["detection_events"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(observed_events, expected_events);
    for id in &capacity_ids {
        assert_eq!(clues["notifications"][id.to_string()]["status"], "clue");
    }
    assert!(clues["notifications"].get(local_id.to_string()).is_none());
    assert_eq!(clues["pending"], json!({}));
    assert_eq!(clues["slow_pending"], json!({}));

    let key = ACCOUNT_ID.to_string();
    let mark_version = capacity_alerts::mark_view(ACCOUNT_ID, &clues["marks"][&key])["version"]
        .as_str()
        .unwrap()
        .to_owned();
    alerts
        .store
        .set_mark(ACCOUNT_ID, true, &mark_version, Utc::now(), None)
        .expect("persist the account-scoped degradation mark");
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    let marked_error_id = FIRST_ERROR_ID + 5;
    insert_capacity_error(
        &db.pool,
        marked_error_id,
        ACCOUNT_ID,
        &messages[0],
        Utc::now(),
    )
    .await;
    alerts
        .poll()
        .await
        .expect("suppress new clues while the account is marked");
    let restarted = CapacityAlerts::new(db.clone(), settings.as_ref());
    restarted.initialize(Utc::now()).unwrap();
    restarted
        .poll()
        .await
        .expect("resume the persisted cursor and marks after restart");
    let resumed = restarted
        .store
        .snapshot()
        .expect("read resumed alert state");
    assert_eq!(resumed["marks"][&key]["marked"], true);
    assert_eq!(
        resumed["notifications"][marked_error_id.to_string()]["status"],
        "suppressed"
    );
    assert_eq!(
        resumed["notifications"][marked_error_id.to_string()]["reason"],
        "degradation_mark"
    );
    assert!(resumed["detection_events"]
        .get(format!("error:{marked_error_id}"))
        .is_none());
    assert_eq!(resumed["pending"], json!({}));

    db.close().await;
}

#[tokio::test]
async fn capacity_slow_ttft_poll_persists_one_clue_without_bark() {
    let _database_serial = POSTGRES_PROJECTION_LOCK.lock().await;
    const ACCOUNT_ID: i64 = 967_322;
    const FIRST_USAGE_ID: i64 = 967_322_000;
    let db = open_test_database("projection-capacity-slow").await;
    insert_account(&db.pool, ACCOUNT_ID, "openai", "oauth", json!({}), false).await;
    let directory = tempfile::tempdir().expect("create owned slow clue state directory");
    let settings = test_settings(
        directory.path(),
        database_url_for("projection-capacity-slow"),
        "http://127.0.0.1:9",
    );
    let alerts = CapacityAlerts::new(db.clone(), settings.as_ref());
    alerts.initialize(Utc::now()).unwrap();
    alerts
        .poll()
        .await
        .expect("initialize the error cursor before inserting fixture samples");
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    let newest_sample_at = Utc::now() - Duration::milliseconds(1);
    for index in 0..10_i64 {
        let first_token_ms = if index < 8 { 11_001_i64 } else { 9_000_i64 };
        let duration_ms = if index < 8 { 12_001_i64 } else { 12_000_i64 };
        sqlx::query(
            "INSERT INTO usage_logs (id,account_id,created_at,model,upstream_model,stream,first_token_ms,duration_ms,output_tokens,inbound_endpoint) \
             VALUES ($1,$2,$3,'gpt-6-luna','gpt-6-luna',true,$4,$5,64,'/v1/responses')",
        )
        .bind(FIRST_USAGE_ID + index)
        .bind(ACCOUNT_ID)
        .bind(newest_sample_at - Duration::minutes(index))
        .bind(first_token_ms)
        .bind(duration_ms)
        .execute(&db.pool)
        .await
        .expect("insert one synthetic slow/fast sample from the pinned warning fixture");
    }

    alerts
        .poll()
        .await
        .expect("classify the new slow TTFT sample window");
    let state = alerts
        .store
        .snapshot()
        .expect("read persisted slow clue state");
    let account_key = ACCOUNT_ID.to_string();
    let slow_state = &state["slow_ttft"][&account_key];
    assert_eq!(slow_state["active"], true);
    assert_eq!(slow_state["alerted"], true);
    assert_eq!(slow_state["last_sample_id"], FIRST_USAGE_ID);
    let clue_key = format!("slow:{ACCOUNT_ID}:{FIRST_USAGE_ID}");
    assert_eq!(state["notifications"][&clue_key]["status"], "clue");
    assert!(state["detection_events"].get(&clue_key).is_some());
    assert_eq!(state["slow_pending"], json!({}));
    assert!(fixtures()["slow"]["threshold_expected"]["active"] == true);

    alerts
        .poll()
        .await
        .expect("repeat polling must not create a duplicate slow clue");
    let repeated = alerts
        .store
        .snapshot()
        .expect("read deduplicated slow clue state");
    assert_eq!(repeated["detection_events"].as_object().unwrap().len(), 1);
    assert_eq!(repeated["notifications"].as_object().unwrap().len(), 1);
    assert_eq!(repeated["pending"], json!({}));
    assert_eq!(repeated["slow_pending"], json!({}));

    // The suite shares a database. An OAuth account without reset timestamps
    // has rolling stats windows, so leaving it here can invalidate another
    // test's shared-stats cache when its two snapshots cross a minute boundary.
    sqlx::query("DELETE FROM usage_logs WHERE account_id=$1")
        .bind(ACCOUNT_ID)
        .execute(&db.pool)
        .await
        .expect("remove this test's synthetic slow samples");
    sqlx::query("DELETE FROM accounts WHERE id=$1")
        .bind(ACCOUNT_ID)
        .execute(&db.pool)
        .await
        .expect("remove this test's synthetic OAuth account");
    db.close().await;
}

#[tokio::test]
async fn account_quality_detail_route_is_removed() {
    let _database_serial = POSTGRES_PROJECTION_LOCK.lock().await;
    let directory = tempfile::tempdir().expect("create owned API state directory");
    let settings = test_settings(
        directory.path(),
        database_url_for("projection-account-quality-removed"),
        "http://127.0.0.1:9",
    );
    let runtime = Runtime::new(settings.as_ref().clone())
        .await
        .expect("create runtime against temporary PostgreSQL");

    let api_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local Rust API listener");
    let api_address = api_listener.local_addr().unwrap();
    let runtime_for_server = runtime.clone();
    let api_task = tokio::spawn(async move {
        axum::serve(
            api_listener,
            sub2ops_server::api::router(runtime_for_server),
        )
        .await
        .expect("serve actual Rust API router");
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build loopback-only API client");
    let response = client
        .get(format!(
            "http://{api_address}/api/desktop/v1/accounts/967101/quality"
        ))
        .send()
        .await
        .expect("request the retired account quality route");
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    runtime.close().await;
    api_task.abort();
    let _ = api_task.await;
}

#[test]
fn quota_windows_latest_result_and_recovery_match_pinned_python() {
    let document = fixtures();
    let input = &document["quota"];
    for case in input["windows"].as_array().unwrap() {
        let actual = Value::Array(quota::usage_windows(
            &case["row"],
            at(&case["at"]),
            &case["result"],
        ));
        assert_json_numbers_equal(
            &actual,
            &case["expected"],
            &format!("quota windows {}", case["id"]),
        );
    }

    for case in input["latest_results"].as_array().unwrap() {
        if !case["error_result"].is_null() {
            assert_json_numbers_equal(
                &quota::latest_result(&case["row"], &case["error_result"], at(&input["at"])),
                &case["result"],
                "newer quota error wins over passive snapshot",
            );
        }
    }

    for case in input["candidate_cases"].as_array().unwrap() {
        let actual = recovery_rules::candidates(
            case["rows"].as_array().unwrap(),
            &case["results"],
            &case["scheduler"],
            at(&case["at"]),
            case["enabled"].as_bool().unwrap(),
        )
        .expect("synthetic recovery candidate projection");
        let projection = actual
            .into_iter()
            .map(|item| json!({"account_id":item["account_id"],"reason":item["reason"]}))
            .collect::<Vec<_>>();
        assert_json_numbers_equal(
            &Value::Array(projection),
            &case["expected"],
            &format!("recovery candidate {}", case["id"]),
        );
    }
}

#[test]
fn desktop_usage_projections_stats_and_credit_privacy_match_pinned_python() {
    let document = fixtures();
    let input = &document["usage"];
    let now = at(&input["at"]);
    for case in input["cases"].as_array().unwrap() {
        let row = &case["row"];
        let account_id = row["id"].as_i64().unwrap();
        let actual = usage::project(row, now, &Value::Null);
        // The pinned legacy projection predates direct API Key billing reads.
        // Keep all existing windows and statistics identical while checking the
        // newly available action separately.
        let expected_usage = |value: &Value| {
            let mut expected = value.clone();
            if row["type"] == "apikey" {
                expected["native_read"] = true.into();
                let actions = expected["actions"].as_array_mut().unwrap();
                if !actions.iter().any(|a| a == "query_usage") {
                    actions.insert(0, json!("query_usage"));
                }
            }
            expected
        };
        assert_json_numbers_equal(
            &actual,
            &expected_usage(&case["expected"]),
            &format!("usage projection {}", case["id"]),
        );
        if let Some(expected) = case.get("stats_specs") {
            assert_json_numbers_equal(
                &Value::Array(usage::stats_specs(account_id, &actual)),
                expected,
                &format!("usage statistic windows {}", case["id"]),
            );
        }
        if let Some(expected) = case.get("reset_credits_expected") {
            assert_json_numbers_equal(
                &usage::reset_credits(&row["extra"], now),
                expected,
                &format!("reset-credit privacy and expiry {}", case["id"]),
            );
        }

        if let Some(entries) = case["stats_entries"].as_object() {
            for (variant, entries) in entries {
                let mut projected = actual.clone();
                usage::attach_stats(
                    account_id,
                    &mut projected,
                    &stats_from_entries(entries),
                    500_000,
                );
                assert_json_numbers_equal(
                    &projected,
                    &expected_usage(&case["attached_expected"][variant]),
                    &format!("attached usage statistics {}/{}", case["id"], variant),
                );
            }
        }

        if let Some(rows) = case["tier_override_rows"].as_object() {
            for (variant, override_row) in rows {
                let projected = usage::project(override_row, now, &Value::Null);
                assert_json_numbers_equal(
                    &projected,
                    &expected_usage(&case["tier_override_expected"][variant]),
                    &format!("credential tier override {}/{}", case["id"], variant),
                );
            }
        }
    }

    let stats = usage::stats_rows(input["stats_rows_input"].as_array().unwrap().clone());
    let actual = stats
        .into_iter()
        .map(|((account_id, key), value)| {
            let mut row = json!({"account_id":account_id,"key":key});
            if let Some(fields) = value.as_object() {
                for (field, value) in fields {
                    row[field] = value.clone();
                }
            }
            row
        })
        .collect::<Vec<_>>();
    assert_eq!(
        Value::Array(actual),
        input["stats_rows_expected"],
        "database usage statistics parsing"
    );
}

#[tokio::test]
async fn desktop_runtime_snapshot_reuses_shared_stats_without_upstream_io() {
    let _database_serial = POSTGRES_PROJECTION_LOCK.lock().await;
    const ACCOUNT_ID: i64 = 967_201;
    let seed = open_test_database("projection-snapshot-shared-stats-seed").await;
    insert_runtime_account(
        &seed.pool,
        ACCOUNT_ID,
        "openai",
        "apikey",
        json!({}),
        json!({"quota_daily_limit":100,"quota_daily_used":10}),
        false,
    )
    .await;
    sqlx::query(
        "INSERT INTO usage_logs (id,account_id,created_at,model,input_tokens,output_tokens,total_cost,actual_cost) \
         SELECT $1 + generated.n,$2,now(),'fixture-model',2,2,2.0,3.0 \
         FROM generate_series(1,3) AS generated(n)",
    )
    .bind(967_201_001_i64)
    .bind(ACCOUNT_ID)
    .execute(&seed.pool)
    .await
    .expect("insert one saved usage row for the read-only snapshot");

    sqlx::query("ALTER TABLE usage_logs RENAME TO projection_stats_usage_logs_base")
        .execute(&seed.pool)
        .await
        .expect("rename the synthetic usage table for query-specific cache instrumentation");
    sqlx::query("CREATE TABLE projection_stats_fail_gate (fail_stats boolean NOT NULL)")
        .execute(&seed.pool)
        .await
        .expect("create a read-only cache-miss gate for the stats query");
    sqlx::query("INSERT INTO projection_stats_fail_gate VALUES (false)")
        .execute(&seed.pool)
        .await
        .expect("initialize the stats cache-miss gate as open");
    sqlx::query(
        "CREATE FUNCTION projection_stats_guarded_tokens(value bigint) RETURNS bigint \
         LANGUAGE plpgsql VOLATILE AS $$ \
         BEGIN \
           IF position('jsonb_to_recordset' IN current_query()) > 0 \
              AND EXISTS (SELECT 1 FROM projection_stats_fail_gate WHERE fail_stats) THEN \
             RAISE EXCEPTION 'fixture stats cache miss'; \
           END IF; \
           RETURN value; \
         END \
         $$",
    )
    .execute(&seed.pool)
    .await
    .expect("install the read-only stats-query gate");
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT attname FROM pg_attribute WHERE attrelid=to_regclass('projection_stats_usage_logs_base') \
         AND attnum>0 AND NOT attisdropped ORDER BY attnum",
    )
    .fetch_all(&seed.pool)
    .await
    .expect("read the synthetic usage table's declared columns");
    let projection = columns
        .iter()
        .map(|column| {
            let quoted = format!("\"{}\"", column.replace('"', "\"\""));
            if column == "input_tokens" {
                format!("projection_stats_guarded_tokens(u.{quoted}) AS {quoted}")
            } else {
                format!("u.{quoted} AS {quoted}")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(&format!(
        "CREATE VIEW usage_logs AS SELECT {projection} FROM projection_stats_usage_logs_base u"
    ))
    .execute(&seed.pool)
    .await
    .expect("publish a compatible synthetic usage view with a stats-only gate");

    let (upstream_url, mock, upstream_task) =
        start_projection_http_mock(Some(seed.pool.clone())).await;
    let directory = tempfile::tempdir().expect("create a temporary Runtime state directory");
    let settings = test_settings(
        directory.path(),
        database_url_for("projection-snapshot-shared-stats"),
        &upstream_url,
    );
    let runtime = match Runtime::new(settings.as_ref().clone()).await {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = sqlx::query("DROP VIEW IF EXISTS usage_logs")
                .execute(&seed.pool)
                .await;
            let _ =
                sqlx::query("ALTER TABLE projection_stats_usage_logs_base RENAME TO usage_logs")
                    .execute(&seed.pool)
                    .await;
            let _ = sqlx::query("DROP FUNCTION IF EXISTS projection_stats_guarded_tokens(bigint)")
                .execute(&seed.pool)
                .await;
            let _ = sqlx::query("DROP TABLE IF EXISTS projection_stats_fail_gate")
                .execute(&seed.pool)
                .await;
            upstream_task.abort();
            let _ = upstream_task.await;
            panic!("construct Runtime against the synthetic view: {error}");
        }
    };

    let verification = std::panic::AssertUnwindSafe(async {
        let first = runtime
            .snapshot()
            .await
            .expect("read the first Runtime snapshot from PostgreSQL");
        let first_usage = first["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["id"] == ACCOUNT_ID)
            .expect("snapshot contains the synthetic API-key account")["usage"]
            .clone();
        assert_eq!(first_usage["today"]["requests"], 3);
        assert_eq!(first_usage["today"]["tokens"], 12);
        let projection_row = runtime.actions.queries.account(ACCOUNT_ID).await.unwrap();
        let mut single = runtime
            .account_snapshot_from_row(&projection_row, None)
            .await
            .unwrap();
        let full = first["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["id"] == ACCOUNT_ID)
            .unwrap();
        // Configured quotas are observed at each projection's own wall-clock time.
        for branch in ["usage_windows", "usage"] {
            let windows = if branch == "usage" {
                &mut single[branch]["windows"]
            } else {
                &mut single[branch]
            };
            for (index, window) in windows.as_array_mut().unwrap().iter_mut().enumerate() {
                let previous = &full["usage_windows"][index]["observed_at"];
                assert!(
                    compat::parse_datetime(&window["observed_at"])
                        >= compat::parse_datetime(previous)
                );
                window["observed_at"] = previous.clone();
            }
        }
        assert_json_numbers_equal(
            &single,
            full,
            "single account preserves every overview field",
        );
        assert!(single.get("credentials").is_none());
        assert!(single.get("account_identity").is_none());

        tokio::time::sleep(StdDuration::from_millis(1600)).await;
        sqlx::query("UPDATE projection_stats_fail_gate SET fail_stats=true")
            .execute(&seed.pool)
            .await
            .expect("make a later stats SQL execution fail while leaving other reads available");
        let second = runtime
            .snapshot()
            .await
            .expect("refresh the whole snapshot while the 15-second stats cache remains valid");
        let second_usage = second["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["id"] == ACCOUNT_ID)
            .expect("refreshed snapshot retains the synthetic API-key account")["usage"]
            .clone();
        assert_json_numbers_equal(
            &second_usage["today"],
            &first_usage["today"],
            "the next snapshot retains the shared stats entry",
        );
        assert_eq!(second_usage["stats_status"], "ok");
        runtime.invalidate();
        let failed = runtime
            .account_snapshot_from_row(&projection_row, None)
            .await
            .unwrap();
        assert_eq!(failed["usage"]["stats_status"], "error");
        assert!(failed["usage"]["today"].is_null());
        assert_eq!(
            runtime
                .account_usage(ACCOUNT_ID, "fixture-admin", false)
                .await
                .unwrap()["stats_status"],
            "error"
        );
        sqlx::query("UPDATE projection_stats_fail_gate SET fail_stats=false")
            .execute(&seed.pool)
            .await
            .unwrap();
        runtime.invalidate();
        let restored = runtime
            .account_snapshot_from_row(&projection_row, None)
            .await
            .unwrap();
        assert_eq!(restored["usage"]["stats_status"], "ok");
        assert_eq!(restored["usage"]["today"]["requests"], 3);
        assert!(mock.recorded_calls().is_empty());
    })
    .catch_unwind()
    .await;

    runtime.close().await;
    upstream_task.abort();
    let _ = upstream_task.await;
    sqlx::query("DROP VIEW IF EXISTS usage_logs")
        .execute(&seed.pool)
        .await
        .expect("drop the instrumentation view after Runtime shutdown");
    sqlx::query("ALTER TABLE projection_stats_usage_logs_base RENAME TO usage_logs")
        .execute(&seed.pool)
        .await
        .expect("restore the synthetic usage relation after Runtime shutdown");
    sqlx::query("DROP FUNCTION IF EXISTS projection_stats_guarded_tokens(bigint)")
        .execute(&seed.pool)
        .await
        .expect("remove the owned stats instrumentation function");
    sqlx::query("DROP TABLE IF EXISTS projection_stats_fail_gate")
        .execute(&seed.pool)
        .await
        .expect("remove the owned stats instrumentation gate");
    seed.close().await;
    if let Err(panic) = verification {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn desktop_usage_action_preconditions_reject_without_upstream_requests() {
    let _database_serial = POSTGRES_PROJECTION_LOCK.lock().await;
    const OAUTH_ID: i64 = 967_401;
    const APIKEY_ID: i64 = 967_402;
    const DELETED_ID: i64 = 967_403;
    const GROK_ID: i64 = 967_404;
    let fixture = start_projection_runtime_fixture("projection-usage-preconditions").await;
    insert_runtime_account(
        &fixture.seed.pool,
        OAUTH_ID,
        "openai",
        "oauth",
        json!({"access_token":"fixture-openai-access-token"}),
        json!({}),
        false,
    )
    .await;
    insert_runtime_account(
        &fixture.seed.pool,
        APIKEY_ID,
        "openai",
        "apikey",
        json!({"api_key":"fixture-openai-api-key"}),
        json!({}),
        false,
    )
    .await;
    insert_runtime_account(
        &fixture.seed.pool,
        DELETED_ID,
        "openai",
        "oauth",
        json!({"access_token":"fixture-deleted-access-token"}),
        json!({}),
        true,
    )
    .await;
    insert_runtime_account(
        &fixture.seed.pool,
        GROK_ID,
        "grok",
        "oauth",
        json!({"access_token":"fixture-grok-access-token"}),
        json!({"subscription_tier":"supergrok"}),
        false,
    )
    .await;

    let current = usage_action_payload(&fixture.runtime, OAUTH_ID, "query_usage", None).await;
    let mut stale = current.clone();
    stale["expected_version"] = "0".repeat(64).into();
    let error = fixture
        .runtime
        .usage_action(OAUTH_ID, &stale, "fixture-action-key", None)
        .await
        .expect_err("a stale account DTO version is rejected before HTTP");
    assert_eq!(error.status.as_u16(), 409);

    for action in ["reset_quota", "probe_quota"] {
        let payload = usage_action_payload(&fixture.runtime, OAUTH_ID, action, Some(false)).await;
        let error = fixture
            .runtime
            .usage_action(OAUTH_ID, &payload, "fixture-action-key", None)
            .await
            .expect_err("a reset or probe requires explicit confirmation");
        assert!(error.status.is_client_error());
    }

    let no_credit =
        usage_action_payload(&fixture.runtime, OAUTH_ID, "reset_quota", Some(true)).await;
    let error = fixture
        .runtime
        .usage_action(OAUTH_ID, &no_credit, "fixture-action-key", None)
        .await
        .expect_err("reset requires a persisted, unexpired credit observation");
    assert!(error.status.is_client_error());

    let api_key_query =
        usage_action_payload(&fixture.runtime, APIKEY_ID, "query_usage", None).await;
    let result = fixture
        .runtime
        .usage_action(APIKEY_ID, &api_key_query, "fixture-action-key", None)
        .await
        .expect("API Key billing is available independently of OAuth actions");
    assert_eq!(result["account"]["usage"]["key_quota"]["state"], "error");
    assert_eq!(
        result["account"]["usage"]["key_quota"]["error"],
        "供应商地址无效"
    );

    let deleted_query = json!({
        "action":"query_usage",
        "expected_version":"0".repeat(64)
    });
    let error = fixture
        .runtime
        .usage_action(DELETED_ID, &deleted_query, "fixture-action-key", None)
        .await
        .expect_err("deleted accounts are not eligible for usage actions");
    assert_eq!(error.status.as_u16(), 404);

    let grok_unconfirmed =
        usage_action_payload(&fixture.runtime, GROK_ID, "probe_quota", Some(false)).await;
    let error = fixture
        .runtime
        .usage_action(GROK_ID, &grok_unconfirmed, "fixture-action-key", None)
        .await
        .expect_err("Grok quota probing also requires confirmation");
    assert_eq!(error.status.as_u16(), 409);
    assert!(fixture.http.recorded_calls().is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn desktop_usage_actions_match_paths_methods_privacy_and_reset_warning() {
    let _database_serial = POSTGRES_PROJECTION_LOCK.lock().await;
    const OPENAI_ID: i64 = 967_411;
    const GROK_ID: i64 = 967_412;
    const WARNING_ID: i64 = 967_413;
    let fixture = start_projection_runtime_fixture("projection-usage-paths").await;
    insert_runtime_account(
        &fixture.seed.pool,
        OPENAI_ID,
        "openai",
        "oauth",
        json!({"access_token":"fixture-openai-access-token"}),
        projection_credit_extra(),
        false,
    )
    .await;
    insert_runtime_account(
        &fixture.seed.pool,
        GROK_ID,
        "grok",
        "oauth",
        json!({"access_token":"fixture-grok-access-token"}),
        json!({"subscription_tier":"supergrok"}),
        false,
    )
    .await;
    insert_runtime_account(
        &fixture.seed.pool,
        WARNING_ID,
        "openai",
        "oauth",
        json!({"access_token":"fixture-warning-access-token"}),
        projection_credit_extra(),
        false,
    )
    .await;
    fixture.http.set_response(
        &format!("/api/v1/admin/grok/accounts/{GROK_ID}/quota"),
        200,
        json!({
            "code":0,
            "data":{
                "credentials":{"api_key":"secret-marker"},
                "points":99,
                "referral":"private-invite"
            }
        }),
    );

    let mut serialized_results = Vec::new();
    for action in ["query_usage", "query_reset_credits", "reset_quota"] {
        let payload = usage_action_payload(&fixture.runtime, OPENAI_ID, action, Some(true)).await;
        let result = fixture
            .runtime
            .usage_action(OPENAI_ID, &payload, "fixture-capture-key", None)
            .await
            .unwrap_or_else(|error| {
                panic!("{action} succeeds against the loopback fixture: {error}")
            });
        serialized_results.push(result.to_string());
    }
    let probe = usage_action_payload(&fixture.runtime, GROK_ID, "probe_quota", Some(true)).await;
    let probe_result = fixture
        .runtime
        .usage_action(GROK_ID, &probe, "fixture-capture-key", None)
        .await
        .expect("Grok quota probe succeeds against the loopback fixture");
    serialized_results.push(probe_result.to_string());
    let all_results = serialized_results.join("\n");
    assert!(!all_results.contains("secret-marker"));
    assert!(!all_results.contains("private-invite"));
    assert!(!all_results.contains("fixture-capture-key"));

    let calls = fixture.http.recorded_calls();
    let action_calls = calls
        .iter()
        .map(|call| {
            (
                call.method.clone(),
                call.path_and_query.clone(),
                call.key.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        action_calls,
        vec![
            (
                "GET".to_owned(),
                format!("/api/v1/admin/accounts/{OPENAI_ID}/usage?source=active&force=true"),
                "fixture-capture-key".to_owned()
            ),
            (
                "POST".to_owned(),
                format!("/api/v1/admin/openai/accounts/{OPENAI_ID}/quota/refresh"),
                "fixture-capture-key".to_owned()
            ),
            (
                "POST".to_owned(),
                format!("/api/v1/admin/openai/accounts/{OPENAI_ID}/reset-quota"),
                "fixture-capture-key".to_owned()
            ),
            (
                "GET".to_owned(),
                format!("/api/v1/admin/grok/accounts/{GROK_ID}/quota"),
                "fixture-capture-key".to_owned()
            ),
        ]
    );
    let audit = std::fs::read_to_string(&fixture.runtime.settings.audit_path)
        .expect("usage actions emit a local audit record");
    assert!(!audit.contains("fixture-capture-key"));
    assert!(!audit.contains("secret-marker"));
    assert!(!audit.contains("private-invite"));

    let warning_reset_path = format!("/api/v1/admin/openai/accounts/{WARNING_ID}/reset-quota");
    fixture.http.set_response(
        &warning_reset_path,
        200,
        json!({
            "code":0,
            "data":{
                "code":"success",
                "windows_reset":1,
                "cache_refreshed":false,
                "warning_code":"cache_failed"
            }
        }),
    );
    let warning_payload =
        usage_action_payload(&fixture.runtime, WARNING_ID, "reset_quota", Some(true)).await;
    let warning = fixture
        .runtime
        .usage_action(WARNING_ID, &warning_payload, "fixture-capture-key", None)
        .await
        .expect("a confirmed reset reports success with a cache warning");
    assert!(warning["message"].as_str().unwrap().contains("勿重复重置"));
    let guarded = fixture
        .runtime
        .usage_action(WARNING_ID, &warning_payload, "fixture-capture-key", None)
        .await
        .expect_err("the warning retains the no-replay guard");
    assert_eq!(guarded.status.as_u16(), 502);
    assert_eq!(
        calls_or_now(&fixture.http, &warning_reset_path),
        1,
        "the cache warning keeps the no-replay guard"
    );
    let state = read_oauth_state(&fixture.runtime.settings.usage_path);
    assert!(
        state["scheduler"][WARNING_ID.to_string()]["auto_reset_credit"]["attempt_at"].is_string()
    );

    fixture.close().await;
}

fn calls_or_now(state: &ProjectionHttpState, path: &str) -> usize {
    state
        .recorded_calls()
        .iter()
        .filter(|call| call.path_and_query == path)
        .count()
}

#[tokio::test]
async fn desktop_usage_queries_coalesce_and_monitor_lease_is_per_account() {
    let _database_serial = POSTGRES_PROJECTION_LOCK.lock().await;
    const OPENAI_ID: i64 = 967_421;
    const GROK_ID: i64 = 967_422;
    let fixture = start_projection_runtime_fixture("projection-usage-coalescing").await;
    insert_runtime_account(
        &fixture.seed.pool,
        OPENAI_ID,
        "openai",
        "oauth",
        json!({"access_token":"fixture-openai-access-token"}),
        projection_credit_extra(),
        false,
    )
    .await;
    insert_runtime_account(
        &fixture.seed.pool,
        GROK_ID,
        "grok",
        "oauth",
        json!({"access_token":"fixture-grok-access-token"}),
        json!({"subscription_tier":"supergrok"}),
        false,
    )
    .await;
    let usage_path = format!("/api/v1/admin/accounts/{OPENAI_ID}/usage");
    let usage_path_and_query = format!("{usage_path}?source=active&force=true");
    *fixture
        .http
        .block_path
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(usage_path.clone());
    let payload = usage_action_payload(&fixture.runtime, OPENAI_ID, "query_usage", None).await;
    let first_runtime = fixture.runtime.clone();
    let first_payload = payload.clone();
    let first = tokio::spawn(async move {
        first_runtime
            .usage_action(OPENAI_ID, &first_payload, "fixture-capture-key", None)
            .await
    });
    let entered = tokio::time::timeout(
        StdDuration::from_secs(5),
        fixture.http.entered.clone().acquire_owned(),
    )
    .await
    .expect("first query reaches the blocked mock")
    .expect("mock entry semaphore remains open");
    drop(entered);
    let second_runtime = fixture.runtime.clone();
    let second_payload = payload.clone();
    let second = tokio::spawn(async move {
        second_runtime
            .usage_action(OPENAI_ID, &second_payload, "fixture-capture-key", None)
            .await
    });
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    assert_eq!(calls_or_now(&fixture.http, &usage_path_and_query), 1);
    fixture.http.release.add_permits(1);
    let first = first.await.expect("join the owner query").unwrap();
    let second = second.await.expect("join the coalesced query").unwrap();
    assert_eq!(first, second);
    assert!(first["account"]["credentials"].is_null());
    assert!(!first.to_string().contains("fixture-openai-access-token"));
    assert_eq!(calls_or_now(&fixture.http, &usage_path_and_query), 1);

    let row = fixture
        .runtime
        .actions
        .account(OPENAI_ID, None)
        .await
        .expect("read the OpenAI OAuth account before holding its monitor lease");
    let lease = fixture
        .runtime
        .monitor
        .lease(&row)
        .expect("hold only the OpenAI account's monitor lease");
    let openai_credits =
        usage_action_payload(&fixture.runtime, OPENAI_ID, "query_reset_credits", None).await;
    let busy = fixture
        .runtime
        .usage_action(OPENAI_ID, &openai_credits, "fixture-capture-key", None)
        .await
        .expect_err("OpenAI usage action rejects an active per-account monitor lease");
    assert_eq!(busy.status.as_u16(), 409);

    let grok_probe =
        usage_action_payload(&fixture.runtime, GROK_ID, "probe_quota", Some(true)).await;
    fixture
        .runtime
        .usage_action(GROK_ID, &grok_probe, "fixture-capture-key", None)
        .await
        .expect("a different Grok account can be probed while the OpenAI account is busy");
    drop(lease);
    fixture
        .runtime
        .usage_action(OPENAI_ID, &openai_credits, "fixture-capture-key", None)
        .await
        .expect("the OpenAI credits action proceeds after its lease is released");

    assert_eq!(
        calls_or_now(
            &fixture.http,
            &format!("/api/v1/admin/grok/accounts/{GROK_ID}/quota")
        ),
        1
    );
    assert_eq!(
        calls_or_now(
            &fixture.http,
            &format!("/api/v1/admin/openai/accounts/{OPENAI_ID}/quota/refresh")
        ),
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn desktop_uncertain_reset_requires_confirmed_recovery_before_replay() {
    let _database_serial = POSTGRES_PROJECTION_LOCK.lock().await;
    const ACCOUNT_ID: i64 = 967_431;
    const RECOVERABLE_ID: i64 = 967_433;
    const GROK_ID: i64 = 967_432;
    let fixture = start_projection_runtime_fixture_with_oauth(
        "projection-usage-uncertain-reset",
        json!({
            "oauth_recovery_monitor_enabled": true,
            "oauth_recovery_connection_account_ids": [RECOVERABLE_ID]
        }),
    )
    .await;
    insert_runtime_account(
        &fixture.seed.pool,
        ACCOUNT_ID,
        "openai",
        "oauth",
        json!({"access_token":"fixture-openai-access-token"}),
        projection_credit_extra(),
        false,
    )
    .await;
    insert_runtime_account(
        &fixture.seed.pool,
        GROK_ID,
        "grok",
        "oauth",
        json!({"access_token":"fixture-grok-access-token"}),
        json!({"subscription_tier":"supergrok"}),
        false,
    )
    .await;
    insert_runtime_account(
        &fixture.seed.pool,
        RECOVERABLE_ID,
        "openai",
        "oauth",
        json!({"access_token":"fixture-recoverable-openai-access-token"}),
        projection_credit_extra(),
        false,
    )
    .await;
    let grok_probe_path = format!("/api/v1/admin/grok/accounts/{GROK_ID}/quota");
    fixture.http.set_response(
        &grok_probe_path,
        200,
        json!({
            "code":0,
            "data":{"probe_error":"fixture-private-upstream-body","status_code":429}
        }),
    );
    let grok_probe =
        usage_action_payload(&fixture.runtime, GROK_ID, "probe_quota", Some(true)).await;
    let partial_probe = fixture
        .runtime
        .usage_action(GROK_ID, &grok_probe, "fixture-capture-key", None)
        .await
        .expect_err("a partial Grok probe result is not reported as success");
    assert_eq!(partial_probe.status.as_u16(), 502);
    assert_eq!(partial_probe.detail["code"], "probe_failed");
    assert!(!partial_probe
        .detail
        .to_string()
        .contains("fixture-private-upstream-body"));
    assert_eq!(calls_or_now(&fixture.http, &grok_probe_path), 1);

    let reset_path = format!("/api/v1/admin/openai/accounts/{ACCOUNT_ID}/reset-quota");
    // The upstream answers without saying whether a card was consumed.
    fixture
        .http
        .set_response(&reset_path, 200, json!({"code":0,"data":null}));
    let reset = usage_action_payload(&fixture.runtime, ACCOUNT_ID, "reset_quota", Some(true)).await;
    let first = fixture
        .runtime
        .usage_action(ACCOUNT_ID, &reset, "fixture-capture-key", None)
        .await
        .expect_err("an unconfirmed upstream answer leaves the reset outcome uncertain");
    assert_eq!(first.status.as_u16(), 502);
    assert_eq!(first.detail["code"], "result_uncertain");
    fixture.http.clear_response(&reset_path);
    assert_eq!(calls_or_now(&fixture.http, &reset_path), 1);
    let second = fixture
        .runtime
        .usage_action(ACCOUNT_ID, &reset, "fixture-capture-key", None)
        .await
        .expect_err("an uncertain reset cannot be replayed");
    assert_eq!(second.status.as_u16(), 502);
    assert_eq!(calls_or_now(&fixture.http, &reset_path), 1);

    let count_path = format!("/api/v1/admin/openai/accounts/{ACCOUNT_ID}/quota/refresh");
    let observed = Utc::now();
    let unpersisted = json!({
        "code":0,
        "data":{
            "five_hour":{"utilization":10,"resets_at":compat::datetime(observed + Duration::hours(5)),"window_minutes":300},
            "seven_day":{"utilization":20,"resets_at":compat::datetime(observed + Duration::days(7)),"window_minutes":10080},
            "fetched_at":compat::datetime(observed),
            "rate_limit_reset_credits":{"available_count":1,"credits":[{"expires_at":"2099-12-31T00:00:00+00:00"}],"fetched_at":compat::datetime(observed)},
            "cache_persisted":false
        }
    });
    fixture.http.set_response(&count_path, 200, unpersisted);
    let query =
        usage_action_payload(&fixture.runtime, ACCOUNT_ID, "query_reset_credits", None).await;
    let failed_count = fixture
        .runtime
        .usage_action(ACCOUNT_ID, &query, "fixture-capture-key", None)
        .await
        .expect_err("an unpersisted credit count is not accepted as success");
    assert_eq!(failed_count.status.as_u16(), 502);
    let after_failed_count = fixture
        .runtime
        .usage_action(ACCOUNT_ID, &reset, "fixture-capture-key", None)
        .await
        .expect_err("a failed count query does not clear the no-replay guard");
    assert_eq!(after_failed_count.status.as_u16(), 502);
    assert_eq!(calls_or_now(&fixture.http, &reset_path), 1);

    // A response that claims persistence but reports different cards than the
    // stored snapshot is not a confirmed count either.
    let observed = Utc::now();
    fixture.http.set_response(
        &count_path,
        200,
        json!({
            "code":0,
            "data":{
                "five_hour":{"utilization":10,"resets_at":compat::datetime(observed + Duration::hours(5)),"window_minutes":300},
                "seven_day":{"utilization":20,"resets_at":compat::datetime(observed + Duration::days(7)),"window_minutes":10080},
                "fetched_at":observed.timestamp(),
                "rate_limit_reset_credits":{"available_count":2,"credits":[{"expires_at":"2099-12-31T00:00:00+00:00"},{"expires_at":"2099-12-31T00:00:00+00:00"}]},
                "cache_persisted":true,
                "credits_cache_persisted":true
            }
        }),
    );
    let mismatched_count = fixture
        .runtime
        .usage_action(ACCOUNT_ID, &query, "fixture-capture-key", None)
        .await
        .expect_err("a count that differs from the stored snapshot is not accepted as success");
    assert_eq!(mismatched_count.status.as_u16(), 502);
    assert_eq!(
        mismatched_count.detail["code"],
        "credit_snapshot_unconfirmed"
    );

    fixture.http.clear_response(&count_path);
    fixture
        .runtime
        .usage_action(ACCOUNT_ID, &query, "fixture-capture-key", None)
        .await
        .expect("a persisted credit count refreshes the evidence used by the reset guard");
    assert_eq!(calls_or_now(&fixture.http, &count_path), 3);
    let state = read_oauth_state(&fixture.runtime.settings.usage_path);
    assert_ne!(
        state["scheduler"][ACCOUNT_ID.to_string()]
            ["auto_reset_credit"]["stage"],
        "recovered",
        "a saved count alone does not clear the full Runtime recovery gate for an unconfigured account"
    );
    let retry_without_recovery =
        usage_action_payload(&fixture.runtime, ACCOUNT_ID, "reset_quota", Some(true)).await;
    let still_uncertain = fixture
        .runtime
        .usage_action(
            ACCOUNT_ID,
            &retry_without_recovery,
            "fixture-capture-key",
            None,
        )
        .await
        .expect_err(
            "a persisted count without configured recovery confirmation cannot replay reset",
        );
    assert_eq!(still_uncertain.status.as_u16(), 502);
    assert_eq!(calls_or_now(&fixture.http, &reset_path), 1);

    let recoverable_reset_path =
        format!("/api/v1/admin/openai/accounts/{RECOVERABLE_ID}/reset-quota");
    fixture.http.set_response(
        &recoverable_reset_path,
        500,
        json!({"code":500,"message":"fixture reset outcome unknown"}),
    );
    let recoverable_reset =
        usage_action_payload(&fixture.runtime, RECOVERABLE_ID, "reset_quota", Some(true)).await;
    let uncertain = fixture
        .runtime
        .usage_action(
            RECOVERABLE_ID,
            &recoverable_reset,
            "fixture-capture-key",
            None,
        )
        .await
        .expect_err("the recoverable account begins with the same uncertain upstream outcome");
    assert_eq!(uncertain.status.as_u16(), 502);
    assert_eq!(calls_or_now(&fixture.http, &recoverable_reset_path), 1);

    let recoverable_count_path =
        format!("/api/v1/admin/openai/accounts/{RECOVERABLE_ID}/quota/refresh");
    let recoverable_query = usage_action_payload(
        &fixture.runtime,
        RECOVERABLE_ID,
        "query_reset_credits",
        None,
    )
    .await;
    fixture
        .runtime
        .usage_action(
            RECOVERABLE_ID,
            &recoverable_query,
            "fixture-capture-key",
            None,
        )
        .await
        .expect(
            "a complete, fresh quota and persisted count confirm recovery for the eligible account",
        );
    let state = read_oauth_state(&fixture.runtime.settings.usage_path);
    assert_eq!(
        state["scheduler"][RECOVERABLE_ID.to_string()]["auto_reset_credit"]["stage"],
        "recovered"
    );
    let recovered_reset =
        usage_action_payload(&fixture.runtime, RECOVERABLE_ID, "reset_quota", Some(true)).await;
    fixture.http.clear_response(&recoverable_reset_path);
    fixture
        .runtime
        .usage_action(
            RECOVERABLE_ID,
            &recovered_reset,
            "fixture-capture-key",
            None,
        )
        .await
        .expect("a new manual reset is permitted after confirmed recovery");
    assert_eq!(calls_or_now(&fixture.http, &recoverable_reset_path), 2);
    assert_eq!(calls_or_now(&fixture.http, &recoverable_count_path), 1);
    fixture.close().await;
}

#[test]
fn slow_ttft_and_local_capacity_evidence_match_pinned_python() {
    let document = fixtures();
    let input = &document["slow"];
    let now = at(&input["at"]);
    assert_eq!(input["error_where_present"], true);
    assert!(
        !sub2ops_server::sql::ERRORS_DEGRADATION_WHERE
            .trim()
            .is_empty(),
        "the database query retains a non-empty degradation evidence selection boundary"
    );

    for case in input["warning_cases"].as_array().unwrap() {
        let samples = case["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| slow_ttft::slow_sample(row, &case["account"]))
            .collect::<Vec<_>>();
        assert_eq!(
            slow_ttft::slow_warning(&samples, now).unwrap_or(Value::Null),
            case["expected_warning"],
            "slow warning {}",
            case["id"]
        );
    }
    for case in input["sample_exclusions"].as_array().unwrap() {
        let account = case
            .get("account")
            .cloned()
            .unwrap_or_else(|| json!({"id":1,"platform":"openai","type":"oauth"}));
        assert_eq!(
            slow_ttft::slow_sample(&case["row"], &account).is_some(),
            !case["expected"].is_null(),
            "slow sample inclusion {}",
            case["id"]
        );
    }

    for case in input["throttle_cases"].as_array().unwrap() {
        assert_eq!(
            error_evidence::local_throttle(&case["row"]),
            case["expected"].as_bool().unwrap(),
            "local throttle {}",
            case["id"]
        );
    }
    for case in input["capacity_cases"].as_array().unwrap() {
        assert_eq!(
            error_evidence::local_throttle(&case["row"]),
            case["is_local"].as_bool().unwrap(),
            "capacity/local classification {}",
            case["id"]
        );
        let actual = error_evidence::match_message(&case["row"])
            .map(Value::from)
            .unwrap_or(Value::Null);
        assert_eq!(
            actual, case["expected_message"],
            "capacity clue {}",
            case["id"]
        );
    }
}

#[test]
fn codex_identity_vectors_match_pinned_python() {
    let document = fixtures();
    for case in document["identity"]["cases"].as_array().unwrap() {
        if case["id"] == "custom_leading_and_trailing_versions_rewritten" {
            let actual = model_target::synced_agent(
                &case["input"]["custom"],
                case["input"]["version"].as_str().unwrap(),
            );
            assert_eq!(
                Value::String(actual),
                case["expected"],
                "identity case {}",
                case["id"]
            );
        } else {
            let (version, agent) = model_target::identity(&case["input"]);
            assert_eq!(
                json!([version, agent]),
                case["expected"],
                "identity case {}",
                case["id"]
            );
        }
    }
}

#[test]
fn key_profile_mapping_is_synthetic_and_composes_like_pinned_python() {
    let document = fixtures();
    let input = &document["key_support"];
    let account = &input["account_before"];
    assert_json_numbers_equal(
        &account_templates::combine(&input["profile"]).expect("fixture template profile"),
        &input["expected_mapping"],
        "key profile mapping matches the pinned Python composition",
    );
    assert!(account_templates::eligible(account));
    assert!(!account_templates::passthrough(account));
    assert_json_numbers_equal(
        &account_templates::account_row(account)["model_mapping"],
        &account["model_mapping"],
        "key account projection carries only the existing mapping field",
    );
}

#[tokio::test]
async fn key_template_action_verifies_mapping_and_preserves_the_full_account_row() {
    let _database_serial = POSTGRES_PROJECTION_LOCK.lock().await;
    const ACCOUNT_ID: i64 = 967_501;
    let db = open_test_database("projection-key-template").await;
    sqlx::query(
        "INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,proxy_id,priority,concurrency,load_factor) \
         VALUES ($1,'fixture-key-account','openai','apikey','disabled',false, \
           '{\"model_mapping\":{},\"fixture_marker\":\"preserve\"}'::jsonb, \
           '{\"pool\":{\"size\":4}}'::jsonb,9,3,2,1.5)",
    )
    .bind(ACCOUNT_ID)
    .execute(&db.pool)
    .await
    .expect("insert a key account with synthetic credentials and unrelated state");

    let calls = Arc::new(StdMutex::new(Vec::new()));
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback-only template writer");
    let upstream_address = upstream_listener.local_addr().unwrap();
    let upstream_state = TemplateUpstreamState {
        pool: db.pool.clone(),
        calls: calls.clone(),
    };
    let upstream_app = Router::new()
        .route(
            "/api/v1/admin/accounts/bulk-update",
            post(projection_template_bulk_update),
        )
        .with_state(upstream_state);
    let upstream_task = tokio::spawn(async move {
        axum::serve(upstream_listener, upstream_app)
            .await
            .expect("serve local fixture template writer");
    });
    let upstream_url = format!("http://{upstream_address}");
    let directory = tempfile::tempdir().expect("create owned template action state directory");
    let settings = test_settings(
        directory.path(),
        database_url_for("projection-key-template"),
        &upstream_url,
    );
    let actions = Actions::new(
        Queries {
            db: db.clone(),
            oauth: OAuthState::new(settings.usage_path.clone()),
        },
        Upstream::new(settings.clone()).expect("create fixture-only upstream client"),
        AccountLocks::default(),
        account_templates::Templates::new(
            settings.usage_path.with_file_name("account-templates.json"),
        ),
        settings.clone(),
    )
    .expect("create action service for the temporary database and loopback upstream");

    let before = db
        .one(
            "SELECT to_jsonb(a) AS row FROM accounts a WHERE a.id=$1",
            &[ACCOUNT_ID.into()],
        )
        .await
        .expect("read complete synthetic account row before update")
        .unwrap()["row"]
        .clone();
    let input = fixtures()["key_support"].clone();
    let initial_templates = actions
        .templates
        .view(None)
        .expect("read built-in synthetic template profiles");
    let saved_templates = actions
        .templates
        .edit(
            Some("degraded"),
            &json!({
                "expected_version": initial_templates["version"],
                "name": initial_templates["template_meta"]["degraded"]["name"],
                "whitelist": input["profile"]["whitelist"],
                "mappings": input["profile"]["mappings"],
            }),
            false,
        )
        .expect("save fixed degraded profile for the fixture account");
    let account = actions
        .account(ACCOUNT_ID, None)
        .await
        .expect("read account-specific template version");
    let result = actions
        .apply_template(
            ACCOUNT_ID,
            &json!({
                "expected_version": account_templates::account_version(&account),
                "template_id": "degraded",
                "template_version": saved_templates["version"],
            }),
            "fixture-admin-key",
        )
        .await
        .expect("apply and read back only the model mapping through the loopback writer");
    assert_eq!(result["verified"], true);
    assert_eq!(result["template_id"], "degraded");

    let after = db
        .one(
            "SELECT to_jsonb(a) AS row FROM accounts a WHERE a.id=$1",
            &[ACCOUNT_ID.into()],
        )
        .await
        .expect("read complete synthetic account row after update")
        .unwrap()["row"]
        .clone();
    let mut expected_after = before;
    expected_after["credentials"]["model_mapping"] = input["expected_mapping"].clone();
    assert_eq!(
        after, expected_after,
        "all fields except model_mapping remain unchanged"
    );
    let observed_calls = calls
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    assert_eq!(
        observed_calls,
        vec![(
            "/api/v1/admin/accounts/bulk-update".into(),
            "fixture-admin-key".into(),
            json!({
                "account_ids": [ACCOUNT_ID],
                "credentials": {"model_mapping": input["expected_mapping"]},
            }),
        )],
        "the action sends one narrowly scoped template update with the configured API key"
    );

    db.close().await;
    upstream_task.abort();
    let _ = upstream_task.await;
}

#[tokio::test]
async fn native_account_usage_http_contract_is_private_isolated_and_keeps_child_stats() {
    let _database_serial = POSTGRES_PROJECTION_LOCK.lock().await;
    let fixture = start_projection_runtime_fixture("native-account-usage").await;
    let cases = [
        (969_801, "anthropic", "apikey", json!({}), ""),
        (
            969_802,
            "anthropic",
            "oauth",
            json!({}),
            "accounts/969802/usage",
        ),
        (
            969_803,
            "gemini",
            "service_account",
            json!({}),
            "accounts/969803/usage",
        ),
        (
            969_804,
            "kimi",
            "apikey",
            json!({"account_mode":"coding"}),
            "cn-providers/accounts/969804/quota",
        ),
        (
            969_805,
            "deepseek",
            "apikey",
            json!({}),
            "cn-providers/accounts/969805/balance",
        ),
        (
            969_806,
            "opencode_go",
            "apikey",
            json!({"account_mode":"go"}),
            "accounts/969806/opencode-go-usage",
        ),
    ];
    for (id, platform, kind, credentials, _) in &cases {
        insert_runtime_account(
            &fixture.seed.pool,
            *id,
            platform,
            kind,
            credentials.clone(),
            json!({}),
            false,
        )
        .await;
    }
    let (address, task) = start_projection_api(fixture.runtime.clone()).await;
    let client = reqwest::Client::new();
    let url = format!("http://{address}/api/desktop/v1/accounts/969801/usage");
    assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        client
            .post(&url)
            .header("x-api-key", "fixture-key")
            .send()
            .await
            .unwrap()
            .status(),
        405
    );
    assert_eq!(
        client
            .get(format!("{url}?force=true"))
            .header("x-api-key", "fixture-key")
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    for (id, _, _, _, upstream) in &cases {
        if !upstream.is_empty() {
            fixture.http.set_response(&format!("/api/v1/admin/{upstream}"), 200, json!({"code":0,"data":{
                "success":true,"updated_at":compat::datetime(Utc::now()),"fetched_at":Utc::now().timestamp(),
                "source":"passive","five_hour":{"utilization":25},"gemini_pro_daily":{"utilization":0},
                "tiers":[{"window":"5h","used_percent":12}],"balances":[{"currency":"CNY","balance":0}],
                "snapshot":{"status":"ok","fetched_at":compat::datetime(Utc::now()),"data":{"rolling":{"percent":10}}},
                "credentials":{"api_key":"fixture-secret-marker"},"private":"fixture-secret-marker"
            }}));
        }
        let (status, body) = projection_json_response(
            client
                .get(format!(
                    "http://{address}/api/desktop/v1/accounts/{id}/usage"
                ))
                .header("x-api-key", "fixture-key"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.to_string().contains("fixture-secret-marker"));
        assert!(body["windows"].is_array());
        if *id == 969_801 {
            assert_eq!(body["today"]["requests"], 0);
        }
        if *id == 969_806 {
            fixture.http.set_response("/api/v1/admin/accounts/969806/opencode-go-usage/refresh", 200,
                json!({"code":0,"data":{"snapshot":{"status":"ok","fetched_at":compat::datetime(Utc::now()),"data":{"rolling":{"percent":10}}}}}));
        }
        let payload = usage_action_payload(&fixture.runtime, *id, "query_usage", None).await;
        let refreshed = fixture
            .runtime
            .usage_action(*id, &payload, "fixture-key", None)
            .await
            .unwrap();
        let usage = &refreshed["account"]["usage"];
        assert_eq!(usage["status"], body["status"]);
        assert_eq!(
            usage["windows"].as_array().unwrap().len(),
            body["windows"].as_array().unwrap().len()
        );
        assert_eq!(usage["today"]["requests"], body["today"]["requests"]);
        assert!(!refreshed.to_string().contains("fixture-secret-marker"));
        assert!(refreshed["account"].get("credentials").is_none());
    }
    assert!(fixture
        .http
        .recorded_calls()
        .iter()
        .any(|call| call.path_and_query == "/api/v1/admin/accounts/969802/usage?source=passive"));
    assert!(!fixture
        .http
        .recorded_calls()
        .iter()
        .any(|call| call.path_and_query.contains("accounts/969801/usage")));
    fixture.http.set_response(
        "/api/v1/admin/accounts/969802/usage",
        503,
        json!({"error":"fixture unavailable"}),
    );
    let failed = fixture
        .runtime
        .account_usage(969_802, "fixture-key", false)
        .await
        .unwrap();
    assert_eq!(failed["status"], "error");
    assert_eq!(
        fixture
            .runtime
            .account_usage(969_805, "fixture-key", false)
            .await
            .unwrap()["balances"][0]["balance"],
        0.0
    );
    let payload = usage_action_payload(&fixture.runtime, 969_805, "query_usage", None).await;
    assert_eq!(
        fixture
            .runtime
            .usage_action(969_805, &payload, "fixture-key", None)
            .await
            .unwrap()["account"]["usage"]["balances"][0]["currency"],
        "CNY"
    );
    // Native quota requests retain the child ID; local costs remain owned by it.
    sqlx::query("UPDATE accounts SET parent_account_id=969802 WHERE id=969803")
        .execute(&fixture.seed.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO usage_logs(id,account_id,created_at,model,input_tokens,output_tokens,total_cost,actual_cost,account_rate_multiplier) VALUES(969891,969802,now(),'fixture',1000,1000,10,20,2),(969892,969803,now(),'fixture',10,20,0.5,0.25,3)").execute(&fixture.seed.pool).await.unwrap();
    // Query the fixture statistics directly after the insert.
    let projected = usage::project(
        &fixture
            .runtime
            .actions
            .account(969_803, None)
            .await
            .unwrap(),
        Utc::now(),
        &Value::Null,
    );
    let specs = usage::stats_specs(969_803, &projected);
    let stats = fixture
        .runtime
        .actions
        .queries
        .db
        .all(
            &sub2ops_server::sql::STATS_STATS_SQL.replace("%(windows)s", "$1"),
            &[sub2ops_server::db::Param::Json(json!(specs))],
        )
        .await
        .unwrap();
    let stat = &stats[0];
    assert_eq!(stat["account_id"], 969_803);
    assert_eq!(compat::integer(&stat["tokens"]), Some(30));
    assert_eq!(compat::float(&stat["cost"]), Some(1.5));
    assert_eq!(compat::float(&stat["user_cost"]), Some(0.25));
    let child = fixture
        .runtime
        .account_usage(969_803, "fixture-key", false)
        .await
        .unwrap();
    assert_eq!(child["windows"][1]["used_percent"], 0.0);
    let payload = usage_action_payload(&fixture.runtime, 969_803, "query_usage", None).await;
    let refreshed = fixture
        .runtime
        .usage_action(969_803, &payload, "fixture-key", None)
        .await
        .unwrap();
    assert_eq!(
        refreshed["account"]["usage"]["windows"][1]["used_percent"],
        0.0
    );
    let calls = fixture.http.recorded_calls();
    for path in [
        "/api/v1/admin/accounts/969802/usage?source=active&force=true",
        "/api/v1/admin/accounts/969803/usage?force=true",
        "/api/v1/admin/accounts/969806/opencode-go-usage/refresh",
    ] {
        assert!(
            calls.iter().any(|call| call.path_and_query == path),
            "missing {path}"
        );
    }

    insert_runtime_account(
        &fixture.seed.pool,
        969_807,
        "anthropic",
        "setup-token",
        json!({}),
        json!({}),
        false,
    )
    .await;
    fixture
        .runtime
        .start_quota_refresh("fixture-key")
        .await
        .unwrap();
    let batch = tokio::time::timeout(StdDuration::from_secs(10), async {
        loop {
            let batch = fixture.runtime.quota_progress();
            if batch["status"] == "completed" {
                break batch;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let items = batch["items"].as_array().unwrap();
    assert!(items
        .iter()
        .any(|v| v["account_id"] == 969_802 && v["status"] == "partial"));
    assert_eq!(batch["total"], items.len());
    assert_eq!(batch["completed"], items.len());
    for excluded in [969_801, 969_803, 969_804, 969_805, 969_806, 969_807] {
        assert!(items.iter().all(|v| v["account_id"] != excluded));
    }
    task.abort();
    fixture.close().await;
}

#[tokio::test]
async fn slow_samples_keep_tie_order_and_filter_before_limiting() {
    let _serial = POSTGRES_PROJECTION_LOCK.lock().await;
    let db = open_test_database("slow-indexed-samples").await;
    let id = 967_901;
    for (account, platform, deleted) in [
        (id, "openai", false),
        (id + 1, "openai", false),
        (id + 2, "openai", true),
        (id + 3, "anthropic", false),
    ] {
        insert_account(&db.pool, account, platform, "oauth", json!({}), deleted).await;
    }
    let at = Utc::now();
    for offset in 0..24_i64 {
        sqlx::query("INSERT INTO usage_logs(id,account_id,created_at,model,stream,first_token_ms,duration_ms,output_tokens,inbound_endpoint,image_count) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(967_901_000 + offset).bind(id).bind(at - Duration::minutes(1))
            .bind(if offset == 23 { "fixture-image" } else { "fixture-model" })
            .bind(offset != 22).bind(if offset == 21 { 0_i64 } else { 100_i64 })
            .bind(if offset == 20 { 50_i64 } else { 200_i64 })
            .bind(if offset == 19 { -1_i64 } else { 10_i64 })
            .bind(if offset == 18 { "/v1/audio" } else { "/v1/responses" })
            .bind(if offset == 17 { 1_i32 } else { 0_i32 })
            .execute(&db.pool).await.unwrap();
    }
    let rows = db
        .all(
            sub2ops_server::sql::CAPACITY_SLOW_SQL,
            &[(at - Duration::days(1)).into(), at.into()],
        )
        .await
        .unwrap();
    let selected = rows
        .iter()
        .filter(|r| r["account_id"] == id)
        .map(|r| r["id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        selected,
        (7..=16).rev().map(|i| 967_901_000 + i).collect::<Vec<_>>()
    );
    assert!(rows
        .iter()
        .any(|r| r["account_id"] == id + 1 && r["id"].is_null()));
    assert!(!rows
        .iter()
        .any(|r| r["account_id"] == id + 2 || r["account_id"] == id + 3));
    db.close().await;
}

#[tokio::test]
async fn key_usage_actions_keep_child_statistics_and_recheck_after_lock_wait() {
    let _serial = POSTGRES_PROJECTION_LOCK.lock().await;
    let fixture = start_projection_runtime_fixture("key-usage-actions").await;
    let parent = 969_821;
    let child = 969_822;
    let path = "/fixture-key/v1/usage";
    fixture.http.set_response(
        path,
        200,
        json!({"balance":15,"private":"fixture-parent-secret"}),
    );
    for id in [parent, child] {
        insert_runtime_account(&fixture.seed.pool, id, "anthropic", "apikey",
            json!({"base_url":format!("{}/fixture-key",fixture.base_url), "api_key":if id == parent { "fixture-parent-secret" } else { "fixture-child-secret" }}), json!({}), false).await;
    }
    sqlx::query("UPDATE accounts SET parent_account_id=$1 WHERE id=$2")
        .bind(parent)
        .bind(child)
        .execute(&fixture.seed.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO usage_logs(id,account_id,created_at,model,input_tokens,output_tokens,total_cost,actual_cost,account_rate_multiplier) VALUES(96982101,969821,now(),'fixture',1000,1000,10,20,2),(96982201,969822,now(),'fixture',10,20,0.5,0.25,3)")
        .execute(&fixture.seed.pool).await.unwrap();
    for id in [parent, child] {
        let payload = usage_action_payload(&fixture.runtime, id, "query_usage", None).await;
        let response = fixture
            .runtime
            .usage_action(id, &payload, "fixture-admin", None)
            .await
            .unwrap();
        let usage = &response["account"]["usage"];
        assert_eq!(usage["key_quota"]["state"], "ok");
        assert_eq!(usage["key_quota"]["plans"][0]["remaining"], 15.0);
        assert_eq!(
            usage["today"]["tokens"],
            if id == parent { 2000 } else { 30 }
        );
        assert!(!response.to_string().contains("fixture-parent-secret"));
        assert!(!response.to_string().contains("fixture-child-secret"));
        assert!(response["account"].get("credentials").is_none());
    }
    assert_eq!(
        fixture
            .http
            .recorded_calls()
            .iter()
            .filter(|c| c.path_and_query == path
                && c.key == "fixture-parent-secret"
                && c.method == "GET")
            .count(),
        2
    );
    // A control-lock wait must not reuse the account version read before it.
    let payload = usage_action_payload(&fixture.runtime, child, "query_usage", None).await;
    let control = fixture.runtime.actions.locks.control(child).await;
    let runtime = fixture.runtime.clone();
    let waiting = tokio::spawn(async move {
        runtime
            .usage_action(child, &payload, "fixture-admin", None)
            .await
    });
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    sqlx::query("UPDATE accounts SET name='fixture-changed',updated_at=now() WHERE id=$1")
        .bind(child)
        .execute(&fixture.seed.pool)
        .await
        .unwrap();
    drop(control);
    assert_eq!(waiting.await.unwrap().unwrap_err().status.as_u16(), 409);
    assert_eq!(
        fixture
            .http
            .recorded_calls()
            .iter()
            .filter(|c| c.path_and_query == path)
            .count(),
        2
    );
    fixture.close().await;
}
