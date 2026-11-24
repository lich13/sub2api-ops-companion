#[path = "common/harness.rs"]
mod test_harness;

use serde_json::{json, Value};
use std::{
    env, fs,
    sync::{atomic::Ordering, Arc},
};
use sub2ops_server::{
    account_locks::AccountLocks, account_templates::Templates, actions::Actions, compat::digest,
    db::Database, error::Result, oauth_state::OAuthState, operations::Operations, queries::Queries,
    store::write_json, upstream::Upstream,
};
use test_harness::{operation_settings, CountingOperationBackend, OperationsBatchExt};
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
    backend: Arc<CountingOperationBackend>,
}

impl Fixture {
    async fn new() -> Self {
        let database_url = env::var("DATABASE_URL")
            .expect("runner must provide a temporary DATABASE_URL for operation state tests");
        let db = Database::open(&database_url)
            .await
            .expect("connect to runner-owned PostgreSQL");
        let temp = tempfile::tempdir().expect("create operation state fixture directory");
        let settings = Arc::new(operation_settings(database_url, temp.path(), ""));
        let upstream = Upstream::new(settings.clone()).expect("construct fixture upstream");
        let queries = Queries {
            db: db.clone(),
            oauth: OAuthState::new(temp.path().join("oauth-state.json")),
        };
        let actions = Actions::new(
            queries,
            upstream,
            AccountLocks::default(),
            Templates::new(temp.path().join("account-templates.json")),
            settings,
        )
        .expect("construct operation actions");
        let backend = Arc::new(CountingOperationBackend::default());
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
async fn waiting_queue_skips_execution_config_but_records_completed_appointments() {
    let fixture = Fixture::new().await;
    let mut data = valid_state_with_job_ids(json!([JOB_ID]));
    data["jobs"][JOB_ID]["scheduled_at"] = "2099-01-01T00:00:00Z".into();
    fixture.replace_state(&data);
    fixture.operations.tick().await.unwrap();
    assert_eq!(fixture.backend.test_config_reads.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.backend.dispatch_calls.load(Ordering::SeqCst), 0);
    data["jobs"][JOB_ID]["status"] = "completed".into();
    data["jobs"][JOB_ID]["result"] = json!({"success":true});
    fixture.replace_state(&data);
    fixture.operations.tick().await.unwrap();
    fixture.operations.tick().await.unwrap();
    assert_eq!(fixture.backend.test_config_reads.load(Ordering::SeqCst), 0);
    let state = fixture
        .operations
        .actions
        .queries
        .oauth
        .shared_snapshot()
        .unwrap();
    assert_eq!(state["recovery_history"].as_object().unwrap().len(), 1);
    assert_eq!(
        state["recovery_history"][format!("connection-test:{JOB_ID}")]["result_status"],
        "completed"
    );
    fixture.close().await;
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
                .submit_batch_test(&request, ADMIN_KEY, false)
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
        .submit_batch_test(&replay_request(), ADMIN_KEY, false)
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
async fn public_views_keep_requested_options_without_private_payloads_or_state_writes() {
    let fixture = Fixture::new().await;
    let mut state = valid_state_with_job_ids(json!([JOB_ID]));
    let job = &mut state["jobs"][JOB_ID];
    job["payload"] = json!({"confirmed":true,"mode":"default","model_id":"fixture-test-model",
        "prompt":"fixture prompt","image_data_url":"data:image/png;base64,Zml4dHVyZQ==",
        "audio_data_url":"data:audio/wav;base64,Zml4dHVyZQ==","expected_version":"0".repeat(64),
        "detach_managed":true,"future_option":{"values":[false,0,null]}});
    job["template_target"] = json!({"fixture":true});
    job["retry_policy"] = json!({"max_retries":3,"interval_seconds":10});
    job["notification_suppressed"] = true.into();
    let before = fixture.replace_state(&state);
    let job = &state["jobs"][JOB_ID];
    let mut expected = json!({"id":JOB_ID,"batch_id":BATCH_ID,"account_id":1,"account_name":"fixture-account",
        "action":"test","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "status":"queued","reason":"等待执行","result":null,
        "requested":{"confirmed":true,"mode":"default","model_id":"fixture-test-model","future_option":{"values":[false,0,null]}}});
    assert_eq!(fixture.operations.public(job, false, 0), expected);
    assert_eq!(
        fixture.operations.listing(Some(BATCH_ID)).unwrap()["items"][0],
        expected
    );
    expected["events"] = json!([]);
    expected["next_event"] = 0.into();
    assert_eq!(
        fixture.operations.get(JOB_ID, usize::MAX).unwrap(),
        expected
    );
    assert_eq!(fs::read(fixture.state_path()).unwrap(), before);
    assert_eq!(*fixture.operations.store.0.read_shared().unwrap(), state);
    fixture.close().await;
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

#[tokio::test]
async fn listing_keeps_all_pending_and_latest_thirty_terminal_in_stable_order() {
    let fixture = Fixture::new().await;
    let mut state = json!({"version":1,"jobs":{},"requests":{},"batches":{},"batch_requests":{}});
    for n in 0..75 {
        let id = format!("{n:032x}");
        let mut job = valid_job();
        job["id"] = id.clone().into();
        job["batch_id"] = Value::Null;
        job["created_at"] = format!("2026-01-{:02}T00:00:00Z", (n * 17) % 28 + 1).into();
        if n % 9 != 0 {
            job["status"] = "completed".into();
        }
        state["jobs"][id] = job;
    }
    // Compare with the original stable public ordering, including tied dates.
    let mut ordered = state["jobs"]
        .as_object()
        .unwrap()
        .values()
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| b["created_at"].as_str().cmp(&a["created_at"].as_str()));
    let expected = ordered
        .iter()
        .filter(|j| j["status"] == "queued")
        .chain(
            ordered
                .iter()
                .filter(|j| j["status"] == "completed")
                .take(30),
        )
        .map(|j| j["id"].clone())
        .collect::<Vec<_>>();
    let before = fixture.replace_state(&state);
    let listed = fixture.operations.listing(None).unwrap();
    assert_eq!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|j| j["id"].clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(fs::read(fixture.state_path()).unwrap(), before);
    fixture.close().await;
}
