use serde_json::{json, Value};
use std::{fs, path::PathBuf, sync::Arc};
use sub2ops_server::{
    detection_logs, error::Error, model_detection, runtime::Runtime, settings::Settings,
};

const AT: &str = "2026-10-20T12:00:00Z";

fn job_id(number: u8) -> String {
    format!("000000000000000000000000000000{number:02x}")
}

fn account_id(number: u8) -> i64 {
    700_000 + i64::from(number)
}

fn report(prediction: &str, probability: f64, used_outputs: u64) -> Value {
    json!({"prediction":prediction,"probability":probability,"used_outputs":used_outputs})
}

fn job(number: u8, status: &str, at: &str) -> Value {
    let terminal = matches!(
        status,
        "completed" | "failed" | "cancelled" | "interrupted" | "needs_confirmation"
    );
    json!({
        "id":job_id(number),
        "account_id":account_id(number),
        "account_name":format!("fixture-account-{number}"),
        "requested_model":"gpt-6-sol",
        "status":status,
        "automatic":true,
        "created_at":at,
        "queued_at":at,
        "started_at":at,
        "completed_at":if terminal {json!(at)} else {Value::Null},
        "triggers":["scheduled"],
        "error":"",
        "report":Value::Null,
        "result_summary":Value::Null,
        "first_group_valid":false,
        "api_key":"fixture-only-secret",
        "future_private_field":{"credentials":"fixture-only-secret"}
    })
}

fn saved_summary() -> Value {
    json!({
        "outcome":"normal",
        "analysis_available":true,
        "first_prediction":"gpt-6-sol",
        "first_probability":0.72,
        "first_downgrade":false,
        "prediction":"gpt-6-sol",
        "probability":0.91,
        "used_outputs":3,
        "aggregate_downgrade":false,
        "disposition":Value::Null,
        "recovery":Value::Null,
        "verification":Value::Null,
        "restoration":Value::Null
    })
}

#[test]
fn lm_summary_retains_source_null_confidence_and_group_evidence_after_compaction() {
    let mut value = job(90, "completed", AT);
    value["engine"] = "lm_detector".into();
    value["planned_groups"] = 3.into();
    value["bank_version"] = json!({"revision":"1".repeat(40)});
    value["report"] = json!({"decision":"scored","prediction":"gpt-6-sol","prediction_name":"gpt-6-sol","used_outputs":3,"probability":null});
    value["groups"] = json!(["gpt-6-sol", "grok-4.5", "gpt-6-sol"]
        .iter()
        .enumerate()
        .map(|(i, name)| json!({"index":i+1,"report":{"prediction_name":name}}))
        .collect::<Vec<_>>());
    value["classification"] =
        json!({"outcome":"degraded","basis":"sample_family","group_indexes":[2],"reason":""});
    detection_logs::preserve_summary(&mut value);
    let summary = value["result_summary"].clone();
    assert_eq!(summary["engine"], "lm_detector");
    assert!(summary["probability"].is_null());
    assert_eq!(summary["classification"]["group_indexes"], json!([2]));
    assert_eq!(summary["group_predictions"][1]["prediction"], "grok-4.5");
    value["report"] = Value::Null;
    value["groups"] = json!([]);
    detection_logs::preserve_summary(&mut value);
    assert_eq!(value["result_summary"], summary);
}

#[test]
fn relaxed_summary_keeps_its_rule_and_verdict_after_compaction() {
    for engine in ["modeltrace", "lm_detector"] {
        let mut value = job(91, "completed", AT);
        value["engine"] = engine.into();
        value["verdict_mode"] = "relaxed".into();
        value["degraded_model_id"] = "gpt-6-luna".into();
        value["planned_groups"] = 1.into();
        value["report"] =
            json!({"prediction":"gpt-5.6-luna","decision":"partial","used_outputs":1});
        value["groups"] = json!([{"index":1,"status":"completed","transport_complete":true,"report":value["report"]}]);
        value["classification"] = sub2ops_server::verdict_policy::classify(&value);
        detection_logs::preserve_summary(&mut value);
        let saved = value["result_summary"].clone();
        assert_eq!(saved["outcome"], "normal");
        assert_eq!(saved["verdict_mode"], "relaxed");
        assert_eq!(saved["degraded_model_id"], "gpt-6-luna");
        assert!(saved["first_downgrade"].is_null());
        assert!(saved["aggregate_downgrade"].is_null());
        value["report"] = Value::Null;
        value["groups"] = json!([]);
        detection_logs::preserve_summary(&mut value);
        assert_eq!(value["result_summary"], saved);
    }
}

