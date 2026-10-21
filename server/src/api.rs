use crate::{
    compat::string,
    error::{Error, Result},
    runtime::Runtime,
    validation,
};
use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{Query, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{from_fn, Next},
    response::{IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
use futures_util::FutureExt;
use serde_json::{json, Value};
use std::{collections::BTreeMap, convert::Infallible, panic::AssertUnwindSafe, sync::Arc};

// Requests can carry two existing 16 MiB media fields. Desktop response limits
// are independent of this inbound JSON limit.
const MAX_REQUEST_BODY_BYTES: usize = 36 * 1024 * 1024;

async fn json_payload(body: Body) -> Result<Value> {
    json_payload_optional(body, false).await
}

async fn json_payload_optional(body: Body, empty_object: bool) -> Result<Value> {
    let raw = to_bytes(body, MAX_REQUEST_BODY_BYTES)
        .await
        .map_err(|_| Error::invalid())?;
    if empty_object && raw.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&raw).map_err(|_| Error::invalid())
}

pub fn router(runtime: Arc<Runtime>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/api/desktop/v1/{*path}", any(desktop))
        .route("/v1/models", get(catalog))
        .route("/models", get(catalog))
        .route("/backend-api/codex/models", get(catalog))
        .fallback(|| async { Error::new(404, "Not Found") })
        .layer(from_fn(desktop_response))
        .with_state(runtime)
}

async fn desktop_response(request: Request, next: Next) -> Response {
    let desktop = request.uri().path().starts_with("/api/desktop/v1/");
    let mut response = if desktop {
        AssertUnwindSafe(next.run(request))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Error::internal().into_response())
    } else {
        next.run(request).await
    };
    if desktop {
        let request_id = uuid::Uuid::new_v4().simple().to_string();
        if let Some(failure) = response.extensions().get::<crate::error::InternalFailure>() {
            // Neither the URL nor the nested error is safe to include in logs.
            eprintln!("desktop_failure request_id={request_id}");
            let mut detail = failure.0.clone();
            detail["request_id"] = request_id.clone().into();
            let (mut parts, _) = response.into_parts();
            parts.headers.remove(header::CONTENT_LENGTH);
            response = Response::from_parts(
                parts,
                Json(json!({"detail":detail})).into_response().into_body(),
            );
        }
        response.headers_mut().insert(
            "x-request-id",
            HeaderValue::from_str(&request_id).expect("UUID is a valid header value"),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

async fn health(State(runtime): State<Arc<Runtime>>) -> Result<Json<Value>> {
    runtime.db.one("SELECT 1 AS ok", &[]).await?;
    Ok(Json(json!({"status":"ok"})))
}
fn params(request: &Request) -> Result<BTreeMap<String, String>> {
    Query::<BTreeMap<String, String>>::try_from_uri(request.uri())
        .map(|v| v.0)
        .map_err(|_| Error::invalid())
}
async fn catalog(State(runtime): State<Arc<Runtime>>, request: Request) -> Result<Response> {
    let query = params(&request)?;
    let path = request.uri().path();
    if path != "/backend-api/codex/models"
        && query.get("client_version").is_none_or(|v| v.is_empty())
    {
        return Err(Error::new(404, "Not Found"));
    }
    let headers = request
        .headers()
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_owned(), v.to_owned()))
        })
        .collect();
    let reply = runtime
        .catalog
        .proxy(path, request.uri().query().unwrap_or(""), &headers)
        .await;
    let mut response = Response::builder().status(reply.status);
    for (key, value) in reply.headers {
        response = response.header(key, value);
    }
    response
        .body(Body::from(reply.body))
        .map_err(|_| Error::internal())
}
fn path_id(value: &str) -> Result<i64> {
    validation::coercible_integer(&json!(value), i64::MIN, i64::MAX)
}
fn number(query: &BTreeMap<String, String>, name: &str, min: i64, max: i64) -> Result<Option<i64>> {
    query
        .get(name)
        .map(|v| validation::coercible_integer(&json!(v), min, max))
        .transpose()
}
fn boolean(query: &BTreeMap<String, String>, name: &str) -> Result<bool> {
    query
        .get(name)
        .map(|value| validation::coercible_boolean(&json!(value)))
        .transpose()
        .map(|value| value.unwrap_or(false))
}
fn get_string<'a>(query: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    query.get(name).map(String::as_str)
}
fn category(query: &BTreeMap<String, String>) -> Result<Option<&str>> {
    match get_string(query, "category") {
        None => Ok(None),
        Some(value @ ("degradation" | "other")) => Ok(Some(value)),
        _ => Err(Error::new(422, "错误分类无效")),
    }
}
fn version(payload: &Value, key: &str) -> Result<()> {
    validation::text(&payload[key], 64, 64).map(|_| ())
}
fn request_payload(payload: &Value, allowed: &[&str], required: &[&str]) -> Result<()> {
    validation::fields(payload, allowed, required)
}

