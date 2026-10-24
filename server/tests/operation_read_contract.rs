use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    env, fs,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use sub2ops_server::{
    account_locks::AccountLocks,
    account_templates::Templates,
    actions::Actions,
    compat::digest,
    db::Database,
    error::{Error, Result},
    oauth_state::OAuthState,
    operations::{Backend, Operations},
    queries::Queries,
    settings::Settings,
    store::write_json,
    upstream::Upstream,
};
use tokio::sync::Semaphore;

const BATCH_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const JOB_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const MISSING_JOB_ID: &str = "cccccccccccccccccccccccccccccccc";
const REQUEST_ID: &str = "fixture-operation-replay-0001";
const CLIENT_ID: &str = "fixture-operation-client-0001";
const ADMIN_KEY: &str = "fixture-operation-admin-key";

struct Fixture {
    _temp: tempfile::TempDir,
    db: Database,
    operations: Arc<Operations>,
    backend: Arc<FixtureBackend>,
}

struct FixtureBackend {
    dispatch_calls: Arc<AtomicUsize>,
}

impl Backend for FixtureBackend {
    fn mark(&self, _id: i64) -> Result<Value> {
        Ok(json!({"version":"0".repeat(64)}))
    }

    fn dispatch<'a>(
        &'a self,
        _row: &'a Value,
        _job: &'a Value,
        _key: &'a str,
    ) -> BoxFuture<'a, Result<Value>> {
        self.dispatch_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json!({"fixture":true})) })
    }

    fn model_get(&self, _id: &str) -> Result<Value> {
        Err(Error::new(404, "fixture model operation absent"))
    }

    fn model_jobs(&self) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }

    fn model_cancel<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { Err(Error::new(404, "fixture model operation absent")) })
    }

    fn test_config(&self) -> Result<(String, usize)> {
        Ok(("fixture-test-model".into(), 1))
    }
}

impl Fixture {
    async fn new() -> Self {
        let database_url = env::var("DATABASE_URL")
            .expect("runner must provide a temporary DATABASE_URL for operation state tests");
        let db = Database::open(&database_url, false)
            .await
            .expect("connect to runner-owned PostgreSQL");
        let temp = tempfile::tempdir().expect("create operation state fixture directory");
        let settings = Arc::new(Settings {
            database_url,
            bind: "127.0.0.1:0".into(),
            base_path: String::new(),
            audit_path: temp.path().join("audit.jsonl"),
            usage_path: temp.path().join("usage-state.json"),
            oauth_path: temp.path().join("oauth-state.json"),
            bark_path: temp.path().join("bark-state.json"),
            connection_path: temp.path().join("sub2api-config.json"),
            model_path: temp.path().join("group-models.json"),
            keepalive_path: temp.path().join("keepalive-state.json"),
            oauth: json!({}),
            bark: json!({"enabled":false}),
            bark_valid: true,
            base_url: String::new(),
            verify_url: String::new(),
            read_only: false,
        });
        let upstream = Upstream::new(settings.clone()).expect("construct fixture upstream");
        let queries = Queries {
            db: db.clone(),
            oauth: OAuthState::new(temp.path().join("oauth-state.json"), false),
        };
        let actions = Actions::new(
            queries,
            upstream,
            AccountLocks::default(),
            Templates::new(temp.path().join("account-templates.json"), false),
            settings,
        )
        .expect("construct operation actions");
        let backend = Arc::new(FixtureBackend {
            dispatch_calls: Arc::new(AtomicUsize::new(0)),
        });
        let operations = Operations::new(actions, backend.clone(), Arc::new(Semaphore::new(8)))
            .expect("construct durable operation queue");
        Self {
            _temp: temp,
            db,
            operations,
            backend,
        }
    }

    fn state_path(&self) -> &std::path::Path {
        self.operations.store.0.path()
    }

    fn replace_state(&self, state: &Value) -> Vec<u8> {
        write_json(self.state_path(), state).expect("replace fixture operation state");
        fs::read(self.state_path()).expect("read fixture operation state")
    }

    async fn close(&self) {
        self.operations.close().await;
        self.db.close().await;
    }
}

fn replay_request() -> Value {
    json!({
        "action":"test",
        "payload":{"confirmed":true},
        "accounts":[{"account_id":1,"expected_version":"0".repeat(64)}],
        "request_id":REQUEST_ID,
        "client_id":CLIENT_ID
    })
}

