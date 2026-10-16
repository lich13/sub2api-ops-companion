use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header::CONTENT_TYPE, HeaderValue, Request, StatusCode},
    response::Response,
    routing::post,
    Router,
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use sub2ops_server::{
    compat, fingerprint_bank, model_target, model_test_stream, modeltrace, settings::Settings,
};
use tokio::{sync::Mutex, task::JoinHandle};

const FIXTURES: &str = include_str!("../../tests/contracts/modeltrace-fixtures.json");
const BUNDLED_BANK: &str = include_str!("../resources/modeltrace/unified_bank.json");

fn fixtures() -> Value {
    serde_json::from_str(FIXTURES).expect("contract fixture JSON")
}

fn close(actual: f64, expected: f64, tolerance: f64, context: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{context}: actual={actual:.17e}, expected={expected:.17e}, tolerance={tolerance:.3e}"
    );
}

#[test]
fn fixture_is_pinned_and_contains_only_synthetic_inputs() {
    let fixture = fixtures();
    assert_eq!(fixture["schema_version"], 1);
    assert_eq!(
        fixture["reference_sha"],
        "6be7c291ed38dcc337cb88f693639040a3463899"
    );
    assert_eq!(fixture["synthetic_only"], true);
}

#[test]
fn modeltrace_validation_scores_analysis_and_challenges_match_pinned_python() {
    let fixture = fixtures();
    let model = &fixture["modeltrace"];
    let snapshot = modeltrace::bundled().expect("bundled ModelTrace bank");
    fingerprint_bank::validate(&snapshot.0).expect("bundled bank schema");
    assert_eq!(snapshot.1, model["bank_version"]);

    for case in model["validation_cases"].as_array().unwrap() {
        let actual = modeltrace::validate(case["input"].as_str().unwrap());
        let expected = case["expected_numbers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|number| number.as_u64().unwrap() as usize)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "validation case {}", case["id"]);
    }

    for case in model["scoring_cases"].as_array().unwrap() {
        let numbers = case["numbers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|number| number.as_u64().unwrap() as usize)
            .collect::<Vec<_>>();
        let actual = modeltrace::scores(&numbers, &snapshot.0);
        let expected = case["expected_scores"].as_array().unwrap();
        assert_eq!(
            actual.len(),
            expected.len(),
            "score count for {}",
            case["id"]
        );
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            close(
                *actual,
                expected.as_f64().unwrap(),
                1.0e-12,
                &format!("{} model index {index}", case["id"]),
            );
        }
    }

    for case in model["analysis_cases"].as_array().unwrap() {
        let outputs = case["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|output| output.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let actual = modeltrace::analyze(&outputs, &snapshot).unwrap();
        let expected = &case["expected"];
        if expected.is_null() {
            assert!(actual.is_null(), "analysis case {}", case["id"]);
            continue;
        }
        assert_eq!(
            actual["prediction"], expected["prediction"],
            "{}",
            case["id"]
        );
        assert_eq!(
            actual["prediction_name"], expected["prediction_name"],
            "{}",
            case["id"]
        );
        assert_eq!(
            actual["used_outputs"], expected["used_outputs"],
            "{}",
            case["id"]
        );
        assert_eq!(
            actual["bank_version"], expected["bank_version"],
            "{}",
            case["id"]
        );
        close(
            actual["probability"].as_f64().unwrap(),
            expected["probability"].as_f64().unwrap(),
            1.0e-12,
            &format!("{} prediction probability", case["id"]),
        );
        let actual_rows = actual["results"].as_array().unwrap();
        let expected_rows = expected["results"].as_array().unwrap();
        assert_eq!(actual_rows.len(), expected_rows.len(), "{}", case["id"]);
        let mut probability_sum = 0.0;
        for (index, (actual, expected)) in actual_rows.iter().zip(expected_rows).enumerate() {
            assert_eq!(
                actual["model"], expected["model"],
                "{} row {index}",
                case["id"]
            );
            assert_eq!(
                actual["display_name"], expected["display_name"],
                "{} row {index}",
                case["id"]
            );
            close(
                actual["score"].as_f64().unwrap(),
                expected["score"].as_f64().unwrap(),
                1.0e-12,
                &format!("{} row {index} score", case["id"]),
            );
            let probability = actual["probability"].as_f64().unwrap();
            close(
                probability,
                expected["probability"].as_f64().unwrap(),
                1.0e-12,
                &format!("{} row {index} probability", case["id"]),
            );
            probability_sum += probability;
        }
        close(
            probability_sum,
            1.0,
            1.0e-12,
            &format!("{} normalized probabilities", case["id"]),
        );
    }

    let too_many = vec!["1".to_owned(); 4];
    let error = modeltrace::analyze(&too_many, &snapshot).unwrap_err();
    assert_eq!(error.status.as_u16(), 422);
    assert_eq!(error.detail, "最多分析三组样本");

    for case in model["isolated_answer_cases"].as_array().unwrap() {
        assert_eq!(
            model_test_stream::isolated_answer(case["input"].as_str().unwrap()),
            case["expected_answer"].as_str().unwrap(),
            "isolated answer case {}",
            case["id"]
        );
    }

    let bounds = &model["challenge_expectations"];
    let challenges = modeltrace::challenges();
    assert_eq!(challenges.len(), bounds["count"].as_u64().unwrap() as usize);
    let mut lengths = std::collections::BTreeSet::new();
    for (length, prompt) in challenges {
        assert!(length >= bounds["minimum_length"].as_u64().unwrap() as usize);
        assert!(length <= bounds["maximum_length"].as_u64().unwrap() as usize);
        assert!(lengths.insert(length), "challenge lengths must be unique");
        assert!(prompt.contains(&format!("{length} 个 1 到 355")));
        assert!(prompt.contains(&format!("输出第 {length} 个整数后立即停止")));
        assert!(prompt.contains("禁止调用或借助任何工具"));
    }
}

fn mutation_path<'a>(mut value: &'a mut Value, path: &[Value]) -> &'a mut Value {
    for component in path {
        value = match (value, component) {
            (Value::Object(object), Value::String(key)) => object
                .get_mut(key)
                .unwrap_or_else(|| panic!("missing object fixture key {key}")),
            (Value::Array(array), Value::Number(index)) => array
                .get_mut(index.as_u64().unwrap() as usize)
                .expect("fixture array index"),
            _ => panic!("fixture mutation path does not match JSON"),
        };
    }
    value
}

