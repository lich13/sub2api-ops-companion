use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use fs2::FileExt;
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use sqlx::types::Json as SqlJson;
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io,
    sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use sub2ops_server::{
    compat::account_version, fingerprint_bank::FingerprintBank, model_target, model_test_stream,
    modeltrace, runtime::Runtime, settings::Settings,
};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinHandle};

const FIXTURES: &str = include_str!("../../tests/contracts/modeltrace-runtime/fixtures.json");
const MODELTRACE_FIXTURES: &str = include_str!("../../tests/contracts/modeltrace-fixtures.json");
const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";
static NEXT_ACCOUNT_ID: AtomicI64 = AtomicI64::new(9_500_000);

fn runtime_fixtures() -> Value {
    serde_json::from_str(FIXTURES).expect("pinned runtime fixture JSON")
}

fn bank_settings(directory: &std::path::Path, read_only: bool) -> Arc<Settings> {
    Arc::new(Settings {
        database_url: "postgres://example.invalid/sub2ops".into(),
        bind: "127.0.0.1:0".into(),
        base_path: "/".into(),
        audit_path: directory.join("audit.jsonl"),
        usage_path: directory.join("oauth-state.json"),
        oauth_path: directory.join("oauth-config.json"),
        bark_path: directory.join("bark-config.json"),
        fallback_path: directory.join("key-fallback.json"),
        connection_path: directory.join("sub2api-config.json"),
        model_path: directory.join("group-models.json"),
        oauth: json!({}),
        bark: json!({}),
        bark_valid: false,
        base_url: String::new(),
        verify_url: String::new(),
        read_only,
    })
}

fn valid_bank_cache_state() -> Value {
    let (bank, mut version) = modeltrace::bundled().expect("bundled ModelTrace snapshot");
    let manifest = modeltrace::manifest();
    version["core_blob"] = manifest["files"]["core"]["gitBlob"].clone();
    version["challenge_blob"] = manifest["files"]["challenge"]["gitBlob"].clone();
    let raw_bank = serde_json::to_string(&bank).unwrap();
    version["sha256"] = sub2ops_server::compat::sha256(&raw_bank).into();
    json!({
        "version":1,
        "active":{"raw_bank":raw_bank,"version":version},
        "source":"bundled",
        "checked_at":0,
        "synced_at":0,
        "cooldown_until":null,
        "last_error":null,
        "result":null
    })
}

fn modeltrace_fixtures() -> Value {
    serde_json::from_str(MODELTRACE_FIXTURES).expect("pinned ModelTrace fixture JSON")
}

fn assert_probability_close(actual: f64, expected: f64, context: &str) {
    assert!(
        (actual - expected).abs() <= 1.0e-12,
        "{context}: actual={actual:.17e}, expected={expected:.17e}"
    );
}

fn assert_report(actual: &Value, expected: &Value, context: &str) {
    assert_eq!(actual["prediction"], expected["prediction"], "{context}");
    assert_eq!(
        actual["used_outputs"], expected["used_outputs"],
        "{context}"
    );
    assert_eq!(
        actual["bank_version"], expected["bank_version"],
        "{context}"
    );
    assert_probability_close(
        actual["probability"].as_f64().expect("actual probability"),
        expected["probability"]
            .as_f64()
            .expect("oracle probability"),
        context,
    );
}

fn response_output(text: &str, model: &str) -> Value {
    json!({
        "status":"completed",
        "model":model,
        "output":[{"type":"message","phase":"final","content":[{"type":"output_text","text":text}]}]
    })
}

fn responses_json(text: &str) -> ResponsePlan {
    let mut response = response_output(text, "fixture-returned-model");
    response["object"] = "response".into();
    response["output_text"] = text.into();
    response["usage"] = json!({
        "input_tokens":23,
        "output_tokens":19,
        "output_tokens_details":{"reasoning_tokens":2}
    });
    ResponsePlan::bytes(
        StatusCode::OK,
        "application/json",
        response.to_string().into_bytes(),
    )
}

fn responses_sse(text: &str) -> ResponsePlan {
    let mut response = response_output(text, "fixture-returned-model");
    response["usage"] = json!({
        "input_tokens":23,
        "output_tokens":19,
        "output_tokens_details":{"reasoning_tokens":2}
    });
    let event = json!({"type":"response.completed","response":response});
    ResponsePlan::bytes(
        StatusCode::OK,
        "text/event-stream",
        format!("data: {event}\n\n").into_bytes(),
    )
}

fn chat_json(text: &str) -> ResponsePlan {
    ResponsePlan::bytes(
        StatusCode::OK,
        "application/json",
        json!({
            "object":"chat.completion",
            "model":"fixture-returned-model",
            "choices":[{"index":0,"message":{"content":text,"reasoning_content":"private fixture reasoning"},"finish_reason":"stop"}]
        })
        .to_string()
        .into_bytes(),
    )
}

fn chat_sse_done(text: &str) -> ResponsePlan {
    let delta = json!({
        "model":"fixture-returned-model",
        "choices":[{"index":0,"delta":{"content":text,"reasoning_content":"private fixture reasoning"},"finish_reason":null}]
    });
    ResponsePlan::bytes(
        StatusCode::OK,
        "text/event-stream",
        format!("data: {delta}\r\n\r\ndata: [DONE]\r\n\r\n").into_bytes(),
    )
}

fn chat_length_sse(text: &str) -> ResponsePlan {
    let event = json!({
        "model":"fixture-returned-model",
        "choices":[{
            "index":0,
            "delta":{"content":text,"reasoning_content":"private fixture reasoning"},
            "finish_reason":"length"
        }],
        "usage":{
            "completion_tokens":4096,
            "completion_tokens_details":{"reasoning_tokens":3890}
        }
    });
    ResponsePlan::bytes(
        StatusCode::OK,
        "text/event-stream",
        format!("data: {event}\n\n").into_bytes(),
    )
}

fn incomplete_value(reason: &str, text: &str) -> Value {
    let mut response = response_output(text, "fixture-returned-model");
    response["object"] = "response".into();
    response["status"] = "incomplete".into();
    response["incomplete_details"] = json!({"reason":reason});
    response["usage"] = json!({
        "input_tokens":120,
        "output_tokens":4096,
        "output_tokens_details":{"reasoning_tokens":3890}
    });
    response
}

fn incomplete_json(reason: &str, text: &str) -> ResponsePlan {
    ResponsePlan::bytes(
        StatusCode::OK,
        "application/json",
        incomplete_value(reason, text).to_string().into_bytes(),
    )
}

fn incomplete_sse(reason: &str, text: &str) -> ResponsePlan {
    let response = incomplete_value(reason, text);
    let event = json!({"type":"response.incomplete","response":response});
    ResponsePlan::bytes(
        StatusCode::OK,
        "text/event-stream",
        format!("event: response.incomplete\r\ndata: {event}\r\n\r\n").into_bytes(),
    )
}

fn status_error(status: StatusCode, payload: Value) -> ResponsePlan {
    ResponsePlan::bytes(status, "application/json", payload.to_string().into_bytes())
}

#[derive(Clone)]
enum PlannedBody {
    Bytes(Vec<u8>),
    Hang(Vec<u8>),
    Broken(Vec<u8>),
    Gated {
        first: Vec<u8>,
        second: Vec<u8>,
        release: Arc<Semaphore>,
    },
}

#[derive(Clone)]
struct ResponsePlan {
    status: StatusCode,
    content_type: String,
    body: PlannedBody,
}

impl ResponsePlan {
    fn bytes(status: StatusCode, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: content_type.into(),
            body: PlannedBody::Bytes(body),
        }
    }

    fn hanging_sse(body: Vec<u8>) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "text/event-stream".into(),
            body: PlannedBody::Hang(body),
        }
    }

    fn broken_sse(body: Vec<u8>) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "text/event-stream".into(),
            body: PlannedBody::Broken(body),
        }
    }

    fn gated_sse(first: Vec<u8>, second: Vec<u8>, release: Arc<Semaphore>) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "text/event-stream".into(),
            body: PlannedBody::Gated {
                first,
                second,
                release,
            },
        }
    }

    fn into_response(self) -> Response {
        let (body, content_length) = match self.body {
            PlannedBody::Bytes(bytes) => {
                let length = bytes.len();
                (Body::from(bytes), Some(length))
            }
            PlannedBody::Hang(bytes) => {
                let first = stream::once(async move { Ok::<Bytes, io::Error>(Bytes::from(bytes)) });
                let body = first.chain(stream::pending::<Result<Bytes, io::Error>>());
                (Body::from_stream(body), None)
            }
            PlannedBody::Broken(bytes) => {
                let first = stream::once(async move { Ok::<Bytes, io::Error>(Bytes::from(bytes)) });
                let second = stream::once(async {
                    Err::<Bytes, io::Error>(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "synthetic response body reset",
                    ))
                });
                (Body::from_stream(first.chain(second)), None)
            }
            PlannedBody::Gated {
                first,
                second,
                release,
            } => {
                let first = stream::once(async move { Ok::<Bytes, io::Error>(Bytes::from(first)) });
                let second = stream::once(async move {
                    let permit = release.acquire_owned().await.map_err(|_| {
                        io::Error::new(io::ErrorKind::BrokenPipe, "fixture stream gate closed")
                    })?;
                    drop(permit);
                    Ok::<Bytes, io::Error>(Bytes::from(second))
                });
                (Body::from_stream(first.chain(second)), None)
            }
        };
        let mut response = Response::builder()
            .status(self.status)
            .header(header::CONTENT_TYPE, self.content_type);
        if let Some(length) = content_length {
            response = response.header(header::CONTENT_LENGTH, length.to_string());
        }
        response.body(body).expect("synthetic loopback response")
    }
}

struct RequestGate {
    first_requests: usize,
    arrivals: AtomicUsize,
    permits: [Arc<Semaphore>; 3],
}

impl RequestGate {
    fn new(first_requests: usize) -> Arc<Self> {
        Arc::new(Self {
            first_requests,
            arrivals: AtomicUsize::new(0),
            permits: std::array::from_fn(|_| Arc::new(Semaphore::new(0))),
        })
    }

    fn release_group(&self, index: usize, count: usize) {
        self.permits[index].add_permits(count);
    }

    fn release_all(&self, count: usize) {
        for index in 0..3 {
            self.release_group(index, count);
        }
    }
}

#[derive(Clone)]
struct CapturedRequest {
    path: String,
    headers: HeaderMap,
    payload: Value,
    body_len: usize,
    sample_index: usize,
    challenge_length: usize,
}

