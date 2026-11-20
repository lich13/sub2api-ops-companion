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
use sub2ops_server::{db::Database, runtime::Runtime, settings::Settings};
use tokio::{net::TcpListener, task::JoinHandle};

const ADMIN: &str = "fixture-catalog-admin";

fn native() -> Value {
    json!({"models":[{"slug":"fixture-native-model","display_name":"Native","unknown":{"array":[false,0,null]}}],
        "future_envelope":{"native":true}})
}

#[derive(Clone, Default)]
struct Upstream {
    calls: Arc<Mutex<Vec<Value>>>,
    pages: Arc<Mutex<BTreeMap<String, Value>>>,
    admin: Arc<Mutex<BTreeMap<String, (u16, Value)>>>,
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
        .push(json!({"path":request.uri().to_string(),"authorization":header("authorization")}));
    let (status, body) = if path.starts_with("/api/v1/admin/") {
        if header("x-api-key") == ADMIN {
            mock.admin
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .unwrap_or_else(|| (200, json!({"code":0,"data":[]})))
        } else {
            (401, json!({"code":401}))
        }
    } else {
        (
            200,
            mock.pages
                .lock()
                .unwrap()
                .get(&request.uri().to_string())
                .cloned()
                .unwrap_or_else(native),
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
        let owner = Database::open(&database_url).await.unwrap();
        let schema = format!("catalog_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&owner.pool)
            .await
            .unwrap();
        let mut url = reqwest::Url::parse(&database_url).unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let seed = Database::open(url.as_str()).await.unwrap();
        sqlx::raw_sql(
            include_str!("../../tests/contracts/http/schema.sql")
                .split("-- Every row below")
                .next()
                .unwrap(),
        )
        .execute(&seed.pool)
        .await
        .unwrap();
        sqlx::raw_sql("INSERT INTO groups (id,name,platform) VALUES (7,'fixture-seven','openai'),(8,'fixture-eight','openai');")
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
            ("USAGE_QUERY_STATE_PATH", "state.json"),
            ("SUB2API_CONFIG_PATH", "connection.json"),
            ("KEEPALIVE_CONFIG_PATH", "keepalive-state.json"),
        ] {
            env.insert(
                key.into(),
                root.path().join(file).to_string_lossy().into_owned(),
            );
        }
        let runtime = Runtime::new(Settings::from_env(&env).unwrap())
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
            "accounts/7/model-tests",
            json!({
                "model_id":"fixture-model",
                "expected_version":version.clone(),
                "request_id":request_id.clone(),
                "concurrency":1
            })
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
            PUT,
            format!("account-templates/{custom_id}"),
            json!({"expected_version":version.clone(),"name":"fixture-template"})
        ),
        case!(
            DELETE,
            format!("account-templates/{custom_id}"),
            json!({"expected_version":version.clone()})
        ),
        case!(GET, "modeltrace/fingerprint-bank"),
        case!(POST, "modeltrace/fingerprint-bank/sync"),
        case!(GET, "lm-detector/fingerprint-bank"),
        case!(POST, "lm-detector/fingerprint-bank/sync"),
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
        case!(POST, "actions/bark-test"),
    ];
    assert_eq!(cases.len(), 44);