fn replay_signature() -> String {
    let mut canonical = replay_request();
    canonical
        .as_object_mut()
        .unwrap()
        .shift_remove("request_id");
    digest(&canonical)
}

fn batch_shell(job_ids: Value) -> Value {
    json!({
        "id":BATCH_ID,
        "action":"test",
        "request_id":REQUEST_ID,
        "client_id":CLIENT_ID,
        "request_hash":replay_signature(),
        "job_ids":job_ids
    })
}

fn valid_job() -> Value {
    json!({
        "id":JOB_ID,
        "batch_id":BATCH_ID,
        "account_id":1,
        "account_name":"fixture-account",
        "action":"test",
        "payload":{"confirmed":true,"mode":"default","model_id":"fixture-test-model"},
        "expected_version":"0".repeat(64),
        "client_id":CLIENT_ID,
        "request_id":"fixture-operation-job-0001",
        "request_hash":"fixture-job-hash",
        "created_at":"2026-01-01T00:00:00Z",
        "updated_at":"2026-01-01T00:00:00Z",
        "status":"queued",
        "reason":"等待执行",
        "result":null
    })
}

fn valid_state_with_job_ids(job_ids: Value) -> Value {
    json!({
        "version":1,
        "jobs":{JOB_ID:valid_job()},
        "requests":{},
        "batches":{BATCH_ID:batch_shell(job_ids)},
        "batch_requests":{REQUEST_ID:BATCH_ID}
    })
}

fn malformed_states() -> Vec<(&'static str, Value)> {
    vec![
        (
            "missing job_ids",
            json!({
                "version":1,
                "jobs":{JOB_ID:valid_job()},
                "requests":{},
                "batches":{BATCH_ID:{"action":"test","request_id":REQUEST_ID,"client_id":CLIENT_ID}},
                "batch_requests":{REQUEST_ID:BATCH_ID}
            }),
        ),
        (
            "job_ids is not an array",
            valid_state_with_job_ids(json!(JOB_ID)),
        ),
        (
            "dangling job id",
            valid_state_with_job_ids(json!([MISSING_JOB_ID])),
        ),
        ("job belongs to another batch", {
            let mut data = valid_state_with_job_ids(json!([JOB_ID]));
            data["jobs"][JOB_ID]["batch_id"] = "dddddddddddddddddddddddddddddddd".into();
            data
        }),
    ]
}

fn assert_read_error<T>(result: Result<T>, case: &str) {
    let error = match result {
        Ok(_) => panic!("{case} unexpectedly succeeded"),
        Err(error) => error,
    };
    assert_eq!(error.status.as_u16(), 503, "{case}");
    assert_eq!(error.detail, "操作状态无法读取", "{case}");
}

