use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::{json, Value};
use sqlx::{types::Json as SqlJson, PgPool};
use std::{
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};
use sub2ops_server::{
    compat::{config_revision, parse_datetime},
    db::Database,
    key_fallback::{self, KeyFallback},
    runtime::Runtime,
    settings::Settings,
};
use tokio::{net::TcpListener, task::JoinHandle};

const ADMIN: &str = "fixture-fallback-admin";

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../tests/contracts/fallback/fixtures.json")).unwrap()
}

#[derive(Clone)]
struct Mock {
    pool: PgPool,
    calls: Arc<Mutex<Vec<Value>>>,
    fail: Vec<i64>,
    changes: Value,
    response: Arc<Mutex<Option<Value>>>,
    delay: Arc<Mutex<Duration>>,
    config_path: Arc<Mutex<Option<std::path::PathBuf>>>,
}

async fn upstream(State(mock): State<Mock>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path();
    let payload: Value =
        serde_json::from_slice(&to_bytes(body, 8192).await.unwrap()).unwrap_or(Value::Null);
    let id = path
        .strip_prefix("/api/v1/admin/accounts/")
        .and_then(|v| v.strip_suffix("/schedulable"))
        .and_then(|v| v.parse::<i64>().ok());
    let first = {
        let mut calls = mock.calls.lock().unwrap();
        let config = mock
            .config_path
            .lock()
            .unwrap()
            .as_ref()
            .map(|path| serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap());
        calls.push(json!({"method":parts.method.as_str(),"path":path,"payload":payload,"config":config,
                          "api_key":parts.headers.get("x-api-key").and_then(|v|v.to_str().ok()),
                          "authorization":parts.headers.get("authorization").and_then(|v|v.to_str().ok())}));
        calls.len() == 1
    };
    if parts.method != Method::POST || id.is_none() {
        return (StatusCode::NOT_FOUND, Json(json!({"code":1}))).into_response();
    }
    let id = id.unwrap();
    if first {
        for (aid, patch) in mock.changes.as_object().unwrap() {
            let aid: i64 = aid.parse().unwrap();
            if let Some(deleted) = parse_datetime(&patch["deleted_at"]) {
                sqlx::query("UPDATE accounts SET deleted_at=$1 WHERE id=$2")
                    .bind(deleted)
                    .bind(aid)
                    .execute(&mock.pool)
                    .await
                    .unwrap();
            }
            if let Some(kind) = patch["type"].as_str() {
                sqlx::query("UPDATE accounts SET type=$1 WHERE id=$2")
                    .bind(kind)
                    .bind(aid)
                    .execute(&mock.pool)
                    .await
                    .unwrap();
            }
            if let Some(enabled) = patch["schedulable"].as_bool() {
                sqlx::query("UPDATE accounts SET schedulable=$1 WHERE id=$2")
                    .bind(enabled)
                    .bind(aid)
                    .execute(&mock.pool)
                    .await
                    .unwrap();
            }
        }
    }
    let delay = *mock.delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    if let Some(response) = mock.response.lock().unwrap().clone() {
        return Json(response).into_response();
    }
    if mock.fail.contains(&id) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"code":1,"message":"fixture rejection"})),
        )
            .into_response();
    }
    let enabled = payload["schedulable"].as_bool().unwrap();
    sqlx::query("UPDATE accounts SET schedulable=$1 WHERE id=$2")
        .bind(enabled)
        .bind(id)
        .execute(&mock.pool)
        .await
        .unwrap();
    Json(json!({"code":0,"data":{"id":id,"schedulable":enabled}})).into_response()
}

struct Harness {
    _directory: tempfile::TempDir,
    runtime: Arc<Runtime>,
    owner: Database,
    schema: String,
    mock: Mock,
    server: JoinHandle<()>,
}

