use axum::{
    body::Body,
    extract::{Request, State},
    response::Response,
    Router,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, Mutex},
};
use sub2ops_server::{
    db::Database, model_config::ModelConfig, runtime::Runtime, settings::Settings,
};
use tokio::{net::TcpListener, task::JoinHandle};

const ADMIN: &str = "fixture-catalog-admin";
const QUERY: &str = "client_version=0.116.0";

fn native() -> Value {
    json!({"models":[{"slug":"fixture-native-model","display_name":"Native","unknown":{"array":[false,0,null]}}],
        "future_envelope":{"native":true}})
}

#[derive(Clone, Default)]
struct Upstream {
    calls: Arc<Mutex<Vec<Value>>>,
    status: Arc<Mutex<Option<u16>>>,
    pages: Arc<Mutex<BTreeMap<String, Value>>>,
}

async fn upstream(State(mock): State<Upstream>, request: Request) -> Response {
    let path = request.uri().path();
    let header = |name| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    mock.calls
        .lock()
        .unwrap()
        .push(json!({"method":request.method().as_str(),
        "path":request.uri().to_string(),"authorization":header("authorization"),
        "x-real-ip":header("x-real-ip"),"x-api-key":header("x-api-key"),
        "if-none-match":header("if-none-match"),"host":header("host"),
        "connection":header("connection"),"transfer-encoding":header("transfer-encoding"),
        "cookie":header("cookie"),"x-unlisted-fixture":header("x-unlisted-fixture"),
        "user-agent":header("user-agent")}));
    let (status, body) = if path.starts_with("/api/v1/admin/") {
        if header("x-api-key") == ADMIN {
            (200, json!({"code":0,"data":[]}))
        } else {
            (401, json!({"code":401}))
        }
    } else {
        let status = mock.status.lock().unwrap().unwrap_or(200);
        (
            status,
            if status == 200 {
                mock.pages
                    .lock()
                    .unwrap()
                    .get(&request.uri().to_string())
                    .cloned()
                    .unwrap_or_else(native)
            } else {
                json!({"error":"fixture-revoked"})
            },
        )
    };
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("etag", "\"native\"")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

struct Harness {
    _root: tempfile::TempDir,
    runtime: Arc<Runtime>,
    owner: Database,
    schema: String,
    mock: Upstream,
    server: JoinHandle<()>,
}

impl Harness {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let database_url = std::env::var("DATABASE_URL").unwrap();
        let owner = Database::open(&database_url, false).await.unwrap();
        let schema = format!("catalog_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&owner.pool)
            .await
            .unwrap();
        let mut url = reqwest::Url::parse(&database_url).unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let seed = Database::open(url.as_str(), false).await.unwrap();
        sqlx::raw_sql(
            include_str!("../../tests/contracts/http/schema.sql")
                .split("-- Every row below")
                .next()
                .unwrap(),
        )
        .execute(&seed.pool)
        .await
        .unwrap();
        sqlx::raw_sql("INSERT INTO groups (id,name,platform) VALUES (7,'fixture-seven','openai'),(8,'fixture-eight','openai');
            INSERT INTO api_keys (id,name,group_id,key) VALUES (7,'fixture-seven',7,'fixture-caller-7'),(8,'fixture-eight',8,'fixture-caller-8');")
            .execute(&seed.pool).await.unwrap();
        seed.close().await;
        let mock = Upstream::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(upstream).with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut env = BTreeMap::from([
            ("DATABASE_URL".into(), url.to_string()),
            ("SUB2API_BASE_URL".into(), base.clone()),
            ("SUB2API_VERIFY_BASE_URL".into(), base),
            ("BASE_PATH".into(), String::new()),
        ]);
        for (key, file) in [
            ("AUDIT_PATH", "audit.jsonl"),
            ("OAUTH_CONFIG_PATH", "oauth.json"),
            ("BARK_CONFIG_PATH", "bark.json"),
            ("KEY_FALLBACK_CONFIG_PATH", "fallback.json"),
            ("USAGE_QUERY_STATE_PATH", "state.json"),
            ("SUB2API_CONFIG_PATH", "connection.json"),
            ("GROUP_MODEL_CONFIG_PATH", "models.json"),
            ("KEEPALIVE_CONFIG_PATH", "keepalive-state.json"),
        ] {
            env.insert(
                key.into(),
                root.path().join(file).to_string_lossy().into_owned(),
            );
        }
        let runtime = Runtime::new(Settings::from_env(&env, false).unwrap())
            .await
            .unwrap();
        Self {
            _root: root,
            runtime,
            owner,
            schema,
            mock,
            server,
        }
    }
    async fn proxy(
        &self,
        caller: i64,
        etag: Option<&str>,
    ) -> sub2ops_server::model_catalog::HttpReply {
        let mut headers = BTreeMap::from([
            (
                "authorization".into(),
                format!("Bearer fixture-caller-{caller}"),
            ),
            ("x-real-ip".into(), "127.0.0.1".into()),
        ]);
        if let Some(etag) = etag {
            headers.insert("if-none-match".into(), etag.into());
        }
        self.runtime
            .catalog
            .proxy("/v1/models", QUERY, &headers)
            .await
    }
    async fn close(self) {
        self.runtime.close().await;
        self.server.abort();
        let _ = self.server.await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.owner.pool)
            .await
            .unwrap();
        self.owner.close().await;
    }
}

async fn desktop_status(
    client: &reqwest::Client,
    base: &str,
    method: reqwest::Method,
    route: &str,
    body: Option<Value>,
) -> reqwest::StatusCode {
    let request = client.request(method, format!("{base}/api/desktop/v1/{route}"));
    let request = if let Some(body) = body {
        request.json(&body)
    } else {
        request
    };
    request.send().await.unwrap().status()
}

#[test]
fn model_store_atomic_permissions_restart_conflict_and_corruption() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("model.json");
    let store = ModelConfig::new(path.clone(), false);
    let initial = store.group(7).unwrap();
    let saved = store
        .save(
            7,
            &json!({"fixture-model":{"display_name":"测试","future":0}}),
            initial["revision"].as_str().unwrap(),
        )
        .unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        ModelConfig::new(path.clone(), false).group(7).unwrap(),
        saved
    );
    let names = fs::read_dir(root.path())
        .unwrap()
        .map(|p| p.unwrap().file_name().into_string().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        ["model.json".to_owned(), "model.json.lock".to_owned()].into()
    );
    assert_eq!(
        store
            .save(7, &json!({}), initial["revision"].as_str().unwrap())
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    fs::write(&path, "{").unwrap();
    assert!(store
        .save(7, &json!({}), saved["revision"].as_str().unwrap())
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{");
}

#[test]
fn independent_model_stores_preserve_concurrent_group_changes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("model.json");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles = (7..=8)
        .map(|id| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let store = ModelConfig::new(path, false);
                let before = store.group(id).unwrap();
                barrier.wait();
                store
                    .save(
                        id,
                        &json!({"fixture-model":{"display_name":format!("group-{id}")}}),
                        before["revision"].as_str().unwrap(),
                    )
                    .unwrap();
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }
    let store = ModelConfig::new(path, false);
    for id in 7..=8 {
        assert_eq!(
            store.group(id).unwrap()["overrides"]["fixture-model"]["display_name"],
            format!("group-{id}")
        );
    }
}

#[tokio::test]
async fn native_auth_on_every_hit_group_isolation_etag_and_forwarded_identity() {
    let h = Harness::new().await;
    let before = h.runtime.catalog.store.group(7).unwrap();
    h.runtime
        .catalog
        .store
        .save(
            7,
            &json!({"fixture-native-model":{"display_name":"Seven"}}),
            before["revision"].as_str().unwrap(),
        )
        .unwrap();
    let first = h.proxy(7, None).await;
    assert_eq!(first.status, 200);
    let body: Value = serde_json::from_slice(&first.body).unwrap();
    assert_eq!(body["models"][0]["display_name"], "Seven");
    assert_eq!(
        body["models"][0]["unknown"],
        native()["models"][0]["unknown"]
    );
    assert_eq!(body["future_envelope"], native()["future_envelope"]);
    let cached = h.proxy(7, Some(&first.headers["etag"])).await;
    assert_eq!(cached.status, 304);
    assert!(cached.body.is_empty());
    assert_eq!(h.mock.calls.lock().unwrap().len(), 2);
    let other = h.proxy(8, None).await;
    assert_eq!(
        serde_json::from_slice::<Value>(&other.body).unwrap(),
        native()
    );
    *h.mock.status.lock().unwrap() = Some(401);
    let rejected = h.proxy(7, Some(&first.headers["etag"])).await;
    assert_eq!(rejected.status, 401);
    assert_eq!(
        serde_json::from_slice::<Value>(&rejected.body).unwrap(),
        json!({"error":"fixture-revoked"})
    );
    let calls = h.mock.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 4);
    for (index, call) in calls.iter().enumerate() {
        assert_eq!(call["method"], "GET");
        assert_eq!(call["path"], format!("/v1/models?{QUERY}"));
        assert_eq!(call["x-real-ip"], "127.0.0.1");
        assert_eq!(call["if-none-match"], "");
        assert_eq!(
            call["authorization"],
            format!("Bearer fixture-caller-{}", if index == 2 { 8 } else { 7 })
        );
    }
    h.close().await;
}