fn allowed(path: &[&str]) -> Option<&'static [&'static str]> {
    match path {
        ["capabilities"
        | "connection-status"
        | "snapshot"
        | "account-priorities"
        | "detection-logs"
        | "errors"
        | "usage-records"
        | "usage-record-options"
        | "recoveries"
        | "proxies"
        | "config"
        | "model-groups"] => Some(&["GET"]),
        ["account-operations"] => Some(&["GET"]),
        ["account-priority-orders"] => Some(&["POST"]),
        ["account-priority-orders", _] => Some(&["GET"]),
        ["account-priority-orders", _, "cancel"] => Some(&["POST"]),
        ["account-operations", "batch"] => Some(&["POST"]),
        ["account-operations", _] => Some(&["GET"]),
        ["account-operations", _, "cancel"] | ["account-operations", "batches", _, "cancel"] => {
            Some(&["POST"])
        }
        ["accounts", _] => Some(&["DELETE"]),
        ["accounts", _, "settings" | "models"] | ["accounts", _, "model-tests", "latest"] => {
            Some(&["GET"])
        }
        ["accounts", _, "operations" | "schedulable" | "recover-state" | "priority" | "test" | "model-tests"
        | "usage-action"] => Some(&["POST"]),
        ["accounts", _, "groups" | "degradation-mark"] => Some(&["PUT"]),
        ["accounts", _, "model-detection" | "keepalive"] => Some(&["GET", "PUT"]),
        ["errors" | "usage-records", _] => Some(&["GET"]),
        ["proxies", _, "test" | "quality-check"] => Some(&["POST"]),
        ["account-templates"] => Some(&["GET", "PUT", "POST"]),
        ["account-templates", "apply" | "custom"] => Some(&["POST"]),
        ["account-templates", _] | ["account-templates", "custom", _] => Some(&["PUT", "DELETE"]),
        ["modeltrace", "fingerprint-bank"] => Some(&["GET"]),
        ["modeltrace", "fingerprint-bank", "sync"] => Some(&["POST"]),
        ["model-tests", _] => Some(&["GET"]),
        ["model-tests", _, "cancel" | "retry"] => Some(&["POST"]),
        ["quota-refresh"] => Some(&["GET", "POST"]),
        ["config", "oauth" | "bark" | "key_fallback"] => Some(&["PUT"]),
        ["actions", "bark-test"] => Some(&["POST"]),
        ["model-groups", _, "reasoning"] => Some(&["GET", "PUT", "DELETE"]),
        ["model-groups", _, "reasoning", "resolve"] => Some(&["POST"]),
        _ => None,
    }
}
async fn desktop(State(runtime): State<Arc<Runtime>>, request: Request) -> Result<Response> {
    let route = request
        .uri()
        .path()
        .strip_prefix("/api/desktop/v1/")
        .unwrap_or("")
        .trim_end_matches('/')
        .to_owned();
    let path = route.split('/').collect::<Vec<_>>();
    let method = request.method().clone();
    let allowed = allowed(&path).ok_or_else(|| Error::new(404, "Not Found"))?;
    if !allowed.contains(&method.as_str()) {
        let mut response = Error::new(405, "Method Not Allowed").into_response();
        response.headers_mut().insert(
            header::ALLOW,
            HeaderValue::from_str(&allowed.join(", ")).expect("static method list"),
        );
        return Ok(response);
    }
    let query = params(&request)?;
    let key = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    // Match FastAPI's parameter validation before entering an authenticated
    // handler. This checks types only; domain reads and writes still need auth.
    match path.as_slice() {
        ["accounts" | "proxies" | "model-groups", id, ..] | ["errors" | "usage-records", id] => {
            validation::coercible_integer(&json!(id), i64::MIN, i64::MAX)?;
        }
        _ => {}
    }
    let integer_queries: &[&str] = match path.as_slice() {
        ["errors"] => &["account_id", "before_id", "limit"],
        ["recoveries"] => &["before_id", "limit"],
        ["detection-logs"] => &["limit"],
        ["usage-records"] => &["account_id", "api_key_id", "user_id", "after_id", "limit"],
        ["usage-record-options"] => &["user_id", "limit"],
        ["account-templates"] => &["account_id"],
        ["account-operations", _] if method == Method::GET => &["after_event"],
        _ => &[],
    };
    for name in integer_queries {
        number(&query, name, i64::MIN, i64::MAX)?;
    }
    if path == ["usage-records"] {
        boolean(&query, "include_summary")?;
        boolean(&query, "mismatch_only")?;
    }
    if path == ["usage-record-options"] && !query.contains_key("kind") {
        return Err(Error::invalid());
    }
    if matches!(path.as_slice(), ["accounts", _, "models"])
        && query
            .get("purpose")
            .is_some_and(|value| value != "model_test")
    {
        return Err(Error::invalid());
    }
    let mut payload = json!({});
    if crate::request_schema::has_body(&path, method.as_str()) {
        payload = if matches!(path.as_slice(), ["account-priority-orders", _, "cancel"]) {
            json_payload_optional(request.into_body(), true).await?
        } else {
            json_payload(request.into_body()).await?
        };
        crate::request_schema::validate(&path, method.as_str(), &mut payload)?;
    }
    if path == ["connection-status"] {
        if let Err(error) = runtime.authenticate(&key, true).await {
            if error.detail["connection"].is_object() {
                return Ok(Json(error.detail["connection"].clone()).into_response());
            }
            return Err(error);
        }
        return Ok(Json(runtime.upstream.status()).into_response());
    }
    runtime.authenticate(&key, method != Method::GET).await?;
    if method != Method::GET {
        runtime.writable()?;
    }
    let mut status = StatusCode::OK;
    let result: Value = match path.as_slice() {
        ["capabilities"] => {
            json!({"api_version":1,"service":"Sub2API Ops Companion","platforms":["macos","android"],"version":crate::VERSION,
            "upstream_connection":runtime.upstream.status(),"implementation":"rust","build_revision":crate::BUILD_REVISION,"features":{"detection_logs":true,"global_detection_model":true,"account_priority_ordering":true}})
        }
        ["snapshot"] => runtime.snapshot().await?,
        ["account-priorities"] => runtime.priorities.view().await?,
        ["account-priority-orders"] => runtime.priorities.submit(&payload).await?,
        ["account-priority-orders", id] => runtime.priorities.get(id)?,
        ["account-priority-orders", id, "cancel"] => runtime.priorities.cancel(id).await?,
        ["errors"] => {
            runtime
                .actions
                .queries
                .errors(
                    number(&query, "account_id", 1, i64::MAX)?,
                    number(&query, "before_id", 1, i64::MAX)?,
                    number(&query, "limit", 1, 100)?.unwrap_or(50) as usize,
                    category(&query)?,
                )
                .await?
        }
        ["errors", id] => {
            runtime
                .error_detail(path_id(id)?, category(&query)?)
                .await?
        }
        ["detection-logs"] => runtime.detection.logs(
            get_string(&query, "before"),
            number(&query, "limit", 1, 100)?.unwrap_or(50) as usize,
        )?,
        ["recoveries"] => {
            runtime
                .actions
                .queries
                .recoveries(
                    number(&query, "before_id", 1, i64::MAX)?,
                    number(&query, "limit", 1, 100)?.unwrap_or(50) as usize,
                )
                .await?
        }
        ["usage-records"] => {
            let filter = crate::usage_records::Filter {
                from_at: query.get("from_at").cloned(),
                to_at: query.get("to_at").cloned(),
                start_date: query.get("start_date").cloned(),
                end_date: query.get("end_date").cloned(),
                include_summary: boolean(&query, "include_summary")?,
                account_id: number(&query, "account_id", i64::MIN, i64::MAX)?,
                api_key_id: number(&query, "api_key_id", i64::MIN, i64::MAX)?,
                user_id: number(&query, "user_id", i64::MIN, i64::MAX)?,
                model: query.get("model").cloned(),
                request_type: query.get("request_type").cloned(),
                mismatch_only: boolean(&query, "mismatch_only")?,
                cursor: query.get("cursor").cloned(),
                after_id: number(&query, "after_id", i64::MIN, i64::MAX)?,
                limit: number(&query, "limit", 1, 100)?.map(|n| n as usize),
            };
            crate::usage_records::UsageRecords(runtime.db.clone())
                .list_at(&filter, runtime.monitor.now())
                .await?
        }
        ["usage-record-options"] => {
            crate::usage_records::UsageRecords(runtime.db.clone())
                .options(
                    get_string(&query, "kind").ok_or_else(Error::invalid)?,
                    get_string(&query, "q"),
                    number(&query, "user_id", 1, i64::MAX)?,
                    get_string(&query, "cursor"),
                    number(&query, "limit", 1, 100)?.unwrap_or(50) as usize,
                )
                .await?
        }
        ["usage-records", id] => {
            crate::usage_records::UsageRecords(runtime.db.clone())
                .detail(path_id(id)?)
                .await?
        }
        ["account-operations"] => runtime.operations.listing(get_string(&query, "batch_id"))?,
        ["account-operations", "batch"] => {
            status = StatusCode::ACCEPTED;
            runtime
                .operations
                .submit_batch(&payload, &key, false)
                .await?
        }
        ["account-operations", "batches", id, "cancel"] => {
            runtime.operations.cancel_batch(id).await?
        }
        ["account-operations", id] => runtime.operations.get(
            id,
            number(&query, "after_event", 0, i64::MAX)?.unwrap_or(0) as usize,
        )?,
        ["account-operations", id, "cancel"] => runtime.operations.cancel(id).await?,
        ["accounts", id, "operations"] => {
            status = StatusCode::ACCEPTED;
            runtime
                .operations
                .submit(path_id(id)?, &payload, &key)
                .await?
        }
        ["accounts", id, "schedulable"] => {
            request_payload(
                &payload,
                &["schedulable", "expected_version", "detach_managed"],
                &["schedulable", "expected_version"],
            )?;
            version(&payload, "expected_version")?;
            validation::boolean(&payload["schedulable"])?;
            if let Some(v) = payload.get("detach_managed") {
                validation::boolean(v)?;
            }
            runtime
                .set_schedulable(path_id(id)?, &payload, &key)
                .await?
        }
        ["accounts", id] => {
            request_payload(
                &payload,
                &["expected_version", "detach_managed"],
                &["expected_version"],
            )?;
            version(&payload, "expected_version")?;
            if let Some(v) = payload.get("detach_managed") {
                validation::boolean(v)?;
            }
            runtime.delete_account(path_id(id)?, &payload, &key).await?
        }
        ["accounts", id, "recover-state"] => {
            request_payload(&payload, &["expected_version"], &["expected_version"])?;
            version(&payload, "expected_version")?;
            runtime
                .recover_account(path_id(id)?, &payload, &key)
                .await?
        }
        ["accounts", id, "priority"] => {
            request_payload(
                &payload,
                &["expected_version", "priority"],
                &["expected_version", "priority"],
            )?;
            version(&payload, "expected_version")?;
            let values = validation::operation_payload("priority", &payload)?;
            let id = path_id(id)?;
            let row = runtime
                .actions
                .account(id, Some(string(&payload["expected_version"])))
                .await?;
            let versions = crate::compat::operation_versions(&row, false);
            runtime
                .actions
                .apply_setting(id, "priority", &values, string(&versions["priority"]), &key)
                .await?
        }
        ["accounts", id, "groups"] => {
            request_payload(
                &payload,
                &["expected_version", "group_ids", "scope_group_ids"],
                &["expected_version", "group_ids", "scope_group_ids"],
            )?;
            version(&payload, "expected_version")?;
            validation::operation_payload("groups", &payload)?;
            runtime
                .actions
                .set_groups(path_id(id)?, &payload, &key)
                .await?
        }
        ["accounts", id, "degradation-mark"] => {
            request_payload(
                &payload,
                &["marked", "expected_mark_version"],
                &["marked", "expected_mark_version"],
            )?;
            version(&payload, "expected_mark_version")?;
            validation::boolean(&payload["marked"])?;
            runtime.set_degradation(path_id(id)?, &payload).await?
        }
        ["accounts", id, "settings"] => runtime.actions.settings_view(path_id(id)?).await?,
        ["proxies"] => json!(runtime.actions.proxies(&key).await?),
        ["proxies", id, action] => runtime.proxies.run(&key, path_id(id)?, action).await?,
        ["accounts", id, "models"] => {
            let purpose = get_string(&query, "purpose");
            if purpose.is_some_and(|value| value != "model_test") {
                return Err(Error::new(422, "模型目录用途无效"));
            }
            json!(
                runtime
                    .actions
                    .models(path_id(id)?, &key, purpose == Some("model_test"))
                    .await?
            )
        }
        ["account-templates"] if method == Method::GET => {
            let row = if let Some(id) = number(&query, "account_id", 1, i64::MAX)? {
                Some(runtime.actions.account(id, None).await?)
            } else {
                None
            };
            runtime.actions.templates.view(row.as_ref())?
        }
        ["account-templates"] if method == Method::PUT => {
            request_payload(
                &payload,
                &["expected_version", "full", "degraded", "takeover"],
                &["expected_version", "full", "degraded", "takeover"],
            )?;
            version(&payload, "expected_version")?;
            runtime.actions.templates.save(&payload)?
        }
        ["account-templates", "apply"] => {
            status = StatusCode::ACCEPTED;
            runtime
                .operations
                .submit_batch(&payload, &key, true)
                .await?
        }
        ["account-templates"] | ["account-templates", "custom"] => {
            template_request(&mut payload, false)?;
            runtime.actions.templates.edit(None, &payload, false)?
        }
        ["account-templates", id] | ["account-templates", "custom", id] => {
            template_request(&mut payload, method == Method::DELETE)?;
            runtime
                .actions
                .templates
                .edit(Some(id), &payload, method == Method::DELETE)?
        }
        ["accounts", id, "model-detection"] if method == Method::GET => {
            crate::model_target::account(&runtime.actions, path_id(id)?).await?;
            runtime.detection.view(path_id(id)?)?
        }
        ["accounts", id, "keepalive"] if method == Method::GET => {
            runtime.keepalive.view(path_id(id)?).await?
        }
        ["accounts", id, "keepalive"] => runtime.keepalive.save(path_id(id)?, &payload).await?,
        ["accounts", id, "model-detection"] => {
            payload = crate::model_detection::detection_request(&payload)?;
            runtime.detection.save(path_id(id)?, &payload).await?
        }
        ["modeltrace", "fingerprint-bank"] => runtime.tests.bank.public(),
        ["modeltrace", "fingerprint-bank", "sync"] => runtime.tests.bank.request_sync()?,
        ["accounts", id, "test"] => {
            return Ok(test_response(runtime, path_id(id)?, payload, key));
        }
        ["accounts", id, "model-tests"] => {
            runtime
                .tests
                .start(
                    path_id(id)?,
                    &crate::model_tests::model_request(&payload)?,
                    None,
                )
                .await?
        }
        ["accounts", id, "model-tests", "latest"] => runtime.tests.latest(path_id(id)?)?,
        ["model-tests", id] => runtime.tests.get(id)?,
        ["model-tests", id, "cancel"] => runtime.tests.cancel(id).await?,
        ["model-tests", id, "retry"] => {
            request_payload(&payload, &["request_id"], &["request_id"])?;
            runtime
                .tests
                .retry_failed(id, validation::request_id(&payload["request_id"])?)
                .await?
        }
        ["quota-refresh"] if method == Method::GET => runtime.quota_progress(),
        ["quota-refresh"] => runtime.start_quota_refresh(&key).await?,
        ["config"] => runtime.config.snapshot(None)?,
        ["config", section] => {
            request_payload(
                &payload,
                &["expected_revision", "changes"],
                &["expected_revision", "changes"],
            )?;
            version(&payload, "expected_revision")?;
            if !payload["changes"].is_object() {
                return Err(Error::invalid());
            }
            runtime
                .config
                .save(
                    section,
                    &payload["changes"],
                    "desktop:admin",
                    Some(string(&payload["expected_revision"])),
                )
                .await?
        }
        ["accounts", id, "usage-action"] => {
            request_payload(
                &payload,
                &["action", "expected_version", "confirmed"],
                &["action", "expected_version"],
            )?;
            version(&payload, "expected_version")?;
            if let Some(v) = payload.get("confirmed") {
                validation::boolean(v)?;
            }
            if !matches!(
                string(&payload["action"]),
                "query_usage" | "query_reset_credits" | "reset_quota" | "probe_quota"
            ) {
                return Err(Error::invalid());
            }
            runtime
                .usage_action(path_id(id)?, &payload, &key, None)
                .await?
        }
        ["actions", "bark-test"] => {
            let push = runtime
                .config
                .bark
                .push(
                    "Sub2API Ops Companion",
                    "Bark 推送通道正常。",
                    8,
                    &json!({}),
                )
                .await;
            if !push.success {
                return Err(Error::new(
                    502,
                    format!("Bark 测试失败：{}", push.error_code),
                ));
            }
            json!({"message":"Bark 测试消息已发送"})
        }
        ["model-groups"] => runtime.catalog.list_groups().await?,
        ["model-groups", id, "reasoning"] if method == Method::GET => {
            runtime.catalog.read_reasoning(path_id(id)?, &key).await?
        }
        ["model-groups", id, "reasoning", "resolve"] => {
            reasoning_request(&mut payload, "resolve")?;
            runtime
                .catalog
                .resolve_reasoning(path_id(id)?, &key, &payload)
                .await?
        }
        ["model-groups", id, "reasoning"] if method == Method::PUT => {
            reasoning_request(&mut payload, "save")?;
            runtime
                .catalog
                .save_reasoning(path_id(id)?, &key, &payload)
                .await?
        }
        ["model-groups", id, "reasoning"] => {
            reasoning_request(&mut payload, "delete")?;
            runtime
                .catalog
                .remove_reasoning(path_id(id)?, &key, &payload)
                .await?
        }
        _ => return Err(Error::new(404, "Not Found")),
    };
    if method != Method::GET {
        runtime.invalidate().await;
    }
    let mut result = result;
    if path.first() == Some(&"model-groups") {
        if let Some(object) = result.as_object_mut() {
            object.retain(|key, _| !key.starts_with('_'));
        }
    }
    Ok((status, Json(result)).into_response())
}
fn template_request(payload: &mut Value, delete: bool) -> Result<()> {
    if delete {
        request_payload(payload, &["expected_version"], &["expected_version"])?;
    } else {
        request_payload(
            payload,
            &["expected_version", "name", "whitelist", "mappings"],
            &["expected_version", "name"],
        )?;
        validation::text(&payload["name"], 1, 40)?;
        for key in ["whitelist", "mappings"] {
            if payload.get(key).is_none() {
                payload[key] = json!([]);
            }
        }
        crate::account_templates::profile(
            &json!({"whitelist":payload["whitelist"],"mappings":payload["mappings"]}),
        )?;
    }
    version(payload, "expected_version")
}
fn reasoning_request(payload: &mut Value, kind: &str) -> Result<()> {
    crate::request_schema::reasoning(payload, kind)
}

