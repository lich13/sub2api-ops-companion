use axum::{
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, types::Json as SqlJson, PgPool};
use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};
use sub2ops_server::{
    db::Database,
    runtime::Runtime,
    settings::Settings,
    usage_records::{date_window, project, timestamp, Filter, UsageRecords},
};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";
const USAGE_SCHEMA: &str = "usage_records_parity";
const CONCURRENT_WRITE_LEGACY_ID: &str = "py:tests.test_usage_records:UsageRecordSummaryTests.test_summary_and_rows_share_read_transaction_and_watermark_during_concurrent_write";
const NATIVE_COMPACTION_SNAPSHOT_ID: &str =
    "d013e5a949e6bbab592a8504abed01c6a07a0b6ae17caefd85ff13f78a776e9f";
const ADMIN_KEY: &str = "fixture-admin-key";
const FIXTURES: &str = include_str!("../../tests/contracts/usage/fixtures.json");
const USAGE_SCHEMA_SQL: &str = include_str!("../../tests/contracts/usage/schema.sql");
static SQL_LOG_BARRIER_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn fixtures() -> Value {
    serde_json::from_str(FIXTURES).expect("fixed Python usage fixtures must parse")
}

fn database_url(application_name: &str) -> String {
    let raw = std::env::var("DATABASE_URL")
        .expect("usage parity runner must provide a temporary PostgreSQL URL");
    let mut url = reqwest::Url::parse(&raw).expect("temporary PostgreSQL URL must parse");
    url.query_pairs_mut()
        .append_pair("application_name", application_name)
        .append_pair("options", &format!("-csearch_path={USAGE_SCHEMA}"));
    url.to_string()
}

async fn prepare_schema() -> (PgPool, PgPool) {
    let raw = std::env::var("DATABASE_URL")
        .expect("usage parity runner must provide a temporary PostgreSQL URL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&raw)
        .await
        .expect("connect to owned temporary PostgreSQL");
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {USAGE_SCHEMA} CASCADE"))
        .execute(&admin)
        .await
        .expect("remove the prior owned usage test schema");
    sqlx::query(&format!("CREATE SCHEMA {USAGE_SCHEMA}"))
        .execute(&admin)
        .await
        .expect("create the isolated usage test schema");
    sqlx::query(&format!("SET search_path TO {USAGE_SCHEMA}"))
        .execute(&admin)
        .await
        .expect("scope synthetic usage tables to the isolated schema");
    for statement in USAGE_SCHEMA_SQL
        .split(';')
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        sqlx::query(statement)
            .execute(&admin)
            .await
            .expect("create a synthetic usage table");
    }
    let seed = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url("usage-parity-seed"))
        .await
        .expect("connect to isolated usage schema for fixture seeding");
    let search_path: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&seed)
        .await
        .expect("read the isolated usage schema name");
    assert_eq!(search_path, USAGE_SCHEMA);
    (admin, seed)
}

async fn remove_schema(admin: &PgPool) {
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {USAGE_SCHEMA} CASCADE"))
        .execute(admin)
        .await
        .expect("remove the owned usage test schema");
}

async fn seed_snapshot(seed: &PgPool, fixture: &Value, snapshot_id: &str) {
    let snapshot = fixture["snapshots"]
        .get(snapshot_id)
        .expect("legacy operation snapshot exists");
    sqlx::query("TRUNCATE usage_logs, api_keys, users, accounts, groups")
        .execute(seed)
        .await
        .expect("clear only the owned usage fixture tables");
    for table in ["users", "api_keys", "accounts", "groups", "usage_logs"] {
        let rows = snapshot[table]
            .as_array()
            .expect("snapshot table rows are arrays");
        for row in rows {
            let statement = format!(
                "INSERT INTO {table} SELECT * FROM jsonb_populate_record(NULL::{table}, $1::jsonb)"
            );
            sqlx::query(&statement)
                .bind(SqlJson(row.clone()))
                .execute(seed)
                .await
                .unwrap_or_else(|_| panic!("seed synthetic {table} row"));
        }
    }
}

fn parse_logged_statements(log: &str, application_name: &str) -> Vec<String> {
    fn is_record_start(line: &str) -> bool {
        let bytes = line.as_bytes();
        bytes.len() >= 19
            && bytes[0..4].iter().all(u8::is_ascii_digit)
            && bytes[4] == b'-'
            && bytes[5..7].iter().all(u8::is_ascii_digit)
            && bytes[7] == b'-'
            && bytes[8..10].iter().all(u8::is_ascii_digit)
            && bytes[10] == b' '
            && bytes[11..13].iter().all(u8::is_ascii_digit)
            && bytes[13] == b':'
            && bytes[14..16].iter().all(u8::is_ascii_digit)
            && bytes[16] == b':'
            && bytes[17..19].iter().all(u8::is_ascii_digit)
    }

    let marker = format!("[{application_name}]");
    let mut statements = Vec::new();
    let mut active: Option<String> = None;
    for line in log.lines() {
        if is_record_start(line) {
            if let Some(query) = active.take() {
                statements.push(
                    query
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .to_ascii_lowercase(),
                );
            }
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
        } else if !line.trim_start().starts_with("DETAIL:") {
            if let Some(query) = active.as_mut() {
                query.push(' ');
                query.push_str(line.trim());
            }
        }
    }
    if let Some(query) = active {
        statements.push(
            query
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase(),
        );
    }
    statements
}

fn logged_statements(application_name: &str) -> Vec<String> {
    let path = std::env::var("SERVER_USAGE_PARITY_PG_LOG")
        .expect("usage parity runner must provide its temporary PostgreSQL log");
    parse_logged_statements(
        &fs::read_to_string(path).expect("read the owned temporary PostgreSQL log"),
        application_name,
    )
}

