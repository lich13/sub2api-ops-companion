use super::*;

struct LmState {
    mode: usize,
    numeric: AtomicUsize,
    baselines: AtomicUsize,
    requests: Mutex<Vec<Value>>,
    release: Arc<Semaphore>,
}
struct LmProvider {
    state: Arc<LmState>,
    task: JoinHandle<()>,
}
impl Drop for LmProvider {
    fn drop(&mut self) {
        self.state.release.add_permits(8);
        self.task.abort();
    }
}
async fn provider(h: &RuntimeHarness, mode: usize) -> LmProvider {
    let state = Arc::new(LmState {
        mode,
        numeric: AtomicUsize::new(0),
        baselines: AtomicUsize::new(0),
        requests: Mutex::new(vec![]),
        release: Arc::new(Semaphore::new(0)),
    });
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    h.runtime
        .tests
        .set_test_endpoint(&format!(
            "http://{}/v1/responses",
            listener.local_addr().unwrap()
        ))
        .unwrap();
    let router = Router::new()
        .fallback(lm_response)
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    LmProvider { state, task }
}
async fn lm_response(State(state): State<Arc<LmState>>, request: Request) -> Response {
    let bytes = to_bytes(request.into_body(), 2_000_000).await.unwrap();
    let payload: Value = serde_json::from_slice(&bytes).unwrap();
    let prompt = prompt_from_payload(&payload).unwrap();
    state.requests.lock().unwrap().push(payload.clone());
    let bank: Value = serde_json::from_str(include_str!(
        "../../resources/lm-detector/tokenizer_bank.json"
    ))
    .unwrap();
    let prefix = bank["wrapper"]["prefix"].as_str().unwrap();
    let suffix = bank["wrapper"]["suffix"].as_str().unwrap();
    let probe = prompt
        .strip_prefix(prefix)
        .and_then(|s| s.strip_suffix(suffix));
    let (text, tokens) = if let Some(text) = probe {
        assert!(payload.get("max_output_tokens").is_none());
        if state.mode == 3 {
            return ResponsePlan::hanging_sse(
                b"data: {\"type\":\"response.created\"}\n\n".to_vec(),
            )
            .into_response();
        }
        let tokens = if text.is_empty() {
            let n = state.baselines.fetch_add(1, Ordering::SeqCst);
            if state.mode == 2 && n > 0 {
                49
            } else {
                31
            }
        } else {
            let index = bank["probes"]
                .as_array()
                .unwrap()
                .iter()
                .position(|p| p["text"] == text)
                .unwrap();
            31 + bank["classes"][0]["counts"][index].as_i64().unwrap()
        };
        ("OK".to_owned(), tokens)
    } else {
        let n = state.numeric.fetch_add(1, Ordering::SeqCst);
        if state.mode == 8 && n < 3 {
            return status_error(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error":{"message":"fixture retryable rejection"}}),
            )
            .into_response();
        }
        if state.mode == 5 {
            return status_error(
                StatusCode::UNAUTHORIZED,
                json!({"error":{"message":"fixture rejected"}}),
            )
            .into_response();
        }
        if state.mode == 7 {
            return ResponsePlan::broken_sse(partial_delta("1, 2, 3")).into_response();
        }
        if state.mode == 6 && n == 2 {
            state.release.acquire().await.unwrap().forget();
        }
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/contracts/lm-detector/analyze.json"
        ))
        .unwrap();
        (
            if state.mode == 4 {
                "1, 2, 3".to_owned()
            } else {
                fixture["cases"][0]["outputs"][n % 3]["text"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            },
            200,
        )
    };
    let mut response = response_output(&text, "fixture-model");
    response["object"] = "response".into();
    if !(state.mode == 1 && probe.is_some()) {
        response["usage"] = json!({"input_tokens":tokens,"output_tokens":1});
    }
    ResponsePlan::bytes(
        StatusCode::OK,
        "application/json",
        serde_json::to_vec(&response).unwrap(),
    )
    .into_response()
}
async fn start(
    h: &RuntimeHarness,
    id: i64,
    count: usize,
    probe: bool,
    concurrency: usize,
) -> Value {
    let row = h.runtime.actions.account(id, None).await.unwrap();
    h.runtime.tests.start(id,&json!({"model_id":"gpt-6-luna","expected_version":account_version(&row),"request_id":uuid::Uuid::new_v4().simple().to_string(),"engine":"lm_detector","sample_count":count,"tokenizer_probe":probe,"concurrency":concurrency}),None).await.unwrap()
}
fn platform(h: &RuntimeHarness, engine: &str, count: usize) {
    h.runtime
        .config
        .oauth
        .transaction(|v| {
            v["model_detection_platforms"] =
                json!({"openai":{"engine":engine,"sample_count":count}});
            v["model_detection_model_id"] = "gpt-6-luna".into();
            Ok(())
        })
        .unwrap();
}
async fn wait<F: Fn(&Value) -> bool>(h: &RuntimeHarness, id: &str, ready: F) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let job = h.runtime.tests.get(id).unwrap();
            if ready(&job) {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn lm_manual_counts_tokenizer_missing_usage_drift_and_invalid_ranking() {
    for (mode, kind, count, concurrency) in [
        (0, "oauth", 1, 1),
        (1, "apikey", 1, 2),
        (2, "apikey", 3, 3),
        (4, "apikey", 1, 1),
    ] {
        let h = RuntimeHarness::new().await;
        h.seed_identity().await;
        let p = provider(&h, mode).await;
        let id = h.seed_account(kind, None, json!({})).await;
        let job = start(&h, id, count, true, concurrency).await;
        let result = h.wait_for_terminal(job["id"].as_str().unwrap()).await;
        assert_eq!(result["engine"], "lm_detector");
        assert_eq!(result["planned_groups"], count);
        assert_eq!(p.state.numeric.load(Ordering::SeqCst), count);
        assert_eq!(result["status"], "completed");
        assert!(result["automatic_disposition"].is_null());
        if count == 1 {
            assert!(result["report"]["probability"].is_null());
        }
        if mode == 1 {
            assert_eq!(result["tokenizer"]["status"], "failed");
            assert!(result["tokenizer"]["error"]
                .as_str()
                .unwrap()
                .contains("输入 Token"));
        } else {
            assert_eq!(result["tokenizer"]["status"], "completed");
            assert!(result["tokenizer"]["requests"].as_u64().unwrap() <= 22);
            assert_eq!(p.state.baselines.load(Ordering::SeqCst), 2);
        }
        if mode == 2 {
            assert_eq!(result["tokenizer"]["baseline_drift"], true);
        }
        if mode == 4 {
            assert_eq!(result["report"]["decision"], "unscorable");
        }
        let state = fs::read_to_string(h._temp.path().join("model-test-results.json")).unwrap();
        assert!(!state.contains("47, 203, 12"));
        h.close().await;
    }
}
#[tokio::test]
async fn lm_cancellation_and_network_failures_never_replay_or_erase_numeric_result() {
    for mode in [3, 5, 7] {
        let h = RuntimeHarness::new().await;
        let p = provider(&h, mode).await;
        let id = h.seed_account("apikey", None, json!({})).await;
        let job = start(&h, id, 1, true, 1).await;
        let key = job["id"].as_str().unwrap();
        let result = if mode == 3 {
            let before = wait(&h, key, |v| {
                v["phase"] == "tokenizer"
                    && v["tokenizer"]["requests"].as_u64().is_some_and(|n| n > 0)
            })
            .await;
            assert_eq!(before["report"]["decision"], "partial");
            let cancelled = h.runtime.tests.cancel(key).await.unwrap();
            assert_eq!(cancelled["report"], before["report"]);
            assert_eq!(cancelled["tokenizer"]["status"], "cancelled");
            assert_eq!(cancelled["status"], "completed");
            cancelled
        } else {
            h.wait_for_terminal(key).await
        };
        if mode != 3 {
            assert!(result["tokenizer"].is_null());
            assert_eq!(result["status"], "failed");
        }
        assert_eq!(p.state.numeric.load(Ordering::SeqCst), 1);
        assert_ne!(result["can_retry"], true);
        assert!(h
            .runtime
            .tests
            .retry_failed(key, &uuid::Uuid::new_v4().simple().to_string())
            .await
            .is_err());
        h.close().await;
    }
}
#[tokio::test]
async fn lm_three_scheduled_samples_wait_and_settings_only_affect_new_jobs() {
    let h = RuntimeHarness::new().await;
    let p = provider(&h, 6).await;
    let id = h.seed_account("apikey", None, json!({})).await;
    platform(&h, "lm_detector", 3);
    h.runtime
        .detection
        .update(id, json!({"enabled":true,"schedule_mode":"fixed"}))
        .unwrap();
    let hold = h
        .runtime
        .budget
        .clone()
        .acquire_many_owned(h.runtime.budget.available_permits() as u32)
        .await
        .unwrap();
    let job = h
        .runtime
        .detection
        .trigger(id, &["scheduled".into()])
        .await
        .unwrap();
    let key = job["id"].as_str().unwrap();
    assert_eq!(job["engine"], "lm_detector");
    assert_eq!(job["planned_groups"], 3);
    assert!(job["resource_revision"].is_string());
    platform(&h, "modeltrace", 1);
    drop(hold);
    let partial = wait(&h, key, |v| v["completed_groups"] == 2).await;
    assert_eq!(partial["status"], "running");
    assert!(partial["automatic_disposition"].is_null());
    assert_eq!(partial["planned_groups"], 3);
    p.state.release.add_permits(1);
    let completed = h.wait_for_terminal(key).await;
    assert_eq!(completed["classification"]["outcome"], "normal");
    assert_eq!(completed["resource_revision"], job["resource_revision"]);
    assert_eq!(completed["report"]["used_outputs"], 3);
    let row = h.runtime.actions.account(id, None).await.unwrap();
    let payload = json!({"model_id":"gpt-6-luna","expected_version":account_version(&row),"request_id":job["request_id"],"engine":"lm_detector","sample_count":1,"concurrency":1});
    assert!(h.runtime.tests.start(id, &payload, None).await.is_err());
    h.close().await;
}
#[tokio::test]
async fn lm_only_scheduled_and_rechecks_use_platform_engine_and_normal_rechecks_clear_marks() {
    let h = RuntimeHarness::new().await;
    let _p = provider(&h, 0).await;
    platform(&h, "lm_detector", 1);
    let hold = h
        .runtime
        .budget
        .clone()
        .acquire_many_owned(h.runtime.budget.available_permits() as u32)
        .await
        .unwrap();
    for cause in ["slow:fixture", "error:fixture", "scheduled"] {
        let id = h.seed_account("apikey", None, json!({})).await;
        h.runtime
            .detection
            .update(id, json!({"enabled":true,"schedule_mode":"fixed"}))
            .unwrap();
        let job = h
            .runtime
            .detection
            .trigger(id, &[cause.into()])
            .await
            .unwrap();
        assert_eq!(
            job["engine"],
            if cause == "scheduled" {
                "lm_detector"
            } else {
                "modeltrace"
            }
        );
        h.runtime
            .tests
            .cancel(job["id"].as_str().unwrap())
            .await
            .unwrap();
    }
    drop(hold);
    let id = h.seed_account("apikey", None, json!({})).await;
    h.runtime
        .detection
        .update(id, json!({"enabled":true,"schedule_mode":"fixed"}))
        .unwrap();
    let mark = h.runtime.detection.mark(id).unwrap();
    h.runtime
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            mark["version"].as_str().unwrap(),
            chrono::Utc::now(),
            None,
        )
        .unwrap();
    let job = h
        .runtime
        .detection
        .trigger(id, &["scheduled".into()])
        .await
        .unwrap();
    assert_eq!(job["degradation_recheck"], true);
    let result = h.wait_for_terminal(job["id"].as_str().unwrap()).await;
    let receipt = h.runtime.detection.complete_recheck(&result).await.unwrap();
    assert_eq!(receipt["mark_cleared"], true);
    assert_eq!(
        h.runtime.actions.account(id, None).await.unwrap()["schedulable"],
        true
    );
    let _invalid = provider(&h, 4).await;
    platform(&h, "lm_detector", 3);
    let id = h.seed_account("apikey", None, json!({})).await;
    h.runtime
        .detection
        .update(id, json!({"enabled":true,"schedule_mode":"fixed"}))
        .unwrap();
    let mark = h.runtime.detection.mark(id).unwrap();
    h.runtime
        .detection
        .capacity
        .store
        .set_mark(
            id,
            true,
            mark["version"].as_str().unwrap(),
            chrono::Utc::now(),
            None,
        )
        .unwrap();
    // An indeterminate completed recheck must retain the mark and scheduling.
    let job = h
        .runtime
        .detection
        .trigger(id, &["scheduled".into()])
        .await
        .unwrap();
    let result = h.wait_for_terminal(job["id"].as_str().unwrap()).await;
    assert_eq!(result["planned_groups"], 3);
    assert_eq!(result["classification"]["outcome"], "unavailable");
    let receipt = h.runtime.detection.complete_recheck(&result).await.unwrap();
    assert_ne!(receipt["mark_cleared"], true);
    assert_eq!(h.runtime.detection.mark(id).unwrap()["marked"], true);
    assert_eq!(
        h.runtime.actions.account(id, None).await.unwrap()["schedulable"],
        true
    );
    h.close().await;
}