    for retired in [
        "/v1/models?client_version=0.146.0",
        "/models?client_version=0.146.0",
        "/backend-api/codex/models",
        "/api/desktop/v1/model-groups",
        "/api/desktop/v1/model-groups/7/reasoning",
    ] {
        assert_eq!(
            client
                .get(format!("{base}{retired}"))
                .header("x-api-key", ADMIN)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16(),
            404,
            "retired route {retired} must return 404"
        );
    }

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
async fn keepalive_reads_preserve_history_and_validate_the_entire_changed_state() {
    use sub2ops_server::store::write_json;

    let h = Harness::new().await;
    let keepalive = &h.runtime.keepalive;
    let mut control = serde_json::to_value(keepalive.control(47_007).unwrap()).unwrap();
    control["enabled"] = true.into();
    control["generation"] = 7.into();
    control["restores"] = json!(["2030-01-01T00:00:00Z"]);
    control["notifications"] = json!([{"id":"fixture-notice","generation":7,"status":"pending",
        "attempts":2,"next_at":"2030-01-01T00:03:00Z"}]);
    let mut state = json!({"version":1,"accounts":{"47007":control}});
    write_json(keepalive.store.path(), &state).unwrap();
    let before = fs::read(keepalive.store.path()).unwrap();
    for _ in 0..2 {
        let mut read = keepalive.control(47_007).unwrap();
        assert_eq!(
            serde_json::to_value(&read).unwrap(),
            state["accounts"]["47007"]
        );
        read.restores.clear();
        read.notifications.clear();
    }
    assert!(!keepalive.control(47_008).unwrap().enabled);
    assert_eq!(fs::read(keepalive.store.path()).unwrap(), before);

    // A malformed unrelated record must invalidate the whole snapshot,
    // including reads that were cached before the external replacement.
    state["accounts"]["47008"] = json!({"enabled":"invalid","generation":0});
    write_json(keepalive.store.path(), &state).unwrap();
    let error = keepalive
        .control(47_007)
        .err()
        .expect("reject corrupt state");
    assert_eq!(error.status.as_u16(), 503);
    assert!(keepalive.control(47_009).is_err());
    state["accounts"]
        .as_object_mut()
        .unwrap()
        .shift_remove("47008");
    state["accounts"]["47007"]["generation"] = 8.into();
    write_json(keepalive.store.path(), &state).unwrap();
    assert_eq!(keepalive.control(47_007).unwrap().generation, 8);
    h.close().await;
}

#[tokio::test]
async fn keepalive_skips_database_while_waiting_or_disabled() {
    use sub2ops_server::store::write_json;

    let h = Harness::new().await;
    let keepalive = &h.runtime.keepalive;
    let now = chrono::Utc::now();
    let mut cooling = serde_json::to_value(keepalive.control(47_101).unwrap()).unwrap();
    cooling["enabled"] = true.into();
    cooling["last_restore_at"] = json!(now);
    let mut exhausted = cooling.clone();
    exhausted["last_restore_at"] = Value::Null;
    exhausted["restores"] = json!(vec![now; 5]);
    let mut state = json!({"version":1,"accounts":{"47101":cooling,"47102":exhausted}});
    write_json(keepalive.store.path(), &state).unwrap();
    let db = &h.runtime.actions.queries.db.pool;
    sqlx::query("ALTER TABLE accounts RENAME TO fixture_waiting_accounts")
        .execute(db)
        .await
        .unwrap();
    keepalive
        .tick()
        .await
        .expect("waiting accounts need no database query");

    state["accounts"]["47101"]["last_restore_at"] = json!(now - chrono::Duration::minutes(5));
    write_json(keepalive.store.path(), &state).unwrap();
    assert!(
        keepalive.tick().await.is_err(),
        "expired cooldown must resume reading accounts"
    );

    let control = &mut state["accounts"]["47101"];
    control["enabled"] = false.into();
    control["last_restore_at"] = json!(now);
    write_json(keepalive.store.path(), &state).unwrap();
    keepalive
        .tick()
        .await
        .expect("disabled accounts need no database query");
    sqlx::query("ALTER TABLE fixture_waiting_accounts RENAME TO accounts")
        .execute(db)
        .await
        .unwrap();
    keepalive.tick().await.unwrap();
    assert!(!keepalive.control(47_101).unwrap().enabled);
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
            "accounts/7/operations",
            Some(json!({"action":"schedulable","payload":{"schedulable":true}})),
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
    harness.runtime.invalidate();
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
    let projection_row = harness.runtime.actions.queries.account(12).await.unwrap();
    let mut account = harness
        .runtime
        .account_snapshot_from_row(&projection_row, None)
        .await
        .unwrap();
    assert_eq!(account["name"], "fixture-unobserved");
    account["name"] = "fixture-caller-mutation".into();
    assert_eq!(
        harness
            .runtime
            .account_snapshot_from_row(&projection_row, None)
            .await
            .unwrap()["name"],
        "fixture-unobserved"
    );
    assert_eq!(
        harness
            .runtime
            .actions
            .queries
            .account(-1)
            .await
            .unwrap_err()
            .status
            .as_u16(),
        404
    );
    harness.runtime.invalidate();
    assert_eq!(
        harness
            .runtime
            .account_snapshot_from_row(&projection_row, None)
            .await
            .unwrap()["name"],
        "fixture-unobserved"
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
        let models = sub2ops_server::model_directory::account_models_read(&h.runtime.db, child)
            .await
            .unwrap();
        assert_eq!(
            models
                .iter()
                .map(|m| m["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["fixture-first", "fixture-second"]
        );
        assert!(models.iter().all(|m| m.as_object().unwrap().len() == 3));
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

#[tokio::test]
async fn model_operation_summaries_preserve_fields_status_and_full_details() {
    let h = Harness::new().await;
    let jobs: Vec<_> = ["queued", "running", "retrying", "needs_confirmation", "completed"]
        .into_iter()
        .enumerate()
        .map(|(index, status)| {
            let mut job = json!({"id":format!("{:032x}", index + 1),"account_id":7,"account_name":"fixture-account","status":status,"requested_model":"fixture-model","concurrency":2,"report":{"detail":"fixture".repeat(1024)}});
            if index == 1 {
                job["error"] = Value::Null;
            } else if index > 1 {
                job["error"] = json!("fixture-error");
            }
            job
        })
        .collect();
    h.runtime
        .tests
        .transaction(|data| {
            for job in &jobs {
                data["jobs"][job["id"].as_str().unwrap()] = job.clone();
            }
            Ok(())
        })
        .unwrap();
    let listing = h.runtime.operations.listing(None).unwrap();
    let expected: Vec<_> = jobs[..4]
        .iter()
        .map(|job| {
            sub2ops_server::operations::Operations::model_view(&h.runtime.tests.public(job.clone()))
        })
        .collect();
    assert_eq!(listing["items"], json!(expected));
    assert_eq!(listing["items"][0]["reason"], "");
    assert_eq!(listing["items"][1]["reason"], Value::Null);
    assert_eq!(listing["items"][2]["status"], "running");
    for job in &jobs {
        assert_eq!(
            h.runtime.tests.get(job["id"].as_str().unwrap()).unwrap(),
            *job
        );
    }
    h.close().await;
}

#[tokio::test]
async fn admin_model_directory_revocation_evicts_cached_candidates() {
    let h = Harness::new().await;
    sqlx::query("INSERT INTO accounts(id,name,platform,type,credentials) VALUES(92,'fixture-admin-directory','gemini','oauth',$1)")
        .bind(sqlx::types::Json(json!({"access_token":"fixture-catalog-token"})))
        .execute(&h.runtime.db.pool).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/api/desktop/v1/accounts/92/models?purpose=model_test",
        listener.local_addr().unwrap()
    );
    let app = sub2ops_server::api::router(h.runtime.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let request = || client.get(&url).header("x-api-key", ADMIN);
    let admin_route = "/api/v1/admin/accounts/92/models";
    for status in [401, 403] {
        h.mock.admin.lock().unwrap().insert(
            admin_route.into(),
            (200, json!({"code":0,"data":[{"id":"fixture-admin-model"}]})),
        );
        let response = request()
            .query(&[("refresh", "true")])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<Value>().await.unwrap()[0]["id"],
            "fixture-admin-model"
        );

        h.mock
            .admin
            .lock()
            .unwrap()
            .insert(admin_route.into(), (status, json!({"code":status})));
        for read in [request().query(&[("refresh", "true")]), request()] {
            let response = read.send().await.unwrap();
            assert_eq!(response.status(), 502);
            assert_eq!(
                response.json::<Value>().await.unwrap()["detail"]["code"],
                "model_directory_revoked"
            );
        }
        let cache = fs::read(
            h.runtime
                .settings
                .data_dir()
                .join("account-model-cache.json"),
        )
        .unwrap();
        assert!(serde_json::from_slice::<Value>(&cache).unwrap()["entries"]
            .as_object()
            .unwrap()
            .is_empty());
    }
    server.abort();
    let _ = server.await;
    h.close().await;
}

#[tokio::test]
async fn anthropic_detection_candidates_read_every_page_and_do_not_cache_partial_results() {
    let h = Harness::new().await;
    sqlx::query("INSERT INTO accounts(id,name,platform,type,credentials) VALUES(91,'fixture-pages','anthropic','apikey',$1)")
        .bind(sqlx::types::Json(json!({"api_key":"fixture-pages-key","base_url":h.runtime.settings.base_url})))
        .execute(&h.runtime.db.pool).await.unwrap();
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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = sub2ops_server::api::router(h.runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let path = format!("{base}/api/desktop/v1/accounts/91/models?purpose=model_test");
    let response = client
        .get(&path)
        .header("x-api-key", ADMIN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let models = response.json::<Value>().await.unwrap();
    assert_eq!(
        models,
        json!([
            {"id":"fixture-first","display_name":"First","type":"model"},
            {"id":"fixture-second","display_name":"fixture-second","type":"model"}
        ])
    );
    let directory_calls = || {
        h.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["path"].as_str().unwrap().starts_with("/v1/models"))
            .count()
    };
    assert_eq!(directory_calls(), 2);
    assert_eq!(
        client
            .get(&path)
            .header("x-api-key", ADMIN)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        models
    );
    assert_eq!(directory_calls(), 2);
    h.mock.pages.lock().unwrap().insert(
        "/v1/models?after_id=fixture-first".into(),
        json!({"error":"fixture-second-page-error"}),
    );
    assert_eq!(
        client
            .get(format!("{path}&refresh=true"))
            .header("x-api-key", ADMIN)
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    assert_eq!(
        client
            .get(&path)
            .header("x-api-key", ADMIN)
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    let saved: Value = serde_json::from_slice(
        &fs::read(
            h.runtime
                .settings
                .data_dir()
                .join("account-model-cache.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(saved["entries"].as_object().unwrap().is_empty());
    h.mock.pages.lock().unwrap().insert(
        "/v1/models?after_id=fixture-first".into(),
        json!({"data":[{"id":"fixture-second"}],"has_more":false}),
    );
    assert_eq!(
        client
            .get(format!("{path}&refresh=true"))
            .header("x-api-key", ADMIN)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        models
    );
    server.abort();
    let _ = server.await;
    h.close().await;
}

#[tokio::test]
async fn account_directory_prefers_child_proxy_and_inherits_parent_when_unset() {
    let h = Harness::new().await;
    let child_proxy = Upstream::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let child_port = i32::from(listener.local_addr().unwrap().port());
    let app = Router::new()
        .fallback(upstream)
        .with_state(child_proxy.clone());
    let proxy_server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let parent_port = i32::from(
        reqwest::Url::parse(&h.runtime.settings.base_url)
            .unwrap()
            .port()
            .unwrap(),
    );
    sqlx::query("INSERT INTO proxies(id,protocol,host,port) VALUES(71,'http','127.0.0.1',$1),(72,'http','127.0.0.1',$2)")
        .bind(parent_port).bind(child_port).execute(&h.runtime.db.pool).await.unwrap();
    sqlx::query("INSERT INTO accounts(id,name,platform,type,credentials,parent_account_id,proxy_id) VALUES(81,'fixture-parent','grok','oauth',$1,null,71),(82,'fixture-child','grok','oauth','{}',81,null)")
        .bind(sqlx::types::Json(json!({"access_token":"fixture-parent-key","base_url":h.runtime.settings.base_url})))
        .execute(&h.runtime.db.pool).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = sub2ops_server::api::router(h.runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let path = format!("{base}/api/desktop/v1/accounts/82/models?purpose=model_test");
    for (step, expected_child_calls) in [0, 1, 1, 2].into_iter().enumerate() {
        if step == 1 {
            sqlx::query("UPDATE accounts SET proxy_id=72 WHERE id=82")
                .execute(&h.runtime.db.pool)
                .await
                .unwrap();
        } else if step == 3 {
            sqlx::query("UPDATE proxies SET username='fixture-user',password='fixture-password' WHERE id=72").execute(&h.runtime.db.pool).await.unwrap();
        }
        let response = client
            .get(&path)
            .header("x-api-key", ADMIN)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<Value>().await.unwrap()[0]["id"],
            "fixture-native-model"
        );
        assert_eq!(
            h.mock
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call["path"].as_str().unwrap().ends_with("/v1/models"))
                .count(),
            1
        );
        assert_eq!(
            child_proxy.calls.lock().unwrap().len(),
            expected_child_calls
        );
    }
    assert!(child_proxy
        .calls
        .lock()
        .unwrap()
        .iter()
        .all(|call| call["authorization"] == "Bearer fixture-parent-key"));
    server.abort();
    let _ = server.await;
    proxy_server.abort();
    let _ = proxy_server.await;
    h.close().await;
}