async fn sql_log_barrier(pool: &PgPool, application_name: &str) -> String {
    let sequence = SQL_LOG_BARRIER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let marker = format!("usage-parity-log-barrier-{sequence}");
    let statement = format!("SELECT '{marker}'");
    sqlx::query(&statement)
        .execute(pool)
        .await
        .expect("execute a read-only SQL log barrier");
    for _ in 0..100 {
        if logged_statements(application_name)
            .iter()
            .any(|logged| logged.contains(&marker))
        {
            return marker;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("PostgreSQL log did not record barrier {marker}");
}

fn is_pool_session_setup_statement(statement: &str) -> bool {
    matches!(
        statement,
        "set time zone 'utc'" | "set default_transaction_read_only = on"
    )
}

fn operation_statements(
    application_name: &str,
    before_barrier: &str,
    after_barrier: &str,
) -> Vec<String> {
    let statements = logged_statements(application_name);
    let before = statements
        .iter()
        .rposition(|statement| statement.contains(before_barrier))
        .unwrap_or_else(|| panic!("missing PostgreSQL before-barrier {before_barrier}"));
    let after = statements
        .iter()
        .rposition(|statement| statement.contains(after_barrier))
        .unwrap_or_else(|| panic!("missing PostgreSQL after-barrier {after_barrier}"));
    assert!(before < after, "SQL log barriers are out of order");
    statements[before + 1..after]
        .iter()
        .filter(|statement| !is_pool_session_setup_statement(statement))
        .cloned()
        .collect()
}

fn assert_read_only_queries(statements: &[String], context: &str) {
    for statement in statements {
        let sql = statement.trim();
        if sql.is_empty() {
            continue;
        }
        let allowed_control = sql == "begin"
            || sql == "commit"
            || sql == "rollback"
            || sql.starts_with("set time zone ")
            || sql.starts_with("set transaction ")
            || sql.starts_with("set local statement_timeout");
        if !allowed_control {
            assert!(
                sql.starts_with("select "),
                "{context}: unexpected statement {sql}"
            );
        }
        assert!(
            !sql.contains("select *") && !sql.contains("select u.*"),
            "{context}: wildcard projection {sql}"
        );
        assert!(
            !sql.contains("credentials")
                && !sql.contains("password")
                && !sql.contains("request_body")
                && !sql.contains("response_body")
                && !sql.contains("request_headers")
                && !sql.contains("response_headers"),
            "{context}: private column selected"
        );
        assert!(
            !sql.starts_with("insert ")
                && !sql.starts_with("update ")
                && !sql.starts_with("delete ")
                && !sql.starts_with("alter ")
                && !sql.starts_with("drop ")
                && !sql.starts_with("truncate "),
            "{context}: read path attempted a write"
        );
    }
}

fn assert_native_compaction_probe(statements: &[String], context: &str) {
    let probes = statements
        .iter()
        .filter(|statement| {
            statement.contains("pg_catalog.pg_attribute")
                && statement.contains("to_regclass('usage_logs')")
                && statement.contains("native_compaction_v2")
        })
        .count();
    assert_eq!(
        probes, 1,
        "{context}: expected one native compaction catalog probe"
    );
}

fn assert_native_compaction_record(value: &Value, context: &str) {
    let field = value
        .get("native_compaction_v2")
        .unwrap_or_else(|| panic!("{context}: record lacks native_compaction_v2"));
    assert_eq!(
        field,
        &json!(false),
        "{context}: legacy native_compaction_v2 must remain false"
    );
}

fn assert_native_compaction_response(value: &Value, context: &str) {
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        for (index, item) in items.iter().enumerate() {
            assert_native_compaction_record(item, &format!("{context}; item {index}"));
        }
    } else if value.get("id").is_some() {
        assert_native_compaction_record(value, context);
    }
}

fn strip_native_compaction_record(value: &Value) -> Value {
    let Value::Object(object) = value else {
        panic!("native compaction response record must be an object")
    };
    let mut record = object.clone();
    record.remove("native_compaction_v2");
    Value::Object(record)
}

fn strip_native_compaction_response(value: &Value) -> Value {
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        let mut response = value.clone();
        response["items"] =
            Value::Array(items.iter().map(strip_native_compaction_record).collect());
        response
    } else if value.get("id").is_some() {
        strip_native_compaction_record(value)
    } else {
        value.clone()
    }
}

fn rfc3339(value: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value.as_str().expect("fixture timestamp is text"))
        .expect("fixture timestamp has a timezone")
        .with_timezone(&Utc)
}

fn error_status<T>(result: sub2ops_server::error::Result<T>) -> Result<T, u16> {
    result.map_err(|error| error.status.as_u16())
}

async fn replay_direct_case(legacy_id: &str) {
    let fixture = fixtures();
    assert_eq!(fixture["reference_sha"], REFERENCE_SHA);
    let events = fixture["traces"][legacy_id]
        .as_array()
        .unwrap_or_else(|| panic!("missing pinned direct trace for {legacy_id}"));
    assert!(
        !events.is_empty(),
        "empty pinned direct trace for {legacy_id}"
    );
    let has_database = events.iter().any(|event| event.get("snapshot").is_some());
    let mut admin: Option<PgPool> = None;
    let mut seed: Option<PgPool> = None;
    let mut database: Option<Database> = None;
    if has_database {
        let (admin_pool, seed_pool) = prepare_schema().await;
        let service_database = Database::open(&database_url("usage-parity-direct"), true)
            .await
            .expect("open an isolated read-only usage database");
        admin = Some(admin_pool);
        seed = Some(seed_pool);
        database = Some(service_database);
    }
    if legacy_id == CONCURRENT_WRITE_LEGACY_ID {
        assert_summary_snapshot_with_concurrent_write(
            admin.as_ref().expect("temporary PostgreSQL admin pool"),
            seed.as_ref().expect("temporary PostgreSQL seed pool"),
            &fixture,
            &events[0],
        )
        .await;
    }
    for event in events {
        let operation = event["operation"].as_str().expect("legacy operation kind");
        match operation {
            "project" => {
                let actual = project(&event["input"]).expect("project legacy row");
                assert_native_compaction_record(&actual, &format!("legacy projection {legacy_id}"));
                let comparable = strip_native_compaction_record(&actual);
                assert_eq!(
                    comparable, event["expected"],
                    "legacy projection {legacy_id}"
                );
                assert!(
                    !actual.to_string().contains("PRIVATE_"),
                    "projection leaked a fixture secret"
                );
            }
            "timestamp" => {
                let input = event["input"].as_str().unwrap_or("");
                let actual = timestamp(input).map(|value| value.to_rfc3339());
                if let Some(status) = event["error_status"].as_u64() {
                    assert_eq!(
                        actual.map_err(|error| error.status.as_u16()),
                        Err(status as u16),
                        "legacy timestamp {legacy_id}"
                    );
                } else {
                    assert_eq!(
                        actual.expect("valid timestamp"),
                        event["expected"].as_str().unwrap(),
                        "legacy timestamp {legacy_id}"
                    );
                }
            }
            "list" | "options" | "detail" => {
                let snapshot_id = event["snapshot"].as_str().expect("operation snapshot id");
                let seed = seed.as_ref().expect("temporary PostgreSQL seed pool");
                seed_snapshot(seed, &fixture, snapshot_id).await;
                let database = database.as_ref().expect("read-only PostgreSQL service");
                let application_name = "usage-parity-direct";
                let before_barrier = sql_log_barrier(&database.pool, application_name).await;
                let service = UsageRecords(database.clone());
                let result: sub2ops_server::error::Result<Value> = match operation {
                    "list" => {
                        let kwargs = event["input"]["kwargs"].clone();
                        let filter: Filter = serde_json::from_value(kwargs)
                            .expect("legacy list kwargs map to the Rust filter");
                        service.list_at(&filter, rfc3339(&event["now"])).await
                    }
                    "options" => {
                        let kwargs = &event["input"]["kwargs"];
                        let kind = kwargs["kind"].as_str().expect("directory kind");
                        let q = kwargs["q"].as_str();
                        let user_id = kwargs["user_id"].as_i64();
                        let cursor = kwargs["cursor"].as_str();
                        let limit = kwargs["limit"].as_u64().unwrap_or(50) as usize;
                        service.options(kind, q, user_id, cursor, limit).await
                    }
                    "detail" => {
                        let id = event["input"]["args"][0]
                            .as_i64()
                            .expect("legacy detail ID is an integer");
                        service.detail(id).await
                    }
                    _ => unreachable!(),
                };
                let after_barrier = sql_log_barrier(&database.pool, application_name).await;
                let statements =
                    operation_statements(application_name, &before_barrier, &after_barrier);
                let database_delta = event["database_statement_delta"].as_i64().unwrap_or(0);
                if database_delta == 0 {
                    assert_eq!(
                        statements.len(),
                        0,
                        "legacy rejected before SQL: {legacy_id}; input={:?}",
                        event["input"]
                    );
                } else {
                    assert!(
                        !statements.is_empty(),
                        "legacy database operation emitted no SQL: {legacy_id}; input={:?}",
                        event["input"]
                    );
                    let context = format!("{legacy_id}; input={:?}", event["input"]);
                    assert_read_only_queries(&statements, &context);
                }
                if let Some(status) = event["error_status"].as_u64() {
                    assert_eq!(
                        error_status(result),
                        Err(status as u16),
                        "legacy service error {legacy_id}; input={:?}",
                        event["input"]
                    );
                } else {
                    let actual = result.expect("Rust usage service response");
                    let actual = if matches!(operation, "list" | "detail") {
                        assert_native_compaction_response(
                            &actual,
                            &format!("legacy operation {legacy_id}"),
                        );
                        strip_native_compaction_response(&actual)
                    } else {
                        actual
                    };
                    assert_eq!(
                        actual, event["expected"],
                        "legacy operation {legacy_id}; input={:?}",
                        event["input"]
                    );
                    assert!(
                        !actual.to_string().contains("PRIVATE_"),
                        "usage response leaked a fixture secret"
                    );
                }
            }
            other => panic!("unrecognized pinned operation {other}"),
        }
    }
    if let Some(database) = database {
        database.close().await;
    }
    if let Some(seed) = seed {
        seed.close().await;
    }
    if let Some(admin) = admin {
        remove_schema(&admin).await;
        admin.close().await;
    }
}