#[tokio::test]
async fn corrupt_config_and_database_failure_preserve_native_catalog_and_private_status() {
    let h = Harness::new().await;
    fs::write(&h.runtime.settings.model_path, "{fixture-private-invalid").unwrap();
    let fallback = h.proxy(7, None).await;
    assert_eq!(fallback.status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&fallback.body).unwrap(),
        native()
    );
    assert_eq!(fallback.headers["x-sub2ops-catalog"], "fallback");
    assert_eq!(h.runtime.catalog.status()["state"], "fallback");
    assert_eq!(
        fs::read(&h.runtime.settings.model_path).unwrap(),
        b"{fixture-private-invalid"
    );
    h.runtime.db.close().await;
    let unavailable = h.proxy(7, None).await;
    assert_eq!(unavailable.status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&unavailable.body).unwrap(),
        native()
    );
    assert_eq!(unavailable.headers["x-sub2ops-catalog"], "fallback");
    assert!(!h
        .runtime
        .catalog
        .status()
        .to_string()
        .contains("fixture-private"));
    assert_eq!(h.mock.calls.lock().unwrap().len(), 2);
    h.close().await;
}

#[tokio::test]
async fn composite_does_not_publish_unsaved_alias_or_write_account_state() {
    let h = Harness::new().await;
    sqlx::raw_sql("UPDATE groups SET platform='composite' WHERE id=7;
        INSERT INTO accounts (id,platform,type,schedulable,credentials) VALUES (1,'openai','apikey',true,'{\"model_mapping\":{\"public\":\"fixture-native-model\"}}');
        INSERT INTO account_groups VALUES (1,7);
        INSERT INTO composite_model_routes (id,group_id,public_model,match_type,target_platform,upstream_model,endpoint,priority,enabled)
        VALUES (1,7,'fixture-alias','exact','openai','public','responses',0,true);")
        .execute(&h.runtime.db.pool).await.unwrap();
    for enabled in [true, false] {
        sqlx::query("UPDATE accounts SET schedulable=$1 WHERE id=1")
            .bind(enabled)
            .execute(&h.runtime.db.pool)
            .await
            .unwrap();
        let before = h.runtime.monitor.row(1).await.unwrap();
        let response = h.proxy(7, None).await;
        assert_eq!(response.status, 200);
        assert_eq!(
            serde_json::from_slice::<Value>(&response.body).unwrap(),
            native()
        );
        assert_eq!(h.runtime.monitor.row(1).await.unwrap(), before);
    }
    let calls = h.mock.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|v| v["method"] == "GET"));
    assert!(!h.runtime.settings.model_path.exists());
    h.close().await;
}