#[tokio::test]
async fn corrupted_operation_state_returns_503_without_rewrite_or_dispatch() {
    let fixture = Fixture::new().await;
    let request = replay_request();
    for (case, state) in malformed_states() {
        let before = fixture.replace_state(&state);
        assert_read_error(fixture.operations.listing(None), case);
        assert_eq!(fs::read(fixture.state_path()).unwrap(), before, "{case}");
        assert_read_error(fixture.operations.listing(Some(BATCH_ID)), case);
        assert_eq!(fs::read(fixture.state_path()).unwrap(), before, "{case}");
        assert_read_error(fixture.operations.cancel_batch(BATCH_ID).await, case);
        assert_eq!(fs::read(fixture.state_path()).unwrap(), before, "{case}");
        assert_read_error(
            fixture
                .operations
                .submit_batch(&request, ADMIN_KEY, false)
                .await,
            case,
        );
        assert_eq!(fs::read(fixture.state_path()).unwrap(), before, "{case}");
        assert!(fixture.operations.keys.lock().unwrap().is_empty(), "{case}");
        assert_eq!(
            fixture.backend.dispatch_calls.load(Ordering::SeqCst),
            0,
            "{case}"
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn empty_batch_and_missing_batch_keep_listing_cancel_semantics() {
    let fixture = Fixture::new().await;
    let empty = json!({
        "version":1,
        "jobs":{},
        "requests":{},
        "batches":{BATCH_ID:batch_shell(json!([]))},
        "batch_requests":{}
    });
    fixture.replace_state(&empty);

    let listed = fixture
        .operations
        .listing(Some(BATCH_ID))
        .expect("an empty persisted batch remains listable");
    assert_eq!(listed["pending"], 0);
    assert!(listed["items"].as_array().unwrap().is_empty());
    let cancelled = fixture
        .operations
        .cancel_batch(BATCH_ID)
        .await
        .expect("an empty test batch remains cancellable");
    assert_eq!(cancelled["pending"], 0);
    assert!(cancelled["items"].as_array().unwrap().is_empty());

    let missing = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let error = fixture.operations.listing(Some(missing)).unwrap_err();
    assert_eq!(error.status.as_u16(), 404);
    let error = fixture.operations.cancel_batch(missing).await.unwrap_err();
    assert_eq!(error.status.as_u16(), 404);
    let error = fixture
        .operations
        .listing(Some("not-a-batch-id"))
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 404);
    let error = fixture
        .operations
        .cancel_batch("not-a-batch-id")
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 404);
    assert_eq!(fixture.backend.dispatch_calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn idempotent_replay_returns_existing_batch_without_duplicate_dispatch() {
    let fixture = Fixture::new().await;
    let state = valid_state_with_job_ids(json!([JOB_ID]));
    fixture.replace_state(&state);
    let before = fs::read(fixture.state_path()).unwrap();

    let replayed = fixture
        .operations
        .submit_batch(&replay_request(), ADMIN_KEY, false)
        .await
        .expect("matching batch request id replays the existing batch");
    assert_eq!(replayed["batch_id"], BATCH_ID);
    assert_eq!(replayed["items"].as_array().unwrap().len(), 1);
    assert_eq!(fs::read(fixture.state_path()).unwrap(), before);
    assert_eq!(
        fixture.operations.keys.lock().unwrap().get(JOB_ID),
        Some(&ADMIN_KEY.to_owned())
    );
    assert_eq!(fixture.backend.dispatch_calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn historical_ownership_fields_are_omitted_without_rewriting_saved_jobs() {
    let fixture = Fixture::new().await;
    let mut job = valid_job();
    job["action"] = "delete".into();
    job["status"] = "completed".into();
    job["payload"] = json!({"detach_managed":true});
    job["managed_child_ids"] = json!([2]);
    job["detached"] = true.into();
    job["result"] = json!({"deleted":true,"detached":true});
    let original = job.clone();
    let view = fixture.operations.public(&job, true, 0);
    assert!(view.get("managed_child_ids").is_none());
    assert!(view.get("detached").is_none());
    assert!(view["requested"].get("detach_managed").is_none());
    assert_eq!(view["result"], json!({"deleted":true}));
    assert_eq!(job, original);
    assert_eq!(fixture.backend.dispatch_calls.load(Ordering::SeqCst), 0);
    fixture.db.close().await;
}

#[tokio::test]
async fn stopped_continuous_test_cannot_be_requeued_by_a_late_busy_result() {
    let fixture = Fixture::new().await;
    let mut state = valid_state_with_job_ids(json!([JOB_ID]));
    let job = &mut state["jobs"][JOB_ID];
    job["repeat"] = "quota_cycle".into();
    job["repeat_series_id"] = JOB_ID.into();
    job["repeat_state"] = "active".into();
    job["status"] = "running".into();
    job["started_at"] = "2030-01-01T00:00:00Z".into();
    job["scheduled_at"] = "2030-01-01T00:00:00Z".into();
    fixture.replace_state(&state);
    let stopped = fixture.operations.cancel(JOB_ID).await.unwrap();
    assert_eq!(stopped["repeat_state"], "stopped");
    assert_eq!(stopped["status"], "running");
    fixture
        .operations
        .update(
            JOB_ID,
            json!({"status":"queued","reason":"等待账号空闲","started_at":null}),
        )
        .unwrap();
    let current = fixture.operations.get(JOB_ID, 0).unwrap();
    assert_eq!(current["status"], "cancelled");
    assert_eq!(current["repeat_state"], "stopped");
    assert_eq!(fixture.backend.dispatch_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.operations.listing(None).unwrap()["pending"], 0);
    fixture.close().await;
}