struct Scenario {
    replies: Vec<Vec<ResponsePlan>>,
    attempts: [usize; 3],
    gate: Option<Arc<RequestGate>>,
}

impl Scenario {
    fn sampled(
        outputs: &[String],
        response: fn(&str) -> ResponsePlan,
        gate: Option<Arc<RequestGate>>,
    ) -> Self {
        assert!(!outputs.is_empty(), "scenario requires a synthetic output");
        Self {
            replies: (0..3)
                .map(|index| vec![response(&outputs[index % outputs.len()])])
                .collect(),
            attempts: [0; 3],
            gate,
        }
    }

    fn single(plan: ResponsePlan) -> Self {
        Self {
            replies: vec![vec![plan.clone()], vec![plan.clone()], vec![plan]],
            attempts: [0; 3],
            gate: None,
        }
    }

    fn with_replies(replies: Vec<Vec<ResponsePlan>>, gate: Option<Arc<RequestGate>>) -> Self {
        assert_eq!(replies.len(), 3, "one fixture reply list per sample group");
        assert!(replies.iter().all(|items| !items.is_empty()));
        Self {
            replies,
            attempts: [0; 3],
            gate,
        }
    }
}

struct MockState {
    requests: Mutex<Vec<CapturedRequest>>,
    scenario: Mutex<Scenario>,
    prompt_indices: Mutex<HashMap<String, usize>>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl MockState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            scenario: Mutex::new(Scenario::single(responses_json("1, 2, 3"))),
            prompt_indices: Mutex::new(HashMap::new()),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }

    fn set_scenario(&self, scenario: Scenario) {
        *self.scenario.lock().unwrap() = scenario;
        self.requests.lock().unwrap().clear();
        self.prompt_indices.lock().unwrap().clear();
        self.active.store(0, Ordering::SeqCst);
        self.peak.store(0, Ordering::SeqCst);
    }

    fn captured(&self) -> Vec<CapturedRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    fn sample_index(&self, payload: &Value) -> Option<usize> {
        let prompt = prompt_from_payload(payload)?.to_owned();
        assert_challenge_prompt(&prompt);
        let mut indices = self.prompt_indices.lock().unwrap();
        let next = indices.len() % 3;
        let index = *indices.entry(prompt).or_insert(next);
        assert!(index < 3);
        Some(index)
    }
}

fn prompt_from_payload(payload: &Value) -> Option<&str> {
    let prompt = payload["input"][0]["content"][0]["text"]
        .as_str()
        .or_else(|| payload["messages"][0]["content"].as_str())?;
    Some(prompt)
}

fn challenge_length(prompt: &str) -> Option<usize> {
    let (_, rest) = prompt.split_once("输出第 ")?;
    let (length, _) = rest.split_once(" 个整数后立即停止")?;
    let length = length.parse::<usize>().ok()?;
    prompt
        .contains(&format!("输出第 {length} 个整数后立即停止"))
        .then_some(length)
}

fn assert_challenge_prompt(prompt: &str) {
    let length =
        challenge_length(prompt).expect("challenge contains one consistent requested count");
    assert!(
        (292..=332).contains(&length),
        "challenge length is in the oracle range"
    );
    assert!(prompt.contains(&format!(" {length} 个 1 到 355")));
    assert!(prompt.contains("禁止调用或借助任何工具"));
    for blocked_tool in [
        "Python",
        "代码执行器",
        "计算器",
        "搜索",
        "API",
        "外部随机数生成器",
    ] {
        assert!(
            prompt.contains(blocked_tool),
            "challenge prohibits {blocked_tool}"
        );
    }
}

fn assert_three_distinct_challenges(requests: &[CapturedRequest]) {
    assert_eq!(
        requests.len(),
        3,
        "Runtime dispatches three distinct challenges"
    );
    let prompts = requests
        .iter()
        .map(|request| prompt_from_payload(&request.payload).unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(prompts.len(), 3, "each dispatched prompt is unique");
    assert!(requests.iter().all(|request| request.body_len > 0));
    assert!(requests.iter().all(|request| request.payload.is_object()));
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            == Some("application/json")
    }));
    let lengths = requests
        .iter()
        .map(|request| request.challenge_length)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(lengths.len(), 3, "each challenge length is distinct");
    assert!(lengths.iter().all(|length| (292..=332).contains(length)));
}