async fn wait_for_summary_advisory_wait(admin: &PgPool, application_name: &str) {
    for _ in 0..250 {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
             WHERE application_name = $1 AND state = 'active' \
             AND wait_event_type = 'Lock' AND wait_event = 'advisory' \
             AND query ILIKE '%sum(u.actual_cost)%')",
        )
        .bind(application_name)
        .fetch_one(admin)
        .await
        .expect("observe the synthetic summary query waiting for an advisory lock");
        if waiting {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("summary SELECT did not reach the test advisory-lock barrier");
}

async fn assert_summary_snapshot_with_concurrent_write(
    admin: &PgPool,
    seed: &PgPool,
    fixture: &Value,
    event: &Value,
) {
    let snapshot_id = event["snapshot"].as_str().expect("legacy summary snapshot");
    seed_snapshot(seed, fixture, snapshot_id).await;
    sqlx::query("ALTER TABLE usage_logs RENAME TO usage_logs_parity_data")
        .execute(seed)
        .await
        .expect("move fixture usage rows behind the test-only view");
    sqlx::query(
        "CREATE FUNCTION usage_parity_summary_gate() RETURNS boolean \
         LANGUAGE plpgsql VOLATILE AS $$ \
         BEGIN \
             IF pg_catalog.current_query() ILIKE '%sum(u.actual_cost)%' THEN \
                 PERFORM pg_catalog.pg_advisory_lock(902014, 4040); \
                 PERFORM pg_catalog.pg_advisory_unlock(902014, 4040); \
             END IF; \
             RETURN TRUE; \
         END; \
         $$",
    )
    .execute(seed)
    .await
    .expect("create the test-only summary lock gate");
    sqlx::query(
        "CREATE VIEW usage_logs AS \
         SELECT data.* FROM usage_logs_parity_data AS data \
         WHERE usage_parity_summary_gate()",
    )
    .execute(seed)
    .await
    .expect("route usage reads through the test-only summary gate");

    let blocker_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url("usage-parity-summary-blocker"))
        .await
        .expect("connect the advisory-lock blocker to the isolated schema");
    let mut blocker_connection = blocker_pool
        .acquire()
        .await
        .expect("acquire the advisory-lock blocker connection");
    sqlx::query("SELECT pg_advisory_lock(902014, 4040)")
        .execute(&mut *blocker_connection)
        .await
        .expect("hold the summary lock until the concurrent write commits");

    let application_name = "usage-parity-summary-concurrent";
    let summary_database = Database::open(&database_url(application_name), true)
        .await
        .expect("open the isolated read-only concurrent-summary service");
    let first_filter = Filter {
        include_summary: true,
        limit: Some(1),
        ..Filter::default()
    };
    let observed_at = rfc3339(&event["now"]);
    let first_service = UsageRecords(summary_database.clone());
    let summary_task =
        tokio::spawn(async move { first_service.list_at(&first_filter, observed_at).await });
    wait_for_summary_advisory_wait(admin, application_name).await;

    let writer = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url("usage-parity-summary-writer"))
        .await
        .expect("connect the concurrent writer to the isolated schema");
    let mut transaction = writer
        .begin()
        .await
        .expect("begin the concurrent usage write");
    sqlx::query(
        "INSERT INTO usage_logs_parity_data (id, created_at, actual_cost) \
         VALUES (3, $1, 4.0)",
    )
    .bind(rfc3339(&event["now"]) - chrono::Duration::hours(1))
    .execute(&mut *transaction)
    .await
    .expect("insert a concurrent backdated usage row");
    let updated = sqlx::query("UPDATE usage_logs_parity_data SET actual_cost = 99.0 WHERE id = 2")
        .execute(&mut *transaction)
        .await
        .expect("update a row visible to the old summary and page");
    assert_eq!(updated.rows_affected(), 1);
    transaction
        .commit()
        .await
        .expect("commit the concurrent usage write while summary is blocked");
    let writer_sum: String =
        sqlx::query_scalar("SELECT sum(actual_cost)::text FROM usage_logs_parity_data")
            .fetch_one(&writer)
            .await
            .expect("confirm the concurrent transaction committed");
    assert_eq!(writer_sum, "103.1");

    let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock(902014, 4040)")
        .fetch_one(&mut *blocker_connection)
        .await
        .expect("release the test-only summary barrier");
    assert!(unlocked, "the test connection held the summary barrier");
    let first = tokio::time::timeout(Duration::from_secs(5), summary_task)
        .await
        .expect("the summary query completed after releasing the barrier")
        .expect("the concurrent summary task did not panic")
        .expect("the read-only summary call succeeded");
    summary_database.close().await;
    assert_eq!(
        first["latest_id"], 2,
        "watermark uses the pre-write snapshot"
    );
    assert_eq!(first["summary"], json!({"actual_cost":"0.3"}));
    assert_eq!(first["items"][0]["id"], 2);
    assert_eq!(first["items"][0]["actual_cost"], "0.2");
    let cursor = first["next_cursor"]
        .as_str()
        .expect("the first page retains the pre-write cursor")
        .to_owned();

    let continuation_database = Database::open(&database_url(application_name), true)
        .await
        .expect("reopen the isolated read-only service for the cursor continuation");
    let continuation = UsageRecords(continuation_database.clone());
    let page = continuation
        .list_at(
            &Filter {
                cursor: Some(cursor),
                include_summary: true,
                limit: Some(1),
                ..Filter::default()
            },
            rfc3339(&event["now"]),
        )
        .await
        .expect("read the cursor continuation after the concurrent commit");
    assert_eq!(page["latest_id"], 2, "cursor preserves the old watermark");
    assert_eq!(page["items"][0]["id"], 1);
    assert!(page["next_cursor"].is_null());
    assert!(page.get("summary").is_none());

    let new_count = continuation
        .list_at(
            &Filter {
                after_id: Some(2),
                include_summary: true,
                limit: Some(1),
                ..Filter::default()
            },
            rfc3339(&event["now"]),
        )
        .await
        .expect("count the row committed after the original watermark");
    assert_eq!(new_count["new_count"], 1);
    assert_eq!(new_count["latest_id"], 3);
    let refreshed = continuation
        .list_at(
            &Filter {
                include_summary: true,
                ..Filter::default()
            },
            rfc3339(&event["now"]),
        )
        .await
        .expect("refresh the summary after the concurrent commit");
    assert_eq!(refreshed["latest_id"], 3);
    assert_eq!(refreshed["summary"]["actual_cost"], "103.1");

    continuation_database.close().await;
    writer.close().await;
    drop(blocker_connection);
    blocker_pool.close().await;
    sqlx::query("DROP VIEW usage_logs")
        .execute(seed)
        .await
        .expect("remove the test-only usage view");
    sqlx::query("DROP FUNCTION usage_parity_summary_gate()")
        .execute(seed)
        .await
        .expect("remove the test-only summary gate");
    sqlx::query("ALTER TABLE usage_logs_parity_data RENAME TO usage_logs")
        .execute(seed)
        .await
        .expect("restore the fixture usage table for the pinned trace replay");
}

