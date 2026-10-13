mod model;
mod runner;
mod store;

use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use model::{id, now, version, Failure, Job, Submission};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use store::{Batch, Store};
use tokio::sync::Mutex;

pub struct Shared {
    pub store: Mutex<Store>,
    pub db: sqlx::PgPool,
    pub client: reqwest::Client,
    pub upstream: String,
    pub model: String,
    pub admin_key: String,
    pub concurrency: usize,
    auth: Mutex<HashMap<String, Instant>>,
    keys: Mutex<HashMap<String, String>>,
}
pub type App = Arc<Shared>;

impl Shared {
    async fn authenticate(&self, key: &str, fresh: bool) -> Result<(), Failure> {
        if key.is_empty() || key.len() > 4096 || key.bytes().any(|b| b < 33) {
            return Err(Failure(401, "auth_rejected", "管理员 API Key 无效"));
        }
        let hash = model::digest(&json!(key));
        let mut cache = self.auth.lock().await;
        if !fresh && cache.get(&hash).is_some_and(|time| *time > Instant::now()) {
            return Ok(());
        }
        let response = self
            .client
            .get(format!("{}/api/v1/admin/groups/all", self.upstream))
            .header("x-api-key", key)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|_| Failure(502, "auth_unavailable", "无法验证管理员权限"))?;
        if matches!(response.status().as_u16(), 401 | 403) {
            cache.remove(&hash);
            return Err(Failure(401, "auth_rejected", "管理员 API Key 无效"));
        }
        if !response.status().is_success() {
            return Err(Failure(502, "auth_unavailable", "管理员验证暂不可用"));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| Failure(502, "auth_unavailable", "无法读取管理员验证"))?;
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Failure(502, "auth_unavailable", "管理员验证格式无效"))?;
        if bytes.len() > 2_000_000 || body["code"] != 0 || !body["data"].is_array() {
            return Err(Failure(502, "auth_unavailable", "管理员验证格式无效"));
        }
        cache.retain(|_, expiry| *expiry > Instant::now());
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(hash, Instant::now() + Duration::from_secs(15));
        Ok(())
    }
    async fn rows(&self, ids: Option<&[i64]>) -> Result<Vec<Value>, Failure> {
        let rows = if let Some(ids) = ids {
            sqlx::query_scalar::<_, sqlx::types::Json<Value>>(
                "SELECT row_data FROM probe_accounts WHERE id=ANY($1) ORDER BY id",
            )
            .bind(ids)
            .fetch_all(&self.db)
            .await?
        } else {
            sqlx::query_scalar::<_, sqlx::types::Json<Value>>(
                "SELECT row_data FROM probe_accounts ORDER BY id",
            )
            .fetch_all(&self.db)
            .await?
        };
        Ok(rows
            .into_iter()
            .map(|r| r.0)
            .filter(|r| r["deleted_at"].is_null())
            .collect())
    }
}