fn apply_bank_mutation(bank: &Value, mutation: &Value) -> Value {
    let mut candidate = bank.clone();
    match mutation["op"].as_str().unwrap() {
        "oversize_models" => {
            let first = candidate["models"][0].clone();
            let mut models = Vec::with_capacity(257);
            let mut order = Vec::with_capacity(257);
            for index in 0..257 {
                let mut model = first.clone();
                let id = format!("fixture-model-{index}");
                model["id"] = id.clone().into();
                order.push(Value::String(id));
                models.push(model);
            }
            candidate["models"] = Value::Array(models);
            candidate["robust"]["model_order"] = Value::Array(order);
        }
        "set" => {
            *mutation_path(&mut candidate, mutation["path"].as_array().unwrap()) =
                mutation["value"].clone();
        }
        "remove" => {
            let path = mutation["path"].as_array().unwrap();
            let (last, parent_path) = path.split_last().expect("remove path");
            let parent = mutation_path(&mut candidate, parent_path);
            match (parent, last) {
                (Value::Object(object), Value::String(key)) => {
                    object.remove(key).expect("fixture object field to remove");
                }
                (Value::Array(array), Value::Number(index)) => {
                    array.remove(index.as_u64().unwrap() as usize);
                }
                _ => panic!("fixture remove path does not match JSON"),
            }
        }
        "truncate" => {
            mutation_path(&mut candidate, mutation["path"].as_array().unwrap())
                .as_array_mut()
                .expect("truncate target is an array")
                .pop()
                .expect("truncate non-empty array");
        }
        "append" => mutation_path(&mut candidate, mutation["path"].as_array().unwrap())
            .as_array_mut()
            .expect("append target is an array")
            .push(mutation["value"].clone()),
        "zero_vectors" => {
            let count = mutation["count"].as_u64().unwrap() as usize;
            let dimension = mutation["dimension"].as_u64().unwrap() as usize;
            let vectors = (0..count)
                .map(|_| json!(vec![0.0_f64; dimension]))
                .collect::<Vec<_>>();
            *mutation_path(&mut candidate, mutation["path"].as_array().unwrap()) =
                Value::Array(vectors);
        }
        operation => panic!("unsupported fixture mutation {operation}"),
    }
    candidate
}

