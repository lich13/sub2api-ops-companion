use super::*;

struct LmState {
    mode: usize,
    numeric: AtomicUsize,
    baselines: AtomicUsize,
    probes_active: AtomicUsize,
    probes_peak: AtomicUsize,
    failed_prompt: Mutex<Option<String>>,
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
        probes_active: AtomicUsize::new(0),
        probes_peak: AtomicUsize::new(0),
        failed_prompt: Mutex::new(None),
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
        let active = state.probes_active.fetch_add(1, Ordering::SeqCst) + 1;
        state.probes_peak.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(15)).await;
        state.probes_active.fetch_sub(1, Ordering::SeqCst);
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
        if state.mode == 9 && n == 1 {
            *state.failed_prompt.lock().unwrap() = Some(prompt.into());
        }
        let failed_group = state.mode == 9
            && state.failed_prompt.lock().unwrap().as_deref() == Some(prompt)
            && state
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|p| prompt_from_payload(p) == Some(prompt))
                .count()
                <= 3;
        if state.mode == 8 && n < 3 || failed_group {
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
    if payload.get("messages").is_some() {
        response = if payload.get("max_tokens").is_some() && payload.get("stream_options").is_none()
        {
            json!({"type":"message","model":payload["model"],"content":[{"type":"text","text":text}],"stop_reason":"end_turn","usage":{"input_tokens":tokens,"output_tokens":1}})
        } else {
            json!({"model":payload["model"],"choices":[{"message":{"content":text},"finish_reason":"stop"}],"usage":{"prompt_tokens":tokens,"completion_tokens":1}})
        };
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
            assert_eq!(p.state.probes_peak.load(Ordering::SeqCst), 1);
            let steps = result["tokenizer"]["steps"].as_array().unwrap();
            assert!(steps.first().unwrap()["probe"].is_null());
            assert!(steps.last().unwrap()["probe"].is_null());
            assert!(steps[1..steps.len() - 1]
                .iter()
                .all(|s| s["probe"].is_string()));
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

#[tokio::test]
async fn lm_retry_preserves_successes_and_original_probes_and_rejects_changed_context() {
    let h = RuntimeHarness::new().await;
    let p = provider(&h, 9).await;
    let id = h.seed_account("apikey", None, json!({})).await;
    let job = start(&h, id, 3, true, 1).await;
    let key = job["id"].as_str().unwrap();
    let failed = h.wait_for_terminal(key).await;
    assert_eq!(failed["can_retry"], true);
    assert_eq!(failed["completed_groups"], 2);
    assert_eq!(p.state.baselines.load(Ordering::SeqCst), 0);
    let retry = uuid::Uuid::new_v4().simple().to_string();
    let first = h.runtime.tests.retry_failed(key, &retry).await.unwrap();
    assert_eq!(
        h.runtime.tests.retry_failed(key, &retry).await.unwrap()["id"],
        first["id"]
    );
    let completed = h.wait_for_terminal(key).await;
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["bank_version"], failed["bank_version"]);
    for i in [0, 2] {
        assert_eq!(completed["groups"][i], failed["groups"][i]);
    }
    assert_eq!(p.state.numeric.load(Ordering::SeqCst), 6);
    assert_eq!(completed["tokenizer"]["status"], "completed");
    assert_eq!(p.state.baselines.load(Ordering::SeqCst), 2);
    assert_eq!(p.state.probes_peak.load(Ordering::SeqCst), 1);
    let _p = provider(&h, 8).await;
    let failed_job = start(&h, id, 1, false, 1).await;
    let key = failed_job["id"].as_str().unwrap();
    assert_eq!(h.wait_for_terminal(key).await["can_retry"], true);
    sqlx::query("UPDATE accounts SET credentials=credentials || '{\"header_override_enabled\":true,\"header_overrides\":{\"X-Fixture\":\"changed\"}}'::jsonb WHERE id=$1").bind(id).execute(&h.runtime.db.pool).await.unwrap();
    assert_eq!(
        h.runtime
            .tests
            .retry_failed(key, &uuid::Uuid::new_v4().simple().to_string())
            .await
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    h.runtime.tests.close().await;
    let restarted = sub2ops_server::model_tests::ModelTests::new(
        h.runtime.actions.clone(),
        h.runtime.tests.bank.clone(),
        h.runtime.tests.hooks.clone(),
        h.runtime.budget.clone(),
    );
    restarted.startup().unwrap();
    assert_eq!(
        restarted
            .retry_failed(key, &uuid::Uuid::new_v4().simple().to_string())
            .await
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    restarted.close().await;
    h.close().await;
}

#[tokio::test]
async fn api_key_platforms_use_their_protocol_auth_directory_and_scheduled_recheck() {
    let h = RuntimeHarness::new().await;
    let _p = provider(&h, 0).await;
    for (platform, configured, protocol, endpoint, auth) in [
        ("openai", "", "responses", "/v1/responses", "authorization"),
        ("anthropic", "", "messages", "/v1/messages", "x-api-key"),
        (
            "grok",
            "",
            "chat_completions",
            "/v1/chat/completions",
            "authorization",
        ),
        (
            "deepseek",
            "adaptive",
            "chat_completions",
            "/v1/chat/completions",
            "authorization",
        ),
        (
            "deepseek",
            "responses",
            "responses",
            "/v1/responses",
            "authorization",
        ),
        (
            "deepseek",
            "anthropic",
            "messages",
            "/v1/messages",
            "x-api-key",
        ),
    ] {
        let id = h.seed_account("apikey", None, json!({})).await;
        sqlx::query("UPDATE accounts SET platform=$1,credentials=credentials || $2 WHERE id=$3")
            .bind(platform).bind(SqlJson(json!({"api_protocol":configured,"header_override_enabled":true,"header_overrides":{"X-Fixture":"preserved"}}))).bind(id).execute(&h.runtime.db.pool).await.unwrap();
        let target = h.target_for(id).await;
        assert_eq!(target.protocol, protocol);
        if protocol == "chat_completions" {
            let payload = target.payload("fixture");
            assert_eq!(payload["max_tokens"], 4096);
            assert_eq!(payload["stream_options"]["include_usage"], true);
            assert!(payload.get("max_completion_tokens").is_none());
        }
        assert!(target.url.ends_with(endpoint));
        assert!(model_directory::endpoint(&target)
            .unwrap()
            .path()
            .ends_with("/v1/models"));
        assert_eq!(target.headers["x-fixture"], "preserved");
        assert!(target.headers.contains_key(auth));
        assert_eq!(
            h.state.directory_headers.lock().unwrap()[&id]["x-fixture"],
            "preserved"
        );
        assert!(h.state.directory_headers.lock().unwrap()[&id].contains_key(auth));
        if protocol == "messages" {
            assert_eq!(target.headers["anthropic-version"], "2023-06-01");
            assert!(!target.headers.contains_key("authorization"));
            sqlx::query("UPDATE accounts SET extra=extra || '{\"anthropic_apikey_auth_scheme\":\"authorization_bearer\"}'::jsonb WHERE id=$1").bind(id).execute(&h.runtime.db.pool).await.unwrap();
            let bearer = h.target_for(id).await;
            assert!(bearer.headers.contains_key("authorization"));
            assert!(!bearer.headers.contains_key("x-api-key"));
            assert!(h.state.directory_headers.lock().unwrap()[&id].contains_key("authorization"));
        }
        let view = h.runtime.detection.view(id).await.unwrap();
        assert_eq!(view["enabled"], false);
        if platform != "openai" {
            assert_eq!(view["model_id"], "");
            let request =
                json!({"expected_version":view["version"],"enabled":true,"interval_minutes":15});
            assert!(h
                .runtime
                .detection
                .save(id, &request)
                .await
                .unwrap_err()
                .detail
                .as_str()
                .unwrap()
                .contains("选择"));
            let disabled = h.runtime.detection.save(id,&json!({"expected_version":view["version"],"enabled":false,"interval_minutes":15})).await.unwrap();
            assert_eq!(disabled["model_id"], "");
            assert!(h
                .runtime
                .detection
                .trigger(id, &["slow:fixture".into()])
                .await
                .unwrap()
                .is_null());
            let row = h.runtime.actions.account(id, None).await.unwrap();
            assert_eq!(h.runtime.tests.start(id,&json!({"model_id":"gpt-6-luna","engine":"modeltrace","expected_version":account_version(&row),"request_id":uuid::Uuid::new_v4().simple().to_string()}),None).await.unwrap_err().status.as_u16(),422);
        }
        h.runtime.config.save("oauth", &json!({"model_detection_platforms":{platform:{"engine":"lm_detector","sample_count":1}}}), "fixture", None).await.unwrap();
        let current = h.runtime.detection.view(id).await.unwrap();
        let saved = h.runtime.detection.save(id, &json!({"expected_version":current["version"],"enabled":true,"schedule_mode":"fixed","min_interval_minutes":5,"interval_minutes":15,"model_override":"gpt-6-luna"})).await.unwrap();
        assert_eq!(saved["model_source"], "account");
        assert_eq!(saved["enabled"], true);
        let mark = h.runtime.detection.mark(id).unwrap();
        assert_eq!(
            h.runtime
                .set_degradation(
                    id,
                    &json!({"marked":true,"expected_mark_version":mark["version"]})
                )
                .await
                .unwrap()["degradation_mark"]["marked"],
            true
        );
        let job = h
            .runtime
            .detection
            .trigger(id, &["scheduled".into()])
            .await
            .unwrap();
        assert_eq!(job["engine"], "lm_detector");
        assert_eq!(job["planned_groups"], 1);
        assert_eq!(job["degradation_recheck"], true);
        let result = h.wait_for_terminal(job["id"].as_str().unwrap()).await;
        assert_eq!(result["status"], "completed");
        assert_eq!(
            result["groups"][0]["diagnostics"]["response_protocol"],
            protocol
        );
        assert_eq!(
            h.runtime.detection.complete_recheck(&result).await.unwrap()["mark_cleared"],
            true
        );
        assert_eq!(
            h.runtime.actions.account(id, None).await.unwrap()["schedulable"],
            true
        );
    }
    for platform in ["anthropic", "grok", "deepseek"] {
        let id = h.seed_account("apikey", None, json!({})).await;
        sqlx::query("UPDATE accounts SET platform=$1 WHERE id=$2")
            .bind(platform)
            .bind(id)
            .execute(&h.runtime.db.pool)
            .await
            .unwrap();
        h.state
            .directories
            .lock()
            .unwrap()
            .insert(id, json!({"data":[{"id":"claude-opus-5"}]}));
        h.runtime
            .detection
            .update(
                id,
                json!({"enabled":true,"schedule_mode":"fixed","model_override":"claude-opus-5"}),
            )
            .unwrap();
        let job = h
            .runtime
            .detection
            .trigger(id, &["scheduled".into()])
            .await
            .unwrap();
        let result = h.wait_for_terminal(job["id"].as_str().unwrap()).await;
        assert_eq!(result["classification"]["outcome"], "degraded");
        assert_eq!(result["automatic_disposition"]["schedule_verified"], true);
        assert_eq!(h.runtime.detection.mark(id).unwrap()["marked"], true);
        assert_eq!(
            h.runtime.actions.account(id, None).await.unwrap()["schedulable"],
            false
        );
    }
    let settings = h.runtime.config.current().unwrap();
    for platform in ["openai", "anthropic", "grok", "deepseek"] {
        assert_eq!(
            settings["model_detection_platforms"][platform]["engine"],
            "lm_detector"
        );
    }
    for platform in ["anthropic", "grok", "deepseek"] {
        let id = h.seed_account("oauth", None, json!({})).await;
        sqlx::query("UPDATE accounts SET platform=$1 WHERE id=$2")
            .bind(platform)
            .bind(id)
            .execute(&h.runtime.db.pool)
            .await
            .unwrap();
        assert!(model_target::account(&h.runtime.actions, id).await.is_err());
    }
    h.close().await;
}

#[tokio::test]
async fn continuous_appointment_is_accepted_through_the_real_http_router() {
    let h = RuntimeHarness::new().await;
    let id = h
        .seed_account("oauth", None, json!({"codex_plan_type":"plus"}))
        .await;
    let reset = chrono::Utc::now() + chrono::Duration::hours(2);
    h.state.quotas.lock().unwrap().insert(id,json!({"five_hour":{"utilization":0,"resets_at":reset.to_rfc3339()},"seven_day":{"utilization":0,"resets_at":(reset+chrono::Duration::days(7)).to_rfc3339()}}));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://{}/api/desktop/v1", listener.local_addr().unwrap());
    let routes = sub2ops_server::api::router(h.runtime.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let row = h.runtime.actions.account(id, None).await.unwrap();
    let request = json!({"action":"test","payload":{"confirmed":true,"mode":"default","model_id":""},"repeat":"quota_cycle","expected_version":sub2ops_server::compat::operation_versions(&row)["test"],"request_id":uuid::Uuid::new_v4().simple().to_string(),"client_id":"fixture-cycle-http"});
    let send = |body: Value| {
        client
            .post(format!("{base}/accounts/{id}/operations"))
            .header("x-api-key", "fixture-model-admin-key")
            .json(&body)
            .send()
    };
    for patch in [
        json!({"repeat":"unknown"}),
        json!({"scheduled_at":"2031-04-05T10:00:00Z"}),
        json!({"payload":{"confirmed":true,"mode":"text","model_id":""}}),
    ] {
        let mut invalid = request.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert_eq!(
            send(invalid).await.unwrap().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let response = send(request.clone()).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let job: Value = response.json().await.unwrap();
    assert_eq!(job["repeat_state"], "active");
    assert!(sub2ops_server::compat::parse_datetime(&job["scheduled_at"]).unwrap() > reset);
    let replay: Value = send(request.clone()).await.unwrap().json().await.unwrap();
    assert_eq!(replay["id"], job["id"]);
    let mut duplicate = request;
    duplicate["request_id"] = uuid::Uuid::new_v4().simple().to_string().into();
    assert_eq!(
        send(duplicate).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    let cancelled: Value = client
        .post(format!(
            "{base}/account-operations/{}/cancel",
            job["id"].as_str().unwrap()
        ))
        .header("x-api-key", "fixture-model-admin-key")
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancelled["repeat_state"], "stopped");
    task.abort();
    h.close().await;
}