async fn mock_model_upstream(State(state): State<Arc<MockState>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path().to_owned();
    let method = parts.method;
    let headers = parts.headers;
    let bytes = to_bytes(body, 2_000_000).await.unwrap_or_default();
    let body_len = bytes.len();
    let payload = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    let prompt =
        prompt_from_payload(&payload).expect("provider request includes a challenge prompt");
    let challenge_length =
        challenge_length(prompt).expect("provider challenge includes a requested count");
    let index = state
        .sample_index(&payload)
        .expect("provider receives one of the three distinct challenge prompts");
    state.requests.lock().unwrap().push(CapturedRequest {
        path,
        headers,
        payload,
        body_len,
        sample_index: index,
        challenge_length,
    });

    let (plan, gate, arrival) = {
        let mut scenario = state.scenario.lock().unwrap();
        let attempt = scenario.attempts[index];
        scenario.attempts[index] += 1;
        let replies = &scenario.replies[index];
        let plan = replies
            .get(attempt)
            .or_else(|| replies.last())
            .expect("scenario reply")
            .clone();
        let gate = scenario.gate.clone();
        let arrival = gate
            .as_ref()
            .map(|gate| gate.arrivals.fetch_add(1, Ordering::SeqCst));
        (plan, gate, arrival)
    };

    let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.peak.fetch_max(active, Ordering::SeqCst);
    if method != Method::POST {
        state.active.fetch_sub(1, Ordering::SeqCst);
        return (StatusCode::METHOD_NOT_ALLOWED, "fixture accepts POST only").into_response();
    }
    if let (Some(gate), Some(arrival)) = (gate, arrival) {
        if arrival < gate.first_requests {
            match gate.permits[index].clone().acquire_owned().await {
                Ok(permit) => drop(permit),
                Err(_) => {
                    state.active.fetch_sub(1, Ordering::SeqCst);
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }
        }
    }
    state.active.fetch_sub(1, Ordering::SeqCst);
    plan.into_response()
}

struct RuntimeHarness {
    _temp: tempfile::TempDir,
    runtime: Arc<Runtime>,
    state: Arc<MockState>,
    upstream_url: String,
    server: Option<JoinHandle<()>>,
}

impl RuntimeHarness {
    async fn new() -> Self {
        let database_url = std::env::var("DATABASE_URL")
            .expect("runtime parity runner must supply disposable PostgreSQL");
        let temp = tempfile::tempdir().expect("synthetic ModelTrace state directory");
        let state = MockState::new();
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback-only model provider fixture");
        let upstream_url = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .fallback(mock_model_upstream)
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve loopback ModelTrace fixture");
        });

        let connection_path = temp.path().join("sub2api-config.json");
        fs::write(
            &connection_path,
            serde_json::to_vec(&json!({"base_url":upstream_url,"verify_base_url":""})).unwrap(),
        )
        .expect("write local fixture configuration");
        let settings = Settings {
            database_url,
            bind: "127.0.0.1:0".into(),
            base_path: "/".into(),
            audit_path: temp.path().join("audit.jsonl"),
            usage_path: temp.path().join("oauth-state.json"),
            oauth_path: temp.path().join("oauth-config.json"),
            bark_path: temp.path().join("bark-config.json"),
            fallback_path: temp.path().join("key-fallback.json"),
            connection_path,
            model_path: temp.path().join("group-models.json"),
            oauth: json!({
                "oauth_auto_reset_credit_enabled":false,
                "oauth_daily_test_enabled":false,
                "oauth_daily_test_time":"14:08",
                "oauth_early_probe_batch_size":1,
                "oauth_recovery_connection_account_ids":[],
                "oauth_recovery_model_account_ids":[],
                "oauth_recovery_monitor_enabled":false,
                "oauth_recovery_test_concurrency":1,
                "oauth_recovery_test_model_id":"gpt-6-luna",
                "oauth_usage_refresh_concurrency":1
            }),
            bark: json!({
                "enabled":false,
                "device_key":"fixture-bark-device-key",
                "server_url":upstream_url
            }),
            bark_valid: true,
            base_url: upstream_url.clone(),
            verify_url: String::new(),
            read_only: false,
        };
        let runtime = Runtime::new(settings)
            .await
            .expect("construct real Runtime with temporary PostgreSQL and loopback provider");
        Self {
            _temp: temp,
            runtime,
            state,
            upstream_url,
            server: Some(server),
        }
    }

    async fn seed_account(&self, kind: &str, parent_account_id: Option<i64>, extra: Value) -> i64 {
        let id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        let credentials = if parent_account_id.is_some() {
            json!({})
        } else if kind == "oauth" {
            json!({
                "access_token":format!("fixture-access-token-{id}"),
                "chatgpt_account_id":format!("fixture-account-{id}"),
                "base_url":format!("{}/v1",self.upstream_url)
            })
        } else {
            json!({
                "api_key":format!("fixture-api-key-{id}"),
                "base_url":format!("{}/v1",self.upstream_url),
                "model_mapping":{"gpt-6-luna":"fixture-mapped-model"}
            })
        };
        sqlx::query("INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,parent_account_id,updated_at) VALUES ($1,$2,'openai',$3,'active',true,$4,$5,$6,now())")
            .bind(id)
            .bind(format!("fixture-modeltrace-account-{id}"))
            .bind(kind)
            .bind(SqlJson(credentials))
            .bind(SqlJson(extra))
            .bind(parent_account_id)
            .execute(&self.runtime.db.pool)
            .await
            .expect("insert synthetic account into temporary PostgreSQL");
        id
    }

    async fn seed_identity_values(&self, user_agent: &str, version: &str) {
        for (key, value) in [
            ("openai_codex_user_agent", user_agent),
            ("openai_codex_client_version", version),
        ] {
            sqlx::query("INSERT INTO settings (key,value) VALUES ($1,$2) ON CONFLICT (key) DO UPDATE SET value=EXCLUDED.value")
                .bind(key)
                .bind(value)
                .execute(&self.runtime.db.pool)
                .await
                .expect("write synthetic Codex identity into temporary PostgreSQL");
        }
    }

    async fn seed_identity(&self) {
        self.seed_identity_values(
            "codex_app/0.144.0 (macOS 15.6; arm64) iTerm (codex_app; 0.144.0)",
            "0.159.3",
        )
        .await;
    }

    async fn seed_model_allowlist(&self, account_id: i64) {
        let group_id = NEXT_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed);
        sqlx::query(
            "INSERT INTO groups (id,name,platform,model_allowlist) VALUES ($1,$2,'openai',$3)",
        )
        .bind(group_id)
        .bind(format!("fixture-modeltrace-group-{group_id}"))
        .bind(SqlJson(json!({"enabled":true,"models":["gpt-6-luna"]})))
        .execute(&self.runtime.db.pool)
        .await
        .expect("insert synthetic enabled model allowlist");
        sqlx::query("INSERT INTO account_groups (account_id,group_id) VALUES ($1,$2)")
            .bind(account_id)
            .bind(group_id)
            .execute(&self.runtime.db.pool)
            .await
            .expect("attach synthetic model allowlist to account");
    }

    async fn start_manual(&self, account_id: i64, concurrency: i64) -> Value {
        let row = self
            .runtime
            .actions
            .account(account_id, None)
            .await
            .expect("read seeded account through the real query layer");
        self.runtime
            .tests
            .start(
                account_id,
                &json!({
                    "model_id":"gpt-6-luna",
                    "expected_version":account_version(&row),
                    "request_id":uuid::Uuid::new_v4().simple().to_string(),
                    "concurrency":concurrency
                }),
                None,
            )
            .await
            .expect("start a real ModelTests job")
    }

    async fn start_automatic(&self, account_id: i64) -> Value {
        let row = self
            .runtime
            .actions
            .account(account_id, None)
            .await
            .expect("read seeded automatic-test account");
        let control = self.runtime.detection.control(account_id).unwrap();
        let mark = self.runtime.detection.mark(account_id).unwrap();
        let automatic = json!({
            "planned_groups":3,
            "detection_generation":control["generation"],
            "detection_mark_version":mark["version"],
            "triggers":["fixture"],
            "degradation_recheck":false
        });
        self.runtime
            .tests
            .start(
                account_id,
                &json!({
                    "model_id":"gpt-6-luna",
                    "expected_version":account_version(&row),
                    "request_id":uuid::Uuid::new_v4().simple().to_string(),
                    "concurrency":3
                }),
                Some(&automatic),
            )
            .await
            .expect("start a real automatic ModelTests job")
    }

    async fn wait_for_requests(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if self.state.count() >= expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("loopback provider did not receive expected requests");
    }

    async fn wait_for_progress(&self, id: &str, groups: usize) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let job = self
                    .runtime
                    .tests
                    .get(id)
                    .expect("ModelTrace progress is readable");
                let observed = job["groups"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|group| group["diagnostics"]["ttft_ms"].is_number())
                    .count();
                if observed >= groups {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("loopback streams did not reach first-text progress");
    }

    async fn wait_for_group_status(&self, id: &str, status: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let job = self
                    .runtime
                    .tests
                    .get(id)
                    .expect("ModelTrace job remains readable while polling group status");
                if job["groups"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|group| group["status"] == status)
                {
                    return job;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("ModelTrace group did not reach the expected status")
    }

    async fn target_for(&self, account_id: i64) -> model_test_stream::Target {
        let row = model_target::account(&self.runtime.actions, account_id)
            .await
            .expect("read account for real target construction");
        model_target::target(&self.runtime.actions, &row, "gpt-6-luna")
            .await
            .expect("construct the real ModelTrace provider target")
            .0
    }

    async fn wait_for_terminal(&self, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let job = self
                    .runtime
                    .tests
                    .get(id)
                    .expect("ModelTrace job remains readable");
                if matches!(
                    job["status"].as_str(),
                    Some(
                        "completed" | "failed" | "cancelled" | "needs_confirmation" | "interrupted"
                    )
                ) {
                    return job;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("ModelTrace job did not reach a terminal state")
    }

    async fn close(mut self) {
        self.runtime.close().await;
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
    }
}

impl Drop for RuntimeHarness {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

fn auth_header(request: &CapturedRequest) -> String {
    request.headers[header::AUTHORIZATION]
        .to_str()
        .expect("synthetic authorization header")
        .to_owned()
}

fn header_text(request: &CapturedRequest, name: header::HeaderName) -> Option<String> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn header_text_from_target(target: &model_test_stream::Target, name: &str) -> Option<String> {
    reqwest::header::HeaderName::from_bytes(name.as_bytes())
        .ok()
        .and_then(|name| target.headers.get(name))
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn group_prompt(index: usize) -> String {
    modeltrace::challenges()[index].1.clone()
}

fn output_from_fixture(case: &Value, index: usize) -> String {
    case["outputs"][index]
        .as_str()
        .expect("fixture output text")
        .to_owned()
}

fn sse_event(event: &Value) -> Vec<u8> {
    format!("data: {event}\n\n").into_bytes()
}

fn partial_delta(text: &str) -> Vec<u8> {
    sse_event(&json!({
        "type":"response.output_text.delta",
        "output_index":0,
        "content_index":0,
        "delta":text
    }))
}

fn completed_event(text: &str) -> Vec<u8> {
    sse_event(&json!({
        "type":"response.completed",
        "response":response_output(text,"fixture-returned-model")
    }))
}

fn gated_replies(outputs: &[String], release: Arc<Semaphore>) -> Vec<Vec<ResponsePlan>> {
    outputs
        .iter()
        .map(|text| {
            vec![ResponsePlan::gated_sse(
                partial_delta("1,2,3"),
                completed_event(text),
                release.clone(),
            )]
        })
        .collect()
}

async fn execute_target(
    target: &model_test_stream::Target,
    prompt: &str,
) -> (
    model_test_stream::TransportResult<(String, Option<String>)>,
    Value,
) {
    let client = target.client().expect("loopback transport client");
    let mut diagnostics = json!({});
    let result =
        model_test_stream::execute(target, prompt, 80, &client, None, &mut diagnostics).await;
    (result, diagnostics)
}

#[test]
fn runtime_fixture_is_pinned_and_synthetic() {
    let fixture = runtime_fixtures();
    let model = &fixture["modeltrace_runtime"];
    let legacy_fixture = modeltrace_fixtures();
    assert_eq!(fixture["schema_version"], 1);
    assert_eq!(fixture["reference_sha"], REFERENCE_SHA);
    assert_eq!(fixture["synthetic_only"], true);
    assert_eq!(legacy_fixture["schema_version"], 1);
    assert_eq!(legacy_fixture["reference_sha"], REFERENCE_SHA);
    assert_eq!(legacy_fixture["synthetic_only"], true);
    assert_eq!(
        model["challenge_length_contract"],
        json!({"count":3,"minimum":292,"maximum":332,"distinct":true})
    );
    let challenges = modeltrace::challenges();
    let lengths = challenges
        .iter()
        .map(|(length, _)| *length)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(challenges.len(), 3);
    assert_eq!(lengths.len(), 3);
    assert!(lengths.iter().all(|length| (292..=332).contains(length)));
    for (_, prompt) in &challenges {
        assert_challenge_prompt(prompt);
    }
    assert_eq!(model["below_99"]["outputs"].as_array().unwrap().len(), 3);
    assert_eq!(model["above_99"]["outputs"].as_array().unwrap().len(), 3);
    assert!(
        model["below_99"]["prefix_reports"][0]["probability"]
            .as_f64()
            .unwrap()
            < 0.99
    );
    assert_eq!(model["below_99"]["display_probability_2dp"], 0.99);
    assert!(
        model["above_99"]["prefix_reports"][0]["probability"]
            .as_f64()
            .unwrap()
            >= 0.99
    );
    assert_eq!(model["above_99"]["display_probability_2dp"], 0.99);
}

#[test]
fn pinned_legacy_80_number_vectors_match_validation_and_analysis() {
    let fixture = runtime_fixtures()["modeltrace_runtime"]["legacy_80_number_vectors"].clone();
    let (bank, version) = modeltrace::bundled().expect("pinned bundled bank");
    assert_eq!(fixture["bank_is_pinned"], true);
    assert_eq!(fixture["bank_sha256"], version["sha256"]);

    let output = fixture["valid_sequence"]["output"].as_str().unwrap();
    let numbers = modeltrace::validate(output);
    assert_eq!(numbers.len(), fixture["valid_sequence"]["validated_count"]);
    assert_eq!(numbers.len(), 80);
    let report = modeltrace::analyze(&[output.to_owned()], &(bank.clone(), version.clone()))
        .expect("analyze the exact pinned 80-number sequence");
    assert_report(
        &report,
        &fixture["valid_sequence"]["report"],
        "legacy fixed 80-number sequence vs pinned Python oracle",
    );

    let thought = &fixture["thought_fence"];
    let plain = thought["plain_output"].as_str().unwrap();
    let wrapped = thought["wrapped_output"].as_str().unwrap();
    let plain_numbers = modeltrace::validate(plain);
    let wrapped_numbers = modeltrace::validate(wrapped);
    assert_eq!(plain_numbers.len(), thought["plain_validated_count"]);
    assert_eq!(wrapped_numbers.len(), thought["wrapped_validated_count"]);
    assert_eq!(plain_numbers.len(), 80);
    assert_eq!(wrapped_numbers, plain_numbers);
    assert_eq!(
        modeltrace::validate(thought["invalid_mixed_text"].as_str().unwrap()).len(),
        thought["invalid_mixed_count"]
    );
    assert!(modeltrace::validate(thought["invalid_mixed_text"].as_str().unwrap()).is_empty());
    let plain_report = modeltrace::analyze(&[plain.to_owned()], &(bank.clone(), version.clone()))
        .expect("analyze the plain pinned sequence");
    let wrapped_report = modeltrace::analyze(&[wrapped.to_owned()], &(bank, version))
        .expect("analyze the thought/fence wrapped pinned sequence");
    assert_report(
        &plain_report,
        &thought["plain_report"],
        "legacy plain thought/fence sequence vs pinned Python oracle",
    );
    assert_report(
        &wrapped_report,
        &thought["wrapped_report"],
        "legacy wrapped thought/fence sequence vs pinned Python oracle",
    );
    assert_report(
        &wrapped_report,
        &plain_report,
        "thought prose and fence do not change the scored values",
    );
}

#[test]
fn legacy_native_identity_pairing_and_version_validation_are_preserved() {
    let (version, agent) = model_target::identity(&json!({
        "openai_codex_user_agent":"bad/1.0 (Linux; x86_64) term (codex-tui; 0.1.0)",
        "openai_codex_client_version":"0.160.1-beta.2"
    }));
    assert_eq!(version, "0.160.1-beta.2");
    assert_eq!(
        model_target::originator(agent.split('/').next().unwrap()),
        Some("codex-tui".into())
    );
    assert_eq!(
        agent,
        "codex-tui/0.160.1-beta.2 (Linux; x86_64) term (codex-tui; 0.160.1-beta.2)"
    );
    assert!(!agent.contains("bad/"));
}

#[test]
fn legacy_hard_http_failures_are_not_retryable_but_503_is() {
    for (status, retryable) in [
        (StatusCode::UNAUTHORIZED, false),
        (StatusCode::TOO_MANY_REQUESTS, false),
        (StatusCode::SERVICE_UNAVAILABLE, true),
    ] {
        let failure = model_test_stream::failure(status.as_u16(), &json!({}));
        assert_eq!(
            failure.retryable, retryable,
            "HTTP {status} retryability matches the pinned ModelTrace contract"
        );
    }
    assert!(!model_test_stream::failure(0, &json!({"error":{"message":"Concurrency limit exceeded for account, please retry later"}})).retryable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_http_concurrency_one_two_three_matches_oracle_and_redacts_summary() {
    let harness = RuntimeHarness::new().await;
    let fixture = runtime_fixtures()["modeltrace_runtime"]["below_99"].clone();
    let outputs = (0..3)
        .map(|index| output_from_fixture(&fixture, index))
        .collect::<Vec<_>>();

    for concurrency in [1, 2, 3] {
        let gate = RequestGate::new(3);
        harness.state.set_scenario(Scenario::sampled(
            &outputs,
            responses_sse,
            Some(gate.clone()),
        ));
        let account_id = harness.seed_account("apikey", None, json!({})).await;
        let started = harness.start_manual(account_id, concurrency).await;
        let job_id = started["id"].as_str().unwrap().to_owned();

        let peak_barrier = concurrency as usize;
        harness.wait_for_requests(peak_barrier).await;
        assert_eq!(
            harness.state.active(),
            peak_barrier,
            "all first {concurrency} HTTP requests should be held at the loopback gate"
        );
        assert_eq!(harness.state.peak(), peak_barrier);
        let first_batch = harness.state.captured();
        for request in &first_batch {
            gate.release_group(request.sample_index, 1);
        }

        if concurrency == 1 {
            for expected_count in 2..=3 {
                harness.wait_for_requests(expected_count).await;
                if let Some(index) = harness
                    .state
                    .captured()
                    .last()
                    .map(|request| request.sample_index)
                {
                    gate.release_group(index, 1);
                }
            }
        } else if concurrency == 2 {
            harness.wait_for_requests(3).await;
            if let Some(index) = harness
                .state
                .captured()
                .last()
                .map(|request| request.sample_index)
            {
                gate.release_group(index, 1);
            }
        }

        let job = harness.wait_for_terminal(&job_id).await;
        assert_eq!(
            harness.state.count(),
            3,
            "one HTTP request per planned group"
        );
        let captured = harness.state.captured();
        assert_three_distinct_challenges(&captured);
        assert_eq!(harness.state.peak(), peak_barrier);
        assert_eq!(job["status"], "completed");
        assert_eq!(job["completed_groups"], 3);
        assert_eq!(job["valid_groups"], 3);
        assert!(
            job["groups"]
                .as_array()
                .unwrap()
                .iter()
                .all(|group| { group["ttft_ms"].as_u64().is_some() }),
            "each actual group records a first-text duration"
        );
        assert_report(
            &job["report"],
            &fixture["prefix_reports"][2],
            "three-sample Runtime analysis vs pinned Python oracle",
        );
        let request_indexes = captured
            .iter()
            .map(|request| request.sample_index)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(request_indexes, [0, 1, 2].into_iter().collect());
        assert!(captured
            .iter()
            .all(|request| request.path == "/v1/responses"
                && request.payload["model"] == "gpt-6-luna"));

        let persisted = fs::read_to_string(harness.runtime.tests.store.path())
            .expect("read the actual ModelTests result transaction");
        assert!(!persisted.contains(&outputs[0]));
        assert!(!persisted.contains("fixture-api-key-"));
        assert!(!persisted.contains("input_text"));
        for request in &captured {
            let prompt = prompt_from_payload(&request.payload).unwrap();
            assert!(!persisted.contains(prompt));
        }
    }
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_api_key_oauth_and_chat_targets_send_expected_http_payloads() {
    let harness = RuntimeHarness::new().await;
    harness.seed_identity().await;
    let text = "1, 2, 3";

    let api_key_id = harness
        .seed_account(
            "apikey",
            None,
            json!({"model_mapping":{"gpt-6-luna":"fixture-mapped-model"}}),
        )
        .await;
    let api_key = harness.target_for(api_key_id).await;
    let api_prompt = group_prompt(0);
    assert_eq!(
        api_key.url,
        format!("{}/v1/responses", harness.upstream_url)
    );
    assert_eq!(api_key.model, "gpt-6-luna");
    assert_eq!(api_key.protocol, "responses");
    assert_eq!(
        api_key.headers[header::AUTHORIZATION],
        format!("Bearer fixture-api-key-{api_key_id}")
    );
    let api_payload = api_key.payload(&api_prompt);
    assert_eq!(api_payload["model"], "gpt-6-luna");
    assert_eq!(api_payload["max_output_tokens"], 4096);
    assert_eq!(api_payload["stream"], true);
    assert_eq!(api_payload["store"], false);
    assert_eq!(api_payload["input"][0]["content"][0]["text"], api_prompt);
    assert!(api_payload.get("tools").is_none());
    assert!(api_payload.get("reasoning").is_none());
    assert!(api_payload.get("reasoning_effort").is_none());
    assert!(api_payload.get("thinking").is_none());

    harness
        .state
        .set_scenario(Scenario::single(responses_json(text)));
    let (result, diagnostics) = execute_target(&api_key, &api_prompt).await;
    assert_eq!(
        result.unwrap(),
        (text.into(), Some("fixture-returned-model".into()))
    );
    assert_eq!(harness.state.count(), 1);
    assert_eq!(diagnostics["response_protocol"], "responses");
    assert_eq!(diagnostics["input_tokens"], 23);
    assert_eq!(diagnostics["output_tokens"], 19);
    assert_eq!(diagnostics["reasoning_tokens"], 2);
    let request = &harness.state.captured()[0];
    assert_eq!(request.path, "/v1/responses");
    assert_eq!(
        auth_header(request),
        format!("Bearer fixture-api-key-{api_key_id}")
    );
    assert_eq!(request.payload["model"], "gpt-6-luna");
    assert_eq!(request.payload["max_output_tokens"], 4096);
    for name in ["originator", "version", "openai-beta", "chatgpt-account-id"] {
        assert!(
            request.headers.get(name).is_none(),
            "API-key request has no OAuth {name} header"
        );
    }
    assert_eq!(
        header_text(request, header::USER_AGENT).as_deref(),
        Some("codex_app/0.159.3 (macOS 15.6; arm64) iTerm (codex_app; 0.159.3)")
    );

    let oauth_id = harness.seed_account("oauth", None, json!({})).await;
    let mut oauth = harness.target_for(oauth_id).await;
    assert_eq!(oauth.url, "https://chatgpt.com/backend-api/codex/responses");
    assert_eq!(oauth.protocol, "responses");
    assert!(oauth.oauth);
    assert_eq!(
        header_text_from_target(&oauth, "version"),
        Some("0.159.3".into())
    );
    assert_eq!(
        header_text_from_target(&oauth, "originator"),
        Some("codex_app".into())
    );
    assert_eq!(
        header_text_from_target(&oauth, "openai-beta"),
        Some("responses=experimental".into())
    );
    assert_eq!(
        header_text_from_target(&oauth, "chatgpt-account-id"),
        Some(format!("fixture-account-{oauth_id}"))
    );
    assert!(header_text_from_target(&oauth, "user-agent")
        .unwrap()
        .contains("codex_app/0.159.3"));
    let oauth_prompt = group_prompt(0);
    let oauth_payload = oauth.payload(&oauth_prompt);
    assert!(oauth_payload.get("max_output_tokens").is_none());
    assert_eq!(oauth_payload["stream"], true);
    oauth.url = format!("{}/backend-api/codex/responses", harness.upstream_url);
    harness
        .state
        .set_scenario(Scenario::single(responses_json(text)));
    let (result, _) = execute_target(&oauth, &oauth_prompt).await;
    assert_eq!(result.unwrap().0, text);
    assert_eq!(harness.state.count(), 1);
    let request = &harness.state.captured()[0];
    assert_eq!(request.path, "/backend-api/codex/responses");
    assert_eq!(
        auth_header(request),
        format!("Bearer fixture-access-token-{oauth_id}")
    );
    assert!(request.payload.get("max_output_tokens").is_none());
    assert_eq!(
        header_text(request, header::USER_AGENT),
        header_text_from_target(&oauth, "user-agent")
    );
    assert_eq!(
        header_text(request, header::HeaderName::from_static("originator")).as_deref(),
        Some("codex_app")
    );
    assert_eq!(
        header_text(request, header::HeaderName::from_static("version")).as_deref(),
        Some("0.159.3")
    );
    assert_eq!(
        header_text(request, header::HeaderName::from_static("openai-beta")).as_deref(),
        Some("responses=experimental")
    );
    let account_header = format!("fixture-account-{oauth_id}");
    assert_eq!(
        header_text(
            request,
            header::HeaderName::from_static("chatgpt-account-id")
        )
        .as_deref(),
        Some(account_header.as_str())
    );

    harness
        .seed_identity_values(
            "codex-tui/0.146.1 (macOS 15.6; arm64) iTerm2 (codex-tui; 0.146.1)",
            "0.160.0",
        )
        .await;
    let mut legacy_oauth = harness.target_for(oauth_id).await;
    assert_eq!(
        header_text_from_target(&legacy_oauth, "user-agent").as_deref(),
        Some("codex-tui/0.160.0 (macOS 15.6; arm64) iTerm2 (codex-tui; 0.160.0)")
    );
    legacy_oauth.url = format!("{}/backend-api/codex/responses", harness.upstream_url);
    harness
        .state
        .set_scenario(Scenario::single(responses_json(text)));
    let (result, _) = execute_target(&legacy_oauth, &oauth_prompt).await;
    assert_eq!(result.unwrap().0, text);
    assert_eq!(harness.state.count(), 1);
    let request = &harness.state.captured()[0];
    assert_eq!(
        header_text(request, header::USER_AGENT).as_deref(),
        Some("codex-tui/0.160.0 (macOS 15.6; arm64) iTerm2 (codex-tui; 0.160.0)")
    );
    assert_eq!(
        header_text(request, header::HeaderName::from_static("originator")).as_deref(),
        Some("codex-tui")
    );
    assert_eq!(
        header_text(request, header::HeaderName::from_static("version")).as_deref(),
        Some("0.160.0")
    );

    let chat_id = harness
        .seed_account("apikey", None, json!({"openai_responses_supported":false}))
        .await;
    let chat = harness.target_for(chat_id).await;
    let chat_prompt = group_prompt(0);
    assert_eq!(chat.protocol, "chat_completions");
    assert_eq!(
        chat.url,
        format!("{}/v1/chat/completions", harness.upstream_url)
    );
    let chat_payload = chat.payload(&chat_prompt);
    assert_eq!(chat_payload["max_completion_tokens"], 4096);
    assert!(chat_payload.get("max_output_tokens").is_none());
    assert!(chat_payload.get("tools").is_none());
    assert!(chat_payload.get("reasoning").is_none());
    assert!(chat_payload.get("reasoning_effort").is_none());
    assert!(chat_payload.get("thinking").is_none());
    harness
        .state
        .set_scenario(Scenario::single(chat_json(text)));
    let (result, diagnostics) = execute_target(&chat, &chat_prompt).await;
    assert_eq!(result.unwrap().0, text);
    assert_eq!(harness.state.count(), 1);
    assert_eq!(diagnostics["response_protocol"], "chat_completions");
    let request = &harness.state.captured()[0];
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.payload["max_completion_tokens"], 4096);

    harness
        .state
        .set_scenario(Scenario::single(chat_sse_done(text)));
    let (result, diagnostics) = execute_target(&chat, &chat_prompt).await;
    assert_eq!(result.unwrap().0, text);
    assert_eq!(harness.state.count(), 1);
    assert_eq!(diagnostics["end_reason"], "done");
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transport_diagnostics_redact_untrusted_event_names_and_content_types() {
    let harness = RuntimeHarness::new().await;
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let target = harness.target_for(account_id).await;
    let secret = "sk-test-synthetic-diagnostic-value";
    let event = json!({"type":secret});
    let terminal =
        json!({"type":"response.completed","response":response_output("1,2,3", "fixture-model")});
    harness
        .state
        .set_scenario(Scenario::single(ResponsePlan::bytes(
            StatusCode::OK,
            &format!("application/{secret}"),
            format!("data: {event}\n\ndata: {terminal}\n\n").into_bytes(),
        )));
    let (result, diagnostics) = execute_target(&target, &group_prompt(0)).await;
    assert_eq!(result.unwrap().0, "1,2,3");
    assert_eq!(harness.state.count(), 1);
    assert_eq!(diagnostics["first_event_type"], "[REDACTED]");
    assert_eq!(diagnostics["last_event_type"], "response.completed");
    assert_eq!(diagnostics["content_type"], "application/[REDACTED]");
    assert_eq!(diagnostics["end_reason"], "completed");
    assert!(!diagnostics.to_string().contains(secret));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("audit.jsonl");
    sub2ops_server::audit::write(&path, "model_test_transport", diagnostics.clone(), false);
    let stored = sub2ops_server::audit::read(&path, 1, None);
    assert_eq!(stored[0]["payload"], diagnostics);
    assert!(!fs::read_to_string(path).unwrap().contains(secret));
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn responses_sse_event_names_multiline_crlf_eof_and_small_terminal_frames() {
    let harness = RuntimeHarness::new().await;
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let target = harness.target_for(account_id).await;

    harness
        .state
        .set_scenario(Scenario::single(responses_sse("1,2,3")));
    let (result, diagnostics) = execute_target(&target, &group_prompt(0)).await;
    assert_eq!(result.unwrap().0, "1,2,3");
    assert_eq!(diagnostics["end_reason"], "completed");
    assert_eq!(diagnostics["response_protocol"], "responses");

    let delta = json!({"type":"response.output_text.delta","delta":"1 1 "});
    let completed = json!({"response":{"status":"completed","model":"actual"}});
    let delta_then_completed = ResponsePlan::bytes(
        StatusCode::OK,
        "text/event-stream",
        format!(
            "event: response.output_text.delta\r\ndata: {delta}\r\n\r\nevent: response.completed\r\ndata: {completed}"
        )
        .into_bytes(),
    );
    harness
        .state
        .set_scenario(Scenario::single(delta_then_completed));
    let (result, diagnostics) = execute_target(&target, &group_prompt(0)).await;
    assert_eq!(result.unwrap(), ("1 1 ".into(), Some("actual".into())));
    assert_eq!(diagnostics["events"], 2);
    assert_eq!(
        diagnostics["first_event_type"],
        "response.output_text.delta"
    );
    assert_eq!(diagnostics["last_event_type"], "response.completed");

    let response = response_output("1,2,3", "fixture-returned-model");
    let event = json!({"type":"response.completed","response":response});
    let serialized = event.to_string();
    let split = serialized
        .find(",\"response\"")
        .expect("fixture response separates event type and payload")
        + 1;
    let multiline_eof = ResponsePlan::bytes(
        StatusCode::OK,
        "text/event-stream",
        format!(
            "event: response.completed\r\ndata: {}\r\ndata: {}\r\n",
            &serialized[..split],
            &serialized[split..]
        )
        .into_bytes(),
    );
    harness.state.set_scenario(Scenario::single(multiline_eof));
    let (result, diagnostics) = execute_target(&target, &group_prompt(0)).await;
    assert_eq!(result.unwrap().0, "1,2,3");
    assert_eq!(diagnostics["end_reason"], "completed");

    let oauth_id = harness.seed_account("oauth", None, json!({})).await;
    let mut oauth = harness.target_for(oauth_id).await;
    oauth.url = format!("{}/backend-api/codex/responses", harness.upstream_url);
    let small_terminal = ResponsePlan::hanging_sse(sse_event(&event));
    harness.state.set_scenario(Scenario::single(small_terminal));
    let started = Instant::now();
    let (result, diagnostics) = tokio::time::timeout(
        Duration::from_millis(500),
        execute_target(&oauth, &group_prompt(0)),
    )
    .await
    .expect("a small completed frame returns within the legacy 500 ms bound");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "small terminal does not wait for four KiB or stream EOF"
    );
    assert_eq!(result.unwrap().0, "1,2,3");
    assert_eq!(diagnostics["end_reason"], "completed");
    assert_eq!(harness.state.count(), 1);
    let request = &harness.state.captured()[0];
    for field in [
        "tools",
        "reasoning",
        "reasoning_effort",
        "thinking",
        "max_output_tokens",
    ] {
        assert!(
            request.payload.get(field).is_none(),
            "OAuth request omits {field}"
        );
    }
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raw_99_boundary_early_stream_cancellation_and_automatic_first_group_match_oracle() {
    let harness = RuntimeHarness::new().await;
    let fixture = runtime_fixtures()["modeltrace_runtime"].clone();

    let below = fixture["below_99"].clone();
    let below_outputs = (0..3)
        .map(|index| output_from_fixture(&below, index))
        .collect::<Vec<_>>();
    assert!(below["prefix_reports"][0]["probability"].as_f64().unwrap() < 0.99);
    assert_eq!(below["display_probability_2dp"], 0.99);
    harness
        .state
        .set_scenario(Scenario::sampled(&below_outputs, responses_sse, None));
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let started = harness.start_manual(account_id, 3).await;
    let below_job = harness
        .wait_for_terminal(started["id"].as_str().unwrap())
        .await;
    assert_eq!(harness.state.count(), 3);
    assert_eq!(below_job["completion_reason"], "samples_finished");
    assert_report(
        &below_job["report"],
        &below["prefix_reports"][2],
        "raw probability below 99 percent despite display rounding to 0.99",
    );

    let above = fixture["above_99"].clone();
    let above_outputs = (0..3)
        .map(|index| output_from_fixture(&above, index))
        .collect::<Vec<_>>();
    let gate = RequestGate::new(3);
    harness.state.set_scenario(Scenario::sampled(
        &above_outputs,
        responses_sse,
        Some(gate.clone()),
    ));
    let early_account = harness.seed_account("apikey", None, json!({})).await;
    let started = harness.start_manual(early_account, 3).await;
    let early_id = started["id"].as_str().unwrap().to_owned();
    harness.wait_for_requests(3).await;
    assert_eq!(harness.state.active(), 3);
    assert_eq!(harness.state.peak(), 3);
    let early_requests = harness.state.captured();
    assert_three_distinct_challenges(&early_requests);
    assert!(early_requests
        .iter()
        .any(|request| request.sample_index == 0));
    assert_eq!(
        above["prefix_reports"][0]["probability"],
        0.9904193970315156
    );
    gate.release_group(0, 1);
    let early_job = harness.wait_for_terminal(&early_id).await;
    assert_eq!(early_job["completion_reason"], "confidence_99");
    assert_eq!(early_job["completed_groups"], 1);
    let early_groups = early_job["groups"].as_array().unwrap();
    assert_eq!(
        early_groups
            .iter()
            .filter(|group| group["status"] == "completed")
            .count(),
        1
    );
    assert!(early_groups.iter().all(|group| {
        group["status"] == "completed"
            || matches!(group["status"].as_str(), Some("skipped" | "cancelled"))
    }));
    assert_report(
        &early_job["report"],
        &above["prefix_reports"][0],
        "raw probability at or above 99 percent triggers early finish",
    );
    assert_eq!(harness.state.count(), 3);
    let terminal_snapshot = early_job.clone();
    gate.release_all(2);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        harness.runtime.tests.get(&early_id).unwrap(),
        terminal_snapshot
    );
    assert_eq!(
        harness.state.count(),
        3,
        "early finish does not dispatch late groups"
    );

    let automatic = fixture["automatic_first_group"].clone();
    let automatic_text = output_from_fixture(&automatic, 0);
    harness
        .state
        .set_scenario(Scenario::sampled(&[automatic_text], responses_sse, None));
    let automatic_account = harness.seed_account("apikey", None, json!({})).await;
    harness.seed_model_allowlist(automatic_account).await;
    let started = harness.start_automatic(automatic_account).await;
    let automatic_job = harness
        .wait_for_terminal(started["id"].as_str().unwrap())
        .await;
    assert_eq!(
        harness.state.count(),
        1,
        "automatic target prediction ends after sample one"
    );
    assert_eq!(automatic_job["completion_reason"], "automatic_degradation");
    assert_eq!(automatic_job["first_group_prediction"], "gpt-5.6-luna");
    assert_eq!(
        automatic_job["report"]["prediction"],
        automatic["prefix_reports"][0]["prediction"]
    );
    assert_eq!(automatic_job["completed_groups"], 1);
    assert!(automatic_job["groups"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .all(|group| group["status"] == "skipped"));
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_token_and_filter_terminals_keep_samples_but_unknown_is_not_success() {
    let harness = RuntimeHarness::new().await;
    let fixture = runtime_fixtures()["modeltrace_runtime"]["below_99"].clone();
    let outputs = (0..3)
        .map(|index| output_from_fixture(&fixture, index))
        .collect::<Vec<_>>();
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let target = harness.target_for(account_id).await;

    for reason in ["max_output_tokens", "max_tokens", "content_filter"] {
        for sse in [false, true] {
            let text = if reason == "content_filter" {
                ""
            } else {
                outputs[0].as_str()
            };
            let plan = if sse {
                incomplete_sse(reason, text)
            } else {
                incomplete_json(reason, text)
            };
            harness.state.set_scenario(Scenario::single(plan));
            let prompt = group_prompt(0);
            let (result, diagnostics) = execute_target(&target, &prompt).await;
            assert_eq!(result.unwrap().0, text, "{reason} sse={sse}");
            assert_eq!(harness.state.count(), 1, "{reason} sse={sse}");
            assert_eq!(diagnostics["end_reason"], reason);
            assert_eq!(diagnostics["incomplete_reason"], reason);
            assert_eq!(diagnostics["provider_status"], "incomplete");
            assert_eq!(diagnostics["input_tokens"], 120);
            assert_eq!(diagnostics["output_tokens"], 4096);
            assert_eq!(diagnostics["reasoning_tokens"], 3890);
            assert_eq!(diagnostics["max_output_tokens"], 4096);
            assert_eq!(
                diagnostics["last_event_type"],
                if sse {
                    "response.incomplete"
                } else {
                    "response"
                }
            );
            let serialized = diagnostics.to_string();
            if !text.is_empty() {
                assert!(!serialized.contains(text), "diagnostics omit the sample");
            }
            assert!(
                !serialized.contains(&prompt),
                "diagnostics omit the challenge"
            );
            assert!(
                !serialized.contains("fixture-api-key-"),
                "diagnostics omit the credential"
            );
        }
    }

    for expected_valid in [3, 0] {
        let replies = (0..3)
            .map(|index| {
                let output = if expected_valid == 3 {
                    outputs[index].as_str()
                } else {
                    ""
                };
                vec![incomplete_sse("max_output_tokens", output)]
            })
            .collect::<Vec<_>>();
        harness
            .state
            .set_scenario(Scenario::with_replies(replies, None));
        let run_account = harness.seed_account("apikey", None, json!({})).await;
        let started = harness.start_manual(run_account, 3).await;
        let job = harness
            .wait_for_terminal(started["id"].as_str().unwrap())
            .await;
        assert_eq!(
            harness.state.count(),
            3,
            "a provider token terminal uses three requests, not nine"
        );
        assert_eq!(job["status"], "completed");
        assert_eq!(job["attempts"], 3);
        assert_eq!(job["completed_groups"], 3);
        assert_eq!(job["valid_groups"], expected_valid);
        assert_eq!(job["can_retry"], false);
        assert!(job["groups"].as_array().unwrap().iter().all(|group| {
            group["attempts"] == 1 && group["diagnostics"]["end_reason"] == "max_output_tokens"
        }));
        if expected_valid == 3 {
            assert!(job["groups"]
                .as_array()
                .unwrap()
                .iter()
                .all(|group| { group["ttft_ms"].as_u64().is_some() }));
        }
    }

    harness.state.set_scenario(Scenario::single(incomplete_json(
        "unknown_terminal",
        "1,2,3",
    )));
    let unknown_account = harness.seed_account("apikey", None, json!({})).await;
    let started = harness.start_manual(unknown_account, 1).await;
    let unknown = harness
        .wait_for_terminal(started["id"].as_str().unwrap())
        .await;
    assert_eq!(harness.state.count(), 1);
    assert_eq!(unknown["status"], "failed");
    assert_eq!(unknown["error_code"], "incomplete_response");
    assert_eq!(unknown["attempts"], 1);
    assert_eq!(unknown["can_retry"], false);

    let no_terminal = ResponsePlan::bytes(
        StatusCode::OK,
        "text/event-stream",
        b"data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\"}}\n\n"
            .to_vec(),
    );
    harness.state.set_scenario(Scenario::single(no_terminal));
    let (result, _) = execute_target(&target, &group_prompt(0)).await;
    let failure = result.expect_err("EOF without a terminal is retryable incomplete stream");
    assert_eq!(failure.code, "incomplete_stream");
    assert!(failure.retryable);
    assert_eq!(harness.state.count(), 1);

    for (payload, expected_code) in [
        (json!({"success":true}), "protocol_mismatch"),
        (
            json!({"error":{"message":"Invalid API key","code":"invalid_api_key"}}),
            "auth_or_quota",
        ),
    ] {
        harness
            .state
            .set_scenario(Scenario::single(ResponsePlan::bytes(
                StatusCode::OK,
                "application/json",
                payload.to_string().into_bytes(),
            )));
        let (result, _) = execute_target(&target, &group_prompt(0)).await;
        let failure = result.expect_err("200 JSON protocol errors remain fatal");
        assert_eq!(failure.code, expected_code);
        assert!(!failure.retryable);
        assert_eq!(harness.state.count(), 1);
    }
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_payload_budget_and_terminal_diagnostics_match_both_protocols() {
    let harness = RuntimeHarness::new().await;
    let text = "1,2,3";
    let responses_id = harness.seed_account("apikey", None, json!({})).await;
    let responses = harness.target_for(responses_id).await;
    let events = [
        json!({"type":"response.created","response":{"status":"in_progress"}}),
        json!({"type":"response.completed","response":incomplete_value("max_output_tokens", text)}),
    ];
    let body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>()
        .into_bytes();
    harness
        .state
        .set_scenario(Scenario::single(ResponsePlan::bytes(
            StatusCode::OK,
            "text/event-stream",
            body,
        )));
    let (result, diagnostics) = execute_target(&responses, &group_prompt(0)).await;
    assert_eq!(result.unwrap().0, text);
    assert_eq!(diagnostics["end_reason"], "max_output_tokens");
    assert_eq!(diagnostics["first_event_type"], "response.created");
    assert_eq!(diagnostics["last_event_type"], "response.completed");
    assert_eq!(diagnostics["reasoning_tokens"], 3890);
    assert_eq!(diagnostics["events"], 2);
    let request = &harness.state.captured()[0];
    assert_eq!(request.payload["max_output_tokens"], 4096);
    for field in ["tools", "reasoning", "reasoning_effort", "thinking"] {
        assert!(
            request.payload.get(field).is_none(),
            "Responses omits {field}"
        );
    }
    assert_eq!(harness.state.count(), 1);

    let chat_id = harness
        .seed_account("apikey", None, json!({"openai_responses_supported":false}))
        .await;
    let chat = harness.target_for(chat_id).await;
    assert_eq!(chat.protocol, "chat_completions");
    harness
        .state
        .set_scenario(Scenario::single(chat_length_sse(text)));
    let (result, diagnostics) = execute_target(&chat, &group_prompt(0)).await;
    assert_eq!(result.unwrap().0, text);
    assert_eq!(diagnostics["end_reason"], "length");
    assert_eq!(diagnostics["first_event_type"], "chat.completion");
    assert_eq!(diagnostics["last_event_type"], "chat.completion");
    assert_eq!(diagnostics["reasoning_tokens"], 3890);
    assert_eq!(diagnostics["events"], 1);
    let request = &harness.state.captured()[0];
    assert_eq!(request.payload["max_completion_tokens"], 4096);
    for field in ["tools", "reasoning", "reasoning_effort", "thinking"] {
        assert!(request.payload.get(field).is_none(), "Chat omits {field}");
    }
    assert_eq!(harness.state.count(), 1);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uncertain_body_disconnect_is_not_replayed_and_cannot_be_manually_retried() {
    let harness = RuntimeHarness::new().await;
    harness
        .state
        .set_scenario(Scenario::single(ResponsePlan::broken_sse(partial_delta(
            "1,2,3",
        ))));
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let started = harness.start_manual(account_id, 1).await;
    let job_id = started["id"].as_str().unwrap().to_owned();
    let request_id = uuid::Uuid::new_v4().simple().to_string();
    let failed = harness.wait_for_terminal(&job_id).await;
    assert_eq!(harness.state.count(), 1);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error_code"], "network_error");
    assert_eq!(failed["uncertain"], true);
    assert_eq!(failed["can_retry"], false);
    assert_eq!(failed["attempts"], 1);
    assert_eq!(failed["groups"][0]["uncertain"], true);

    let retry = harness
        .runtime
        .tests
        .retry_failed(&job_id, &request_id)
        .await
        .expect_err("uncertain dispatch must not be manually replayed");
    assert_eq!(retry.status.as_u16(), 409);
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert_eq!(
        harness.state.count(),
        1,
        "no delayed automatic replay after uncertain failure"
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn definite_upstream_rejection_retries_only_failed_group_and_releases_slot() {
    let harness = RuntimeHarness::new().await;
    let fixture = runtime_fixtures()["modeltrace_runtime"]["below_99"].clone();
    let outputs = (0..3)
        .map(|index| output_from_fixture(&fixture, index))
        .collect::<Vec<_>>();
    let unavailable = status_error(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({"error":{"message":"fixture upstream unavailable"}}),
    );
    let gate = RequestGate::new(3);
    let replies = vec![
        vec![unavailable, responses_sse(&outputs[0])],
        vec![responses_sse(&outputs[1])],
        vec![responses_sse(&outputs[2])],
    ];
    harness
        .state
        .set_scenario(Scenario::with_replies(replies, Some(gate.clone())));
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let started = harness.start_manual(account_id, 1).await;
    let job_id = started["id"].as_str().unwrap();

    harness.wait_for_requests(1).await;
    gate.release_group(0, 1);
    harness.wait_for_requests(2).await;
    assert_eq!(
        harness.state.captured()[1].sample_index,
        1,
        "the group slot is free while a definite rejection backs off"
    );
    gate.release_group(1, 1);
    harness.wait_for_requests(3).await;
    assert_eq!(harness.state.captured()[2].sample_index, 2);
    gate.release_group(2, 1);

    let job = harness.wait_for_terminal(job_id).await;
    assert_eq!(harness.state.count(), 4);
    let indexes = harness
        .state
        .captured()
        .iter()
        .map(|request| request.sample_index)
        .collect::<Vec<_>>();
    assert_eq!(indexes, [0, 1, 2, 0]);
    assert_eq!(job["status"], "completed");
    assert_eq!(job["attempts"], 4);
    assert_eq!(job["groups"][0]["attempts"], 2);
    assert_eq!(job["groups"][1]["attempts"], 1);
    assert_eq!(job["groups"][2]["attempts"], 1);
    assert_eq!(job["completed_groups"], 3);
    assert_eq!(job["can_retry"], false);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fatal_401_and_429_cancel_dispatched_peers_without_retry() {
    let harness = RuntimeHarness::new().await;
    let outputs = ["1,2,3".to_owned(), "4,5,6".to_owned(), "7,8,9".to_owned()];

    for (status, expected_error) in [
        (StatusCode::UNAUTHORIZED, "auth_or_quota"),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
    ] {
        let gate = RequestGate::new(3);
        let replies = vec![
            vec![status_error(
                status,
                json!({"error":{"message":"fixture fatal response"}}),
            )],
            vec![responses_sse(&outputs[1])],
            vec![responses_sse(&outputs[2])],
        ];
        harness
            .state
            .set_scenario(Scenario::with_replies(replies, Some(gate.clone())));
        let account_id = harness.seed_account("apikey", None, json!({})).await;
        let started = harness.start_manual(account_id, 3).await;
        let job_id = started["id"].as_str().unwrap().to_owned();
        harness.wait_for_requests(3).await;
        assert_eq!(harness.state.active(), 3);
        let fatal_index = harness.state.captured()[0].sample_index;
        gate.release_group(fatal_index, 1);

        let job = harness.wait_for_terminal(&job_id).await;
        assert_eq!(job["status"], "failed");
        assert_eq!(job["error_code"], expected_error);
        assert_eq!(job["attempts"], 3);
        assert_eq!(harness.state.count(), 3);
        assert_eq!(
            job["groups"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|group| group["status"] == "failed")
                .count(),
            1
        );
        assert_eq!(
            job["groups"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|group| group["status"] == "cancelled")
                .count(),
            2
        );

        gate.release_all(1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(harness.state.count(), 3, "fatal peers are never retried");
        assert_eq!(
            harness.runtime.tests.get(&job_id).unwrap()["status"],
            "failed"
        );
    }
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_aborts_a_pending_retry_timer_without_late_dispatch() {
    let harness = RuntimeHarness::new().await;
    let retries = vec![
        status_error(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":{"message":"fixture upstream unavailable"}}),
        ),
        responses_sse("1,2,3"),
    ];
    let replies = (0..3).map(|_| retries.clone()).collect::<Vec<_>>();
    let gate = RequestGate::new(3);
    harness
        .state
        .set_scenario(Scenario::with_replies(replies, Some(gate.clone())));
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let started = harness.start_manual(account_id, 3).await;
    let job_id = started["id"].as_str().unwrap().to_owned();
    harness.wait_for_requests(3).await;
    assert_eq!(
        harness.state.active(),
        3,
        "all first attempts are held at the gate"
    );
    gate.release_all(1);
    let waiting = harness.wait_for_group_status(&job_id, "retrying").await;
    assert_eq!(waiting["attempts"], 3);
    assert_eq!(harness.state.count(), 3);

    let cancelled = harness.runtime.tests.cancel(&job_id).await.unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert_eq!(
        harness.state.count(),
        3,
        "the cancelled one-second retry timers cannot issue a second wave of POSTs"
    );
    assert_eq!(
        harness.runtime.tests.get(&job_id).unwrap()["status"],
        "cancelled"
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_waits_for_pending_progress_write_before_terminal_state() {
    let harness = RuntimeHarness::new().await;
    let gate = RequestGate::new(1);
    harness.state.set_scenario(Scenario::sampled(
        &["1,2,3".to_owned()],
        responses_sse,
        Some(gate.clone()),
    ));
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let started = harness.start_manual(account_id, 1).await;
    let job_id = started["id"].as_str().unwrap().to_owned();
    harness.wait_for_requests(1).await;

    let lock_path = harness.runtime.tests.store.path().with_extension("lock");
    let (ready, held) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let locker = std::thread::spawn(move || {
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .expect("open the synthetic ModelTrace result lock");
        FileExt::lock_exclusive(&lock_file).expect("hold the synthetic progress-write lock");
        let _ = ready.send(());
        // The independent thread always drops the injected lock, even when a
        // blocked runtime cannot poll the test future or the assertion fails.
        let explicitly_released = released.recv_timeout(Duration::from_secs(5)).is_ok();
        drop(lock_file);
        explicitly_released
    });
    held.await.unwrap();
    let index = harness.state.captured()[0].sample_index;
    gate.release_group(index, 1);
    tokio::time::sleep(Duration::from_millis(80)).await;

    let runtime = harness.runtime.clone();
    let cancel_id = job_id.clone();
    let cancellation = tokio::spawn(async move { runtime.tests.cancel(&cancel_id).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(25)).await;
    let pending = !cancellation.is_finished();
    let _ = release.send(());
    let explicitly_released = locker.join().unwrap();
    let cancelled = tokio::time::timeout(Duration::from_secs(3), cancellation)
        .await
        .expect("cancel finishes after the pending progress write drains")
        .unwrap();
    assert!(
        explicitly_released,
        "the injected file lock must release before the watchdog"
    );
    assert!(
        pending,
        "cancel waits while the already-started synchronous progress write owns the job"
    );
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(
        harness.runtime.tests.get(&job_id).unwrap()["status"],
        "cancelled",
        "the drained progress write cannot replace the terminal cancellation"
    );
    assert!(
        cancelled["groups"].as_array().unwrap().iter().all(|group| {
            !matches!(
                group["status"].as_str(),
                Some("queued" | "running" | "receiving" | "retrying" | "analyzing")
            )
        }),
        "all uncompleted groups become terminal when the task is cancelled"
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_during_predispatch_write_sends_no_request_and_finishes_all_groups() {
    let harness = RuntimeHarness::new().await;
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let budget = harness
        .runtime
        .budget
        .clone()
        .acquire_many_owned(24)
        .await
        .unwrap();
    let started = harness.start_manual(account_id, 1).await;
    let job_id = started["id"].as_str().unwrap().to_owned();
    assert_eq!(started["status"], "queued");
    assert_eq!(harness.state.count(), 0);

    let lock_path = harness.runtime.tests.store.path().with_extension("lock");
    let (ready, held) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let locker = std::thread::spawn(move || {
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        FileExt::lock_exclusive(&lock_file).unwrap();
        let _ = ready.send(());
        let explicitly_released = released.recv_timeout(Duration::from_secs(5)).is_ok();
        drop(lock_file);
        explicitly_released
    });
    held.await.unwrap();
    drop(budget);
    // The worker's normal queue polling reaches its first durable dispatch
    // transition, while the independent process-style lock keeps it pending.
    tokio::time::sleep(Duration::from_millis(350)).await;
    let runtime = harness.runtime.clone();
    let cancel_id = job_id.clone();
    let cancellation = tokio::spawn(async move { runtime.tests.cancel(&cancel_id).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(25)).await;
    let pending = !cancellation.is_finished();
    let _ = release.send(());
    let explicitly_released = locker.join().unwrap();
    let cancelled = tokio::time::timeout(Duration::from_secs(3), cancellation)
        .await
        .unwrap()
        .unwrap();
    assert!(
        explicitly_released,
        "the injected write lock must drain before its watchdog"
    );
    assert!(pending, "cancellation waits for the pending dispatch write");
    assert_eq!(cancelled["status"], "cancelled");
    assert!(cancelled["groups"]
        .as_array()
        .unwrap()
        .iter()
        .all(|group| group["status"] == "cancelled"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        harness.state.count(),
        0,
        "a cancelled dispatch cannot send any upstream request"
    );
    assert_eq!(
        harness.runtime.tests.get(&job_id).unwrap()["status"],
        "cancelled"
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_text_progress_diagnostics_are_frozen_before_later_stream_events() {
    let harness = RuntimeHarness::new().await;
    let release = Arc::new(Semaphore::new(0));
    let gated = ResponsePlan::gated_sse(
        partial_delta("1,2,3"),
        completed_event("1,2,3"),
        release.clone(),
    );
    let replies = vec![
        vec![gated],
        vec![responses_sse("4,5,6")],
        vec![responses_sse("7,8,9")],
    ];
    harness
        .state
        .set_scenario(Scenario::with_replies(replies, None));
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let started = harness.start_manual(account_id, 1).await;
    let job_id = started["id"].as_str().unwrap().to_owned();
    harness.wait_for_requests(1).await;
    harness.wait_for_progress(&job_id, 1).await;
    let progress = harness.runtime.tests.get(&job_id).unwrap();
    let first_snapshot = progress["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["diagnostics"]["ttft_ms"].is_number())
        .expect("first event persisted with TTFT")
        .clone();
    assert_eq!(first_snapshot["diagnostics"]["events"], 1);
    assert_eq!(
        first_snapshot["diagnostics"]["last_event_type"],
        "response.output_text.delta"
    );

    release.add_permits(1);
    let finished = harness.wait_for_terminal(&job_id).await;
    let final_diagnostics = finished["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["index"] == 1)
        .unwrap();
    assert_eq!(final_diagnostics["diagnostics"]["events"], 2);
    assert_eq!(
        final_diagnostics["diagnostics"]["last_event_type"],
        "response.completed"
    );
    assert_eq!(first_snapshot["diagnostics"]["events"], 1);
    assert_eq!(
        first_snapshot["diagnostics"]["last_event_type"], "response.output_text.delta",
        "the earlier nested progress snapshot does not acquire later diagnostic fields"
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_drains_streams_releases_shared_credential_lock_and_prevents_late_writes() {
    let harness = RuntimeHarness::new().await;
    let fixture = runtime_fixtures()["modeltrace_runtime"]["below_99"].clone();
    let outputs = (0..3)
        .map(|index| output_from_fixture(&fixture, index))
        .collect::<Vec<_>>();
    let parent = harness.seed_account("apikey", None, json!({})).await;
    let first_child = harness
        .seed_account("apikey", Some(parent), json!({}))
        .await;
    let second_child = harness
        .seed_account("apikey", Some(parent), json!({}))
        .await;
    let release = Arc::new(Semaphore::new(0));
    harness.state.set_scenario(Scenario::with_replies(
        gated_replies(&outputs, release.clone()),
        None,
    ));

    let first = harness.start_manual(first_child, 3).await;
    let first_id = first["id"].as_str().unwrap().to_owned();
    harness.wait_for_requests(3).await;
    harness.wait_for_progress(&first_id, 3).await;
    let second = harness.start_manual(second_child, 3).await;
    let second_id = second["id"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        harness.state.count(),
        3,
        "siblings cannot overlap one parent's credentials"
    );
    assert_eq!(
        harness.runtime.tests.get(&second_id).unwrap()["status"],
        "queued"
    );

    let cancelled = harness.runtime.tests.cancel(&first_id).await.unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    release.add_permits(20);
    harness.wait_for_requests(6).await;
    harness.wait_for_progress(&second_id, 3).await;
    let second_job = harness.wait_for_terminal(&second_id).await;
    assert_eq!(second_job["status"], "completed");
    assert_eq!(harness.state.count(), 6);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        harness.runtime.tests.get(&first_id).unwrap()["status"],
        "cancelled",
        "late terminal frames from aborted streams must not overwrite the cancel state"
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_tests_allow_two_global_jobs_and_queue_the_third_independent_account() {
    let harness = RuntimeHarness::new().await;
    let fixture = runtime_fixtures()["modeltrace_runtime"]["below_99"].clone();
    let outputs = (0..3)
        .map(|index| output_from_fixture(&fixture, index))
        .collect::<Vec<_>>();
    let release = Arc::new(Semaphore::new(0));
    harness.state.set_scenario(Scenario::with_replies(
        gated_replies(&outputs, release.clone()),
        None,
    ));
    let first_account = harness.seed_account("apikey", None, json!({})).await;
    let second_account = harness.seed_account("apikey", None, json!({})).await;
    let third_account = harness.seed_account("apikey", None, json!({})).await;

    let first = harness.start_manual(first_account, 3).await;
    let first_id = first["id"].as_str().unwrap().to_owned();
    let second = harness.start_manual(second_account, 3).await;
    let second_id = second["id"].as_str().unwrap().to_owned();
    harness.wait_for_requests(6).await;
    harness.wait_for_progress(&first_id, 3).await;
    harness.wait_for_progress(&second_id, 3).await;

    let third = harness.start_manual(third_account, 3).await;
    let third_id = third["id"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        harness.state.count(),
        6,
        "the third independent job cannot exceed the global two-job cap"
    );
    assert_eq!(
        harness.runtime.tests.get(&third_id).unwrap()["status"],
        "queued"
    );

    let cancelled = harness.runtime.tests.cancel(&first_id).await.unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    release.add_permits(20);
    harness.wait_for_requests(9).await;
    let second_job = harness.wait_for_terminal(&second_id).await;
    let third_job = harness.wait_for_terminal(&third_id).await;
    assert_eq!(second_job["status"], "completed");
    assert_eq!(third_job["status"], "completed");
    assert_eq!(harness.state.count(), 9);
    assert_eq!(
        harness.runtime.tests.get(&first_id).unwrap()["status"],
        "cancelled"
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_schedulability_and_passive_quota_changes_do_not_change_the_live_target() {
    let harness = RuntimeHarness::new().await;
    let fixture = runtime_fixtures()["modeltrace_runtime"]["below_99"].clone();
    let outputs = (0..3)
        .map(|index| output_from_fixture(&fixture, index))
        .collect::<Vec<_>>();
    let release = Arc::new(Semaphore::new(0));
    harness.state.set_scenario(Scenario::with_replies(
        gated_replies(&outputs, release.clone()),
        None,
    ));
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let started = harness.start_manual(account_id, 1).await;
    let job_id = started["id"].as_str().unwrap().to_owned();
    harness.wait_for_requests(1).await;
    harness.wait_for_progress(&job_id, 1).await;

    sqlx::query("UPDATE accounts SET schedulable=false,extra=$1,updated_at=now() WHERE id=$2")
        .bind(SqlJson(
            json!({"codex_quota":{"used_percent":42},"quota_daily_used":50}),
        ))
        .bind(account_id)
        .execute(&harness.runtime.db.pool)
        .await
        .expect("update passive quota and schedule state in temporary PostgreSQL");
    release.add_permits(20);
    let job = harness.wait_for_terminal(&job_id).await;
    assert_eq!(harness.state.count(), 3);
    assert_eq!(job["status"], "completed");
    assert_eq!(job["attempts"], 3);
    assert_eq!(job["completed_groups"], 3);
    assert_report(
        &job["report"],
        &fixture["prefix_reports"][2],
        "manual target signature ignores schedulable and passive quota changes",
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_completed_outputs_are_not_retried_and_idempotency_binds_concurrency() {
    let harness = RuntimeHarness::new().await;
    harness
        .state
        .set_scenario(Scenario::single(responses_json("1 1")));
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let started = harness.start_manual(account_id, 2).await;
    let job_id = started["id"].as_str().unwrap().to_owned();
    let job = harness.wait_for_terminal(&job_id).await;
    assert_eq!(
        harness.state.count(),
        3,
        "invalid completed output is not re-dispatched"
    );
    assert_eq!(job["attempts"], 3);
    assert_eq!(job["completed_groups"], 3);
    assert_eq!(job["valid_groups"], 0);
    assert!(job["report"].is_null());
    assert_eq!(job["status"], "completed");

    let row = harness
        .runtime
        .actions
        .account(account_id, None)
        .await
        .unwrap();
    let conflict = harness
        .runtime
        .tests
        .start(
            account_id,
            &json!({
                "model_id":"gpt-6-luna",
                "expected_version":account_version(&row),
                "request_id":started["request_id"],
                "concurrency":1
            }),
            None,
        )
        .await
        .expect_err("idempotency key must bind the requested concurrency");
    assert_eq!(conflict.status.as_u16(), 409);
    assert_eq!(harness.state.count(), 3);
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_conflicted_job_can_be_cancelled_without_a_provider_request() {
    let harness = RuntimeHarness::new().await;
    let account_id = harness.seed_account("apikey", None, json!({})).await;
    let started = harness
        .runtime
        .tests
        .start(
            account_id,
            &json!({
                "model_id":"gpt-6-luna",
                "expected_version":"0".repeat(64),
                "request_id":uuid::Uuid::new_v4().simple().to_string(),
                "concurrency":1
            }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(started["status"], "needs_confirmation");
    let cancelled = harness
        .runtime
        .tests
        .cancel(started["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(harness.state.count(), 0);
    harness.close().await;
}

#[test]
fn offline_fingerprint_bank_cache_preserves_hash_schema_rate_state_and_snapshot() {
    let cached = tempfile::tempdir().unwrap();
    let path = cached.path().join("modeltrace-bank-state.json");
    let state = valid_bank_cache_state();
    fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let bank = FingerprintBank::new(bank_settings(cached.path(), true))
        .expect("load a pinned cache without network access");
    let public = bank.public();
    assert_eq!(public["source"], "cache");
    assert_eq!(public["status"], "idle");
    assert_eq!(
        public["version"]["revision"],
        runtime_fixtures()["modeltrace_runtime"]["bank_version"]["revision"]
    );
    assert_eq!(
        public["version"]["sha256"],
        sub2ops_server::compat::sha256(state["active"]["raw_bank"].as_str().unwrap())
    );
    let bundled = modeltrace::bundled().unwrap();
    assert_eq!(bank.snapshot().0, bundled.0);
    assert!(Arc::ptr_eq(&bank.snapshot(), &bank.snapshot()));
    let mut isolated = bank.snapshot().0.clone();
    isolated["models"][0]["id"] = "fixture-mutated-model".into();
    assert_eq!(bank.snapshot().0, bundled.0);
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "read-only startup leaves cache bytes untouched"
    );
    assert!(!cached.path().join("modeltrace-bank-state.lock").exists());

    let limited = tempfile::tempdir().unwrap();
    let limited_path = limited.path().join("modeltrace-bank-state.json");
    let mut limited_state = valid_bank_cache_state();
    limited_state["checked_at"] = 1000.0.into();
    limited_state["cooldown_until"] = 1600.0.into();
    limited_state["last_error"] = json!({"code":"rate-limit"});
    fs::write(&limited_path, serde_json::to_vec(&limited_state).unwrap()).unwrap();
    let limited_bank = FingerprintBank::new(bank_settings(limited.path(), true))
        .expect("load the persisted rate-limited cache without network access");
    let public = limited_bank.public();
    assert_eq!(public["error"], "rate-limit");
    assert_eq!(public["status"], "error");
    assert_eq!(public["cooldown_until"], 1600.0);

    let bad_hash = tempfile::tempdir().unwrap();
    let bad_hash_path = bad_hash.path().join("modeltrace-bank-state.json");
    let mut corrupt = valid_bank_cache_state();
    corrupt["active"]["version"]["sha256"] = "0".repeat(64).into();
    fs::write(&bad_hash_path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
    let error = FingerprintBank::new(bank_settings(bad_hash.path(), true))
        .err()
        .expect("cache hash mismatch must be rejected");
    assert_eq!(error.status.as_u16(), 503);
    assert_eq!(error.detail, "指纹库缓存损坏");

    let bad_schema = tempfile::tempdir().unwrap();
    let bad_schema_path = bad_schema.path().join("modeltrace-bank-state.json");
    let mut corrupt = valid_bank_cache_state();
    let mut raw: Value =
        serde_json::from_str(corrupt["active"]["raw_bank"].as_str().unwrap()).unwrap();
    raw["schema"] = "untrusted-fixture-schema".into();
    let raw = serde_json::to_string(&raw).unwrap();
    corrupt["active"]["raw_bank"] = raw.clone().into();
    corrupt["active"]["version"]["sha256"] = sub2ops_server::compat::sha256(&raw).into();
    fs::write(&bad_schema_path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
    let error = FingerprintBank::new(bank_settings(bad_schema.path(), true))
        .err()
        .expect("well-hashed but schema-invalid cache must be rejected");
    assert_eq!(error.status.as_u16(), 503);
    assert_eq!(error.detail, "指纹库数据无效");
}