#[derive(Clone, Default)]
struct AuthMock {
    calls: Arc<StdMutex<Vec<(String, String)>>>,
}

async fn auth_mock(State(mock): State<AuthMock>, request: Request<Body>) -> Response {
    let method = request.method().to_string();
    let path = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_owned())
        .unwrap_or_else(|| request.uri().path().to_owned());
    mock.calls.lock().unwrap().push((method, path.clone()));
    let accepted = path == "/api/v1/admin/groups/all"
        && request
            .headers()
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            == Some(ADMIN_KEY);
    let status = if accepted {
        StatusCode::OK
    } else {
        StatusCode::FORBIDDEN
    };
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(if accepted { "[]" } else { "{}" }))
        .expect("build a local synthetic auth response")
}

async fn spawn_auth_mock(mock: AuthMock) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback-only auth mock");
    let address = listener.local_addr().unwrap();
    let application = Router::new().fallback(any(auth_mock)).with_state(mock);
    let server = tokio::spawn(async move {
        axum::serve(listener, application)
            .await
            .expect("serve the synthetic loopback auth endpoint");
    });
    (format!("http://{address}"), server)
}

fn settings(root: &Path, database_url: String, base_url: &str) -> Arc<Settings> {
    fs::create_dir_all(root).expect("create owned usage route state directory");
    let connection_path = root.join("sub2api-config.json");
    fs::write(
        &connection_path,
        serde_json::to_vec(&json!({"base_url":base_url,"verify_base_url":base_url}))
            .expect("serialize loopback-only upstream fixture"),
    )
    .expect("write synthetic loopback upstream configuration");
    Arc::new(Settings {
        database_url,
        bind: "127.0.0.1:0".into(),
        base_path: String::new(),
        audit_path: root.join("audit.jsonl"),
        usage_path: root.join("oauth-state.json"),
        oauth_path: root.join("oauth-config.json"),
        bark_path: root.join("bark-config.json"),
        connection_path,
        model_path: root.join("group-model-config.json"),
        keepalive_path: root.join("keepalive-state.json"),
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
        read_only: true,
    })
}

fn stable_response(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut result = serde_json::Map::new();
            for (key, child) in object {
                if key == "observed_at" {
                    continue;
                }
                if key == "next_cursor" {
                    result.insert(key.clone(), json!(!child.is_null()));
                } else {
                    result.insert(key.clone(), stable_response(child));
                }
            }
            Value::Object(result)
        }
        Value::Array(values) => Value::Array(values.iter().map(stable_response).collect()),
        _ => value.clone(),
    }
}

fn query_url(address: &str, path: &str, params: &Value) -> reqwest::Url {
    let mut url =
        reqwest::Url::parse(&format!("http://{address}{path}")).expect("local route URL parses");
    if let Some(object) = params.as_object() {
        let mut query = url.query_pairs_mut();
        for (key, value) in object {
            if value.is_null() {
                continue;
            }
            let text = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            query.append_pair(key, &text);
        }
    }
    url
}