fn key(headers: &HeaderMap) -> &str {
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}
async fn health(State(app): State<App>) -> Result<Json<Value>, Failure> {
    sqlx::query("SELECT 1").execute(&app.db).await?;
    Ok(Json(json!({"status":"ok","isolation":"fixture-only"})))
}
async fn accounts(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>, Failure> {
    app.authenticate(key(&headers), false).await?;
    Ok(Json(
        json!({"items":app.rows(None).await?.iter().map(|r|json!({"id":r["id"],"name":r["name"],"platform":r["platform"],"type":r["type"],"operation_versions":{"test":version(r)}})).collect::<Vec<_>>()}),
    ))
}
async fn submit(
    State(app): State<App>,
    headers: HeaderMap,
    body: Result<Json<Submission>, axum::extract::rejection::JsonRejection>,
) -> Result<(axum::http::StatusCode, Json<Value>), Failure> {
    let Json(request) = body.map_err(|_| Failure(422, "invalid_request", "批量请求格式无效"))?;
    request.validate()?;
    let key = key(&headers);
    app.authenticate(key, false).await?;
    let signature = request.signature();
    let store = app.store.lock().await;
    let mut data = store.read()?;
    if let Some(bid) = data.requests.get(&request.request_id) {
        if data.batches[bid].signature != signature {
            return Err(Failure(
                409,
                "idempotency_conflict",
                "请求 ID 已用于其他批量操作",
            ));
        }
        app.keys.lock().await.insert(bid.clone(), key.into());
        return Ok((axum::http::StatusCode::ACCEPTED, Json(data.listing(bid)?)));
    }
    let ids: Vec<i64> = request.accounts.iter().map(|a| a.account_id).collect();
    let rows = app.rows(Some(&ids)).await?;
    let bid = id();
    let stamp = now();
    let mut jobs = Vec::new();
    for input in &request.accounts {
        let row = rows
            .iter()
            .find(|r| r["id"].as_i64() == Some(input.account_id));
        let grok = row.is_some_and(|r| r["platform"] == "grok");
        let mut job = Job {
            id: id(),
            batch_id: bid.clone(),
            account_id: input.account_id,
            account_name: row
                .and_then(|r| r["name"].as_str())
                .unwrap_or("未知账号")
                .chars()
                .take(200)
                .collect(),
            action: "test".into(),
            parent_account_id: row.and_then(|r| r["parent_account_id"].as_i64()),
            expected_version: input.expected_version.clone(),
            requested: json!({"confirmed":true,"mode":if grok{"text"}else{"default"},"model_id":if grok{""}else{&app.model}}),
            status: "queued".into(),
            reason: "等待执行".into(),
            created_at: stamp.clone(),
            updated_at: stamp.clone(),
            started_at: None,
            result: None,
        };
        match row {
            None => job.change("failed", "账号不存在或已删除"),
            Some(r)
                if !matches!(r["platform"].as_str(), Some("openai" | "grok"))
                    || !matches!(r["type"].as_str(), Some("oauth" | "apikey")) =>
            {
                job.change("failed", "此账号不支持连接测试")
            }
            Some(r) if version(r) != input.expected_version => job.change(
                "needs_confirmation",
                "账号凭据、代理或模型配置已变化，请刷新后重新选择",
            ),
            _ => (),
        }
        jobs.push(job);
    }
    data.requests.insert(request.request_id, bid.clone());
    data.batches.insert(
        bid.clone(),
        Batch {
            signature,
            jobs: jobs.iter().map(|j| j.id.clone()).collect(),
        },
    );
    data.jobs.extend(jobs);
    store.write(&data)?;
    app.keys.lock().await.insert(bid.clone(), key.into());
    Ok((axum::http::StatusCode::ACCEPTED, Json(data.listing(&bid)?)))
}
async fn listing(
    State(app): State<App>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, Failure> {
    app.authenticate(key(&headers), false).await?;
    let bid = params
        .get("batch_id")
        .filter(|s| model::hex(s, 32))
        .ok_or(Failure(404, "batch_missing", "批量测试不存在"))?;
    Ok(Json(app.store.lock().await.read()?.listing(bid)?))
}
async fn cancel(
    State(app): State<App>,
    headers: HeaderMap,
    Path(bid): Path<String>,
) -> Result<Json<Value>, Failure> {
    app.authenticate(key(&headers), false).await?;
    let store = app.store.lock().await;
    let mut data = store.read()?;
    if !model::hex(&bid, 32) || !data.batches.contains_key(&bid) {
        return Err(Failure(404, "batch_missing", "批量测试不存在"));
    }
    for job in &mut data.jobs {
        if job.batch_id == bid
            && job.started_at.is_none()
            && matches!(job.status.as_str(), "queued" | "needs_confirmation")
        {
            job.change("cancelled", "已停止，未派发测试");
        }
    }
    store.write(&data)?;
    Ok(Json(data.listing(&bid)?))
}

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.into())
}
fn loopback(url: &str) -> bool {
    reqwest::Url::parse(url).ok().is_some_and(|u| {
        u.query().is_none()
            && u.fragment().is_none()
            && matches!(
                u.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
            )
    })
}
async fn serve() -> Result<(), Failure> {
    let database = env("PROBE_DATABASE_URL", "");
    let upstream = env("PROBE_UPSTREAM", "").trim_end_matches('/').to_string();
    let dir = PathBuf::from(env("PROBE_STATE_DIR", ""));
    if env("SUB2OPS_PROBE_ISOLATED", "") != "fixture-only"
        || !loopback(&database)
        || !reqwest::Url::parse(&database)
            .is_ok_and(|url| url.path().starts_with("/probe_fixture_"))
        || !loopback(&upstream)
        || !dir.is_absolute()
        || dir == std::path::Path::new("/data")
    {
        return Err(Failure(
            500,
            "isolation_required",
            "必须使用独立 fixture 配置和 loopback 服务",
        ));
    }
    let model = env("PROBE_MODEL", "fixture-probe-model");
    if !runner::valid_model(&model) {
        return Err(Failure(500, "invalid_model", "测活模型配置无效"));
    }
    let timeout = env("PROBE_TIMEOUT_SECONDS", "90")
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n > 0.)
        .unwrap_or(90.);
    let app = Arc::new(Shared {
        store: Mutex::new(Store::open(&dir)?),
        db: PgPoolOptions::new()
            .min_connections(1)
            .max_connections(8)
            .connect(&database)
            .await?,
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs_f64(timeout))
            .build()
            .map_err(|_| Failure(500, "client_failed", "无法启动请求客户端"))?,
        upstream,
        model,
        admin_key: env("PROBE_ADMIN_KEY", "fixture-admin-connection-039"),
        concurrency: env("PROBE_CONCURRENCY", "2")
            .parse::<usize>()
            .unwrap_or(2)
            .clamp(1, 8),
        auth: Mutex::new(HashMap::new()),
        keys: Mutex::new(HashMap::new()),
    });
    let route = Router::new()
        .route("/healthz", get(health))
        .route("/api/desktop/v1/accounts", get(accounts))
        .route("/api/desktop/v1/account-operations/batch", post(submit))
        .route("/api/desktop/v1/account-operations", get(listing))
        .route(
            "/api/desktop/v1/account-operations/batches/{bid}/cancel",
            post(cancel),
        )
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(app.clone());
    let port = env("PROBE_PORT", "18191")
        .parse::<u16>()
        .map_err(|_| Failure(500, "invalid_port", "端口无效"))?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let worker = tokio::spawn(runner::queue(app.clone()));
    axum::serve(listener, route)
        .with_graceful_shutdown(async {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("signal handler");
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
        })
        .await?;
    worker.abort();
    let _ = worker.await;
    app.db.close().await;
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = serve().await {
        eprintln!("{}: {}", error.1, error.2);
        std::process::exit(1);
    }
}