impl Harness {
    async fn new(case: &Value) -> Self {
        let root_url = std::env::var("DATABASE_URL").expect("owned temporary PostgreSQL required");
        let owner = Database::open(&root_url, false).await.unwrap();
        let schema = format!("fallback_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&owner.pool)
            .await
            .unwrap();
        let mut database_url = reqwest::Url::parse(&root_url).unwrap();
        database_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let scoped = Database::open(database_url.as_str(), false).await.unwrap();
        let ddl = include_str!("../../tests/contracts/http/schema.sql")
            .split("-- Every row below")
            .next()
            .unwrap();
        sqlx::raw_sql(ddl).execute(&scoped.pool).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let mock = Mock {
            pool: scoped.pool.clone(),
            calls: Arc::new(Mutex::new(vec![])),
            fail: case["fail"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| v.as_i64().unwrap())
                .collect(),
            changes: case.get("change_after_first").cloned().unwrap_or(json!({})),
            response: Arc::new(Mutex::new(None)),
            delay: Arc::new(Mutex::new(Duration::ZERO)),
            config_path: Arc::new(Mutex::new(None)),
        };
        let router = Router::new().fallback(upstream).with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let settings = Settings {
            database_url: database_url.to_string(),
            bind: "127.0.0.1:0".into(),
            base_path: String::new(),
            audit_path: root.join("audit.jsonl"),
            usage_path: root.join("state.json"),
            oauth_path: root.join("oauth.json"),
            bark_path: root.join("bark.json"),
            fallback_path: root.join("fallback.json"),
            connection_path: root.join("connection.json"),
            model_path: root.join("models.json"),
            oauth: json!({
                "oauth_recovery_monitor_enabled":true,"oauth_daily_test_enabled":false,
                "oauth_auto_reset_credit_enabled":false,"oauth_daily_test_time":"06:15",
                "oauth_usage_refresh_concurrency":2,"oauth_recovery_test_concurrency":2,
                "oauth_early_probe_batch_size":4,"oauth_recovery_test_model_id":"fixture-model",
                "oauth_recovery_connection_account_ids":[],"oauth_recovery_model_account_ids":[]
            }),
            bark: json!({"enabled":false}),
            bark_valid: true,
            base_url,
            verify_url: String::new(),
            read_only: false,
        };
        let runtime = Runtime::new(settings).await.unwrap();
        runtime.monitor.store.save_admin_token(ADMIN).unwrap();
        for field in ["oauth", "grok", "keys"] {
            for row in case[field].as_array().into_iter().flatten() {
                sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,deleted_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                    .bind(row["id"].as_i64().unwrap())
                    .bind(row["name"].as_str().unwrap())
                    .bind(row["platform"].as_str().unwrap())
                    .bind(row["type"].as_str().unwrap())
                    .bind(row["status"].as_str().unwrap())
                    .bind(row["schedulable"].as_bool().unwrap())
                    .bind(SqlJson(row["credentials"].clone()))
                    .bind(SqlJson(row.get("extra").cloned().unwrap_or(json!({}))))
                    .bind(parse_datetime(&row["deleted_at"]))
                    .bind(parse_datetime(&fixture()["now"]).unwrap())
                    .execute(&runtime.db.pool).await.unwrap();
            }
        }
        Self {
            _directory: directory,
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
        self.mock.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.owner.pool)
            .await
            .unwrap();
        self.owner.close().await;
    }
}

async fn check_case(name: &str) {
    let fixtures = fixture();
    assert_eq!(
        fixtures["reference_sha"],
        "6be7c291ed38dcc337cb88f693639040a3463899"
    );
    let case = fixtures["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap();
    let h = Harness::new(case).await;
    h.runtime
        .config
        .fallback
        .store
        .transaction(|state| {
            *state = case["config"].clone();
            Ok(())
        })
        .unwrap();
    let before = fs::read(&h.runtime.settings.usage_path).unwrap();
    let at = parse_datetime(&fixtures["now"]).unwrap();
    let snapshot = (case["busy"] != true).then_some(&case["snapshot"]);
    for expected in case["reports"].as_array().unwrap() {
        let report = h
            .runtime
            .config
            .fallback
            .run(at, snapshot, |id| {
                Ok(case["gate"].as_array().unwrap().contains(&json!(id)))
            })
            .await
            .unwrap();
        for (field, value) in expected.as_object().unwrap() {
            assert_eq!(&report[field], value, "{name} report field {field}");
        }
    }
    let calls = h.mock.calls.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        case["calls"].as_array().unwrap().len(),
        "{name} exact native request count"
    );
    for (call, expected) in calls.iter().zip(case["calls"].as_array().unwrap()) {
        assert_eq!(call["method"], "POST");
        assert_eq!(
            call["path"],
            format!("/api/v1/admin/accounts/{}/schedulable", expected["id"])
        );
        assert_eq!(
            call["payload"],
            json!({"schedulable":expected["schedulable"]})
        );
        assert_eq!(call["api_key"], ADMIN);
        assert_eq!(call["authorization"], Value::Null);
        assert_eq!(expected["timeout"], 3);
    }
    assert_eq!(
        fs::read(&h.runtime.settings.usage_path).unwrap(),
        before,
        "fallback must not change recovery history"
    );
    h.close().await;
}

#[tokio::test]
async fn config_is_atomic_private_reloadable_and_partial_saves_preserve_coexist() {
    use std::os::unix::fs::PermissionsExt;
    let data = fixture();
    let h = Harness::new(&data["cases"][0]).await;
    let fallback = &h.runtime.config.fallback;
    let loaded = fallback.load();
    assert!(loaded.valid);
    assert!(!loaded.enabled);
    assert!(loaded.ids.is_empty());
    sqlx::query("INSERT INTO accounts (id,platform,type) VALUES (8,'anthropic','apikey'),(9,'openai','oauth')").execute(&h.runtime.db.pool).await.unwrap();
    for id in [1, 8, 9, 99] {
        assert!(fallback
            .save(
                &json!({"enabled":true,"managed_account_ids":[id]}),
                "fixture-user"
            )
            .await
            .is_err());
        assert!(!h.runtime.settings.fallback_path.exists());
    }
    assert!(fallback
        .save(
            &json!({"enabled":true,"managed_account_ids":[99]}),
            "fixture-user"
        )
        .await
        .is_err());
    assert!(!h.runtime.settings.fallback_path.exists());
    let saved = fallback
        .save(
            &json!({"enabled":true,"managed_account_ids":["4",5],"coexist_account_ids":[4,5]}),
            "fixture-user",
        )
        .await
        .unwrap();
    assert_eq!(saved.ids, vec![4, 5]);
    let path = &h.runtime.settings.fallback_path;
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let restarted = KeyFallback::new(h.runtime.actions.clone());
    assert!(restarted.load().valid && restarted.load().enabled);
    assert_eq!(restarted.load().version, saved.version);
    assert_eq!(
        restarted.load().coexist.into_iter().collect::<Vec<_>>(),
        vec![4, 5]
    );
    let current = h.runtime.config.snapshot(Some("key_fallback")).unwrap();
    h.runtime
        .config
        .save(
            "key_fallback",
            &json!({"grok_enabled":false}),
            "fixture-user",
            current["key_fallback"]["revision"].as_str(),
        )
        .await
        .unwrap();
    assert_eq!(
        fallback.load().coexist.into_iter().collect::<Vec<_>>(),
        vec![4, 5]
    );
    fallback
        .save(
            &json!({"enabled":true,"managed_account_ids":[5]}),
            "fixture-user",
        )
        .await
        .unwrap();
    assert_eq!(
        fallback.load().coexist.into_iter().collect::<Vec<_>>(),
        vec![5]
    );
    let before = fs::read(path).unwrap();
    for ids in [
        json!([5, 99]),
        json!([1]),
        json!([0]),
        json!([-1]),
        json!([true]),
        json!([1.5]),
    ] {
        assert!(fallback
            .save(
                &json!({"enabled":true,"managed_account_ids":ids}),
                "fixture-user"
            )
            .await
            .is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
    for coexist in [
        json!([4]),
        json!([true]),
        json!([0]),
        json!([-1]),
        json!([5.5]),
    ] {
        assert!(fallback
            .save(
                &json!({"enabled":true,"managed_account_ids":[5],"coexist_account_ids":coexist}),
                "fixture-user"
            )
            .await
            .is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
    let revision = config_revision(&fallback.load().panel());
    assert!(fallback
        .save_expected(
            &json!({"enabled":true,"managed_account_ids":[5]}),
            "fixture-user",
            Some(&format!("stale-{revision}"))
        )
        .await
        .is_err());
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(h.mock.calls.lock().unwrap().is_empty());
    assert!(fs::read_dir(path.parent().unwrap())
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    h.close().await;
}

#[tokio::test]
async fn corrupt_persisted_ids_and_coexist_fail_closed_without_retarget_or_overwrite() {
    let data = fixture();
    let h = Harness::new(&data["cases"][0]).await;
    let fallback = &h.runtime.config.fallback;
    fallback
        .save(
            &json!({"enabled":true,"managed_account_ids":[4,5]}),
            "fixture-user",
        )
        .await
        .unwrap();
    let path = &h.runtime.settings.fallback_path;
    let baseline: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let mut raws = Vec::new();
    for invalid in [
        json!([1.9]),
        json!([1.0]),
        json!([true]),
        json!([{"id":4}]),
        json!([4,{"id":5}]),
        json!([0]),
        json!([-3]),
        json!([null]),
    ] {
        let mut raw = baseline.clone();
        raw["managed_account_ids"] = invalid;
        raws.push(raw.to_string());
    }
    let mut invalid = baseline.clone();
    invalid["coexist_account_ids"] = json!([99]);
    raws.push(invalid.to_string());
    raws.extend([
        "{\"enabled\":true,\"managed_account_ids\":[Infinity]}".into(),
        "{\"enabled\":true,\"managed_account_ids\":[NaN]}".into(),
    ]);
    for raw in raws {
        fs::write(path, &raw).unwrap();
        let config = fallback.load();
        assert!(!config.valid);
        assert!(config.ids.is_empty());
        let report = fallback
            .run(
                parse_datetime(&data["now"]).unwrap(),
                Some(&data["cases"][0]["snapshot"]),
                |_| Ok(false),
            )
            .await
            .unwrap();
        assert_eq!(report["reason"], "invalid_config");
        assert_eq!(fs::read_to_string(path).unwrap(), raw);
    }
    assert!(h.mock.calls.lock().unwrap().is_empty());
    fs::write(
        path,
        json!({"enabled":true,"managed_account_ids":["4"],"config_version":1}).to_string(),
    )
    .unwrap();
    assert!(fallback.load().valid);
    assert_eq!(fallback.load().ids, vec![4]);
    assert!(fallback.load().coexist.is_empty());
    sqlx::query("UPDATE accounts SET schedulable=true WHERE id=4")
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    let report = fallback
        .run(
            parse_datetime(&data["now"]).unwrap(),
            Some(&data["cases"][1]["snapshot"]),
            |_| Ok(false),
        )
        .await
        .unwrap();
    assert_eq!(report["desired"], false);
    assert_eq!(report["changed_ids"], json!([4]));
    h.close().await;
}

#[tokio::test]
async fn schedulable_http_requires_matching_code_id_boolean_and_bounds_timeout() {
    let data = fixture();
    let h = Harness::new(&data["cases"][0]).await;
    for (response, accepted) in [
        (json!({"code":0,"data":{"id":4,"schedulable":true}}), true),
        (
            json!({"code":"0","data":{"id":"4","schedulable":true}}),
            true,
        ),
        (json!({"code":1,"data":{"id":4,"schedulable":true}}), false),
        (json!({"code":0,"data":{"id":5,"schedulable":true}}), false),
        (json!({"code":0,"data":{"id":4,"schedulable":false}}), false),
        (json!({"code":0,"data":{"id":4}}), false),
        (
            json!({"code":0,"data":{"id":4,"schedulable":"invalid"}}),
            false,
        ),
    ] {
        *h.mock.response.lock().unwrap() = Some(response);
        let result = key_fallback::set_schedulable(&h.runtime.actions, 4, true, ADMIN, 3).await;
        assert_eq!(result["success"], accepted);
    }
    *h.mock.delay.lock().unwrap() = Duration::from_secs(10);
    let before = std::time::Instant::now();
    let result = key_fallback::set_schedulable(&h.runtime.actions, 4, true, ADMIN, 3).await;
    assert_eq!(result["success"], false);
    assert_eq!(result["error_code"], "timeout");
    assert!(before.elapsed() >= Duration::from_millis(2900));
    assert!(before.elapsed() < Duration::from_secs(6));
    assert_eq!(h.mock.calls.lock().unwrap().len(), 8);
    h.close().await;
}

#[tokio::test]
async fn disabling_removing_and_reenabling_evaluate_fresh_without_undoing_prior_writes() {
    let data = fixture();
    let unavailable = &data["cases"][0];
    let available = data["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "any_available")
        .unwrap();
    let h = Harness::new(unavailable).await;
    let fallback = &h.runtime.config.fallback;
    let at = parse_datetime(&data["now"]).unwrap();
    fallback
        .save(
            &json!({"enabled":true,"managed_account_ids":[4,5]}),
            "fixture-user",
        )
        .await
        .unwrap();
    let initial = fallback
        .run(at, Some(&unavailable["snapshot"]), |_| Ok(false))
        .await
        .unwrap();
    assert_eq!(initial["changed_ids"], json!([4]));
    let writes = h.mock.calls.lock().unwrap().len();
    for configuration in [
        json!({"enabled":false,"managed_account_ids":[4,5]}),
        json!({"enabled":true,"managed_account_ids":[]}),
    ] {
        fallback.save(&configuration, "fixture-user").await.unwrap();
        let report = fallback
            .run(at, Some(&available["snapshot"]), |_| Ok(false))
            .await
            .unwrap();
        assert_eq!(report["reason"], "disabled_or_empty");
        assert_eq!(h.mock.calls.lock().unwrap().len(), writes);
        let states: Vec<bool> =
            sqlx::query_scalar("SELECT schedulable FROM accounts WHERE id IN (4,5) ORDER BY id")
                .fetch_all(&h.runtime.db.pool)
                .await
                .unwrap();
        assert_eq!(states, vec![true, true]);
    }
    fallback
        .save(
            &json!({"enabled":true,"managed_account_ids":[4,5]}),
            "fixture-user",
        )
        .await
        .unwrap();
    let report = fallback
        .run(at, Some(&available["snapshot"]), |_| Ok(false))
        .await
        .unwrap();
    assert_eq!(report["desired"], false);
    assert_eq!(report["changed_ids"], json!([4, 5]));
    assert_eq!(h.mock.calls.lock().unwrap().len(), writes + 2);
    h.close().await;
}

#[tokio::test]
async fn dispatch_budget_defers_remaining_accounts_and_next_cycle_resumes_fairly() {
    let data = fixture();
    let mut case = data["cases"][0].clone();
    let original = case["keys"][0].clone();
    case["keys"] = (4..=9)
        .map(|id| {
            let mut row = original.clone();
            row["id"] = id.into();
            row["name"] = format!("fixture-key-{id}").into();
            row["schedulable"] = (id == 9).into();
            row
        })
        .collect::<Vec<_>>()
        .into();
    let h = Harness::new(&case).await;
    let fallback = &h.runtime.config.fallback;
    fallback
        .save(
            &json!({"enabled":true,"managed_account_ids":[4,5,9,6,7,8]}),
            "fixture-user",
        )
        .await
        .unwrap();
    // Each bounded native request finishes before the 3-second timeout. Four
    // calls consume the 10-second dispatch budget; the fifth must wait.
    *h.mock.delay.lock().unwrap() = Duration::from_millis(2600);
    let at = parse_datetime(&data["now"]).unwrap();
    let started = std::time::Instant::now();
    let first = fallback
        .run(at, Some(&case["snapshot"]), |_| Ok(false))
        .await
        .unwrap();
    assert_eq!(first["reason"], "dispatch_budget_exhausted");
    assert_eq!(first["changed_ids"], json!([4, 5, 6, 7]));
    assert!(started.elapsed() >= Duration::from_secs(10));
    assert!(started.elapsed() < Duration::from_secs(15));
    *h.mock.delay.lock().unwrap() = Duration::ZERO;
    let second = fallback
        .run(at, Some(&case["snapshot"]), |_| Ok(false))
        .await
        .unwrap();
    assert_eq!(second["changed_ids"], json!([8]));
    assert_eq!(second["failed_ids"], json!([]));
    let ids: Vec<_> = h
        .mock
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call["path"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        ids,
        [4, 5, 6, 7, 8].map(|id| format!("/api/v1/admin/accounts/{id}/schedulable"))
    );
    h.close().await;
}

async fn check_platform_failures() {
    let data = fixture();
    let original = data["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "opposite_platforms")
        .unwrap();
    for failing in ["openai", "grok"] {
        let mut case = original.clone();
        case["oauth"][0]["schedulable"] = false.into();
        case["keys"][0]["schedulable"] = false.into();
        let h = Harness::new(&case).await;
        let pool = &h.runtime.db.pool;
        let columns:Vec<String>=sqlx::query_scalar("SELECT column_name FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='accounts' ORDER BY ordinal_position").fetch_all(pool).await.unwrap();
        sqlx::raw_sql("ALTER TABLE accounts RENAME TO fixture_account_rows; CREATE FUNCTION fixture_inventory_failure() RETURNS jsonb LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture private inventory details'; END $$;").execute(pool).await.unwrap();
        let columns=columns.into_iter().map(|name| if name=="credentials" {
            format!("CASE WHEN platform='{failing}' AND type='oauth' THEN fixture_inventory_failure() ELSE credentials END AS credentials")
        }else{format!("\"{name}\"")}).collect::<Vec<_>>().join(",");
        sqlx::raw_sql(&format!(
            "CREATE VIEW accounts AS SELECT {columns} FROM fixture_account_rows"
        ))
        .execute(pool)
        .await
        .unwrap();
        let fallback = &h.runtime.config.fallback;
        fallback
            .save(
                &json!({"enabled":true,"managed_account_ids":[4,5]}),
                "fixture-user",
            )
            .await
            .unwrap();
        let result = fallback
            .run(
                parse_datetime(&data["now"]).unwrap(),
                Some(&case["snapshot"]),
                |_| Ok(false),
            )
            .await
            .unwrap();
        assert_eq!(result["platform_reasons"][failing], "inventory_failed");
        assert_eq!(result["desired_by_platform"][failing], Value::Null);
        let id = if failing == "openai" { 5 } else { 4 };
        assert_eq!(result["changed_ids"], json!([id]));
        let calls = h.mock.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0]["path"],
            format!("/api/v1/admin/accounts/{id}/schedulable")
        );
        let audit = fs::read_to_string(&h.runtime.settings.audit_path).unwrap();
        assert!(!audit.contains("fixture private inventory details"));
        assert!(!audit.contains(ADMIN));
        h.close().await;
    }
}

#[tokio::test]
async fn each_platform_inventory_failure_preserves_the_other_platform_and_redacts_details() {
    check_platform_failures().await;
}

#[tokio::test]
async fn unknown_empty_busy_disabled_and_inventory_failure_preserve_existing_state() {
    for name in [
        "mixed_unknown",
        "unknown_empty",
        "unknown_network",
        "unknown_stale",
        "disabled",
        "grok_openai_busy",
    ] {
        check_case(name).await;
    }
    check_platform_failures().await;
}

#[tokio::test]
async fn grok_live_revalidation_and_legacy_disabled_config_preserve_selection_and_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let data = fixture();
    let case = data["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "opposite_platforms")
        .unwrap();
    let h = Harness::new(case).await;
    let fallback = &h.runtime.config.fallback;
    let path = &h.runtime.settings.fallback_path;
    let at = parse_datetime(&data["now"]).unwrap();
    fallback
        .save(
            &json!({"enabled":false,"managed_account_ids":[4]}),
            "fixture-user",
        )
        .await
        .unwrap();
    let old = fs::read(path).unwrap();
    let report = fallback
        .run(at, Some(&case["snapshot"]), |_| Ok(false))
        .await
        .unwrap();
    assert_eq!(report["reason"], "disabled_or_empty");
    assert_eq!(fs::read(path).unwrap(), old);
    fallback
        .save(
            &json!({"enabled":false,"managed_account_ids":[4,5]}),
            "fixture-user",
        )
        .await
        .unwrap();
    let restarted = KeyFallback::new(h.runtime.actions.clone());
    assert_eq!(restarted.load().ids, vec![4, 5]);
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fallback
        .save(
            &json!({"enabled":true,"managed_account_ids":[5]}),
            "fixture-user",
        )
        .await
        .unwrap();
    for change in ["deleted_at=now()", "type='oauth'", "platform='anthropic'"] {
        sqlx::query(&format!("UPDATE accounts SET {change} WHERE id=5"))
            .execute(&h.runtime.db.pool)
            .await
            .unwrap();
        let report = fallback
            .run(at, Some(&case["snapshot"]), |_| Ok(false))
            .await
            .unwrap();
        assert_eq!(report["changed_ids"], json!([]));
        assert!(h.mock.calls.lock().unwrap().is_empty());
        sqlx::query("UPDATE accounts SET platform='grok',type='apikey',deleted_at=NULL WHERE id=5")
            .execute(&h.runtime.db.pool)
            .await
            .unwrap();
    }
    h.close().await;
}

#[test]
fn configured_fallback_path_is_used_without_reading_or_creating_default_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom-fallback.json");
    let environment = std::collections::BTreeMap::from([
        (
            "DATABASE_URL".into(),
            "postgresql://fixture:fixture@127.0.0.1:1/fixture".into(),
        ),
        (
            "OAUTH_CONFIG_PATH".into(),
            directory.path().join("oauth.json").display().to_string(),
        ),
        (
            "BARK_CONFIG_PATH".into(),
            directory.path().join("bark.json").display().to_string(),
        ),
        (
            "SUB2API_CONFIG_PATH".into(),
            directory
                .path()
                .join("connection.json")
                .display()
                .to_string(),
        ),
        (
            "KEY_FALLBACK_CONFIG_PATH".into(),
            path.display().to_string(),
        ),
    ]);
    let settings = Settings::from_env(&environment, false).unwrap();
    assert_eq!(settings.fallback_path, path);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn supported_key_inventory_excludes_other_platforms_oauth_and_deleted_accounts() {
    let rows = json!([
        {"id":8,"platform":"grok","type":"apikey"},
        {"id":4,"platform":"openai","type":"apikey"},
        {"id":9,"platform":"anthropic","type":"apikey"},
        {"id":10,"platform":"openai","type":"oauth"},
        {"id":11,"platform":"openai","type":"apikey","deleted_at":"2026-10-01T00:00:00+00:00"}
    ]);
    let mut accepted = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| key_fallback::live_key(row, None))
        .map(|row| row["id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    accepted.sort();
    assert_eq!(accepted, vec![4, 8]);
}

#[tokio::test]
async fn unsupported_or_unknown_grok_inventory_keeps_existing_key_state() {
    let data = fixture();
    let case = data["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "opposite_platforms")
        .unwrap();
    let h = Harness::new(case).await;
    let fallback = &h.runtime.config.fallback;
    let pool = &h.runtime.db.pool;
    sqlx::query("ALTER TABLE accounts ALTER COLUMN schedulable DROP NOT NULL")
        .execute(pool)
        .await
        .unwrap();
    fallback
        .save(
            &json!({"enabled":true,"managed_account_ids":[5]}),
            "fixture-user",
        )
        .await
        .unwrap();
    for mutation in [
        "schedulable=NULL",
        "platform='openai'",
        "deleted_at=now()",
        "type='apikey'",
    ] {
        sqlx::query(&format!("UPDATE accounts SET {mutation} WHERE id=2"))
            .execute(pool)
            .await
            .unwrap();
        let report = fallback
            .run(
                parse_datetime(&data["now"]).unwrap(),
                Some(&case["snapshot"]),
                |_| Ok(false),
            )
            .await
            .unwrap();
        assert_eq!(report["desired_by_platform"]["grok"], Value::Null);
        assert!(h.mock.calls.lock().unwrap().is_empty());
        sqlx::query("UPDATE accounts SET schedulable=false,platform='grok',type='oauth',deleted_at=NULL WHERE id=2").execute(pool).await.unwrap();
    }
    sqlx::query("DELETE FROM accounts WHERE id=2")
        .execute(pool)
        .await
        .unwrap();
    let report = fallback
        .run(
            parse_datetime(&data["now"]).unwrap(),
            Some(&case["snapshot"]),
            |_| Ok(false),
        )
        .await
        .unwrap();
    assert_eq!(report["desired_by_platform"]["grok"], Value::Null);
    assert!(h.mock.calls.lock().unwrap().is_empty());
    h.close().await;
}

#[tokio::test]
async fn manual_disable_detaches_coexist_before_native_write_and_cannot_be_reopened() {
    let data = fixture();
    let case = data["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "any_available")
        .unwrap();
    let h = Harness::new(case).await;
    let fallback = &h.runtime.config.fallback;
    fallback
        .save(
            &json!({"enabled":true,"managed_account_ids":[4,5],"coexist_account_ids":[4,5]}),
            "fixture-user",
        )
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET schedulable=true WHERE id IN (4,5)")
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    *h.mock.config_path.lock().unwrap() = Some(h.runtime.settings.fallback_path.clone());
    let row = h.runtime.actions.account(4, None).await.unwrap();
    let request = json!({"expected_version":sub2ops_server::compat::account_version(&row),"schedulable":false,"detach_managed":true});
    let response = h.runtime.set_schedulable(4, &request, ADMIN).await.unwrap();
    assert_eq!(response["verified"], true);
    assert_eq!(response["detached"], true);
    let calls = h.mock.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["config"]["managed_account_ids"], json!([5]));
    assert_eq!(calls[0]["config"]["coexist_account_ids"], json!([5]));
    let result = fallback
        .run(
            parse_datetime(&data["now"]).unwrap(),
            Some(&case["snapshot"]),
            |_| Ok(false),
        )
        .await
        .unwrap();
    assert_eq!(result["changed_ids"], json!([]));
    assert_eq!(h.mock.calls.lock().unwrap().len(), 1);
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT schedulable FROM accounts WHERE id=4")
            .fetch_one(&h.runtime.db.pool)
            .await
            .unwrap()
    );
    h.close().await;
}

// Each scenario compares two complete real dispatch cycles with the pinned oracle.
#[tokio::test]
async fn all_unavailable() {
    check_case("all_unavailable").await;
}
#[tokio::test]
async fn any_available() {
    check_case("any_available").await;
}
#[tokio::test]
async fn unknown_network() {
    check_case("unknown_network").await;
}
#[tokio::test]
async fn unknown_empty() {
    check_case("unknown_empty").await;
}
#[tokio::test]
async fn unknown_stale() {
    check_case("unknown_stale").await;
}
#[tokio::test]
async fn mixed_unknown() {
    check_case("mixed_unknown").await;
}
#[tokio::test]
async fn partial_failure() {
    check_case("partial_failure").await;
}
#[tokio::test]
async fn dispatch_filters() {
    check_case("dispatch_filters").await;
}
#[tokio::test]
async fn live_revalidation() {
    check_case("live_revalidation").await;
}
#[tokio::test]
async fn opposite_platforms() {
    check_case("opposite_platforms").await;
}
#[tokio::test]
async fn grok_openai_busy() {
    check_case("grok_openai_busy").await;
}
#[tokio::test]
async fn grok_unknown() {
    check_case("grok_unknown").await;
}
#[tokio::test]
async fn disabled() {
    check_case("disabled").await;
}
#[tokio::test]
async fn empty_selection() {
    check_case("empty_selection").await;
}
#[tokio::test]
async fn coexist_available() {
    check_case("coexist_available").await;
}
#[tokio::test]
async fn coexist_unknown() {
    check_case("coexist_unknown").await;
    check_case("coexist_missing").await;
    check_case("coexist_stale").await;
}
#[tokio::test]
async fn coexist_unavailable() {
    check_case("coexist_unavailable").await;
}
#[tokio::test]
async fn coexist_held() {
    check_case("coexist_held").await;
}