async fn replay_route_case(legacy_id: &str) {
    let fixture = fixtures();
    assert_eq!(fixture["reference_sha"], REFERENCE_SHA);
    let events = fixture["http_traces"][legacy_id]
        .as_array()
        .unwrap_or_else(|| panic!("missing pinned route trace for {legacy_id}"));
    assert!(
        !events.is_empty(),
        "empty pinned route trace for {legacy_id}"
    );
    let reference_now_text = events[0]["reference_now"]
        .as_str()
        .expect("legacy HTTP trace reference time");
    assert!(
        events
            .iter()
            .all(|event| event["reference_now"].as_str() == Some(reference_now_text)),
        "legacy route trace has one fixed reference time: {legacy_id}"
    );
    for event in events {
        if let Some(observed_at) = event["expected_body"]["observed_at"].as_str() {
            assert_eq!(
                observed_at, reference_now_text,
                "legacy route reference time {legacy_id}; params={:?}",
                event["params"]
            );
        }
    }
    let (admin, seed) = prepare_schema().await;
    let auth = AuthMock::default();
    let (mock_url, mock_server) = spawn_auth_mock(auth.clone()).await;
    let temporary = TempDir::new().expect("create route-owned temporary state");
    let runtime_settings = settings(
        temporary.path(),
        database_url("usage-parity-http"),
        &mock_url,
    );
    let reference_now = DateTime::parse_from_rfc3339(reference_now_text)
        .expect("legacy HTTP trace reference time is RFC 3339")
        .with_timezone(&Utc);
    let runtime_clock = reference_now;
    let runtime = Runtime::new_with_clock(
        runtime_settings.as_ref().clone(),
        Arc::new(move || runtime_clock),
    )
    .await
    .expect("create read-only Rust Runtime for usage routes");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind actual Rust usage HTTP routes on loopback");
    let address = listener.local_addr().unwrap();
    let application = sub2ops_server::api::router(runtime.clone());
    let app_server = tokio::spawn(async move {
        axum::serve(listener, application)
            .await
            .expect("serve the actual Rust usage router");
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build a loopback-only route client");
    for event in events {
        seed_snapshot(
            &seed,
            &fixture,
            event["snapshot"].as_str().expect("route snapshot id"),
        )
        .await;
        let application_name = "usage-parity-http";
        let before_barrier = sql_log_barrier(&runtime.db.pool, application_name).await;
        let method = event["method"].as_str().expect("legacy HTTP method");
        let method = Method::from_bytes(method.as_bytes()).expect("valid legacy HTTP method");
        let path = event["path"].as_str().expect("legacy HTTP path");
        let url = query_url(address.to_string().as_str(), path, &event["params"]);
        let mut request = client.request(method, url);
        if let Some(headers) = event["headers"].as_object() {
            for (name, value) in headers {
                if let Some(value) = value.as_str() {
                    request = request.header(name, value);
                }
            }
        }
        let response = request
            .send()
            .await
            .expect("send local Rust usage route request");
        let expected_status = event["expected_status"]
            .as_u64()
            .expect("legacy route status") as u16;
        assert_eq!(
            response.status().as_u16(),
            expected_status,
            "legacy route {legacy_id}: {path}; params={:?}",
            event["params"]
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .expect("desktop response request id")
            .to_owned();
        assert_eq!(request_id.len(), 32, "desktop request id length");
        assert!(request_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let bytes = response.bytes().await.expect("read local route response");
        let actual_body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice::<Value>(&bytes).expect("Rust usage response JSON")
        };
        let after_barrier = sql_log_barrier(&runtime.db.pool, application_name).await;
        if expected_status == 200 {
            let actual_body = if path == "/api/desktop/v1/usage-records"
                || path.starts_with("/api/desktop/v1/usage-records/")
            {
                assert_native_compaction_response(
                    &actual_body,
                    &format!("legacy route {legacy_id}: {path}"),
                );
                strip_native_compaction_response(&actual_body)
            } else {
                actual_body
            };
            assert_eq!(
                stable_response(&actual_body),
                stable_response(&event["expected_body"]),
                "legacy route body {legacy_id}: {path}; params={:?}",
                event["params"]
            );
            assert!(
                !actual_body.to_string().contains("PRIVATE_"),
                "usage HTTP response leaked a fixture secret: {legacy_id}"
            );
        } else if !actual_body.is_null() {
            assert!(
                actual_body.get("detail").is_some(),
                "error response has a detail field"
            );
            assert!(
                !actual_body.to_string().contains("PRIVATE_"),
                "usage HTTP error leaked a fixture secret: {legacy_id}"
            );
        }
        let statements = operation_statements(application_name, &before_barrier, &after_barrier);
        if event["database_statement_delta"].as_i64() == Some(0) {
            assert_eq!(
                statements.len(),
                0,
                "legacy route rejected before database reads: {legacy_id}; path={path}; params={:?}",
                event["params"]
            );
        } else {
            assert!(
                !statements.is_empty(),
                "legacy route database operation emitted no SQL: {legacy_id}; path={path}; params={:?}",
                event["params"]
            );
            let context = format!("{legacy_id}; path={path}; params={:?}", event["params"]);
            assert_read_only_queries(&statements, &context);
        }
    }
    let upstream_calls = auth.calls.lock().unwrap().clone();
    assert!(
        upstream_calls
            .iter()
            .all(|(method, path)| method == "GET" && path == "/api/v1/admin/groups/all"),
        "usage routes contacted only the local auth fixture"
    );
    app_server.abort();
    let _ = app_server.await;
    runtime.close().await;
    mock_server.abort();
    let _ = mock_server.await;
    seed.close().await;
    remove_schema(&admin).await;
    admin.close().await;
}

#[tokio::test]
async fn native_compaction_v2_is_schema_compatible_and_read_only() {
    let fixture = fixtures();
    let (admin, seed) = prepare_schema().await;
    seed_snapshot(&seed, &fixture, NATIVE_COMPACTION_SNAPSHOT_ID).await;
    let database = Database::open(&database_url("usage-parity-native-compaction"), true)
        .await
        .expect("open an isolated read-only usage database");
    let service = UsageRecords(database.clone());
    let at = rfc3339(&json!("2026-09-30T12:00:00+00:00"));
    let filter = Filter {
        from_at: Some("2026-09-30T11:00:00+00:00".into()),
        to_at: Some("2026-09-30T12:00:00+00:00".into()),
        limit: Some(10),
        ..Filter::default()
    };
    let application_name = "usage-parity-native-compaction";

    let before = sql_log_barrier(&database.pool, application_name).await;
    let missing_list = service
        .list_at(&filter, at)
        .await
        .expect("list succeeds when the compatibility column is absent");
    let after = sql_log_barrier(&database.pool, application_name).await;
    let missing_statements = operation_statements(application_name, &before, &after);
    assert_read_only_queries(
        &missing_statements,
        "native_compaction_v2 missing-column list",
    );
    assert_native_compaction_probe(
        &missing_statements,
        "native_compaction_v2 missing-column list",
    );
    let missing_items = missing_list["items"]
        .as_array()
        .expect("missing-column list items");
    assert!(!missing_items.is_empty());
    let missing_false_count = missing_items
        .iter()
        .filter(|row| row["native_compaction_v2"] == json!(false))
        .count();
    assert_eq!(missing_false_count, missing_items.len());

    let before = sql_log_barrier(&database.pool, application_name).await;
    let missing_detail = service
        .detail(1)
        .await
        .expect("detail succeeds when the compatibility column is absent");
    let after = sql_log_barrier(&database.pool, application_name).await;
    let missing_detail_statements = operation_statements(application_name, &before, &after);
    assert_read_only_queries(
        &missing_detail_statements,
        "native_compaction_v2 missing-column detail",
    );
    assert_native_compaction_probe(
        &missing_detail_statements,
        "native_compaction_v2 missing-column detail",
    );
    assert_eq!(missing_detail["native_compaction_v2"], json!(false));
    let missing_list_item = missing_items
        .iter()
        .find(|row| row["id"] == json!(1))
        .expect("missing-column list contains detail row");
    assert_eq!(missing_list_item, &missing_detail);

    sqlx::query("ALTER TABLE usage_logs ADD COLUMN native_compaction_v2 boolean")
        .execute(&seed)
        .await
        .expect("add compatibility column to the owned schema");
    seed_snapshot(&seed, &fixture, NATIVE_COMPACTION_SNAPSHOT_ID).await;
    sqlx::query(
        "UPDATE usage_logs SET native_compaction_v2 = CASE id \
         WHEN 1 THEN TRUE WHEN 2 THEN FALSE WHEN 3 THEN NULL ELSE FALSE END",
    )
    .execute(&seed)
    .await
    .expect("seed true, false, and NULL compatibility values");

    let before = sql_log_barrier(&database.pool, application_name).await;
    let present_list = service
        .list_at(&filter, at)
        .await
        .expect("list succeeds when the compatibility column exists");
    let after = sql_log_barrier(&database.pool, application_name).await;
    let present_statements = operation_statements(application_name, &before, &after);
    assert_read_only_queries(
        &present_statements,
        "native_compaction_v2 present-column list",
    );
    assert_native_compaction_probe(
        &present_statements,
        "native_compaction_v2 present-column list",
    );
    let present_items = present_list["items"]
        .as_array()
        .expect("present-column list items");
    let mut present_values = present_items
        .iter()
        .filter_map(|row| {
            let id = row["id"].as_i64()?;
            (1..=3).contains(&id).then(|| {
                (
                    id,
                    row["native_compaction_v2"]
                        .as_bool()
                        .expect("present compatibility value is boolean"),
                )
            })
        })
        .collect::<Vec<_>>();
    present_values.sort_by_key(|(id, _)| *id);
    assert_eq!(present_values, vec![(1, true), (2, false), (3, false)]);

    for (id, expected) in [(1, true), (2, false), (3, false)] {
        let before = sql_log_barrier(&database.pool, application_name).await;
        let detail = service
            .detail(id)
            .await
            .expect("detail succeeds for a present compatibility column");
        let after = sql_log_barrier(&database.pool, application_name).await;
        let statements = operation_statements(application_name, &before, &after);
        assert_read_only_queries(&statements, "native_compaction_v2 present-column detail");
        assert_native_compaction_probe(&statements, "native_compaction_v2 present-column detail");
        assert_eq!(detail["native_compaction_v2"], json!(expected));
        let list_item = present_items
            .iter()
            .find(|row| row["id"] == json!(id))
            .expect("present-column list contains detail row");
        assert_eq!(list_item, &detail);
    }

    sqlx::query("DROP TABLE usage_logs")
        .execute(&seed)
        .await
        .expect("remove the owned usage table to exercise SQL error propagation");
    let before = sql_log_barrier(&database.pool, application_name).await;
    let failed = service.detail(1).await;
    let after = sql_log_barrier(&database.pool, application_name).await;
    let failed_statements = operation_statements(application_name, &before, &after);
    assert_eq!(error_status(failed), Err(500));
    assert_read_only_queries(
        &failed_statements,
        "native_compaction_v2 missing-table error",
    );
    assert_native_compaction_probe(
        &failed_statements,
        "native_compaction_v2 missing-table error",
    );

    database.close().await;
    seed.close().await;
    remove_schema(&admin).await;
    admin.close().await;
}

#[tokio::test]
async fn legacy_projection_account_cost_fallback_zero_and_missing_are_distinct() {
    replay_direct_case("py:tests.test_usage_records:ProjectionTests.test_account_cost_fallback_zero_and_missing_are_distinct").await;
}

#[tokio::test]
async fn legacy_projection_account_cost_preserves_products_beyond_28_significant_digits() {
    replay_direct_case("py:tests.test_usage_records:ProjectionTests.test_account_cost_preserves_products_beyond_28_significant_digits").await;
}

#[tokio::test]
async fn legacy_projection_all_money_is_decimal_text_and_account_cost_has_independent_basis() {
    replay_direct_case("py:tests.test_usage_records:ProjectionTests.test_all_money_is_decimal_text_and_account_cost_has_independent_basis").await;
}

#[tokio::test]
async fn legacy_projection_explicit_request_type_takes_precedence_over_legacy_flags() {
    replay_direct_case("py:tests.test_usage_records:ProjectionTests.test_explicit_request_type_takes_precedence_over_legacy_flags").await;
}

#[tokio::test]
async fn legacy_projection_legacy_request_types_and_ws_precedence() {
    replay_direct_case(
        "py:tests.test_usage_records:ProjectionTests.test_legacy_request_types_and_ws_precedence",
    )
    .await;
}

#[tokio::test]
async fn legacy_projection_mapping_itself_is_not_a_response_mismatch() {
    replay_direct_case("py:tests.test_usage_records:ProjectionTests.test_mapping_itself_is_not_a_response_mismatch").await;
}

#[tokio::test]
async fn legacy_projection_native_request_type_keeps_legacy_projection_for_cyber_and_live() {
    replay_direct_case("py:tests.test_usage_records:ProjectionTests.test_native_request_type_keeps_legacy_projection_for_cyber_and_live").await;
}

#[tokio::test]
async fn legacy_projection_projection_drops_credentials_headers_bodies_and_unknown_columns() {
    replay_direct_case("py:tests.test_usage_records:ProjectionTests.test_projection_drops_credentials_headers_bodies_and_unknown_columns").await;
}

#[tokio::test]
async fn legacy_projection_request_forwarded_and_response_models_remain_distinct() {
    replay_direct_case("py:tests.test_usage_records:ProjectionTests.test_request_forwarded_and_response_models_remain_distinct").await;
}

#[tokio::test]
async fn legacy_projection_response_mismatch_uses_legacy_model_fallback_and_explicit_flag() {
    replay_direct_case("py:tests.test_usage_records:ProjectionTests.test_response_mismatch_uses_legacy_model_fallback_and_explicit_flag").await;
}

#[tokio::test]
async fn legacy_projection_sparse_history_keeps_unknown_values_null() {
    replay_direct_case(
        "py:tests.test_usage_records:ProjectionTests.test_sparse_history_keeps_unknown_values_null",
    )
    .await;
}

#[tokio::test]
async fn legacy_projection_timestamps_require_timezone_and_normalize_to_utc() {
    replay_direct_case("py:tests.test_usage_records:ProjectionTests.test_timestamps_require_timezone_and_normalize_to_utc").await;
}

#[tokio::test]
async fn legacy_records_account_api_key_and_all_model_stages_can_be_filtered() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_account_api_key_and_all_model_stages_can_be_filtered").await;
}

#[tokio::test]
async fn legacy_records_bad_cursors_are_422_without_database_access() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_bad_cursors_are_422_without_database_access").await;
}

#[tokio::test]
async fn legacy_records_concurrent_backdated_insert_is_excluded_until_refresh() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_concurrent_backdated_insert_is_excluded_until_refresh").await;
}