#[tokio::test]
async fn model_admin_routes_authenticate_and_retired_surfaces_stay_absent() {
    let h = Harness::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = sub2ops_server::api::router(h.runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let path = format!("{base}/api/desktop/v1/model-groups");
    assert_eq!(
        client.get(&path).send().await.unwrap().status().as_u16(),
        401
    );
    let response = client
        .get(&path)
        .header("x-api-key", ADMIN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["groups"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for path in ["/models", "/docs"] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16(),
            404
        );
    }
    assert_eq!(
        client
            .post(format!("{base}/api/desktop/v1/model-groups/7/preview"))
            .header("x-api-key", ADMIN)
            .json(&json!({"extra":1}))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        404
    );
    server.abort();
    let _ = server.await;
    h.close().await;
}

#[tokio::test]
async fn desktop_routes_require_auth_except_safe_connection_status_diagnostics() {
    let h = Harness::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = sub2ops_server::api::router(h.runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let version = "a".repeat(64);
    let revision = "b".repeat(64);
    let binding = "c".repeat(64);
    let request_id = "fixture-request-id-0001".to_owned();
    let client_id = "fixture-client-id-0001".to_owned();
    let hex_request_id = "d".repeat(32);
    let hex_client_id = "e".repeat(32);
    let custom_id = format!("custom-{}", "a".repeat(24));

    macro_rules! case {
        ($method:ident, $route:expr) => {
            (reqwest::Method::$method, ($route).to_owned(), None)
        };
        ($method:ident, $route:expr, $body:expr) => {
            (reqwest::Method::$method, ($route).to_owned(), Some($body))
        };
    }

    let cases = vec![
        case!(GET, "capabilities"),
        case!(GET, "snapshot"),
        case!(GET, "detection-logs"),
        case!(GET, "errors"),
        case!(GET, "usage-records"),
        case!(GET, "usage-record-options?kind=users"),
        case!(GET, "recoveries"),
        case!(GET, "proxies"),
        case!(GET, "config"),
        case!(GET, "model-groups"),
        case!(GET, "account-operations"),
        case!(
            POST,
            "account-operations/batch",
            json!({
                "action":"delete",
                "request_id":request_id.clone(),
                "client_id":client_id.clone(),
                "accounts":[{"account_id":7,"expected_version":version.clone(),"payload":{}}]
            })
        ),
        case!(
            GET,
            "account-operations/00000000-0000-0000-0000-000000000001"
        ),
        case!(
            POST,
            "account-operations/00000000-0000-0000-0000-000000000001/cancel"
        ),
        case!(
            POST,
            "account-operations/batches/00000000-0000-0000-0000-000000000002/cancel"
        ),
        case!(
            DELETE,
            "accounts/7",
            json!({"expected_version":version.clone()})
        ),
        case!(GET, "accounts/7/settings"),
        case!(GET, "accounts/7/models?purpose=model_test"),
        case!(GET, "accounts/7/model-tests/latest"),
        case!(GET, "accounts/7/keepalive"),
        case!(
            PUT,
            "accounts/7/keepalive",
            json!({
                "enabled":true,
                "expected_version":version.clone()
            })
        ),
        case!(
            POST,
            "accounts/7/operations",
            json!({
                "action":"delete",
                "expected_version":version.clone(),
                "request_id":request_id.clone(),
                "client_id":client_id.clone(),
                "payload":{}
            })
        ),
        case!(
            POST,
            "accounts/7/schedulable",
            json!({"expected_version":version.clone(),"schedulable":true})
        ),
        case!(
            POST,
            "accounts/7/recover-state",
            json!({"expected_version":version.clone()})
        ),
        case!(
            POST,
            "accounts/7/priority",
            json!({"expected_version":version.clone(),"priority":0})
        ),
        case!(
            POST,
            "accounts/7/test",
            json!({"expected_version":version.clone()})
        ),
        case!(
            POST,
            "accounts/7/model-tests",
            json!({
                "model_id":"fixture-model",
                "expected_version":version.clone(),
                "request_id":request_id.clone(),
                "concurrency":1
            })
        ),
        case!(
            POST,
            "accounts/7/usage-action",
            json!({"expected_version":version.clone(),"action":"query_usage"})
        ),
        case!(
            PUT,
            "accounts/7/groups",
            json!({"expected_version":version.clone(),"scope_group_ids":[7],"group_ids":[7]})
        ),
        case!(
            PUT,
            "accounts/7/degradation-mark",
            json!({"expected_mark_version":version.clone(),"marked":true})
        ),
        case!(GET, "accounts/7/model-detection"),
        case!(
            PUT,
            "accounts/7/model-detection",
            json!({
                "expected_version":version.clone(),
                "enabled":true,
                "interval_minutes":15,
                "model_id":"fixture-model"
            })
        ),
        case!(GET, "errors/7"),
        case!(GET, "usage-records/7"),
        case!(POST, "proxies/7/test"),
        case!(POST, "proxies/7/quality-check"),
        case!(GET, "account-templates"),
        case!(
            PUT,
            "account-templates",
            json!({
                "expected_version":version.clone(),
                "full":{},"degraded":{},"takeover":{}
            })
        ),
        case!(
            POST,
            "account-templates",
            json!({"expected_version":version.clone(),"name":"fixture-template"})
        ),
        case!(
            POST,
            "account-templates/apply",
            json!({
                "template_id":"full",
                "template_version":version.clone(),
                "request_id":hex_request_id.clone(),
                "client_id":hex_client_id.clone(),
                "accounts":[{"account_id":7,"expected_version":version.clone()}]
            })
        ),
        case!(
            POST,
            "account-templates/custom",
            json!({"expected_version":version.clone(),"name":"fixture-template"})
        ),
        case!(
            PUT,
            format!("account-templates/{custom_id}"),
            json!({"expected_version":version.clone(),"name":"fixture-template"})
        ),
        case!(
            DELETE,
            format!("account-templates/{custom_id}"),
            json!({"expected_version":version.clone()})
        ),
        case!(
            PUT,
            format!("account-templates/custom/{custom_id}"),
            json!({"expected_version":version.clone(),"name":"fixture-template"})
        ),
        case!(
            DELETE,
            format!("account-templates/custom/{custom_id}"),
            json!({"expected_version":version.clone()})
        ),
        case!(GET, "modeltrace/fingerprint-bank"),
        case!(POST, "modeltrace/fingerprint-bank/sync"),
        case!(GET, "model-tests/00000000-0000-0000-0000-000000000003"),
        case!(
            POST,
            "model-tests/00000000-0000-0000-0000-000000000003/cancel"
        ),
        case!(
            POST,
            "model-tests/00000000-0000-0000-0000-000000000003/retry",
            json!({"request_id":request_id.clone()})
        ),
        case!(GET, "quota-refresh"),
        case!(POST, "quota-refresh"),
        case!(
            PUT,
            "config/oauth",
            json!({"expected_revision":version.clone(),"changes":{}})
        ),
        case!(
            PUT,
            "config/bark",
            json!({"expected_revision":version.clone(),"changes":{}})
        ),
        case!(
            PUT,
            "config/key_fallback",
            json!({"expected_revision":version.clone(),"changes":{}})
        ),
        case!(POST, "actions/bark-test"),
        case!(GET, "model-groups/7/reasoning"),
        case!(
            PUT,
            "model-groups/7/reasoning",
            json!({
                "model":"fixture-model",
                "expected_version":version.clone(),
                "expected_revision":revision.clone(),
                "expected_binding":binding.clone(),
                "efforts":["minimal"],
                "default_effort":"minimal"
            })
        ),
        case!(
            DELETE,
            "model-groups/7/reasoning",
            json!({
                "model":"fixture-model",
                "expected_version":version.clone(),
                "expected_revision":revision.clone()
            })
        ),
        case!(
            POST,
            "model-groups/7/reasoning/resolve",
            json!({"model":"fixture-model","efforts":["minimal"],"default_effort":"minimal"})
        ),
    ];
    assert_eq!(cases.len(), 60);

    let mut rejected = Vec::new();
    for (method, route, body) in cases {
        let status = desktop_status(&client, &base, method.clone(), &route, body).await;
        if !matches!(status.as_u16(), 401 | 403) {
            rejected.push(format!("{} {route} returned {}", method, status.as_u16()));
        }
    }
    let diagnostic_response = client
        .get(format!("{base}/api/desktop/v1/connection-status"))
        .send()
        .await
        .unwrap();
    let diagnostic_status = diagnostic_response.status().as_u16();
    let diagnostic = diagnostic_response
        .json::<Value>()
        .await
        .unwrap_or(Value::Null);
    let public_fields = [
        "state",
        "endpoint",
        "http_status",
        "message",
        "retryable",
        "checked_at",
    ];
    let safe_diagnostic = diagnostic.as_object().is_some_and(|fields| {
        fields.len() == public_fields.len()
            && public_fields
                .iter()
                .all(|field| fields.contains_key(*field))
            && diagnostic["state"] == "auth_rejected"
            && diagnostic["endpoint"] == "none"
            && diagnostic["http_status"] == 401
            && diagnostic["message"] == "管理员 API Key 无效"
            && diagnostic["retryable"] == false
            && diagnostic["checked_at"].as_str().is_some()
    });
    let no_upstream_business_calls = h.mock.calls.lock().unwrap().is_empty();
    server.abort();
    let _ = server.await;
    h.close().await;
    assert!(
        rejected.is_empty(),
        "legal desktop management requests without x-api-key must return 401/403: {}",
        rejected.join(", ")
    );
    assert_eq!(diagnostic_status, 200);
    assert!(
        safe_diagnostic,
        "connection-status must return only the static unauthenticated probe fields"
    );
    assert!(
        no_upstream_business_calls,
        "unauthenticated management and diagnostic requests must not call the upstream"
    );
}

#[tokio::test]
async fn keepalive_desktop_api_requires_auth_and_rejects_stale_versions() {
    let h = Harness::new().await;
    let account_id = 47_007_i64;
    sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra) VALUES ($1,'fixture-keepalive-api','openai','oauth','active',true,'{}'::jsonb,'{}'::jsonb)")
        .bind(account_id)
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    h.runtime.monitor.store.save_admin_token(ADMIN).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = sub2ops_server::api::router(h.runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let route = format!("{base}/api/desktop/v1/accounts/{account_id}/keepalive");

    let unauthenticated = client.get(&route).send().await.unwrap();
    assert_eq!(unauthenticated.status().as_u16(), 401);

    let response = client
        .get(&route)
        .header("x-api-key", ADMIN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let current = response.json::<Value>().await.unwrap();
    assert_eq!(current["enabled"], false);
    let same_as_current = "a".repeat(64);
    let stale = if current["version"] == same_as_current {
        "b".repeat(64)
    } else {
        same_as_current
    };
    let conflict = client
        .put(&route)
        .header("x-api-key", ADMIN)
        .json(&json!({"enabled":true,"expected_version":stale}))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status().as_u16(), 409);

    let unchanged = client
        .get(&route)
        .header("x-api-key", ADMIN)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(unchanged["enabled"], false);
    server.abort();
    let _ = server.await;
    h.close().await;
}

#[tokio::test]
async fn desktop_parameter_and_body_shape_errors_keep_422_before_authentication() {
    let h = Harness::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = sub2ops_server::api::router(h.runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let cases = [
        (
            reqwest::Method::GET,
            "usage-records?limit=not-an-integer",
            None,
        ),
        (
            reqwest::Method::POST,
            "accounts/7/schedulable",
            Some(json!({"schedulable":true})),
        ),
    ];
    let mut statuses = Vec::new();
    for (method, route, body) in cases {
        statuses.push(
            desktop_status(&client, &base, method, route, body)
                .await
                .as_u16(),
        );
    }
    server.abort();
    let _ = server.await;
    h.close().await;
    assert_eq!(statuses, [422, 422]);
}

#[tokio::test]
async fn catalog_proxy_keeps_allowlist_case_insensitive_and_drops_untrusted_headers() {
    let h = Harness::new().await;
    let headers = BTreeMap::from([
        ("aUtHoRiZaTiOn".into(), "Bearer fixture-caller-7".into()),
        ("X-aPi-KeY".into(), "fixture-caller-7".into()),
        ("User-Agent".into(), "fixture-catalog-client".into()),
        ("hOsT".into(), "spoof.example.invalid".into()),
        ("cOnNeCtIoN".into(), "close".into()),
        ("tRaNsFeR-EnCoDiNg".into(), "chunked".into()),
        ("cOoKiE".into(), "session=fixture-cookie".into()),
        ("X-Unlisted-Fixture".into(), "fixture-unlisted".into()),
    ]);
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        h.runtime.catalog.proxy("/v1/models", QUERY, &headers),
    )
    .await
    .expect("catalog proxy must finish against the loopback mock");
    assert_eq!(reply.status, 200);
    let calls = h.mock.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["authorization"], "Bearer fixture-caller-7");
    assert_eq!(calls[0]["x-api-key"], "fixture-caller-7");
    assert_eq!(calls[0]["user-agent"], "fixture-catalog-client");
    assert_ne!(calls[0]["host"], "spoof.example.invalid");
    assert_ne!(calls[0]["connection"], "close");
    assert_eq!(calls[0]["transfer-encoding"], "");
    assert_eq!(calls[0]["cookie"], "");
    assert_eq!(calls[0]["x-unlisted-fixture"], "");
    h.close().await;
}

#[tokio::test]
async fn snapshot_invalidation_never_waits_or_republishes_inflight_cache() {
    use std::time::Duration;

    let harness = Harness::new().await;
    sqlx::query("INSERT INTO accounts (id,name,platform,type) VALUES (12,'fixture-before','fixture','apikey')")
        .execute(&harness.runtime.db.pool).await.unwrap();
    let mut blocker = harness.runtime.db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE accounts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let runtime = harness.runtime.clone();
    let inflight = tokio::spawn(async move { runtime.snapshot().await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))")
                .bind(blocker_pid).fetch_one(&harness.owner.pool).await.unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("snapshot must reach the blocked database read");
    assert!(!inflight.is_finished());
    tokio::time::timeout(Duration::from_secs(5), harness.runtime.invalidate())
        .await
        .expect("invalidation must not wait for the snapshot lock or database");
    sqlx::query("UPDATE accounts SET name='fixture-inflight' WHERE id=12")
        .execute(&mut *blocker)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    let first = inflight.await.unwrap().unwrap();
    assert_eq!(first["accounts"][0]["name"], "fixture-inflight");

    // An invalidated in-flight result may satisfy its caller, but must not become
    // the next request's cached value. No second invalidation is sent here.
    sqlx::query("UPDATE accounts SET name='fixture-after' WHERE id=12")
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    let second = harness.runtime.snapshot().await.unwrap();
    assert_eq!(second["accounts"][0]["name"], "fixture-after");
    sqlx::query("UPDATE accounts SET name='fixture-unobserved' WHERE id=12")
        .execute(&harness.runtime.db.pool)
        .await
        .unwrap();
    assert_eq!(
        harness.runtime.snapshot().await.unwrap(),
        second,
        "the subsequent uncontended snapshot is cached normally"
    );
    assert!(harness.mock.calls.lock().unwrap().is_empty());
    harness.close().await;
}

#[tokio::test]
async fn config_partial_save_keeps_other_fields_permissions_and_rejects_stale_or_retired_fields() {
    use std::os::unix::fs::PermissionsExt;
    let h = Harness::new().await;
    h.runtime.config.save("oauth", &json!({"oauth_recovery_test_model_id":"fixture-preserved-model","oauth_daily_test_time":"05:00"}), "fixture", None).await.unwrap();
    let before = h.runtime.config.snapshot(Some("oauth")).unwrap();
    let revision = before["oauth"]["revision"].as_str().unwrap();
    let saved = h
        .runtime
        .config
        .save(
            "oauth",
            &json!({"oauth_daily_test_time":"06:15"}),
            "fixture",
            Some(revision),
        )
        .await
        .unwrap();
    assert_eq!(saved["oauth_daily_test_time"], "06:15");
    assert_eq!(
        saved["oauth_recovery_test_model_id"],
        "fixture-preserved-model"
    );
    assert_ne!(saved["revision"], revision);
    let path = &h.runtime.settings.oauth_path;
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let bytes = fs::read(path).unwrap();
    assert_eq!(
        h.runtime
            .config
            .save(
                "oauth",
                &json!({"oauth_daily_test_time":"07:00"}),
                "fixture",
                Some(revision)
            )
            .await
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    for changes in [
        json!({"oauth_daily_test_time":"25:00"}),
        json!({"oauth_usage_refresh_enabled":true}),
        json!({"oauth_regular_refresh_interval_seconds":60}),
    ] {
        assert_eq!(
            h.runtime
                .config
                .save("oauth", &changes, "fixture", None)
                .await
                .unwrap_err()
                .status
                .as_u16(),
            422
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(
        h.runtime.config.current().unwrap()["oauth_daily_test_time"],
        "06:15"
    );
    assert!(h.mock.calls.lock().unwrap().is_empty());
    h.close().await;
}

#[tokio::test]
async fn account_connection_directory_keeps_all_pages_parent_credentials_and_native_ids() {
    let h = Harness::new().await;
    let base = h.runtime.settings.base_url.clone();
    for (parent, child, platform) in [(21, 22, "anthropic"), (31, 32, "gemini"), (41, 42, "grok")] {
        sqlx::query("INSERT INTO accounts(id,name,platform,type,credentials,parent_account_id) VALUES($1,'fixture-parent',$3,'apikey',$4,null),($2,'fixture-child',$3,'apikey','{}',$1)")
            .bind(parent as i64).bind(child).bind(platform).bind(sqlx::types::Json(json!({"api_key":"fixture-directory-secret","base_url":base,"model_mapping":{"fixture-only":"fixture-first"}}))).execute(&h.runtime.db.pool).await.unwrap();
        if platform == "anthropic" {
            *h.mock.pages.lock().unwrap() = BTreeMap::from([
                (
                    "/v1/models".into(),
                    json!({"data":[{"id":"fixture-first","display_name":"First"}],"has_more":true,"last_id":"fixture-first"}),
                ),
                (
                    "/v1/models?after_id=fixture-first".into(),
                    json!({"data":[{"id":"fixture-second"},{"id":"fixture-first"}],"has_more":false}),
                ),
            ]);
        } else if platform == "gemini" {
            *h.mock.pages.lock().unwrap() = BTreeMap::from([
                (
                    "/v1beta/models".into(),
                    json!({"models":[{"name":"models/fixture-first","displayName":"First"}],"nextPageToken":"page2"}),
                ),
                (
                    "/v1beta/models?pageToken=page2".into(),
                    json!({"models":[{"name":"models/fixture-second"}]}),
                ),
            ]);
        } else {
            *h.mock.pages.lock().unwrap() = BTreeMap::from([(
                "/v1/models".into(),
                json!({"data":[{"id":"fixture-first"},{"id":"fixture-second"}]}),
            )]);
        }
        let models = h.runtime.catalog.account_models(child).await.unwrap();
        assert_eq!(
            models
                .iter()
                .map(|m| m["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["fixture-first", "fixture-second"]
        );
        assert!(models.iter().all(|m| m.as_object().unwrap().len() == 3));
        let calls = h.mock.calls.lock().unwrap().len();
        assert_eq!(
            h.runtime.catalog.account_models(child).await.unwrap(),
            models
        );
        assert_eq!(h.mock.calls.lock().unwrap().len(), calls);
    }
    h.close().await;
}

#[tokio::test]
async fn single_account_usage_is_independent_of_groups_history_and_snapshot_lock() {
    let h = Harness::new().await;
    sqlx::query("INSERT INTO accounts (id,name,platform,type) VALUES (12,'fixture-usage','openai','apikey')")
        .execute(&h.runtime.db.pool).await.unwrap();
    sqlx::query("INSERT INTO usage_logs (id,account_id,created_at,input_tokens,output_tokens) VALUES (12,12,now(),10,20)")
        .execute(&h.runtime.db.pool).await.unwrap();
    let expected = h.runtime.account_usage(12, ADMIN, false).await.unwrap();
    assert_eq!(expected["today"]["requests"], 1);
    let mut blocker = h.runtime.db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE groups, ops_error_logs IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let runtime = h.runtime.clone();
    let snapshot = tokio::spawn(async move { runtime.snapshot().await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))")
                .bind(pid).fetch_one(&h.owner.pool).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let usage = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        h.runtime.account_usage(12, ADMIN, false),
    )
    .await
    .expect("single-account usage must not wait for an unrelated snapshot")
    .unwrap();
    assert_eq!(usage["today"], expected["today"]);
    assert!(!snapshot.is_finished());
    blocker.rollback().await.unwrap();
    snapshot.await.unwrap().unwrap();
    assert!(h.mock.calls.lock().unwrap().is_empty());
    h.close().await;
}
