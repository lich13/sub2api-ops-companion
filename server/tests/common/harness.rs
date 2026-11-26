#![allow(dead_code)]

use axum::{
    body::Body,
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use sqlx::types::Json as SqlJson;
use std::{
    fs,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use sub2ops_server::{
    compat::datetime,
    credit,
    db::Database,
    error::{Error, Result},
    model_detection::{detection_request, Detection},
    operations::{Backend, Operations},
    priority_orders::{self, PriorityOrders},
    request_schema,
    settings::Settings,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

pub fn idle_oauth_config() -> Value {
    json!({
        "oauth_auto_reset_credit_enabled": false,
        "oauth_daily_test_enabled": false,
        "oauth_daily_test_time": "23:59",
        "oauth_recovery_connection_account_ids": [],
        "oauth_recovery_model_account_ids": [],
        "oauth_recovery_monitor_enabled": false,
        "oauth_recovery_test_concurrency": 1,
        "oauth_recovery_test_model_id": "fixture-model",
        "oauth_usage_refresh_concurrency": 1,
    })
}

pub fn runtime_settings(
    database_url: impl Into<String>,
    root: &Path,
    base_url: impl Into<String>,
) -> Settings {
    Settings {
        database_url: database_url.into(),
        bind: "127.0.0.1:0".into(),
        audit_path: root.join("audit.jsonl"),
        usage_path: root.join("oauth-state.json"),
        oauth_path: root.join("oauth-config.json"),
        bark_path: root.join("bark-config.json"),
        connection_path: root.join("sub2api-config.json"),
        keepalive_path: root.join("keepalive-state.json"),
        oauth: idle_oauth_config(),
        bark: json!({"enabled": false}),
        bark_valid: true,
        base_url: base_url.into(),
        verify_url: String::new(),
    }
}

pub fn operation_settings(
    database_url: impl Into<String>,
    root: &Path,
    base_url: impl Into<String>,
) -> Settings {
    Settings {
        database_url: database_url.into(),
        bind: "127.0.0.1:0".into(),
        audit_path: root.join("audit.jsonl"),
        usage_path: root.join("usage-state.json"),
        oauth_path: root.join("oauth-state.json"),
        bark_path: root.join("bark-state.json"),
        connection_path: root.join("sub2api-config.json"),
        keepalive_path: root.join("keepalive-state.json"),
        oauth: json!({}),
        bark: json!({"enabled": false}),
        bark_valid: true,
        base_url: base_url.into(),
        verify_url: String::new(),
    }
}

pub fn offline_bank_settings(root: &Path) -> Settings {
    Settings {
        database_url: "postgres://example.invalid/sub2ops".into(),
        bind: "127.0.0.1:0".into(),
        audit_path: root.join("audit.jsonl"),
        usage_path: root.join("oauth-state.json"),
        oauth_path: root.join("oauth-config.json"),
        bark_path: root.join("bark-config.json"),
        connection_path: root.join("sub2api-config.json"),
        keepalive_path: root.join("keepalive-state.json"),
        oauth: json!({}),
        bark: json!({}),
        bark_valid: false,
        base_url: String::new(),
        verify_url: String::new(),
    }
}

pub fn verified_runtime_settings(
    root: &Path,
    database_url: impl Into<String>,
    base_url: &str,
) -> Arc<Settings> {
    fs::create_dir_all(root).expect("create owned test state directory");
    let connection_path = root.join("sub2api-config.json");
    fs::write(
        &connection_path,
        serde_json::to_vec(&json!({"base_url": base_url, "verify_base_url": base_url}))
            .expect("serialize loopback upstream config"),
    )
    .expect("write loopback upstream config");
    Arc::new(Settings {
        bark: json!({"enabled": false, "device_key": "", "server_url": ""}),
        verify_url: base_url.into(),
        connection_path,
        ..runtime_settings(database_url, root, base_url)
    })
}

pub fn write_loopback_config(root: &Path, base_url: &str) -> PathBuf {
    let path = root.join("sub2api-config.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({"base_url": base_url, "verify_base_url": ""})).unwrap(),
    )
    .expect("write loopback upstream config");
    path
}

pub async fn spawn_loopback(router: Router) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind loopback mock");
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("serve loopback fixture");
    });
    (base_url, server)
}