#[tokio::test]
async fn legacy_records_cursor_freezes_both_default_window_endpoints() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_cursor_freezes_both_default_window_endpoints").await;
}

#[tokio::test]
async fn legacy_records_detail_joins_names_without_dropping_soft_deleted_account() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_detail_joins_names_without_dropping_soft_deleted_account").await;
}

#[tokio::test]
async fn legacy_records_detail_preserves_history_when_related_rows_are_missing() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_detail_preserves_history_when_related_rows_are_missing").await;
}

#[tokio::test]
async fn legacy_records_detail_returns_404_and_invalid_id_is_422() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_detail_returns_404_and_invalid_id_is_422").await;
}

#[tokio::test]
async fn legacy_records_empty_database_and_default_window() {
    replay_direct_case(
        "py:tests.test_usage_records:UsageRecordsTests.test_empty_database_and_default_window",
    )
    .await;
}

#[tokio::test]
async fn legacy_records_invalid_filters_fail_before_database_access() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_invalid_filters_fail_before_database_access").await;
}

#[tokio::test]
async fn legacy_records_mismatch_filter_agrees_with_projected_flags() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_mismatch_filter_agrees_with_projected_flags").await;
}

#[tokio::test]
async fn legacy_records_model_wildcards_backslash_and_sql_text_are_literal_parameters() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_model_wildcards_backslash_and_sql_text_are_literal_parameters").await;
}