enum TestState {
    Prepare(Arc<Runtime>, i64, Value, String),
    Running(
        crate::actions::TestRun,
        tokio::sync::OwnedSemaphorePermit,
        crate::execution::TestPermit,
    ),
    Done,
}
fn test_response(runtime: Arc<Runtime>, id: i64, payload: Value, key: String) -> Response {
    let stream = futures_util::stream::unfold(
        TestState::Prepare(runtime, id, payload, key),
        |state| async move {
            let state = match state {
                TestState::Prepare(runtime, id, payload, key) => {
                    let prepared = async {
                        let permit = runtime
                            .budget
                            .clone()
                            .try_acquire_owned()
                            .map_err(|_| crate::account_locks::busy())?;
                        let slot = runtime
                            .slots
                            .acquire()
                            .ok_or_else(crate::account_locks::busy)?;
                        let lease = runtime.actions.prepare_test(id, &payload).await?;
                        Ok::<_, Error>((
                            runtime.actions.test_stream(id, payload, key, lease, false),
                            permit,
                            slot,
                        ))
                    }
                    .await;
                    match prepared {
                        Ok((run, budget, slot)) => TestState::Running(run, budget, slot),
                        Err(error) => {
                            let event = json!({"type":"error","error":crate::security::safe_error(&error.detail,600)});
                            return Some((
                                Ok::<_, Infallible>(Bytes::from(format!("data: {event}\n\n"))),
                                TestState::Done,
                            ));
                        }
                    }
                }
                state => state,
            };
            match state {
                TestState::Running(mut run, budget, slot) => run.recv().await.map(|event| {
                    (
                        Ok(Bytes::from(format!("data: {event}\n\n"))),
                        TestState::Running(run, budget, slot),
                    )
                }),
                _ => None,
            }
        },
    );
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        "content-type",
        "text/event-stream; charset=utf-8".parse().unwrap(),
    );
    response
        .headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    response
        .headers_mut()
        .insert("x-accel-buffering", "no".parse().unwrap());
    response
}