#[derive(Clone, Default)]
pub struct Gate {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

impl Gate {
    pub fn new() -> Self {
        Self::default()
    }
}

pub fn json_ok(value: Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

pub fn status_json(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

pub fn json_status(status: u16, value: Value) -> Response {
    status_json(
        StatusCode::from_u16(status).expect("fixture HTTP status"),
        value,
    )
}

pub fn sse_response(body: impl Into<Body>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body.into())
        .expect("fixture SSE response")
}

pub fn method_name(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::PATCH => "PATCH",
        _ => "OTHER",
    }
}

pub fn path_id_between(path: &str, prefix: &str, suffix: &str) -> Option<i64> {
    path.strip_prefix(prefix)?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

pub fn account_path_id(path: &str, suffix: &str) -> Option<i64> {
    path_id_between(path, "/api/v1/admin/accounts/", suffix)
}

pub fn default_account_credentials(kind: &str, base_url: &str, token: &str) -> Value {
    if kind == "apikey" {
        json!({
            "api_key": token,
            "base_url": format!("{base_url}/v1"),
        })
    } else {
        json!({
            "plan_type": "plus",
            "access_token": token,
            "refresh_token": "fixture-refresh-token",
        })
    }
}

pub async fn seed_credit_account(
    db: &Database,
    id: i64,
    now: DateTime<Utc>,
    schedulable: bool,
    depleted: bool,
) -> Value {
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
    .execute(&db.pool)
    .await
    .expect("insert synthetic OAuth account");
    credit::account(db, id)
        .await
        .expect("read synthetic OAuth account")
}

pub struct CountingOperationBackend {
    pub dispatch_calls: Arc<AtomicUsize>,
    pub test_config_reads: AtomicUsize,
}

impl Default for CountingOperationBackend {
    fn default() -> Self {
        Self {
            dispatch_calls: Arc::new(AtomicUsize::new(0)),
            test_config_reads: AtomicUsize::new(0),
        }
    }
}

impl Backend for CountingOperationBackend {
    fn mark(&self, _id: i64) -> Result<Value> {
        Ok(json!({"version": "0".repeat(64)}))
    }

    fn dispatch<'a>(
        &'a self,
        _row: &'a Value,
        _job: &'a Value,
        _key: &'a str,
    ) -> BoxFuture<'a, Result<Value>> {
        self.dispatch_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json!({"fixture": true})) })
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
        self.test_config_reads.fetch_add(1, Ordering::SeqCst);
        Ok(("fixture-test-model".into(), 1))
    }
}

pub trait DetectionTestExt {
    fn save_test(&self, id: i64, payload: &Value) -> impl Future<Output = Result<Value>> + Send;
}

impl DetectionTestExt for Detection {
    async fn save_test(&self, id: i64, payload: &Value) -> Result<Value> {
        let request = detection_request(payload)?;
        self.save_validated(id, &request).await
    }
}

pub trait OperationsSubmitExt {
    fn submit_test(
        &self,
        id: i64,
        request: &Value,
        key: &str,
    ) -> impl Future<Output = Result<Value>> + Send;
}

impl OperationsSubmitExt for Arc<Operations> {
    async fn submit_test(&self, id: i64, request: &Value, key: &str) -> Result<Value> {
        self.submit_validated(id, request_schema::submission(request)?, key)
            .await
    }
}

pub trait OperationsBatchExt {
    fn submit_batch_test(
        &self,
        request: &Value,
        key: &str,
        template: bool,
    ) -> impl Future<Output = Result<Value>> + Send;
}

impl OperationsBatchExt for Operations {
    async fn submit_batch_test(&self, request: &Value, key: &str, template: bool) -> Result<Value> {
        let mut normalized = request.clone();
        request_schema::batch(&mut normalized, template)?;
        self.submit_batch_validated(&normalized, key, template)
            .await
    }
}

pub trait PriorityOrdersTestExt {
    fn submit_test(&self, request: &Value) -> impl Future<Output = Result<Value>> + Send;
}

impl PriorityOrdersTestExt for PriorityOrders {
    async fn submit_test(&self, request: &Value) -> Result<Value> {
        priority_orders::validate_request(request)?;
        self.submit_validated(request).await
    }
}