#[tokio::test]
async fn legacy_records_new_count_applies_filters_and_is_not_limited_to_page_size() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_new_count_applies_filters_and_is_not_limited_to_page_size").await;
}

#[tokio::test]
async fn legacy_records_pagination_has_no_overlap_or_gap_at_identical_timestamps() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_pagination_has_no_overlap_or_gap_at_identical_timestamps").await;
}

#[tokio::test]
async fn legacy_records_request_type_filter_matches_legacy_and_explicit_semantics() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_request_type_filter_matches_legacy_and_explicit_semantics").await;
}

#[tokio::test]
async fn legacy_records_user_filter_is_shared_by_pages_and_new_record_count() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_user_filter_is_shared_by_pages_and_new_record_count").await;
}

#[tokio::test]
async fn legacy_records_user_filter_uses_record_ids_after_directory_rows_are_physically_deleted() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_user_filter_uses_record_ids_after_directory_rows_are_physically_deleted").await;
}

#[tokio::test]
async fn legacy_records_window_boundaries_and_descending_time_then_id() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordsTests.test_window_boundaries_and_descending_time_then_id").await;
}

#[tokio::test]
async fn legacy_date_invalid_or_mixed_date_filters_fail_before_database_access() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordDateTests.test_invalid_or_mixed_date_filters_fail_before_database_access").await;
}

#[tokio::test]
async fn legacy_date_leap_day_and_multi_day_range_use_inclusive_calendar_end_date() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordDateTests.test_leap_day_and_multi_day_range_use_inclusive_calendar_end_date").await;
}

#[tokio::test]
async fn legacy_date_pre_date_api_cursor_payload_still_works_and_freezes_default_window() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordDateTests.test_pre_date_api_cursor_payload_still_works_and_freezes_default_window").await;
}

#[tokio::test]
async fn legacy_date_single_shanghai_day_includes_both_day_edges_and_excludes_next_midnight() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordDateTests.test_single_shanghai_day_includes_both_day_edges_and_excludes_next_midnight").await;
}

#[test]
fn pinned_date_window_timestamp_boundary_matrix() {
    let fixture = fixtures();
    let matrix = &fixture["date_boundary_matrix"];
    assert_eq!(matrix["id"], "pinned_date_window_timestamp_boundary_matrix");
    let cases = matrix["cases"]
        .as_array()
        .expect("pinned date/timestamp boundary cases are an array");
    assert_eq!(cases.len(), 12);

    for case in cases {
        let case_id = case["id"].as_str().expect("boundary case id");
        let input = &case["input"];
        let expected = match case.get("error_status") {
            Some(status) => Err(status.as_u64().expect("legacy error status") as u16),
            None => Ok(case["expected"].clone()),
        };
        let actual = match case["operation"].as_str().expect("boundary operation") {
            "date_window" => date_window(
                input["start_date"].as_str().expect("date-window start"),
                input["end_date"].as_str().expect("date-window end"),
            )
            .map(|(start, end)| json!([start.to_rfc3339(), end.to_rfc3339()]))
            .map_err(|error| error.status.as_u16()),
            "timestamp" => timestamp(input.as_str().expect("timestamp input"))
                .map(|value| json!(value.to_rfc3339()))
                .map_err(|error| error.status.as_u16()),
            operation => panic!("unsupported pinned date boundary operation: {operation}"),
        };
        assert_eq!(actual, expected, "boundary case {case_id}; input={input}");
    }
}

#[tokio::test]
async fn legacy_date_year_boundary_uses_next_calendar_day_for_end() {
    pinned_date_window_timestamp_boundary_matrix();
    replay_direct_case("py:tests.test_usage_records:UsageRecordDateTests.test_year_boundary_uses_next_calendar_day_for_end").await;
}

#[tokio::test]
async fn legacy_summary_empty_null_and_zero_cost_totals_are_decimal_strings() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordSummaryTests.test_empty_null_and_zero_cost_totals_are_decimal_strings").await;
}