#[tokio::test]
async fn lm_restart_interrupts_tokenizer_without_losing_numeric_result_or_replaying() {
    let h = RuntimeHarness::new().await;
    let p = provider(&h, 3).await;
    let id = h.seed_account("apikey", None, json!({})).await;
    let job = start(&h, id, 1, true, 1).await;
    let key = job["id"].as_str().unwrap();
    let before = wait(&h, key, |v| {
        v["phase"] == "tokenizer" && v["tokenizer"]["requests"].as_u64().is_some_and(|n| n > 0)
    })
    .await;
    h.runtime.tests.close().await;
    let restarted = sub2ops_server::model_tests::ModelTests::new(
        h.runtime.actions.clone(),
        h.runtime.tests.bank.clone(),
        h.runtime.tests.hooks.clone(),
        h.runtime.budget.clone(),
    );
    restarted.startup().unwrap();
    restarted.resume().unwrap();
    let result = restarted.get(key).unwrap();
    assert_eq!(result["status"], "interrupted");
    assert_eq!(result["tokenizer"]["status"], "interrupted");
    assert_eq!(result["report"], before["report"]);
    assert_ne!(result["can_retry"], true);
    assert_eq!(p.state.numeric.load(Ordering::SeqCst), 1);
    restarted.close().await;
    h.close().await;
}

