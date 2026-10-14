use axum::{
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, types::Json as SqlJson, PgPool};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::Duration as StdDuration,
};
use sub2ops_server::{
    account_locks::AccountLocks,
    account_templates::Templates,
    actions::Actions,
    compat::{datetime, parse_datetime},
    db::Database,
    oauth_queries::{Coordinator, QueryOptions, Source},
    oauth_state::OAuthState,
    queries::Queries,
    runtime::Runtime,
    settings::Settings,
    upstream::Upstream,
    usage_records::{Filter, UsageRecords},
};
use tokio::{
    net::TcpListener,
    sync::{Notify, Semaphore},
    task::JoinHandle,
    time::timeout,
};

const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";
const ADMIN_KEY: &str = "fixture-query-history-admin-key";

fn contract() -> Value {
    serde_json::from_str(include_str!("../../tests/contracts/query/contract.json"))
        .expect("pinned query contract fixture must parse")
}

fn contract_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("SERVER_QUERY_HISTORY_DIR")
            .expect("query-history runner must provide an owned temporary directory"),
    )
}

fn database_url(application_name: &str) -> String {
    let raw = std::env::var("DATABASE_URL")
        .expect("query-history runner must provide a temporary PostgreSQL URL");
    let mut url = reqwest::Url::parse(&raw).expect("temporary PostgreSQL URL must parse");
    url.query_pairs_mut()
        .append_pair("application_name", application_name);
    url.to_string()
}

fn reference_pages() -> Value {
    let path = std::env::var("SERVER_QUERY_HISTORY_REFERENCE_PAGES")
        .expect("query-history runner must provide the pinned Python page oracle");
    serde_json::from_slice(&fs::read(path).expect("read pinned Python page oracle"))
        .expect("pinned Python page oracle must parse")
}

fn pg_log_path() -> PathBuf {
    PathBuf::from(
        std::env::var("SERVER_QUERY_HISTORY_PG_LOG")
            .expect("query-history runner must provide its temporary PostgreSQL log"),
    )
}

fn is_postgres_log_record_start(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() >= 19
        && bytes[0..4].iter().all(|byte| byte.is_ascii_digit())
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(|byte| byte.is_ascii_digit())
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(|byte| byte.is_ascii_digit())
        && bytes[10] == b' '
        && bytes[11..13].iter().all(|byte| byte.is_ascii_digit())
        && bytes[13] == b':'
        && bytes[14..16].iter().all(|byte| byte.is_ascii_digit())
        && bytes[16] == b':'
        && bytes[17..19].iter().all(|byte| byte.is_ascii_digit())
}

fn normalize_statement(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn flush_statement(active: &mut Option<String>, statements: &mut Vec<String>) {
    if let Some(query) = active.take() {
        statements.push(normalize_statement(&query));
    }
}

fn parse_logged_statements(log: &str, application_name: &str) -> Vec<String> {
    let marker = format!("[{application_name}]");
    let mut statements = Vec::new();
    let mut active = None;
    for line in log.lines() {
        if is_postgres_log_record_start(line) {
            flush_statement(&mut active, &mut statements);
            if !line.contains(&marker) {
                continue;
            }
            active = line
                .split_once("statement:")
                .map(|(_, query)| query)
                .or_else(|| {
                    line.split_once("execute ")
                        .and_then(|(_, execute)| execute.split_once(':').map(|(_, query)| query))
                })
                .map(str::to_owned);
        } else if let Some(query) = active.as_mut() {
            query.push(' ');
            query.push_str(line.trim());
        }
    }
    flush_statement(&mut active, &mut statements);
    statements
}

fn logged_statements(application_name: &str) -> Vec<String> {
    let log = fs::read_to_string(pg_log_path()).expect("read temporary PostgreSQL statement log");
    parse_logged_statements(&log, application_name)
}

fn assert_multiline_log_parser_fixture() {
    let log = concat!(
        "2026-10-14 12:00:00.000 UTC [1234] [query-history-parser-fixture] LOG:  statement: SELECT u.id,\n",
        "    u.created_at\n",
        "FROM usage_logs u\n",
        "WHERE u.account_id = $1\n",
        "LIMIT $4\n",
        "2026-10-14 12:00:00.001 UTC [1234] [query-history-parser-fixture] DETAIL:  parameters: $1 = 'fixture-only-value'\n",
        "2026-10-14 12:00:00.002 UTC [1235] [other-fixture] LOG:  statement: SELECT ignored\n",
    );
    let statements = parse_logged_statements(log, "query-history-parser-fixture");
    assert_eq!(
        statements,
        vec!["select u.id, u.created_at from usage_logs u where u.account_id = $1 limit $4"]
    );
    assert!(statements
        .iter()
        .all(|statement| !statement.contains("fixture-only-value")));
}

fn transactions(statements: &[String]) -> Vec<Vec<String>> {
    let mut complete = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for statement in statements {
        if statement == "begin" {
            if let Some(previous) = current.take() {
                complete.push(previous);
            }
            current = Some(vec![statement.clone()]);
            continue;
        }
        if let Some(active) = current.as_mut() {
            active.push(statement.clone());
            if statement == "commit" || statement == "rollback" {
                complete.push(current.take().unwrap());
            }
        }
    }
    if let Some(active) = current {
        complete.push(active);
    }
    complete
}

fn settings(root: &Path, database_url: String, read_only: bool, base_url: &str) -> Arc<Settings> {
    fs::create_dir_all(root).expect("create owned settings directory");
    let connection_path = root.join("sub2api-config.json");
    fs::write(
        &connection_path,
        serde_json::to_vec(&json!({"base_url":base_url,"verify_base_url":base_url}))
            .expect("serialize synthetic loopback connection"),
    )
    .expect("write synthetic loopback connection");
    Arc::new(Settings {
        database_url,
        bind: "127.0.0.1:0".into(),
        base_path: String::new(),
        audit_path: root.join("audit.jsonl"),
        usage_path: root.join("oauth-state.json"),
        oauth_path: root.join("oauth-config.json"),
        bark_path: root.join("bark-config.json"),
        fallback_path: root.join("key-fallback-config.json"),
        connection_path,
        model_path: root.join("group-model-config.json"),
        oauth: json!({
            "oauth_auto_reset_credit_enabled": false,
            "oauth_daily_test_enabled": false,
            "oauth_daily_test_time": "23:59",
            "oauth_early_probe_batch_size": 8,
            "oauth_recovery_connection_account_ids": [],
            "oauth_recovery_model_account_ids": [],
            "oauth_recovery_monitor_enabled": false,
            "oauth_recovery_test_concurrency": 1,
            "oauth_recovery_test_model_id": "fixture-model",
            "oauth_usage_refresh_concurrency": 1
        }),
        bark: json!({"enabled":false,"device_key":"","server_url":""}),
        bark_valid: true,
        base_url: base_url.into(),
        verify_url: base_url.into(),
        read_only,
    })
}

async fn pool(application_name: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url(application_name))
        .await
        .expect("connect to runner-owned PostgreSQL")
}