#[tokio::test]
async fn legacy_summary_joint_date_user_key_account_model_type_and_mismatch_filters_apply_to_summary(
) {
    replay_direct_case("py:tests.test_usage_records:UsageRecordSummaryTests.test_joint_date_user_key_account_model_type_and_mismatch_filters_apply_to_summary").await;
}

#[tokio::test]
async fn legacy_summary_new_count_never_aggregates_even_when_summary_is_requested() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordSummaryTests.test_new_count_never_aggregates_even_when_summary_is_requested").await;
}

#[tokio::test]
async fn legacy_summary_summary_and_rows_share_read_transaction_and_watermark_during_concurrent_write(
) {
    replay_direct_case("py:tests.test_usage_records:UsageRecordSummaryTests.test_summary_and_rows_share_read_transaction_and_watermark_during_concurrent_write").await;
}

#[tokio::test]
async fn legacy_summary_summary_covers_all_pages_and_is_independent_of_page_limit() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordSummaryTests.test_summary_covers_all_pages_and_is_independent_of_page_limit").await;
}

#[tokio::test]
async fn legacy_summary_summary_is_opt_in_and_uses_actual_cost_without_other_cost_bases() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordSummaryTests.test_summary_is_opt_in_and_uses_actual_cost_without_other_cost_bases").await;
}

#[tokio::test]
async fn legacy_summary_summary_preserves_decimal_precision_without_float_or_context_rounding() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordSummaryTests.test_summary_preserves_decimal_precision_without_float_or_context_rounding").await;
}

#[tokio::test]
async fn legacy_summary_summary_retains_history_after_related_entities_are_soft_or_hard_deleted() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordSummaryTests.test_summary_retains_history_after_related_entities_are_soft_or_hard_deleted").await;
}

#[tokio::test]
async fn legacy_options_deleted_entities_cannot_bypass_directory_filter_by_id_name_or_owner() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_deleted_entities_cannot_bypass_directory_filter_by_id_name_or_owner").await;
}

#[tokio::test]
async fn legacy_options_directory_output_is_an_exact_whitelist_and_never_returns_secrets() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_directory_output_is_an_exact_whitelist_and_never_returns_secrets").await;
}

#[tokio::test]
async fn legacy_options_directory_search_escapes_wildcards_and_parameterizes_sql() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_directory_search_escapes_wildcards_and_parameterizes_sql").await;
}

#[tokio::test]
async fn legacy_options_empty_directory_or_unknown_owner_returns_no_cursor() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_empty_directory_or_unknown_owner_returns_no_cursor").await;
}

#[tokio::test]
async fn legacy_options_invalid_option_parameters_and_cursors_do_not_reach_database() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_invalid_option_parameters_and_cursors_do_not_reach_database").await;
}

#[tokio::test]
async fn legacy_options_key_directory_excludes_deleted_and_orphaned_keys_but_retains_disabled_owner_identity(
) {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_key_directory_excludes_deleted_and_orphaned_keys_but_retains_disabled_owner_identity").await;
}

#[tokio::test]
async fn legacy_options_key_search_by_name_or_id_is_combined_with_user_scope() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_key_search_by_name_or_id_is_combined_with_user_scope").await;
}

#[tokio::test]
async fn legacy_options_more_than_100_users_and_keys_are_complete_in_ascending_id_pages() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_more_than_100_users_and_keys_are_complete_in_ascending_id_pages").await;
}

#[tokio::test]
async fn legacy_options_option_cursor_is_bound_to_kind_query_and_user_scope() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_option_cursor_is_bound_to_kind_query_and_user_scope").await;
}

#[tokio::test]
async fn legacy_options_options_use_bounded_read_only_directory_queries_without_usage_history() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_options_use_bounded_read_only_directory_queries_without_usage_history").await;
}

#[tokio::test]
async fn legacy_options_user_search_matches_name_email_or_id() {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_user_search_matches_name_email_or_id").await;
}

#[tokio::test]
async fn legacy_options_users_directory_excludes_deleted_but_retains_duplicate_names_and_disabled_entries(
) {
    replay_direct_case("py:tests.test_usage_records:UsageRecordOptionsTests.test_users_directory_excludes_deleted_but_retains_duplicate_names_and_disabled_entries").await;
}

#[tokio::test]
async fn legacy_route_authenticated_list_and_detail_are_safe_serializable_read_responses() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_authenticated_list_and_detail_are_safe_serializable_read_responses").await;
}

#[tokio::test]
async fn legacy_route_invalid_route_parameters_are_422_and_absent_detail_is_404() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_invalid_route_parameters_are_422_and_absent_detail_is_404").await;
}

#[tokio::test]
async fn legacy_route_list_and_detail_require_admin_header_before_any_database_read() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_list_and_detail_require_admin_header_before_any_database_read").await;
}

#[tokio::test]
async fn legacy_route_malformed_cursor_numbers_return_422_at_http_boundary() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_malformed_cursor_numbers_return_422_at_http_boundary").await;
}

#[tokio::test]
async fn legacy_route_options_route_filters_returns_whitelist_and_never_runs_oauth_or_http() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_options_route_filters_returns_whitelist_and_never_runs_oauth_or_http").await;
}

#[tokio::test]
async fn legacy_route_options_route_rejects_invalid_parameters_and_cross_condition_cursor() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_options_route_rejects_invalid_parameters_and_cross_condition_cursor").await;
}

#[tokio::test]
async fn legacy_route_records_route_passes_user_id_to_list_and_new_count() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_records_route_passes_user_id_to_list_and_new_count").await;
}

#[tokio::test]
async fn legacy_route_route_filters_and_new_count_are_applied() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_route_filters_and_new_count_are_applied").await;
}

#[tokio::test]
async fn legacy_route_route_passes_date_window_and_summary_with_exact_decimal_json() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_route_passes_date_window_and_summary_with_exact_decimal_json").await;
}

#[tokio::test]
async fn legacy_route_route_rejects_invalid_dates_mixed_time_modes_and_summary_boolean() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_route_rejects_invalid_dates_mixed_time_modes_and_summary_boolean").await;
}

#[tokio::test]
async fn legacy_route_route_summary_default_and_explicit_false_do_not_aggregate() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_route_summary_default_and_explicit_false_do_not_aggregate").await;
}

#[tokio::test]
async fn legacy_route_summary_reads_never_trigger_quota_models_oauth_or_http() {
    replay_route_case("py:tests.test_usage_records:UsageRecordsRouteTests.test_summary_reads_never_trigger_quota_models_oauth_or_http").await;
}
