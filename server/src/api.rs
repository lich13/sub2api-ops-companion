use crate::{
    compat::string,
    error::{Error, Result},
    runtime::Runtime,
    validation,
};
mod dispatch;
mod models;
mod query;
use axum::{
    body::{to_bytes, Body},
    extract::{Query, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{from_fn, Next},
    response::{IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
use futures_util::FutureExt;
use query::QueryParams;
use serde_json::{json, Value};
use std::{collections::BTreeMap, panic::AssertUnwindSafe, sync::Arc};
use sub2ops_protocol::routes::{RequestBody, Route};
use tracing::Instrument;

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
        .fallback(|| async { Error::new(404, "Not Found") })
        .layer(from_fn(desktop_response))
        .with_state(runtime)
}

async fn desktop_response(request: Request, next: Next) -> Response {
    let desktop = request.uri().path().starts_with("/api/desktop/v1/");
    if !desktop {
        return next.run(request).await;
    }
    let request_id = uuid::Uuid::new_v4().simple().to_string();
    async {
        let mut response = AssertUnwindSafe(next.run(request))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Error::internal().into_response());
        if let Some(failure) = response.extensions().get::<crate::error::InternalFailure>() {
            // Neither the URL nor the nested error is safe to include in logs.
            tracing::error!("desktop_failure");
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
        response
    }
    .instrument(tracing::info_span!("desktop", request_id = %request_id))
    .await
}

fn accepts_gzip(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .any(|part| {
            let mut segs = part.trim().split(';');
            let coding = segs.next().unwrap_or("").trim();
            if !coding.eq_ignore_ascii_case("gzip") {
                return false;
            }
            !segs.any(|param| {
                let p = param.trim();
                p.strip_prefix("q=")
                    .or_else(|| p.strip_prefix("Q="))
                    .and_then(|q| q.trim().parse::<f32>().ok())
                    .is_some_and(|q| q <= 0.0)
            })
        })
}

async fn health(State(runtime): State<Arc<Runtime>>) -> Result<Json<Value>> {
    runtime.db.one("SELECT 1 AS ok", &[]).await?;
    Ok(Json(json!({"status":"ok"})))
}
async fn desktop(State(runtime): State<Arc<Runtime>>, request: Request) -> Result<Response> {
    let uri = request.uri().clone();
    let path = uri
        .path()
        .strip_prefix("/api/desktop/v1/")
        .unwrap_or("")
        .trim_end_matches('/')
        .split('/')
        .collect::<Vec<_>>();
    let route = Route::resolve(&path).ok_or_else(|| Error::new(404, "Not Found"))?;
    let method = request.method().clone();
    let Some(endpoint) = route.endpoint(method.as_str()) else {
        let allow = route
            .endpoints()
            .iter()
            .map(|e| e.method)
            .collect::<Vec<_>>()
            .join(", ");
        let mut response = Error::new(405, "Method Not Allowed").into_response();
        response.headers_mut().insert(
            header::ALLOW,
            HeaderValue::from_str(&allow).expect("static method list"),
        );
        return Ok(response);
    };
    let route =
        route.parse_numbers(|id| validation::coercible_integer(&json!(id), i64::MIN, i64::MAX))?;
    let query = QueryParams::parse(&request, route)?;
    let accepts_gzip = accepts_gzip(request.headers());
    let if_none_match = request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let key = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let mut payload = match endpoint.body {
        RequestBody::None => Value::Null,
        RequestBody::CancelOrder => json_payload_optional(request.into_body(), true).await?,
        _ => json_payload(request.into_body()).await?,
    };
    crate::request_schema::validate(endpoint.body, &path, method.as_str(), &mut payload)?;
    let call = dispatch::Call {
        runtime: &runtime,
        key: &key,
        path: &path,
        query: &query,
        method: &method,
        payload: &payload,
        accepts_gzip,
        if_none_match: if_none_match.as_deref(),
    };
    let action = call.prepare(route)?;
    if route == Route::ConnectionStatus {
        if let Err(error) = runtime.authenticate(&key, true).await {
            if error.detail["connection"].is_object() {
                return Ok(Json(error.detail["connection"].clone()).into_response());
            }
            return Err(error);
        }
    } else if !(route == Route::HostOperation && runtime.sub2api_config.owns_operation(&key)) {
        runtime.authenticate(&key, method != Method::GET).await?;
    }
    if method != Method::GET {
        runtime.writable()?;
    }
    let response = action.await?;
    let mutates_snapshot = method != Method::GET
        && !matches!(
            route,
            Route::ProbeProxy(_, _)
                | Route::CreationAuth(_, _)
                | Route::CreationCheck
                | Route::CreationModels
                | Route::CreationGroupPreview
                | Route::BarkTest
                | Route::CheckUpdates
                | Route::ValidateHost
        );
    if mutates_snapshot {
        runtime.invalidate_snapshot();
    }
    Ok(response)
}

#[cfg(test)]
mod response_tests {
    use super::*;
    use std::convert::Infallible;

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
            assert_eq!(logs.matches("desktop_failure").count(), 2);
            assert!(logs.contains("Sub2Ops internal task failed"));
            assert!(logs.contains("api.rs") && logs.contains("line="));
            assert!(logs.contains("request_id=") && logs.contains("category="));
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