fn hex_bytes(input: &str) -> Vec<u8> {
    assert_eq!(input.len() % 2, 0, "fixture hex must have complete bytes");
    input
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => panic!("invalid fixture hex"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}

#[test]
fn fingerprint_bank_validation_and_git_fixtures_match_pinned_python() {
    let fixture = fixtures();
    let (bank, _) = modeltrace::bundled().unwrap();
    let mut mismatches = Vec::new();
    for case in fixture["fingerprint_bank"]["validation_cases"]
        .as_array()
        .unwrap()
    {
        let candidate = if case["mutation"].is_null() {
            bank.clone()
        } else {
            apply_bank_mutation(&bank, &case["mutation"])
        };
        let actual = fingerprint_bank::validate(&candidate);
        let expected_valid = case["expected_valid"].as_bool().unwrap();
        let id = case["id"].as_str().unwrap();
        match actual {
            Ok(()) if !expected_valid => {
                mismatches.push(format!("{id}: accepted invalid Python case"))
            }
            Err(error) if expected_valid => mismatches.push(format!(
                "{id}: rejected valid Python case ({})",
                error.detail
            )),
            Err(error) if error.status.as_u16() != 503 || error.detail != "指纹库数据无效" =>
            {
                mismatches.push(format!(
                    "{id}: unexpected error {} {}",
                    error.status, error.detail
                ));
            }
            _ => {}
        }
    }
    for case in fixture["fingerprint_bank"]["git_blob_cases"]
        .as_array()
        .unwrap()
    {
        let raw = hex_bytes(case["raw_hex"].as_str().unwrap());
        assert_eq!(
            fingerprint_bank::git_blob_digest(&raw),
            case["expected_digest"].as_str().unwrap(),
            "git blob fixture {}",
            case["id"]
        );
    }

    for case in fixture["fingerprint_bank"]["git_ref_cases"]
        .as_array()
        .unwrap()
    {
        let raw = hex_bytes(case["raw_hex"].as_str().unwrap());
        let result = fingerprint_bank::parse_git_refs(&raw, case["branch"].as_str().unwrap());
        if let Some(revision) = case["expected"].get("revision").and_then(Value::as_str) {
            assert_eq!(result.unwrap(), revision, "git refs fixture {}", case["id"]);
        } else {
            assert_eq!(
                result.unwrap_err().code,
                case["expected"]["error_code"].as_str().unwrap(),
                "git refs fixture {}",
                case["id"]
            );
        }
    }
    assert!(
        mismatches.is_empty(),
        "bank validation mismatches: {mismatches:#?}"
    );
}

fn assert_modeltrace_bank_error(bank: &Value, version: &Value, input: &str, case: &str) {
    let error =
        modeltrace::analyze(&[input.to_owned()], &(bank.clone(), version.clone())).expect_err(case);
    assert_eq!(error.status.as_u16(), 503, "{case}");
    assert_eq!(error.detail, "指纹库数据无效", "{case}");
}

#[test]
fn modeltrace_analysis_rejects_extreme_finite_results_and_malformed_bank_shapes() {
    let (bank, version) = modeltrace::bundled().expect("bundled ModelTrace bank");
    let input = (1..=100)
        .map(|number| number.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let mut extreme_beta = bank.clone();
    extreme_beta["calibration"]["1"]["beta"] = json!(f64::MAX);
    fingerprint_bank::validate(&extreme_beta)
        .expect("finite positive calibration values remain schema-valid");
    assert_modeltrace_bank_error(
        &extreme_beta,
        &version,
        &input,
        "extreme finite calibration beta",
    );

    let mut tiny_scale = bank.clone();
    tiny_scale["robust"]["hellinger"]["feature_scale"] = json!(vec![1.0e-320_f64; 355]);
    fingerprint_bank::validate(&tiny_scale)
        .expect("positive finite feature scales remain schema-valid");
    assert_modeltrace_bank_error(
        &tiny_scale,
        &version,
        &input,
        "subnormal positive Hellinger feature scales",
    );

    let mut malformed_models = bank.clone();
    malformed_models["models"] = json!({"fixture":"not-an-array"});
    let error = fingerprint_bank::validate(&malformed_models)
        .expect_err("malformed model collection must be rejected");
    assert_eq!(error.status.as_u16(), 503);
    assert_eq!(error.detail, "指纹库数据无效");
    assert_modeltrace_bank_error(
        &malformed_models,
        &version,
        &input,
        "malformed model collection",
    );

    let mut malformed_dimension = bank;
    malformed_dimension["robust"]["hellinger"]["feature_mean"] = json!([0.0]);
    let error = fingerprint_bank::validate(&malformed_dimension)
        .expect_err("malformed feature dimension must be rejected");
    assert_eq!(error.status.as_u16(), 503);
    assert_eq!(error.detail, "指纹库数据无效");
    assert_modeltrace_bank_error(
        &malformed_dimension,
        &version,
        &input,
        "malformed Hellinger feature dimension",
    );
}

fn settings_at(directory: &Path, read_only: bool) -> Arc<Settings> {
    Arc::new(Settings {
        database_url: "postgres://example.invalid/sub2ops".into(),
        bind: "127.0.0.1:0".into(),
        base_path: "/".into(),
        audit_path: directory.join("audit.jsonl"),
        usage_path: directory.join("modeltrace-bank-state.json"),
        oauth_path: directory.join("oauth.json"),
        bark_path: directory.join("bark.json"),
        fallback_path: directory.join("fallback.json"),
        connection_path: directory.join("connection.json"),
        model_path: directory.join("model.json"),
        oauth: json!({}),
        bark: json!({}),
        bark_valid: false,
        base_url: String::new(),
        verify_url: String::new(),
        read_only,
    })
}

fn valid_cache_state() -> Value {
    let (_, mut version) = modeltrace::bundled().unwrap();
    let manifest = modeltrace::manifest();
    version["core_blob"] = manifest["files"]["core"]["gitBlob"].clone();
    version["challenge_blob"] = manifest["files"]["challenge"]["gitBlob"].clone();
    json!({
        "version": 1,
        "active": {"raw_bank": BUNDLED_BANK, "version": version},
        "source": "bundled",
        "checked_at": 0,
        "synced_at": 0,
        "cooldown_until": null,
        "last_error": null,
        "result": null
    })
}

fn assert_bank_error(
    result: sub2ops_server::error::Result<fingerprint_bank::FingerprintBank>,
    expected: &Value,
) {
    let error = result
        .err()
        .expect("invalid cache must reject initialization");
    assert_eq!(
        error.status.as_u16(),
        expected["status"].as_u64().unwrap() as u16
    );
    assert_eq!(error.detail, expected["detail"]);
}

#[test]
fn fingerprint_bank_cache_is_read_only_and_rejects_lost_or_corrupt_state() {
    let expectations = fixtures()["fingerprint_bank"]["cache_expectations"].clone();

    let absent = tempfile::tempdir().unwrap();
    let bank = fingerprint_bank::FingerprintBank::new(settings_at(absent.path(), true)).unwrap();
    assert_eq!(bank.public()["source"], "bundled");
    assert_eq!(
        fs::read_dir(absent.path()).unwrap().count(),
        expectations["read_only_absent_state_files"]
            .as_array()
            .unwrap()
            .len()
    );

    let cached = tempfile::tempdir().unwrap();
    let state_path = cached.path().join("modeltrace-bank-state.json");
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&valid_cache_state()).unwrap(),
    )
    .unwrap();
    let before = fs::read(&state_path).unwrap();
    let bank = fingerprint_bank::FingerprintBank::new(settings_at(cached.path(), true)).unwrap();
    assert_eq!(
        bank.public()["source"],
        expectations["valid_read_only_source"]
    );
    assert_eq!(fs::read(&state_path).unwrap(), before);
    assert!(!cached.path().join("modeltrace-bank-state.lock").exists());

    let lost = tempfile::tempdir().unwrap();
    fs::write(lost.path().join("modeltrace-bank-state.lock"), b"").unwrap();
    assert_bank_error(
        fingerprint_bank::FingerprintBank::new(settings_at(lost.path(), true)),
        &expectations["lost_cache_error"],
    );
    assert!(!lost.path().join("modeltrace-bank-state.json").exists());

    let corrupt_json = tempfile::tempdir().unwrap();
    let corrupt_path = corrupt_json.path().join("modeltrace-bank-state.json");
    fs::write(&corrupt_path, b"{broken").unwrap();
    let before = fs::read(&corrupt_path).unwrap();
    assert_bank_error(
        fingerprint_bank::FingerprintBank::new(settings_at(corrupt_json.path(), true)),
        &expectations["corrupt_json_error"],
    );
    assert_eq!(fs::read(&corrupt_path).unwrap(), before);

    let corrupt_cache = tempfile::tempdir().unwrap();
    let corrupt_path = corrupt_cache.path().join("modeltrace-bank-state.json");
    let mut corrupt_state = valid_cache_state();
    corrupt_state["active"]["raw_bank"] = "not a bank".into();
    fs::write(&corrupt_path, serde_json::to_vec(&corrupt_state).unwrap()).unwrap();
    let before = fs::read(&corrupt_path).unwrap();
    assert_bank_error(
        fingerprint_bank::FingerprintBank::new(settings_at(corrupt_cache.path(), true)),
        &expectations["corrupt_cache_error"],
    );
    assert_eq!(fs::read(&corrupt_path).unwrap(), before);

    let incompatible = tempfile::tempdir().unwrap();
    let incompatible_path = incompatible.path().join("modeltrace-bank-state.json");
    let mut incompatible_state = valid_cache_state();
    let mut old_bank: Value = serde_json::from_str(BUNDLED_BANK).unwrap();
    old_bank["built_at"] = "2000-01-01T00:00:00+00:00".into();
    let old_raw = serde_json::to_string(&old_bank).unwrap();
    incompatible_state["active"]["raw_bank"] = old_raw.clone().into();
    incompatible_state["active"]["version"]["built_at"] = old_bank["built_at"].clone();
    incompatible_state["active"]["version"]["sha256"] = compat::sha256(&old_raw).into();
    fs::write(
        &incompatible_path,
        serde_json::to_vec(&incompatible_state).unwrap(),
    )
    .unwrap();
    let before = fs::read(&incompatible_path).unwrap();
    assert_bank_error(
        fingerprint_bank::FingerprintBank::new(settings_at(incompatible.path(), true)),
        &expectations["incompatible_cache_error"],
    );
    assert_eq!(fs::read(&incompatible_path).unwrap(), before);
}

