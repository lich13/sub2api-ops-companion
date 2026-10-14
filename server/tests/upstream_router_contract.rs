use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use sub2ops_server::{settings::Settings, upstream::Upstream};
use tokio::{net::TcpListener, task::JoinHandle};

#[derive(Clone)]
struct Mock {
    reply: Arc<Mutex<(u16, Value)>>,
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

struct Server {
    mock: Mock,
    url: String,
    task: JoinHandle<()>,
}

impl Server {
    async fn start(status: u16, body: Value) -> Self {
        let mock = Mock {
            reply: Arc::new(Mutex::new((status, body))),
            calls: Arc::new(Mutex::new(vec![])),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(handler).with_state(mock.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { mock, url, task }
    }

    fn count(&self) -> usize {
        self.mock.calls.lock().unwrap().len()
    }

    fn reply(&self, status: u16, body: Value) {
        *self.mock.reply.lock().unwrap() = (status, body);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handler(State(mock): State<Mock>, request: Request) -> Response {
    mock.calls.lock().unwrap().push((
        request.uri().path().to_owned(),
        request
            .headers()
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned(),
    ));
    let (status, body) = mock.reply.lock().unwrap().clone();
    (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
}

fn router(root: &std::path::Path, verify: &str, public: &str) -> Upstream {
    let mut env = BTreeMap::from([
        (
            "DATABASE_URL".into(),
            "postgresql://example.invalid/fixture".into(),
        ),
        ("SUB2API_VERIFY_BASE_URL".into(), verify.into()),
        ("SUB2API_BASE_URL".into(), public.into()),
    ]);
    for name in [
        "OAUTH_CONFIG_PATH",
        "BARK_CONFIG_PATH",
        "SUB2API_CONFIG_PATH",
    ] {
        env.insert(name.into(), root.join(name).to_string_lossy().into_owned());
    }
    Upstream::new(Arc::new(Settings::from_env(&env, false).unwrap())).unwrap()
}

// These tests port every assertion of the pinned 0.1.39 router suite through
// real loopback HTTP. Cache invalidation is observed by the next request count.
#[tokio::test]
async fn legacy_v0214_envelope_uses_admin_api_key_and_sets_runtime_route() {
    let server = Server::start(200, json!({"code":0,"data":[]})).await;
    let dir = tempfile::tempdir().unwrap();
    let router = router(dir.path(), &server.url, "");
    let result = router
        .authenticate("fixture-admin-key", true)
        .await
        .unwrap();
    assert_eq!(result.state, "ok");
    assert_eq!(router.active_url(), server.url);
    assert_eq!(router.status()["endpoint"], "verify");
    assert_eq!(
        *server.mock.calls.lock().unwrap(),
        vec![(
            "/api/v1/admin/groups/all".into(),
            "fixture-admin-key".into()
        )]
    );
}

#[tokio::test]
async fn legacy_404_falls_back_to_public_route_but_auth_failure_does_not() {
    let verify = Server::start(404, json!({"message":"not found"})).await;
    let public = Server::start(200, json!({"code":0,"data":[]})).await;
    let dir = tempfile::tempdir().unwrap();
    let first = router(dir.path(), &verify.url, &public.url);
    first.authenticate("fixture-admin-key", true).await.unwrap();
    assert_eq!(first.active_url(), public.url);
    assert_eq!(first.status()["endpoint"], "public");
    first
        .authenticate("fixture-admin-key", false)
        .await
        .unwrap();
    assert_eq!((verify.count(), public.count()), (1, 1));

    let verify2 = Server::start(403, json!({"message":"forbidden"})).await;
    let public2 = Server::start(200, json!({"code":0,"data":[]})).await;
    let second = router(dir.path(), &verify2.url, &public2.url);
    let error = second
        .authenticate("fixture-admin-key", true)
        .await
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::UNAUTHORIZED);
    assert_eq!(second.status()["state"], "auth_rejected");
    assert_eq!(public2.count(), 0);
}

#[tokio::test]
async fn legacy_protocol_mismatch_is_not_hidden_by_a_second_endpoint() {
    let verify = Server::start(200, json!({"code":0,"data":{}})).await;
    let public = Server::start(200, json!({"code":0,"data":[]})).await;
    let dir = tempfile::tempdir().unwrap();
    let router = router(dir.path(), &verify.url, &public.url);
    assert!(router
        .authenticate("fixture-admin-key", true)
        .await
        .is_err());
    assert_eq!(router.status()["state"], "protocol_mismatch");
    assert_eq!(public.count(), 0);
}

#[tokio::test]
async fn legacy_explicit_upstream_error_does_not_try_public_endpoint() {
    let verify = Server::start(200, json!({"code":1,"message":"fixture error"})).await;
    let public = Server::start(200, json!({"code":0,"data":[]})).await;
    let dir = tempfile::tempdir().unwrap();
    let router = router(dir.path(), &verify.url, &public.url);
    assert!(router.authenticate("fixture-key", true).await.is_err());
    assert_eq!(router.status()["state"], "upstream_error");
    assert_eq!(public.count(), 0);
}

#[test]
fn legacy_connection_status_has_no_address_or_key() {
    let dir = tempfile::tempdir().unwrap();
    let router = router(dir.path(), "", "");
    let status = router.status();
    assert_eq!(status["state"], "network_unreachable");
    let rendered = status.to_string().to_lowercase();
    assert!(!rendered.contains("key"));
    assert!(!rendered.contains("address"));
}

#[tokio::test]
async fn legacy_retryable_failures_back_off_but_manual_check_can_bypass() {
    // An HTTP 503 has the same retryable boundary as a failed socket, while
    // allowing the test to count each probe without a timing-dependent port.
    let verify = Server::start(503, json!({"message":"unavailable"})).await;
    let dir = tempfile::tempdir().unwrap();
    let router = router(dir.path(), &verify.url, "");
    assert!(router
        .authenticate("fixture-admin-key", false)
        .await
        .is_err());
    assert!(router
        .authenticate("fixture-admin-key", false)
        .await
        .is_err());
    assert_eq!(verify.count(), 1);
    assert!(router
        .authenticate("fixture-admin-key", true)
        .await
        .is_err());
    assert_eq!(verify.count(), 2);
}

#[tokio::test]
async fn legacy_invalid_key_clears_cached_success_and_does_not_report_ok() {
    let verify = Server::start(200, json!({"code":0,"data":[]})).await;
    let dir = tempfile::tempdir().unwrap();
    let router = router(dir.path(), &verify.url, "");
    router.authenticate("fixture-key", false).await.unwrap();
    assert!(router.authenticate("", false).await.is_err());
    assert_eq!(router.status()["state"], "auth_rejected");
    assert_eq!(verify.count(), 1);
    router.authenticate("fixture-key", false).await.unwrap();
    assert_eq!(
        verify.count(),
        2,
        "invalid key must clear the success cache"
    );
}

#[tokio::test]
async fn legacy_network_fallback_and_legacy_array_are_supported() {
    // Keep an ephemeral port reserved but not listening, so it cannot be
    // claimed by another service while the connection-refused probe runs.
    let unavailable = tokio::net::TcpSocket::new_v4().unwrap();
    unavailable.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let verify = format!("http://{}", unavailable.local_addr().unwrap());
    let public = Server::start(200, json!([])).await;
    let dir = tempfile::tempdir().unwrap();
    let router = router(dir.path(), &verify, &public.url);
    assert_eq!(
        router
            .authenticate("fixture-key", false)
            .await
            .unwrap()
            .state,
        "ok"
    );
    assert_eq!(router.active_url(), public.url);
    assert_eq!(public.count(), 1);
}

#[tokio::test]
async fn legacy_concurrent_reads_share_probe_and_rejected_auth_drops_cache() {
    let verify = Server::start(200, json!({"code":0,"data":[]})).await;
    let dir = tempfile::tempdir().unwrap();
    let router = router(dir.path(), &verify.url, "");
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let router = router.clone();
        tasks.spawn(async move {
            router
                .authenticate("fixture-key", false)
                .await
                .unwrap()
                .state
        });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap(), "ok");
    }
    assert_eq!(verify.count(), 1);
    verify.reply(401, json!({"message":"forbidden"}));
    assert!(router.authenticate("fixture-key", true).await.is_err());
    assert_eq!(router.status()["state"], "auth_rejected");
    verify.reply(200, json!({"code":0,"data":[]}));
    router.authenticate("fixture-key", false).await.unwrap();
    assert_eq!(
        verify.count(),
        3,
        "rejected fresh probe must clear the success cache"
    );
}

#[tokio::test]
async fn legacy_envelope_code_requires_integer_zero() {
    let verify = Server::start(200, json!({"code":false,"data":[]})).await;
    let dir = tempfile::tempdir().unwrap();
    let router = router(dir.path(), &verify.url, "");
    for code in [json!(false), json!("0"), json!(0.0)] {
        verify.reply(200, json!({"code":code,"data":[]}));
        assert!(router.authenticate("fixture-key", true).await.is_err());
        assert_eq!(router.status()["state"], "protocol_mismatch");
    }
    assert_eq!(verify.count(), 3);
}