async fn seed_account(pool: &PgPool, id: i64, name: &str, credentials: Value) {
    sqlx::query(
        "INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra) \
         VALUES ($1,$2,'openai','oauth','active',true,$3,$4) ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(name)
    .bind(SqlJson(credentials))
    .bind(SqlJson(json!({})))
    .execute(pool)
    .await
    .expect("seed synthetic OAuth account");
}

async fn seed_usage_rows(
    pool: &PgPool,
    account_id: i64,
    first_id: i64,
    timestamps: &[DateTime<Utc>],
) {
    sqlx::query(
        "INSERT INTO accounts (id,name,platform,type,status,schedulable) \
         VALUES ($1,$2,'openai','oauth','active',false) ON CONFLICT (id) DO NOTHING",
    )
    .bind(account_id)
    .bind(format!("fixture-query-account-{account_id}"))
    .execute(pool)
    .await
    .expect("seed usage account");
    for (offset, created_at) in timestamps.iter().enumerate() {
        sqlx::query(
            "INSERT INTO usage_logs \
             (id,account_id,request_id,model,requested_model,upstream_model,request_type, \
              actual_cost,created_at) \
             VALUES ($1,$2,$3,'fixture-query-model','fixture-query-model', \
                     'fixture-query-model',1,0.1250000000,$4)",
        )
        .bind(first_id + offset as i64)
        .bind(account_id)
        .bind(format!(
            "fixture-query-request-{}",
            first_id + offset as i64
        ))
        .bind(*created_at)
        .execute(pool)
        .await
        .expect("seed synthetic usage record");
    }
}

fn id_list(value: &Value) -> Vec<i64> {
    value
        .as_array()
        .expect("expected an array of record IDs")
        .iter()
        .map(|item| item.as_i64().expect("record ID must be an integer"))
        .collect()
}

#[tokio::test]
async fn event_pagination_matches_pinned_python_across_category_account_pages() {
    let fixed = contract();
    assert_eq!(fixed["reference_sha"], REFERENCE_SHA);
    let oracle = reference_pages();
    assert_eq!(oracle["reference_sha"], REFERENCE_SHA);
    let rows = oracle["rows"].as_array().expect("legacy event rows");
    let db = Database::open(&database_url("query-history-events"), true)
        .await
        .expect("open actual business schema as a read-only Rust query database");
    let seed = pool("query-history-event-seed").await;

    for account in oracle["accounts"].as_array().unwrap() {
        sqlx::query(
            "INSERT INTO accounts (id,name,platform,type,deleted_at) VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(account["id"].as_i64().unwrap())
        .bind(account["name"].as_str().unwrap())
        .bind(account["platform"].as_str().unwrap())
        .bind(account["type"].as_str().unwrap())
        .bind(account["deleted_at"].as_str().map(|value| {
            DateTime::parse_from_rfc3339(value)
                .expect("legacy deleted timestamp must parse")
                .with_timezone(&Utc)
        }))
        .execute(&seed)
        .await
        .expect("seed pinned legacy account rows into actual business schema");
    }
    for group in oracle["groups"].as_array().unwrap() {
        sqlx::query(
            "INSERT INTO groups (id,name,deleted_at) VALUES ($1,$2,$3) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(group["id"].as_i64().unwrap())
        .bind(group["name"].as_str().unwrap())
        .bind(group["deleted_at"].as_str().map(|value| {
            DateTime::parse_from_rfc3339(value)
                .expect("legacy deleted group timestamp must parse")
                .with_timezone(&Utc)
        }))
        .execute(&seed)
        .await
        .expect("seed pinned legacy groups into actual business schema");
    }
    // Historical events may outlive either entity. Match the pinned event
    // fixture schema instead of inventing placeholder account/group names.
    for constraint in [
        "ops_error_logs_account_id_fkey",
        "ops_error_logs_group_id_fkey",
    ] {
        sqlx::query(&format!(
            "ALTER TABLE ops_error_logs DROP CONSTRAINT {constraint}"
        ))
        .execute(&seed)
        .await
        .expect("allow orphan history only in the disposable fixture database");
    }
    let removed_unrelated_event = sqlx::query("DELETE FROM ops_error_logs WHERE id=92001")
        .execute(&seed)
        .await
        .expect("remove the schema's unrelated synthetic event before oracle comparison");
    assert_eq!(
        removed_unrelated_event.rows_affected(),
        1,
        "temporary business schema must start with its one unrelated fixture event"
    );
    for row in rows {
        let upstream_errors = if row["upstream_errors"].is_null() {
            json!([])
        } else {
            row["upstream_errors"].clone()
        };
        sqlx::query(
            "INSERT INTO ops_error_logs \
             (id,account_id,group_id,created_at,platform,error_owner,error_phase,error_source, \
              stream,model,requested_model,upstream_model,status_code,upstream_status_code, \
              provider_error_code,error_type,error_message,upstream_error_message,error_body, \
              upstream_error_detail,upstream_errors,request_id,resolved) \
             VALUES ($1,$2,$3,'2026-10-08T03:00:00Z',$4,$5,$6,$7,$8,$9,$10,$11,$12,$13, \
                     $14,$15,$16,$17,$18,$19,$20,$21,$22)",
        )
        .bind(row["id"].as_i64().unwrap())
        .bind(row["account_id"].as_i64())
        .bind(row["group_id"].as_i64())
        .bind(row["platform"].as_str())
        .bind(row["error_owner"].as_str())
        .bind(row["error_phase"].as_str())
        .bind(row["error_source"].as_str())
        .bind(row["stream"].as_bool().unwrap_or(false))
        .bind(row["model"].as_str())
        .bind(row["requested_model"].as_str())
        .bind(row["upstream_model"].as_str())
        .bind(row["status_code"].as_i64().map(|value| value as i32))
        .bind(
            row["upstream_status_code"]
                .as_i64()
                .map(|value| value as i32),
        )
        .bind(row["provider_error_code"].as_str())
        .bind(row["error_type"].as_str())
        .bind(row["error_message"].as_str())
        .bind(row["upstream_error_message"].as_str())
        .bind(row["error_body"].as_str())
        .bind(row["upstream_error_detail"].as_str())
        .bind(SqlJson(upstream_errors))
        .bind(row["request_id"].as_str())
        .bind(row["resolved"].as_bool().unwrap_or(false))
        .execute(&seed)
        .await
        .expect("seed pinned legacy event row into actual business schema");
    }
    seed.close().await;

    let queries = Queries {
        db: db.clone(),
        oauth: OAuthState::new(contract_dir().join("events-readonly-state.json"), true),
    };
    let page_size = fixed["event_pagination"]["page_size"].as_u64().unwrap() as usize;
    let mut expected_scopes = Vec::new();
    for category in fixed["event_pagination"]["categories"].as_array().unwrap() {
        for account_id in fixed["event_pagination"]["account_ids"].as_array().unwrap() {
            expected_scopes.push((category.clone(), account_id.clone()));
        }
    }
    let actual_scopes = oracle["combinations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|combination| {
            (
                combination["category"].clone(),
                combination["account_id"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual_scopes, expected_scopes);
    let mut combinations = 0usize;
    for combination in oracle["combinations"].as_array().unwrap() {
        combinations += 1;
        let category = combination["category"].as_str();
        let account_id = combination["account_id"].as_i64();
        let expected_ids = id_list(&combination["expected_ids"]);
        let expected_pages = combination["pages"].as_array().unwrap();
        let mut cursor = None;
        let mut seen = Vec::new();
        for (index, expected_page) in expected_pages.iter().enumerate() {
            let actual = queries
                .errors(account_id, cursor, page_size, category)
                .await
                .expect("execute actual Rust PostgreSQL error query");
            let actual_ids = actual["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["id"].as_i64().unwrap())
                .collect::<Vec<_>>();
            let page_expected = id_list(&expected_page["ids"]);
            assert_eq!(
                actual_ids, page_expected,
                "pinned Python page mismatch for category={category:?}, account={account_id:?}, page={index}"
            );
            assert!(
                actual_ids.iter().all(|id| !seen.contains(id)),
                "Rust pagination overlapped pages for category={category:?}, account={account_id:?}"
            );
            seen.extend(actual_ids.iter().copied());
            let expected_cursor = expected_page["next_cursor"].as_i64();
            assert_eq!(actual["next_cursor"].as_i64(), expected_cursor);
            if expected_cursor.is_some() {
                assert_eq!(expected_cursor, actual_ids.last().copied());
            }
            cursor = expected_cursor;
        }
        assert_eq!(
            seen, expected_ids,
            "all pages must preserve the old filter result"
        );
        assert_eq!(
            cursor, None,
            "the final Rust page must terminate pagination"
        );
    }
    assert_eq!(combinations, 12, "three categories × four account scopes");
    for expected in oracle["details"].as_array().unwrap() {
        let id = expected["id"].as_i64().unwrap();
        let actual = queries
            .error_detail(id, expected["category"].as_str())
            .await;
        if expected["status"] == 404 {
            assert_eq!(actual.unwrap_err().status.as_u16(), 404);
        } else {
            assert_eq!(actual.unwrap(), expected["value"], "event detail {id}");
        }
    }
    let before_invalid = logged_statements("query-history-events");
    for category in ["", "all", "legacy", "Degradation", "other OR true"] {
        assert_eq!(
            queries
                .errors(None, None, 2, Some(category))
                .await
                .unwrap_err()
                .status
                .as_u16(),
            422
        );
        assert_eq!(
            queries
                .error_detail(1, Some(category))
                .await
                .unwrap_err()
                .status
                .as_u16(),
            422
        );
    }
    assert_eq!(logged_statements("query-history-events"), before_invalid);
    assert_event_routes_match_queries(&oracle).await;
    db.close().await;
}

async fn assert_event_routes_match_queries(oracle: &Value) {
    let auth_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let auth_address = auth_listener.local_addr().unwrap();
    let auth = tokio::spawn(async move {
        let app = Router::new().fallback(any(|request: Request<Body>| async move {
            assert_eq!(request.method(), Method::GET);
            assert_eq!(request.headers().get("x-api-key").unwrap(), ADMIN_KEY);
            assert!(request.headers().get("authorization").is_none());
            Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"code":0,"data":[]}"#))
                .unwrap()
        }));
        axum::serve(auth_listener, app).await.unwrap();
    });
    let settings = settings(
        &contract_dir().join("event-http-runtime"),
        database_url("query-history-events-http"),
        true,
        &format!("http://{auth_address}"),
    );
    let runtime = Runtime::new(settings.as_ref().clone()).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = sub2ops_server::api::router(runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for combination in oracle["combinations"].as_array().unwrap() {
        let mut params = vec![("limit", "3".to_owned())];
        if let Some(category) = combination["category"].as_str() {
            params.push(("category", category.to_owned()));
        }
        if let Some(account) = combination["account_id"].as_i64() {
            params.push(("account_id", account.to_string()));
        }
        for page in combination["pages"].as_array().unwrap() {
            let response = client
                .get(format!("http://{address}/api/desktop/v1/errors"))
                .header("x-api-key", ADMIN_KEY)
                .query(&params)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            let body: Value = response.json().await.unwrap();
            assert_eq!(
                body["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v["id"].clone())
                    .collect::<Vec<_>>(),
                page["ids"].as_array().unwrap().clone()
            );
            assert_eq!(body["next_cursor"], page["next_cursor"]);
            params.retain(|(key, _)| *key != "before_id");
            if let Some(cursor) = page["next_cursor"].as_i64() {
                params.push(("before_id", cursor.to_string()));
            }
        }
    }
    for expected in oracle["details"].as_array().unwrap() {
        let mut request = client
            .get(format!(
                "http://{address}/api/desktop/v1/errors/{}",
                expected["id"]
            ))
            .header("x-api-key", ADMIN_KEY);
        if let Some(category) = expected["category"].as_str() {
            request = request.query(&[("category", category)]);
        }
        let response = request.send().await.unwrap();
        assert_eq!(
            response.status().as_u16(),
            expected["status"].as_u64().unwrap() as u16
        );
        if expected["status"] == 200 {
            assert_eq!(response.json::<Value>().await.unwrap(), expected["value"]);
        }
    }
    let before = logged_statements("query-history-events-http");
    for category in ["", "all", "legacy", "Degradation", "other OR true"] {
        for path in ["errors", "errors/1"] {
            let response = client
                .get(format!("http://{address}/api/desktop/v1/{path}"))
                .header("x-api-key", ADMIN_KEY)
                .query(&[("category", category)])
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 422);
        }
    }
    assert_eq!(logged_statements("query-history-events-http"), before);
    server.abort();
    let _ = server.await;
    auth.abort();
    let _ = auth.await;
    runtime.close().await;
}

async fn assert_invalid_cursor_does_not_reach_database(
    guard_db: &Database,
    admin_pool: &PgPool,
    filter: &Filter,
    at: DateTime<Utc>,
    before_statements: usize,
    application_name: &str,
    label: &str,
) {
    sqlx::query("ALTER TABLE usage_logs RENAME TO usage_logs_cursor_guard")
        .execute(admin_pool)
        .await
        .expect("temporarily hide usage_logs for the cursor guard");
    let result = UsageRecords(guard_db.clone()).list_at(filter, at).await;
    sqlx::query("ALTER TABLE usage_logs_cursor_guard RENAME TO usage_logs")
        .execute(admin_pool)
        .await
        .expect("restore usage_logs after the cursor guard");
    let error = result.expect_err("changed cursor filters must be rejected");
    assert_eq!(error.status.as_u16(), 422, "filter change: {label}");
    assert_eq!(
        logged_statements(application_name).len(),
        before_statements,
        "filter change {label} issued a PostgreSQL statement before rejecting the cursor"
    );
}

#[tokio::test]
async fn each_changed_usage_filter_is_rejected_before_database_access() {
    let page_size = contract()["usage_records"]["page_size"].as_u64().unwrap() as usize;
    let seed = pool("query-history-filter-seed").await;
    let now = Utc::now();
    let account_id = 940_020;
    let first_id = 930_201;
    seed_usage_rows(
        &seed,
        account_id,
        first_id,
        &[
            now - Duration::minutes(3),
            now - Duration::minutes(2),
            now - Duration::minutes(1),
        ],
    )
    .await;
    seed.close().await;

    let source_db = Database::open(&database_url("query-history-cursor-source"), false)
        .await
        .expect("open source database for cursor creation");
    let source_filter = Filter {
        account_id: Some(account_id),
        limit: Some(page_size),
        ..Filter::default()
    };
    let first = UsageRecords(source_db.clone())
        .list_at(&source_filter, now)
        .await
        .expect("create real PostgreSQL page cursor");
    assert_eq!(first["items"][0]["id"], first_id + 2);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    source_db.close().await;

    let guard_db = Database::open(&database_url("query-history-cursor-guard"), false)
        .await
        .expect("open isolated cursor guard database");
    let admin_pool = pool("query-history-cursor-admin").await;
    let baseline_log_count = logged_statements("query-history-cursor-guard").len();
    let mut changed = source_filter.clone();
    changed.cursor = Some(cursor.clone());
    let fixed = contract();
    for field in fixed["usage_records"]["cursor_filter_changes"]
        .as_array()
        .unwrap()
    {
        let name = field.as_str().unwrap();
        let mut mutation = changed.clone();
        match name {
            "from_at" => mutation.from_at = Some(datetime(now - Duration::hours(23))),
            "to_at" => mutation.to_at = Some(datetime(now + Duration::minutes(1))),
            "account_id" => mutation.account_id = Some(account_id + 1),
            "api_key_id" => mutation.api_key_id = Some(4501),
            "user_id" => mutation.user_id = Some(4401),
            "model" => mutation.model = Some("fixture-changed-model".into()),
            "request_type" => mutation.request_type = Some("stream".into()),
            "mismatch_only" => mutation.mismatch_only = true,
            other => panic!("unrecognized pinned filter field {other}"),
        }
        assert_invalid_cursor_does_not_reach_database(
            &guard_db,
            &admin_pool,
            &mutation,
            now,
            baseline_log_count,
            "query-history-cursor-guard",
            name,
        )
        .await;
    }
    admin_pool.close().await;
    guard_db.close().await;
}

#[tokio::test]
async fn usage_date_cursor_keeps_window_and_rejects_changed_or_omitted_dates_before_query() {
    let page_size = contract()["usage_records"]["page_size"].as_u64().unwrap() as usize;
    let seed = pool("query-history-date-seed").await;
    let account_id = 940_030;
    let start_day = (Utc::now() + Duration::hours(8)).date_naive();
    let local_midnight = start_day.and_hms_opt(0, 0, 0).unwrap();
    let day_start = Utc.from_utc_datetime(&(local_midnight - Duration::hours(8)));
    let at = day_start + Duration::hours(12);
    let first_id = 930_301;
    seed_usage_rows(
        &seed,
        account_id,
        first_id,
        &[
            day_start + Duration::minutes(10),
            day_start + Duration::minutes(20),
            day_start + Duration::minutes(30),
        ],
    )
    .await;
    seed.close().await;

    let start_date = start_day.format("%Y-%m-%d").to_string();
    let end_date = start_date.clone();
    let db = Database::open(&database_url("query-history-date-source"), false)
        .await
        .expect("open date cursor database");
    let initial = Filter {
        start_date: Some(start_date.clone()),
        end_date: Some(end_date.clone()),
        account_id: Some(account_id),
        limit: Some(page_size),
        ..Filter::default()
    };
    let first = UsageRecords(db.clone())
        .list_at(&initial, at)
        .await
        .expect("create real date-window cursor");
    assert_eq!(first["items"][0]["id"], first_id + 2);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let later_at = at + Duration::days(10);
    let second = UsageRecords(db.clone())
        .list_at(
            &Filter {
                cursor: Some(cursor.clone()),
                include_summary: true,
                ..initial.clone()
            },
            later_at,
        )
        .await
        .expect("date cursor freezes its original time window");
    assert_eq!(second["items"][0]["id"], first_id + 1);
    assert!(second.get("summary").is_none());
    assert_eq!(second["latest_id"], first["latest_id"]);

    let guard_db = Database::open(&database_url("query-history-date-cursor-guard"), false)
        .await
        .expect("open isolated date cursor guard database");
    let admin_pool = pool("query-history-date-cursor-admin").await;
    let baseline_log_count = logged_statements("query-history-date-cursor-guard").len();
    for field in contract()["usage_records"]["date_cursor_changes"]
        .as_array()
        .unwrap()
    {
        let name = field.as_str().unwrap();
        let mut mutation = initial.clone();
        mutation.cursor = Some(cursor.clone());
        match name {
            "omitted" => {
                mutation.start_date = None;
                mutation.end_date = None;
            }
            "start_date" => {
                let previous = NaiveDate::parse_from_str(&start_date, "%Y-%m-%d")
                    .unwrap()
                    .pred_opt()
                    .unwrap();
                mutation.start_date = Some(previous.format("%Y-%m-%d").to_string());
            }
            "end_date" => {
                let next = NaiveDate::parse_from_str(&end_date, "%Y-%m-%d")
                    .unwrap()
                    .succ_opt()
                    .unwrap();
                mutation.end_date = Some(next.format("%Y-%m-%d").to_string());
            }
            other => panic!("unrecognized pinned date mutation {other}"),
        }
        assert_invalid_cursor_does_not_reach_database(
            &guard_db,
            &admin_pool,
            &mutation,
            later_at,
            baseline_log_count,
            "query-history-date-cursor-guard",
            name,
        )
        .await;
    }
    admin_pool.close().await;
    guard_db.close().await;
    db.close().await;
}

#[tokio::test]
async fn usage_list_count_and_detail_are_bounded_read_only_select_transactions() {
    assert_multiline_log_parser_fixture();
    let seed = pool("query-history-read-seed").await;
    let account_id = 940_010;
    let first_id = 930_101;
    let at = Utc::now();
    seed_usage_rows(
        &seed,
        account_id,
        first_id,
        &[
            at - Duration::minutes(3),
            at - Duration::minutes(2),
            at - Duration::minutes(1),
        ],
    )
    .await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_logs WHERE account_id=$1")
        .bind(account_id)
        .fetch_one(&seed)
        .await
        .unwrap();
    let latest_id_before: i64 = sqlx::query_scalar("SELECT coalesce(max(id),0) FROM usage_logs")
        .fetch_one(&seed)
        .await
        .unwrap();
    seed.close().await;

    let db = Database::open(&database_url("query-history-usage"), false)
        .await
        .expect("open database for usage reads");
    let service = UsageRecords(db.clone());
    let filter = Filter {
        account_id: Some(account_id),
        limit: Some(2),
        ..Filter::default()
    };
    let listed = service
        .list_at(&filter, at)
        .await
        .expect("list usage records in a bounded read-only transaction");
    let listed_ids = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(listed_ids, vec![first_id + 2, first_id + 1]);
    assert!(listed["next_cursor"].is_string());
    let counted = service
        .list_at(
            &Filter {
                after_id: Some(first_id - 1),
                ..filter
            },
            at,
        )
        .await
        .expect("count new records in a bounded read-only transaction");
    assert_eq!(counted["new_count"], 3);
    assert_eq!(counted["latest_id"], latest_id_before);
    let detail = service
        .detail(first_id + 1)
        .await
        .expect("read one usage record in a bounded read-only transaction");
    assert_eq!(detail["id"], first_id + 1);
    assert_eq!(detail["account_id"], account_id);
    db.close().await;

    let verify = pool("query-history-read-verify").await;
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_logs WHERE account_id=$1")
        .bind(account_id)
        .fetch_one(&verify)
        .await
        .unwrap();
    verify.close().await;
    assert_eq!(
        after, before,
        "usage list/count/detail did not alter the fixture rows"
    );

    let statements = logged_statements("query-history-usage");
    let txs = transactions(&statements);
    assert_eq!(
        txs.len(),
        3,
        "one list, one count, and one detail transaction"
    );
    let mut repeatable = 0;
    let mut read_only_detail = 0;
    let timeout_setting = contract()["usage_records"]["statement_timeout"]
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    for tx in &txs {
        assert_eq!(tx.first().map(String::as_str), Some("begin"));
        assert_eq!(tx.last().map(String::as_str), Some("commit"));
        let setup = tx
            .iter()
            .position(|sql| sql.contains("set transaction"))
            .expect("transaction must explicitly set read-only mode");
        let setup_sql = &tx[setup];
        if setup_sql.contains("repeatable read") {
            assert!(setup_sql.contains("read only"));
            repeatable += 1;
        } else {
            assert_eq!(setup_sql, "set transaction read only");
            read_only_detail += 1;
        }
        let bounded = tx
            .iter()
            .position(|sql| sql.contains("set local statement_timeout"))
            .expect("transaction must set a local statement timeout");
        assert!(
            setup < bounded,
            "read-only mode precedes timeout/query work"
        );
        assert!(
            tx[bounded].contains(&format!("'{timeout_setting}'")),
            "read transaction must use the pinned 5-second timeout"
        );
        for sql in &tx[bounded + 1..tx.len() - 1] {
            assert!(sql.starts_with("select "), "only SELECT is allowed: {sql}");
            assert!(
                !sql.contains("select *") && !sql.contains("select u.*"),
                "usage query must use an explicit projection: {sql}"
            );
            for secret_column in [
                "credentials",
                "password",
                "request_body",
                "response_body",
                "request_headers",
                "response_headers",
            ] {
                assert!(
                    !sql.contains(secret_column),
                    "private column selected: {sql}"
                );
            }
        }
    }
    assert_eq!(
        repeatable, 2,
        "list and count use REPEATABLE READ, READ ONLY"
    );
    assert_eq!(read_only_detail, 1, "detail uses READ ONLY");
    let bounded_transactions = txs
        .iter()
        .filter(|tx| {
            tx.iter()
                .any(|sql| sql.starts_with("select ") && sql.contains("limit"))
        })
        .count();
    assert_eq!(
        bounded_transactions, 1,
        "exactly one read transaction contains the page-bounded list query"
    );
}

#[tokio::test]
async fn usage_collection_detail_and_options_reject_all_write_methods_over_http() {
    let root = contract_dir().join("usage-http-runtime");
    let runtime_settings = settings(&root, database_url("query-history-http"), true, "");
    let runtime = Runtime::new(runtime_settings.as_ref().clone())
        .await
        .expect("create read-only Runtime for the actual Axum router");
    let before = logged_statements("query-history-http");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an owned loopback HTTP listener");
    let address = listener.local_addr().unwrap();
    let application = sub2ops_server::api::router(runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, application)
            .await
            .expect("serve the local Rust router");
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build a no-proxy loopback client");
    let paths = [
        "/api/desktop/v1/usage-records",
        "/api/desktop/v1/usage-records/91001",
        "/api/desktop/v1/usage-record-options?kind=users",
    ];
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        for path in paths {
            let response = client
                .request(method.clone(), format!("http://{address}{path}"))
                .send()
                .await
                .expect("send a local usage-route method request");
            assert_eq!(
                response.status(),
                reqwest::StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path}"
            );
        }
    }
    server.abort();
    let _ = server.await;
    assert_eq!(
        logged_statements("query-history-http"),
        before,
        "rejected HTTP write methods must not issue a database query"
    );
    runtime.close().await;
}

#[derive(Clone)]
struct QueryMock {
    state_path: PathBuf,
    status: StatusCode,
    blocked: bool,
    calls: Arc<StdMutex<Vec<(String, String)>>>,
    reservations: Arc<StdMutex<Vec<Option<Value>>>>,
    entered: Arc<Notify>,
    release: Arc<Semaphore>,
}

impl QueryMock {
    fn new(state_path: PathBuf, status: u16, blocked: bool) -> Self {
        Self {
            state_path,
            status: StatusCode::from_u16(status).expect("valid fixture HTTP status"),
            blocked,
            calls: Arc::new(StdMutex::new(Vec::new())),
            reservations: Arc::new(StdMutex::new(Vec::new())),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Semaphore::new(0)),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

async fn query_mock(State(mock): State<QueryMock>, request: Request<Body>) -> Response {
    let method = request.method().to_string();
    let path = request
        .uri()
        .path_and_query()
        .map(|v| v.as_str().to_owned())
        .unwrap_or_else(|| request.uri().path().to_owned());
    mock.calls.lock().unwrap().push((method, path));
    let reservation = fs::read(&mock.state_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    mock.reservations.lock().unwrap().push(reservation);
    mock.entered.notify_one();
    if mock.blocked {
        if let Ok(permit) = mock.release.acquire().await {
            permit.forget();
        }
    }
    Response::builder()
        .status(mock.status)
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .expect("build local synthetic upstream response")
}

async fn spawn_query_mock(mock: QueryMock) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a temporary loopback upstream");
    let address = listener.local_addr().unwrap();
    let application = Router::new().fallback(any(query_mock)).with_state(mock);
    let server = tokio::spawn(async move {
        axum::serve(listener, application)
            .await
            .expect("serve synthetic loopback Sub2API");
    });
    (format!("http://{address}"), server)
}

async fn coordinator_fixture(
    root: &Path,
    id: i64,
    base_url: &str,
    state_path: &Path,
    application_name: &str,
) -> (Database, OAuthState, Coordinator) {
    let database = Database::open(&database_url(application_name), false)
        .await
        .expect("open OAuth query PostgreSQL database");
    let seed = pool("query-history-oauth-seed").await;
    seed_account(
        &seed,
        id,
        &format!("fixture-query-oauth-{id}"),
        json!({"plan_type":"plus","access_token":"fixture-query-oauth-token"}),
    )
    .await;
    seed.close().await;
    let state = OAuthState::new(state_path.to_owned(), false);
    let settings = settings(root, database_url(application_name), false, base_url);
    let queries = Queries {
        db: database.clone(),
        oauth: state.clone(),
    };
    let upstream = Upstream::new(settings.clone()).expect("construct loopback upstream client");
    let actions = Actions::new(
        queries,
        upstream,
        AccountLocks::default(),
        Templates::new(root.join("account-templates.json"), false),
        settings,
    )
    .expect("construct OAuth query actions");
    let coordinator = Coordinator::new(actions, Arc::new(|| 2));
    (database, state, coordinator)
}

fn query_options(at: DateTime<Utc>, timeout_seconds: u64) -> QueryOptions {
    QueryOptions {
        source: Source::Automatic,
        reason: "fixture-query-history-contract".into(),
        at,
        timeout: timeout_seconds,
        ..QueryOptions::default()
    }
}

fn timestamp(value: &Value) -> DateTime<Utc> {
    parse_datetime(value).expect("OAuth persisted timestamp must parse")
}

#[tokio::test]
async fn oauth_failures_persist_one_three_six_twelve_hour_backoff_and_skip_dispatch() {
    let fixed = contract();
    assert_eq!(fixed["reference_sha"], REFERENCE_SHA);
    let expected_delays = fixed["oauth_query"]["backoff_hours"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect::<Vec<_>>();
    let root = contract_dir().join("oauth-backoff");
    let id = 940_041;
    let state_path = root.join("oauth-state.json");
    let mock = QueryMock::new(
        state_path.clone(),
        fixed["oauth_query"]["failure_http_status"]
            .as_u64()
            .unwrap() as u16,
        false,
    );
    let (base_url, server) = spawn_query_mock(mock.clone()).await;
    let (database, state, coordinator) = coordinator_fixture(
        &root,
        id,
        &base_url,
        &state_path,
        "query-history-oauth-backoff",
    )
    .await;
    let mut at = Utc.with_ymd_and_hms(2026, 9, 29, 20, 0, 0).unwrap();
    for (index, hours) in expected_delays.iter().enumerate() {
        let attempt_time = at;
        let result = coordinator
            .query(id, ADMIN_KEY, query_options(attempt_time, 2))
            .await;
        assert_eq!(
            result["success"],
            false,
            "failure {} must remain unsuccessful",
            index + 1
        );
        assert_eq!(result["error_code"], "http_503");
        assert_eq!(
            mock.call_count(),
            index + 1,
            "one upstream call per failed attempt"
        );
        let saved = state.snapshot().expect("read persisted OAuth state");
        let query = &saved["scheduler"][id.to_string()]["quota_query"];
        assert_eq!(query["failure_count"], index as i64 + 1);
        let retry_at = attempt_time + Duration::hours(*hours);
        assert_eq!(timestamp(&query["retry_at"]), retry_at);
        let skip_at = retry_at
            - Duration::seconds(
                fixed["oauth_query"]["skip_before_deadline_seconds"]
                    .as_i64()
                    .unwrap(),
            );
        let skipped = coordinator
            .query(id, ADMIN_KEY, query_options(skip_at, 2))
            .await;
        assert_eq!(skipped["success"], false);
        assert_eq!(skipped["skipped"], true);
        assert_eq!(timestamp(&skipped["next_query_at"]), retry_at);
        assert_eq!(
            mock.call_count(),
            index + 1,
            "pre-deadline backoff must not dispatch another upstream request"
        );
        at = retry_at;
    }
    assert_eq!(mock.call_count(), 5);
    assert!(mock.calls.lock().unwrap().iter().all(|(method, path)| {
        method == "GET" && path.contains("/api/v1/admin/accounts/940041/usage")
    }));
    coordinator.drain(1).await;
    database.close().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn oauth_timeout_reads_disk_reservation_and_consumes_attempt_without_replay() {
    let fixed = contract();
    let root = contract_dir().join("oauth-timeout");
    let id = 940_042;
    let state_path = root.join("oauth-state.json");
    let mock = QueryMock::new(
        state_path.clone(),
        fixed["oauth_query"]["failure_http_status"]
            .as_u64()
            .unwrap() as u16,
        true,
    );
    let (base_url, server) = spawn_query_mock(mock.clone()).await;
    let (database, state, coordinator) = coordinator_fixture(
        &root,
        id,
        &base_url,
        &state_path,
        "query-history-oauth-timeout",
    )
    .await;
    let now = Utc.with_ymd_and_hms(2026, 9, 29, 20, 0, 0).unwrap();
    let timeout_seconds = fixed["oauth_query"]["timeout_seconds"].as_u64().unwrap();
    let coordinator_for_query = coordinator.clone();
    let query_task = tokio::spawn(async move {
        coordinator_for_query
            .query(id, ADMIN_KEY, query_options(now, timeout_seconds))
            .await
    });
    timeout(StdDuration::from_secs(5), mock.entered.notified())
        .await
        .expect("loopback handler must observe the real coordinator request");
    assert_eq!(mock.call_count(), 1);
    let reservation = mock.reservations.lock().unwrap()[0]
        .clone()
        .expect("upstream handler must read state from disk before responding");
    let pending = &reservation["scheduler"][id.to_string()]["quota_query"];
    assert_eq!(
        pending["automatic_attempts"]
            .as_array()
            .unwrap()
            .iter()
            .map(timestamp)
            .collect::<Vec<_>>(),
        vec![now]
    );
    assert_eq!(timestamp(&pending["last_query_at"]), now);
    assert_eq!(pending["last_source"], "automatic");

    let result = timeout(StdDuration::from_secs(8), query_task)
        .await
        .expect("coordinator request should finish at the configured upstream timeout")
        .expect("coordinator task should not panic");
    mock.release.add_permits(1);
    assert_eq!(result["success"], false);
    assert_eq!(result["error_code"], "timeout");
    assert_ne!(result["skipped"], true);
    let saved = state.snapshot().expect("read final persistent query state");
    let query = &saved["scheduler"][id.to_string()]["quota_query"];
    assert_eq!(
        query["automatic_attempts"]
            .as_array()
            .unwrap()
            .iter()
            .map(timestamp)
            .collect::<Vec<_>>(),
        vec![now]
    );
    assert_eq!(query["failure_count"], 1);
    let retry_at = now + Duration::hours(1);
    assert_eq!(timestamp(&query["retry_at"]), retry_at);

    let before_deadline = retry_at
        - Duration::seconds(
            fixed["oauth_query"]["skip_before_deadline_seconds"]
                .as_i64()
                .unwrap(),
        );
    let skipped = coordinator
        .query(id, ADMIN_KEY, query_options(before_deadline, 2))
        .await;
    assert_eq!(skipped["skipped"], true);
    assert_eq!(timestamp(&skipped["next_query_at"]), retry_at);
    assert_eq!(
        mock.call_count(),
        1,
        "unknown timed-out request must not be replayed"
    );
    coordinator.drain(1).await;
    database.close().await;
    server.abort();
    let _ = server.await;
}