#[test]
fn codex_identity_and_target_signature_match_pinned_python_vectors() {
    let fixture = fixtures();
    for case in fixture["target_identity_cases"].as_array().unwrap() {
        let (version, user_agent) = model_target::identity(&case["input"]);
        assert_eq!(
            version,
            case["expected"][0].as_str().unwrap(),
            "{}",
            case["id"]
        );
        assert_eq!(
            user_agent,
            case["expected"][1].as_str().unwrap(),
            "{}",
            case["id"]
        );
    }
    for case in fixture["target_signature_cases"].as_array().unwrap() {
        assert_eq!(
            model_target::signature(&case["row"]),
            case["expected_signature"].as_str().unwrap(),
            "target signature fixture {}",
            case["id"]
        );
    }
}

#[test]
fn stream_failure_and_collector_projection_match_pinned_python() {
    let fixture = fixtures();
    for case in fixture["failure_cases"].as_array().unwrap() {
        let failure =
            model_test_stream::failure(case["status"].as_u64().unwrap() as u16, &case["payload"]);
        assert_eq!(
            failure.code,
            case["expected"]["code"].as_str().unwrap(),
            "{}",
            case["id"]
        );
        assert_eq!(
            failure.retryable,
            case["expected"]["retryable"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
    }

    for case in fixture["collector_cases"].as_array().unwrap() {
        let mut collector =
            model_test_stream::Collector::new(case["expected_numbers"].as_u64().unwrap() as usize);
        let mut failure = None;
        for item in case["events"].as_array().unwrap() {
            if let Err(error) = collector.accept(
                &item["event"],
                item.get("name").and_then(Value::as_str).unwrap_or(""),
                item.get("whole").and_then(Value::as_bool).unwrap_or(false),
            ) {
                failure = Some(error);
                break;
            }
        }
        let projection = json!({
            "error": failure.as_ref().map(|error| json!({"code": error.code, "retryable": error.retryable})),
            "text": collector.text(),
            "completed": collector.completed,
            "returned_model": collector.returned_model,
            "recognized": collector.recognized,
            "protocol": collector.protocol,
            "end_reason": collector.end_reason,
            "metadata": collector.metadata
        });
        assert_eq!(
            projection, case["expected"],
            "collector case {}",
            case["id"]
        );
    }

    let mut collector = model_test_stream::Collector::new(0);
    let at_limit = "🙂".repeat(model_test_stream::MAX_TEXT / "🙂".len());
    collector
        .update(
            (0, 0),
            &Value::String(at_limit.clone()),
            false,
            false,
            false,
        )
        .unwrap();
    assert_eq!(collector.text().len(), model_test_stream::MAX_TEXT);
    let error = collector
        .update((0, 0), &Value::String("x".into()), false, false, false)
        .unwrap_err();
    assert_eq!(error.code, "output_limit");
    assert!(error.retryable);
}

#[derive(Clone)]
struct ReplyPlan {
    status: u16,
    content_type: String,
    body: Vec<u8>,
    disconnect: bool,
    hang: bool,
}

struct MockState {
    reply: ReplyPlan,
    requests: AtomicUsize,
    payloads: Mutex<Vec<Value>>,
}

async fn mock_upstream(State(state): State<Arc<MockState>>, request: Request<Body>) -> Response {
    state.requests.fetch_add(1, Ordering::SeqCst);
    let payload = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    state.payloads.lock().await.push(payload);

    let reply = &state.reply;
    let body = if reply.hang {
        let first = stream::once({
            let bytes = reply.body.clone();
            async move { Ok::<_, std::io::Error>(bytes) }
        });
        Body::from_stream(first.chain(stream::pending::<
            std::result::Result<Vec<u8>, std::io::Error>,
        >()))
    } else if reply.disconnect {
        let first = stream::once({
            let bytes = reply.body.clone();
            async move { Ok::<_, std::io::Error>(bytes) }
        });
        let disconnect = stream::once(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Err::<Vec<u8>, _>(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "synthetic loopback disconnect",
            ))
        });
        Body::from_stream(first.chain(disconnect))
    } else {
        Body::from(reply.body.clone())
    };
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::from_u16(reply.status).unwrap();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&reply.content_type).unwrap(),
    );
    response
}