fn receipt(status: &str, job_id: &str) -> Value {
    json!({
        "status":status,
        "job_id":job_id,
        "marked":true,
        "schedule_verified":true,
        "mark_cleared":true,
        "reason":"fixture receipt",
        "completed_at":AT
    })
}

fn insert_jobs(runtime: &Runtime, rows: impl IntoIterator<Item = Value>) {
    runtime
        .tests
        .transaction(|state| {
            for row in rows {
                let id = row["id"].as_str().unwrap().to_owned();
                state["jobs"].as_object_mut().unwrap().insert(id, row);
            }
            Ok(())
        })
        .unwrap();
}

fn item<'a>(page: &'a Value, id: &str) -> &'a Value {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["job_id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("missing fixture job {id}"))
}

fn state_paths(runtime: &Runtime) -> [PathBuf; 3] {
    [
        runtime.tests.store.path().to_path_buf(),
        runtime.detection.store.path().to_path_buf(),
        runtime.monitor.store.0.path().to_path_buf(),
    ]
}

fn file_state(paths: &[PathBuf]) -> Vec<Option<Vec<u8>>> {
    paths.iter().map(|path| fs::read(path).ok()).collect()
}

struct Harness {
    _root: tempfile::TempDir,
    runtime: Arc<Runtime>,
}

impl Harness {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let connection_path = root.path().join("connection.json");
        fs::write(
            &connection_path,
            br#"{"base_url":"http://127.0.0.1:9","verify_base_url":""}"#,
        )
        .unwrap();
        let database_url = std::env::var("DATABASE_URL").expect("isolated test DATABASE_URL");
        let settings = Settings {
            database_url,
            bind: "127.0.0.1:0".into(),
            base_path: String::new(),
            audit_path: root.path().join("audit.jsonl"),
            usage_path: root.path().join("oauth-state.json"),
            oauth_path: root.path().join("oauth-config.json"),
            bark_path: root.path().join("bark-config.json"),
            connection_path,
            model_path: root.path().join("group-models.json"),
            keepalive_path: root.path().join("keepalive-state.json"),
            oauth: json!({
                "model_detection_model_id":"gpt-6-luna",
                "oauth_auto_reset_credit_enabled":false,
                "oauth_daily_test_enabled":false,
                "oauth_daily_test_time":"05:00",
                "oauth_early_probe_batch_size":1,
                "oauth_recovery_connection_account_ids":[],
                "oauth_recovery_model_account_ids":[],
                "oauth_recovery_monitor_enabled":false,
                "oauth_recovery_test_concurrency":1,
                "oauth_recovery_test_model_id":"gpt-6-luna",
                "oauth_usage_refresh_concurrency":1
            }),
            bark: json!({"enabled":false}),
            bark_valid: true,
            base_url: "http://127.0.0.1:9".into(),
            verify_url: String::new(),
            read_only: false,
        };
        let runtime = Runtime::new(settings).await.unwrap();
        Self {
            _root: root,
            runtime,
        }
    }

    async fn close(self) {
        self.runtime.close().await;
    }
}

