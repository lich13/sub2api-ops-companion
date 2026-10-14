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
        "if-none-match":header("if-none-match")}));
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
                native()
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
    tokio::time::timeout(Duration::from_millis(100), harness.runtime.invalidate())
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