#[tokio::test]
async fn lm_definite_failures_retry_the_pinned_context_and_tokenizer_holds_account_exclusion() {
    let h = RuntimeHarness::new().await;
    let p = provider(&h, 8).await;
    let id = h.seed_account("apikey", None, json!({})).await;
    let job = start(&h, id, 1, false, 1).await;
    let key = job["id"].as_str().unwrap();
    let failed = h.wait_for_terminal(key).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["can_retry"], true);
    platform(&h, "modeltrace", 3);
    let retried = h
        .runtime
        .tests
        .retry_failed(key, &uuid::Uuid::new_v4().simple().to_string())
        .await
        .unwrap();
    assert_eq!(retried["engine"], "lm_detector");
    assert_eq!(retried["bank_version"], job["bank_version"]);
    let completed = h.wait_for_terminal(key).await;
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["classification"]["outcome"], "normal");
    assert_eq!(p.state.numeric.load(Ordering::SeqCst), 4);
    let probes = provider(&h, 3).await;
    let first = start(&h, id, 1, true, 1).await;
    let first_key = first["id"].as_str().unwrap();
    wait(&h, first_key, |v| {
        v["phase"] == "tokenizer" && v["tokenizer"]["requests"].as_u64().is_some_and(|n| n > 0)
    })
    .await;
    let second = start(&h, id, 1, false, 1).await;
    assert_eq!(second["status"], "queued");
    assert!(h
        .runtime
        .actions
        .locks
        .try_account(&h.runtime.actions.account(id, None).await.unwrap())
        .is_err());
    assert_eq!(probes.state.numeric.load(Ordering::SeqCst), 1);
    h.runtime.tests.cancel(first_key).await.unwrap();
    let second = h.wait_for_terminal(second["id"].as_str().unwrap()).await;
    assert_eq!(second["status"], "completed");
    assert_eq!(probes.state.numeric.load(Ordering::SeqCst), 2);
    h.close().await;
}