#[tokio::test]
async fn lists_all_terminal_automatic_statuses_and_excludes_manual_or_active_jobs() {
    let harness = Harness::new().await;
    let runtime = &harness.runtime;
    let mut rows = Vec::new();
    for (number, status) in [
        (1, "completed"),
        (2, "failed"),
        (3, "cancelled"),
        (4, "interrupted"),
        (5, "needs_confirmation"),
    ] {
        rows.push(job(number, status, AT));
    }
    let mut completed = rows[0].clone();
    completed["report"] = report("gpt-6-sol", 0.9, 3);
    completed["first_group_valid"] = json!(true);
    completed["first_group_prediction"] = json!("gpt-6-sol");
    completed["first_group_probability"] = json!(0.8);
    completed["account_name"] = json!("Bearer fixture-access-token");
    completed["error"] = json!("api_key=fixture-only-secret");
    rows[0] = completed;

    let mut manual = job(6, "completed", AT);
    manual["automatic"] = json!(false);
    rows.push(manual);
    for (number, status) in [(7, "queued"), (8, "running"), (9, "retrying")] {
        rows.push(job(number, status, AT));
    }
    insert_jobs(runtime, rows);

    let before = file_state(&state_paths(runtime));
    let page = runtime.detection.logs(None, 100).unwrap();
    let after = file_state(&state_paths(runtime));
    assert_eq!(before, after, "reading logs must not mutate any state file");

    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    let statuses = items
        .iter()
        .filter_map(|row| row["status"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        statuses,
        [
            "completed",
            "failed",
            "cancelled",
            "interrupted",
            "needs_confirmation"
        ]
        .into_iter()
        .collect()
    );
    let completed = item(&page, &job_id(1));
    assert_eq!(completed["summary"]["outcome"], "normal");
    assert_eq!(completed["details_available"], true);
    assert!(completed.get("api_key").is_none());
    assert!(completed.get("future_private_field").is_none());
    assert!(!completed.to_string().contains("fixture-only-secret"));
    assert!(!completed["account_name"]
        .as_str()
        .unwrap()
        .contains("fixture-access-token"));
    assert_eq!(completed["account_id"], account_id(1));
    assert_eq!(page["next_cursor"], Value::Null);
    harness.close().await;
}

#[tokio::test]
async fn saved_summary_survives_report_cleanup_while_missing_history_stays_unavailable() {
    let harness = Harness::new().await;
    let runtime = &harness.runtime;
    let mut preserved = job(20, "completed", AT);
    preserved["report"] = report("gpt-6-luna", 0.88, 5);
    preserved["first_group_valid"] = json!(true);
    preserved["first_group_prediction"] = json!("gpt-6-sol");
    preserved["first_group_probability"] = json!(0.72);
    detection_logs::preserve_summary(&mut preserved);
    assert_eq!(preserved["result_summary"]["aggregate_downgrade"], true);
    assert_eq!(preserved["result_summary"]["analysis_available"], true);
    preserved["report"] = Value::Null;
    preserved["first_group_valid"] = json!(false);
    preserved["first_group_prediction"] = Value::Null;
    preserved["first_group_probability"] = Value::Null;
    preserved["requested_model"] = json!("");

    let missing = job(21, "completed", AT);
    insert_jobs(runtime, [preserved.clone(), missing]);
    let page = runtime.detection.logs(None, 100).unwrap();
    let restored = item(&page, &job_id(20));
    assert_eq!(restored["details_available"], false);
    assert_eq!(restored["summary"]["analysis_available"], true);
    assert_eq!(restored["summary"]["outcome"], "aggregate_degraded");
    assert_eq!(restored["summary"]["first_prediction"], "gpt-6-sol");
    assert_eq!(restored["summary"]["first_probability"], 0.72);
    assert_eq!(restored["summary"]["prediction"], "gpt-6-luna");
    assert_eq!(restored["summary"]["probability"], 0.88);
    assert_eq!(restored["summary"]["used_outputs"], 5);
    assert_eq!(restored["summary"]["aggregate_downgrade"], true);

    let unavailable = item(&page, &job_id(21));
    assert_eq!(unavailable["details_available"], false);
    assert_eq!(unavailable["summary"]["outcome"], "unavailable");
    assert_eq!(unavailable["summary"]["analysis_available"], false);
    assert_eq!(unavailable["summary"]["prediction"], "");
    assert_eq!(unavailable["summary"]["first_prediction"], "");
    harness.close().await;
}

#[tokio::test]
async fn same_timestamp_cursor_pages_are_stable_and_read_only() {
    let harness = Harness::new().await;
    let runtime = &harness.runtime;
    insert_jobs(
        runtime,
        [
            job(31, "completed", AT),
            job(32, "completed", AT),
            job(33, "completed", AT),
        ],
    );
    let paths = state_paths(runtime);
    let before = file_state(&paths);
    let first = runtime.detection.logs(None, 2).unwrap();
    assert_eq!(
        first["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["job_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        [job_id(33), job_id(32)]
    );
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let second = runtime.detection.logs(Some(&cursor), 2).unwrap();
    assert_eq!(
        second["items"][0]["job_id"],
        job_id(31),
        "the equal-time id tie-break must neither duplicate nor skip records"
    );
    assert_eq!(second["next_cursor"], Value::Null);
    assert_eq!(before, file_state(&paths));
    harness.close().await;
}

#[tokio::test]
async fn submicrosecond_cursor_pages_preserve_created_at_precision() {
    let harness = Harness::new().await;
    let runtime = &harness.runtime;
    insert_jobs(
        runtime,
        [
            (31, "2026-10-20T12:00:00.123456900+00:00"),
            (32, "2026-10-20T12:00:00.123456800+00:00"),
            (33, "2026-10-20T12:00:00.123456700+00:00"),
        ]
        .into_iter()
        .map(|(number, at)| job(number, "completed", at)),
    );

    let expected = [
        (job_id(31), "2026-10-20T12:00:00.123456900+00:00"),
        (job_id(32), "2026-10-20T12:00:00.123456800+00:00"),
        (job_id(33), "2026-10-20T12:00:00.123456700+00:00"),
    ];
    let mut cursor = None;
    for (index, (expected_id, expected_created_at)) in expected.iter().enumerate() {
        let page = runtime.detection.logs(cursor.as_deref(), 1).unwrap();
        let items = page["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "page {index} must contain one record");
        assert_eq!(items[0]["job_id"].as_str(), Some(expected_id.as_str()));
        assert_eq!(
            items[0]["created_at"].as_str(),
            Some(*expected_created_at),
            "created_at must preserve all nanosecond digits on page {index}"
        );

        if index + 1 < expected.len() {
            cursor = Some(
                page["next_cursor"]
                    .as_str()
                    .expect("next page cursor")
                    .to_owned(),
            );
        } else {
            assert_eq!(page["next_cursor"], Value::Null);
        }
    }

    harness.close().await;
}

#[tokio::test]
async fn lifecycle_hooks_persist_terminal_summaries_and_log_reads_are_read_only() {
    let harness = Harness::new().await;
    let runtime = &harness.runtime;
    let cancelled_id = job_id(61);
    let interrupted_id = job_id(62);

    let mut queued = job(61, "queued", AT);
    queued["groups"] = json!([{"index":1,"status":"queued","error":"","attempts":0}]);
    let mut running = job(62, "running", AT);
    running["groups"] = json!([{"index":1,"status":"running","error":"","attempts":1}]);
    running["report"] = report("gpt-6-astra", 0.88, 5);
    insert_jobs(runtime, [queued, running]);

    let cancelled = runtime.tests.cancel(&cancelled_id).await.unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["groups"][0]["status"], "cancelled");
    let cancelled_summary =
        runtime.tests.data().unwrap()["jobs"][&cancelled_id]["result_summary"].clone();
    assert_eq!(cancelled_summary["outcome"], "cancelled");
    assert_eq!(cancelled_summary["analysis_available"], false);

    runtime.tests.startup().unwrap();
    let interrupted = runtime.tests.data().unwrap()["jobs"][&interrupted_id].clone();
    assert_eq!(interrupted["status"], "interrupted");
    assert_eq!(interrupted["report"], report("gpt-6-astra", 0.88, 5));
    assert_eq!(interrupted["result_summary"]["outcome"], "interrupted");
    assert_eq!(interrupted["result_summary"]["analysis_available"], true);
    assert_eq!(interrupted["result_summary"]["prediction"], "gpt-6-astra");
    assert_eq!(interrupted["result_summary"]["probability"], 0.88);
    assert_eq!(interrupted["result_summary"]["used_outputs"], 5);

    let paths = state_paths(runtime);
    let before = file_state(&paths);
    let logs = runtime.detection.logs(None, 100).unwrap();
    assert_eq!(
        before,
        file_state(&paths),
        "GET logs must not mutate state files"
    );
    assert_eq!(
        item(&logs, &cancelled_id)["summary"]["outcome"],
        "cancelled"
    );
    let interrupted_log = item(&logs, &interrupted_id);
    assert_eq!(interrupted_log["summary"]["outcome"], "interrupted");
    assert_eq!(interrupted_log["summary"]["analysis_available"], true);
    assert_eq!(interrupted_log["summary"]["prediction"], "gpt-6-astra");
    assert_eq!(interrupted_log["summary"]["probability"], 0.88);
    assert_eq!(interrupted_log["summary"]["used_outputs"], 5);
    assert_eq!(interrupted_log["details_available"], true);

    harness.close().await;
}

#[tokio::test]
async fn malformed_summary_and_automatic_job_metadata_are_rejected() {
    let harness = Harness::new().await;
    let runtime = &harness.runtime;
    let id = job_id(52);
    let mut fixture = job(52, "completed", AT);
    fixture["result_summary"] = saved_summary();
    insert_jobs(runtime, [fixture]);
    let valid_state = runtime.tests.data().unwrap();
    let path = runtime.tests.store.path();

    let mut malformed_summaries = Vec::new();
    let mut invalid_boolean = saved_summary();
    invalid_boolean["analysis_available"] = json!("true");
    malformed_summaries.push(invalid_boolean);
    let mut invalid_probability = saved_summary();
    invalid_probability["first_probability"] = json!(1.01);
    malformed_summaries.push(invalid_probability);
    let mut invalid_receipt = saved_summary();
    invalid_receipt["verification"] = json!({"status":"verified","completed_at":"not-a-timestamp"});
    malformed_summaries.push(invalid_receipt);

    for summary in malformed_summaries {
        let mut malformed = valid_state.clone();
        malformed["jobs"][&id]["result_summary"] = summary;
        sub2ops_server::store::write_json(path, &malformed).unwrap();
        assert_eq!(
            runtime
                .detection
                .logs(None, 10)
                .unwrap_err()
                .status
                .as_u16(),
            503
        );
    }

    let mut invalid_automatic = valid_state.clone();
    invalid_automatic["jobs"][&id]["automatic"] = json!("true");
    sub2ops_server::store::write_json(path, &invalid_automatic).unwrap();
    assert_eq!(
        runtime
            .detection
            .logs(None, 10)
            .unwrap_err()
            .status
            .as_u16(),
        503
    );

    let mut mismatched_key = valid_state.clone();
    let row = mismatched_key["jobs"]
        .as_object_mut()
        .unwrap()
        .remove(&id)
        .unwrap();
    mismatched_key["jobs"]["fixture-storage-key"] = row;
    sub2ops_server::store::write_json(path, &mismatched_key).unwrap();
    assert_eq!(
        runtime
            .detection
            .logs(None, 10)
            .unwrap_err()
            .status
            .as_u16(),
        503
    );

    sub2ops_server::store::write_json(path, &valid_state).unwrap();
    assert_eq!(
        runtime.detection.logs(None, 10).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    harness.close().await;
}

#[tokio::test]
async fn outcomes_receipts_and_merged_or_late_notifications_remain_independent() {
    let harness = Harness::new().await;
    let runtime = &harness.runtime;
    let mut normal = job(41, "completed", AT);
    normal["result_summary"] = saved_summary();
    let mut plain_normal = job(48, "completed", AT);
    plain_normal["result_summary"] = saved_summary();
    let mut degraded = job(42, "completed", AT);
    degraded["result_summary"] = saved_summary();
    degraded["result_summary"]["first_prediction"] = json!("gpt-5.6-luna");
    degraded["result_summary"]["first_downgrade"] = json!(true);
    degraded["result_summary"]["outcome"] = json!("degraded");

    let mut recheck = job(43, "completed", AT);
    recheck["result_summary"] = saved_summary();
    recheck["degradation_recheck"] = json!(true);
    recheck["triggers"] = json!(["recovery:fixture"]);

    let mut restored = job(44, "completed", AT);
    restored["result_summary"] = saved_summary();
    restored["result_summary"]["verification"] = receipt("verified", &job_id(44));

    let credit_fingerprint = "fixture-credit-episode-45";
    let mut credit = job(45, "completed", AT);
    credit["result_summary"] = saved_summary();
    credit["recovery_context"] = json!({"kind":"credit","fingerprint":credit_fingerprint});

    let mut pending = job(46, "completed", AT);
    pending["result_summary"] = saved_summary();

    let mut late = job(47, "completed", AT);
    late["result_summary"] = saved_summary();
    insert_jobs(
        runtime,
        [
            normal,
            degraded,
            recheck,
            restored,
            credit,
            pending,
            late,
            plain_normal,
        ],
    );

    runtime
        .detection
        .store
        .transaction(|state| {
            state["disposition_notifications"][job_id(41)] = json!({
                "kind":"disposition","status":"delivered","delivered_at":AT,
                "job_id":job_id(41),"created_at":AT,"reason":"fixture"
            });
            let mut control = model_detection::defaults();
            control["recheck_recovery"] = receipt("completed", &job_id(43));
            state["accounts"][account_id(43).to_string()] = control;
            Ok(())
        })
        .unwrap();

    runtime
        .monitor
        .store
        .transaction(|state| {
            state["pending_events"]["fixture-pending-46"] = json!({"model_test_job_id":job_id(46)});
            state["recovery_history"]["fixture-recovery-44"] = json!({
                "model_test_job_id":job_id(44),"account_id":account_id(44),"recovered_at":AT
            });
            state["scheduler"][account_id(45).to_string()]["credit_result_notifications"]
                [credit_fingerprint] = json!({
                "status":"delivered","episode":credit_fingerprint,"outcome":"restored",
                "account_id":account_id(45),"attempts":1,"completed_at":AT,"next_at":null,
                "delivered_at":AT,"reason":"fixture","job_id":job_id(45)
            });
            Ok(())
        })
        .unwrap();

    let first = runtime.detection.logs(None, 100).unwrap();
    let normal = item(&first, &job_id(41));
    assert_eq!(normal["summary"]["outcome"], "normal");
    assert_eq!(normal["notification"]["status"], "delivered");
    assert_eq!(normal["notification"]["merged"], false);
    assert_eq!(normal["summary"]["disposition"]["status"], "completed");
    assert_eq!(normal["summary"]["disposition"]["marked"], true);
    assert_eq!(normal["summary"]["disposition"]["schedule_verified"], true);
    assert_eq!(normal["summary"]["verification"], Value::Null);
    assert_eq!(normal["summary"]["restoration"], Value::Null);
    let plain_normal = item(&first, &job_id(48));
    assert_eq!(plain_normal["summary"]["outcome"], "normal");
    for field in ["disposition", "recovery", "verification", "restoration"] {
        assert_eq!(plain_normal["summary"][field], Value::Null, "{field}");
    }

    let degraded = item(&first, &job_id(42));
    assert_eq!(degraded["summary"]["outcome"], "degraded");
    for field in ["disposition", "recovery", "verification", "restoration"] {
        assert_eq!(degraded["summary"][field], Value::Null, "{field}");
    }

    let recheck = item(&first, &job_id(43));
    assert_eq!(recheck["summary"]["recovery"]["status"], "completed");
    assert_eq!(recheck["summary"]["recovery"]["mark_cleared"], true);
    assert!(recheck["triggers"]
        .as_array()
        .unwrap()
        .contains(&json!("recheck")));

    let restored = item(&first, &job_id(44));
    assert_eq!(restored["summary"]["verification"]["status"], "verified");
    assert_eq!(restored["summary"]["restoration"]["status"], "restored");

    let credit = item(&first, &job_id(45));
    assert_eq!(credit["notification"]["status"], "delivered");
    assert_eq!(credit["notification"]["merged"], true);
    assert_eq!(
        credit["notification"]["delivered_at"],
        "2026-10-20T12:00:00+00:00"
    );

    let pending = item(&first, &job_id(46));
    assert_eq!(pending["notification"]["status"], "queued");
    assert_eq!(pending["notification"]["merged"], false);

    assert_eq!(item(&first, &job_id(47))["notification"], Value::Null);
    runtime
        .detection
        .store
        .transaction(|state| {
            state["disposition_notifications"][job_id(47)] = json!({
                "kind":"disposition","status":"delivered","delivered_at":AT,
                "job_id":job_id(47),"created_at":AT,"reason":"late fixture"
            });
            Ok(())
        })
        .unwrap();
    let after_notice = runtime.detection.logs(None, 100).unwrap();
    let late = item(&after_notice, &job_id(47));
    assert_eq!(late["notification"]["status"], "delivered");
    assert_eq!(late["notification"]["merged"], false);
    assert_eq!(late["summary"]["disposition"]["marked"], true);
    harness.close().await;
}

#[tokio::test]
async fn invalid_query_and_corrupt_store_inputs_have_bounded_status_errors() {
    let harness = Harness::new().await;
    let runtime = &harness.runtime;
    insert_jobs(runtime, [job(51, "completed", AT)]);
    runtime.detection.store.transaction(|_| Ok(())).unwrap();
    runtime.monitor.store.transaction(|_| Ok(())).unwrap();

    for (before, limit) in [(None, 0), (None, 101), (Some("not-a-cursor"), 10)] {
        let error = runtime.detection.logs(before, limit).unwrap_err();
        assert_eq!(error.status.as_u16(), 422);
    }

    for path in state_paths(runtime) {
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"{").unwrap();
        let error = runtime.detection.logs(None, 10).unwrap_err();
        assert_eq!(
            error.status.as_u16(),
            503,
            "corrupt store: {}",
            path.display()
        );
        fs::write(path, original).unwrap();
    }
    let path = &runtime.settings.usage_path;
    let original = fs::read(path).unwrap();
    for section in ["pending_events", "recovery_history"] {
        runtime.detection.logs(None, 10).unwrap();
        let mut corrupt: Value = serde_json::from_slice(&original).unwrap();
        corrupt[section]["fixture-invalid-row"] = json!(false);
        fs::write(path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        assert_eq!(
            runtime
                .detection
                .logs(None, 10)
                .unwrap_err()
                .status
                .as_u16(),
            503
        );
        fs::write(path, &original).unwrap();
    }
    harness.close().await;
}

// The following reader fragments are copied from the 0.1.47 baseline
// (28a89996f8ffa380c90d4e7b21c7c5284a9d0e83) and only renamed for this test.
fn validate_0147(value: &Value) -> sub2ops_server::error::Result<()> {
    let error = || Error::new(503, "模型测试状态损坏");
    if !value.is_object()
        || value["version"] != 1
        || value["accounts"].as_object().is_none_or(|rows| {
            rows.values().any(|v| {
                !v.is_object() || string(&v["id"]).is_empty() || v["account_id"].as_i64().is_none()
            })
        })
        || value.get("requests").is_some_and(|v| !v.is_object())
        || value.get("jobs").is_some_and(|v| {
            v.as_object().is_none_or(|rows| {
                rows.values()
                    .any(|v| !v.is_object() || string(&v["id"]).is_empty())
            })
        })
    {
        return Err(error());
    }
    Ok(())
}

fn normalized_0147(mut value: Value) -> Value {
    if value.get("requests").is_none() {
        value["requests"] = json!({});
    }
    if value.get("jobs").is_none() {
        value["jobs"] = value["accounts"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, v)| (string(&v["id"]).to_owned(), v.clone()))
            .collect::<serde_json::Map<_, _>>()
            .into();
    }
    value
}

const OAUTH_FIELDS_0147: &[&str] = &[
    "oauth_auto_reset_credit_enabled",
    "oauth_daily_test_enabled",
    "oauth_daily_test_time",
    "oauth_early_probe_batch_size",
    "oauth_recovery_connection_account_ids",
    "oauth_recovery_model_account_ids",
    "oauth_recovery_monitor_enabled",
    "oauth_recovery_test_concurrency",
    "oauth_recovery_test_model_id",
    "oauth_usage_refresh_concurrency",
];

fn oauth_values_0147(raw: &Value, defaults: &Value) -> Value {
    OAUTH_FIELDS_0147
        .iter()
        .map(|key| {
            (
                (*key).to_owned(),
                raw.get(*key).unwrap_or(&defaults[*key]).clone(),
            )
        })
        .collect()
}

#[test]
fn baseline_0147_readers_accept_new_optional_summary_and_config_keys() {
    let id = job_id(61);
    let summary = json!({
        "outcome":"aggregate_degraded","analysis_available":true,
        "first_prediction":"gpt-6-sol","first_probability":0.72,
        "prediction":"gpt-6-luna","probability":0.88,"used_outputs":5,
        "first_downgrade":false,"aggregate_downgrade":true,
        "disposition":null,"recovery":null,"verification":null,"restoration":null
    });
    let current_state = json!({
        "version":1,"accounts":{},"requests":{},
        "jobs":{id.clone():{"id":id,"account_id":account_id(61),"automatic":true,
            "status":"completed","result_summary":summary}}
    });
    assert_eq!(
        current_state["version"], 1,
        "adding a summary must not bump the state version"
    );
    validate_0147(&current_state).unwrap();
    let old_read = normalized_0147(current_state.clone());
    assert_eq!(old_read["jobs"][&id]["result_summary"], summary);

    let new_config = json!({
        "model_detection_model_id":"gpt-6-luna",
        "oauth_daily_test_enabled":false
    });
    sub2ops_server::store::valid_object(&new_config).unwrap();
    let old_defaults = json!({"oauth_auto_reset_credit_enabled":false,"oauth_daily_test_enabled":true,
        "oauth_daily_test_time":"05:00","oauth_early_probe_batch_size":8,
        "oauth_recovery_connection_account_ids":[],"oauth_recovery_model_account_ids":[],
        "oauth_recovery_monitor_enabled":true,"oauth_recovery_test_concurrency":2,
        "oauth_recovery_test_model_id":"gpt-5.6-luna","oauth_usage_refresh_concurrency":4});
    let old_config_view = oauth_values_0147(&new_config, &old_defaults);
    assert_eq!(old_config_view["oauth_daily_test_enabled"], false);
    assert!(old_config_view.get("model_detection_model_id").is_none());
    assert_eq!(new_config["model_detection_model_id"], "gpt-6-luna");
}

fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

#[test]
fn sol_rule_changes_only_future_verdicts_and_keeps_saved_history() {
    let mut old = job(90, "completed", AT);
    old["first_group_valid"] = true.into();
    old["first_group_prediction"] = "gpt-5.6-sol".into();
    old["report"] = report("gpt-5.6-sol", 0.9, 3);
    old["result_summary"] =
        json!({"outcome":"degraded","first_downgrade":true,"aggregate_downgrade":true});
    let saved = old.clone();
    let summary = detection_logs::summary(&old);
    assert_eq!(summary["outcome"], "degraded");
    assert_eq!(summary["first_downgrade"], true);
    assert_eq!(summary["aggregate_downgrade"], true);
    assert_eq!(old, saved);
    old["result_summary"] = Value::Null;
    let fresh = detection_logs::summary(&old);
    assert_eq!(fresh["outcome"], "normal");
    assert_eq!(fresh["first_downgrade"], false);
    assert_eq!(fresh["aggregate_downgrade"], false);
    old["completion_reason"] = "automatic_degradation".into();
    assert_eq!(detection_logs::summary(&old)["outcome"], "degraded");
}