#[cfg(test)]
mod response_tests {
    use super::*;

    #[tokio::test]
    async fn request_limit_accepts_boundary_and_rejects_chunked_overflow() {
        for (length, chunked) in [
            (MAX_REQUEST_BODY_BYTES, false),
            (MAX_REQUEST_BODY_BYTES, true),
            (MAX_REQUEST_BODY_BYTES + 1, false),
            (MAX_REQUEST_BODY_BYTES + 1, true),
        ] {
            let mut bytes = vec![b' '; length];
            bytes[..2].copy_from_slice(b"{}");
            let body = if chunked {
                let bytes = axum::body::Bytes::from(bytes);
                let chunks: Vec<_> = (0..bytes.len())
                    .step_by(65_536)
                    .map(|start| {
                        Ok::<_, Infallible>(bytes.slice(start..(start + 65_536).min(bytes.len())))
                    })
                    .collect();
                Body::from_stream(futures_util::stream::iter(chunks))
            } else {
                Body::from(bytes)
            };
            let result = json_payload(body).await;
            if length == MAX_REQUEST_BODY_BYTES {
                assert_eq!(result.unwrap(), json!({}));
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
                assert_eq!(error.detail["code"], "invalid_request");
            }
        }
    }

    #[tokio::test]
    async fn desktop_failures_are_correlated_private_and_uncacheable() {
        // The subprocess isolates the process-wide panic hook and lets the
        // parent inspect the actual stderr, including caught panic output.
        const CHILD: &str = "SUB2OPS_ERROR_CONTRACT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "api::response_tests::desktop_failures_are_correlated_private_and_uncacheable",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "response contract process failed");
            let logs = String::from_utf8(output.stderr).unwrap();
            assert_eq!(logs.matches("desktop_failure request_id=").count(), 2);
            assert!(logs.contains("Sub2Ops internal task failed"));
            assert!(!logs.contains("fixture-private"));
            assert!(!String::from_utf8(output.stdout)
                .unwrap()
                .contains("fixture-private"));
            return;
        }
        crate::error::install_panic_redaction();
        let routes = Router::new()
            .route(
                "/api/desktop/v1/failure",
                get(|| async { Error::from(std::io::Error::other("fixture-private-error")) }),
            )
            .route(
                "/api/desktop/v1/panic",
                get(|| async {
                    panic!("fixture-private-panic");
                    #[allow(unreachable_code)]
                    Error::internal()
                }),
            )
            .route(
                "/api/desktop/v1/invalid",
                get(|| async { Error::invalid() }),
            )
            .route(
                "/api/desktop/v1/auth",
                get(|| async { Error::new(401, "管理员 API Key 无效") }),
            )
            .route(
                "/api/desktop/v1/ok",
                get(|| async { Json(json!({"ok":true})) }),
            )
            .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
            .fallback(|| async { Error::new(404, "Not Found") })
            .layer(from_fn(desktop_response));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut ids = std::collections::BTreeSet::new();
        for (path, status) in [
            ("failure", 500),
            ("panic", 500),
            ("invalid", 422),
            ("auth", 401),
            ("ok", 200),
            ("missing", 404),
        ] {
            let response = client
                .get(format!(
                    "http://{address}/api/desktop/v1/{path}?key=fixture-private-query"
                ))
                .header("x-request-id", "fixture-private-client-value")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(response.headers()["cache-control"], "no-store");
            let request_id = response.headers()["x-request-id"]
                .to_str()
                .unwrap()
                .to_owned();
            assert_eq!(request_id.len(), 32);
            assert!(request_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert!(
                ids.insert(request_id.clone()),
                "each response needs a distinct correlation ID"
            );
            let body = response.text().await.unwrap();
            assert!(!body.contains("fixture-private"));
            let value: Value = serde_json::from_str(&body).unwrap();
            if status == 500 {
                assert_eq!(value["detail"]["request_id"], request_id);
                assert_eq!(value["detail"]["code"], "internal_error");
            } else if status == 401 {
                assert_eq!(value["detail"], "管理员 API Key 无效");
            } else if status == 422 {
                assert_eq!(value["detail"]["code"], "invalid_request");
            }
        }
        let health = client
            .get(format!("http://{address}/healthz"))
            .send()
            .await
            .unwrap();
        assert!(!health.headers().contains_key("x-request-id"));
        server.abort();
        let _ = server.await;
    }
}