struct MockServer {
    url: String,
    state: Arc<MockState>,
    task: Option<JoinHandle<()>>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl MockServer {
    async fn start(reply: ReplyPlan) -> Self {
        let state = Arc::new(MockState {
            reply,
            requests: AtomicUsize::new(0),
            payloads: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/", post(mock_upstream))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            url: format!("http://{address}/"),
            state,
            task: Some(task),
        }
    }

    async fn stop(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

#[tokio::test]
async fn execute_matches_pinned_http_and_stream_fixtures_without_replaying() {
    let fixture = fixtures();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for case in fixture["execute_cases"].as_array().unwrap() {
        let expected = &case["expected"];
        let reply = ReplyPlan {
            status: case["status"].as_u64().unwrap() as u16,
            content_type: case["content_type"].as_str().unwrap().to_owned(),
            body: case["body"].as_str().unwrap().as_bytes().to_vec(),
            disconnect: case
                .get("disconnect")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            hang: false,
        };
        let mock = MockServer::start(reply).await;
        let target = model_test_stream::Target {
            url: mock.url.clone(),
            headers: reqwest::header::HeaderMap::new(),
            proxy: None,
            model: "fixture-model".into(),
            oauth: false,
            protocol: case["protocol"].as_str().unwrap().into(),
        };
        let mut diagnostics = json!({});
        let outcome = model_test_stream::execute(
            &target,
            "synthetic prompt",
            3,
            &client,
            None,
            &mut diagnostics,
        )
        .await;

        assert_eq!(
            mock.state.requests.load(Ordering::SeqCst),
            expected["request_count"].as_u64().unwrap() as usize,
            "request count for {}",
            case["id"]
        );
        let payloads = mock.state.payloads.lock().await;
        assert_eq!(payloads.len(), 1, "captured requests for {}", case["id"]);
        assert_eq!(payloads[0], expected["request_payload"], "{}", case["id"]);
        assert_eq!(
            payloads[0]["model"], expected["request_models"][0],
            "requested model for {}",
            case["id"]
        );
        drop(payloads);

        if let Some(text) = expected["outcome"].get("text").and_then(Value::as_str) {
            assert_eq!(
                outcome.unwrap(),
                (
                    text.to_owned(),
                    expected["outcome"]["model"].as_str().map(str::to_owned)
                ),
                "execute result for {}",
                case["id"]
            );
        } else {
            let error = outcome.expect_err("fixture expects a transport failure");
            assert_eq!(
                error.code,
                expected["outcome"]["error_code"].as_str().unwrap(),
                "execute error for {}",
                case["id"]
            );
            if let Some(retryable) = expected["outcome"]
                .get("retryable")
                .and_then(Value::as_bool)
            {
                assert_eq!(
                    error.retryable, retryable,
                    "execute retryability for {}",
                    case["id"]
                );
            }
        }
        for (key, value) in expected["diagnostics"].as_object().unwrap() {
            assert_eq!(
                diagnostics[key], *value,
                "diagnostics {key} for {}",
                case["id"]
            );
        }
        mock.stop().await;
    }
}

#[tokio::test]
async fn cancelling_a_live_stream_does_not_resubmit_the_loopback_request() {
    let event = json!({
        "model": "fixture-model",
        "choices": [{"index": 0, "delta": {"content": "partial answer"}, "finish_reason": null}]
    });
    let body = format!("data: {}\n\n", serde_json::to_string(&event).unwrap()).into_bytes();
    let mock = MockServer::start(ReplyPlan {
        status: 200,
        content_type: "text/event-stream".into(),
        body,
        disconnect: false,
        hang: true,
    })
    .await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let target = model_test_stream::Target {
        url: mock.url.clone(),
        headers: reqwest::header::HeaderMap::new(),
        proxy: None,
        model: "fixture-model".into(),
        oauth: false,
        protocol: "chat_completions".into(),
    };
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let callback: model_test_stream::Progress = Arc::new(move |value| {
        let sender = sender.clone();
        Box::pin(async move {
            sender
                .send(value)
                .map_err(|_| sub2ops_server::error::Error::internal())
        })
    });
    let mut diagnostics = json!({});
    let task = tokio::spawn(async move {
        let _ = model_test_stream::execute(
            &target,
            "synthetic prompt",
            3,
            &client,
            Some(callback),
            &mut diagnostics,
        )
        .await;
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = receiver.recv().await.expect("progress before cancellation");
            if event["diagnostics"].get("ttft_ms").is_some() {
                break;
            }
        }
    })
    .await
    .expect("loopback stream should reach first text");
    assert_eq!(mock.state.requests.load(Ordering::SeqCst), 1);
    let payloads = mock.state.payloads.lock().await;
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["model"], "fixture-model");
    assert_eq!(payloads[0]["messages"][0]["content"], "synthetic prompt");
    drop(payloads);

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(mock.state.requests.load(Ordering::SeqCst), 1);
    mock.stop().await;
}
