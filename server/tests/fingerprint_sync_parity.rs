use axum::http::{Response as HttpResponse, StatusCode};
use futures_util::future::BoxFuture;
use reqwest::{Response, Url};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use sub2ops_server::{
    compat::sha256,
    fingerprint_bank::{self, Failure, FingerprintBank, Transport},
    modeltrace,
    settings::Settings,
};
use tokio::sync::{Notify, Semaphore};

const SOURCE_COMMIT: &str = "a7edbadabec28fdf8b5cd8181764af80136a668a";
const COMMIT_PATH: &str = "/commits/hanmo";
const INFO_REFS_PATH: &str = "/info/refs?service=git-upload-pack";
const PINNED_CORE: &[u8] =
    include_bytes!("../../tests/contracts/modeltrace-source/fingerprint-core.js");
const PINNED_CHALLENGE: &[u8] =
    include_bytes!("../../tests/contracts/modeltrace-source/challenge-browser.js");
const PINNED_BANK: &[u8] = include_bytes!("../resources/modeltrace/unified_bank.json");

#[derive(Clone, Debug)]
struct Call {
    url: String,
    accept: String,
}

#[derive(Clone)]
enum Outcome {
    Http {
        status: StatusCode,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Failure {
        code: &'static str,
        retry_at: Option<f64>,
    },
}

#[derive(Clone)]
struct Plan {
    url_contains: String,
    outcome: Outcome,
}

impl Plan {
    fn json(url_contains: impl Into<String>, status: StatusCode, value: Value) -> Self {
        Self::http(
            url_contains,
            status,
            vec![("content-type".into(), "application/json".into())],
            value.to_string().into_bytes(),
        )
    }

    fn http(
        url_contains: impl Into<String>,
        status: StatusCode,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            url_contains: url_contains.into(),
            outcome: Outcome::Http {
                status,
                headers,
                body,
            },
        }
    }

    fn failure(url_contains: impl Into<String>, code: &'static str) -> Self {
        Self {
            url_contains: url_contains.into(),
            outcome: Outcome::Failure {
                code,
                retry_at: None,
            },
        }
    }
}

struct Gate {
    url_contains: String,
    entered: Arc<Notify>,
    release: Arc<Semaphore>,
    armed: bool,
}

#[derive(Clone)]
struct GateControl {
    entered: Arc<Notify>,
    release: Arc<Semaphore>,
}

struct QueueTransport {
    plans: Mutex<VecDeque<Plan>>,
    calls: Mutex<Vec<Call>>,
    gate: Mutex<Option<Gate>>,
}

impl QueueTransport {
    fn new(plans: impl IntoIterator<Item = Plan>) -> Arc<Self> {
        Arc::new(Self {
            plans: Mutex::new(plans.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
            gate: Mutex::new(None),
        })
    }

    fn append(&self, plans: impl IntoIterator<Item = Plan>) {
        self.plans.lock().unwrap().extend(plans);
    }

    fn block_first(&self, url_contains: &str) -> GateControl {
        let control = GateControl {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Semaphore::new(0)),
        };
        *self.gate.lock().unwrap() = Some(Gate {
            url_contains: url_contains.into(),
            entered: control.entered.clone(),
            release: control.release.clone(),
            armed: true,
        });
        control
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn count_matching(&self, needle: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.url.contains(needle))
            .count()
    }
}

impl Transport for QueueTransport {
    fn get<'a>(
        &'a self,
        url: Url,
        accept: &'a str,
    ) -> BoxFuture<'a, fingerprint_bank::SyncResult<Response>> {
        Box::pin(async move {
            let call = Call {
                url: url.to_string(),
                accept: accept.to_owned(),
            };
            let plan = self
                .plans
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected fingerprint request: {}", call.url));
            assert!(
                call.url.contains(&plan.url_contains),
                "request {} did not match next fixture {}",
                call.url,
                plan.url_contains
            );
            self.calls.lock().unwrap().push(call);

            let gate = {
                let mut gate = self.gate.lock().unwrap();
                gate.as_mut().and_then(|gate| {
                    if gate.armed && url.as_str().contains(&gate.url_contains) {
                        gate.armed = false;
                        Some((gate.entered.clone(), gate.release.clone()))
                    } else {
                        None
                    }
                })
            };
            if let Some((entered, release)) = gate {
                entered.notify_one();
                let permit = release.acquire_owned().await.map_err(|_| Failure {
                    code: "network",
                    retry_at: None,
                })?;
                drop(permit);
            }

            match plan.outcome {
                Outcome::Failure { code, retry_at } => Err(Failure { code, retry_at }),
                Outcome::Http {
                    status,
                    headers,
                    body,
                } => {
                    let mut response = HttpResponse::builder().status(status);
                    for (name, value) in headers {
                        response = response.header(name, value);
                    }
                    let response = response.body(body).expect("synthetic HTTP response");
                    Ok(Response::from(response))
                }
            }
        })
    }
}

#[derive(Clone)]
struct Clock(Arc<Mutex<f64>>);

impl Clock {
    fn new(now: f64) -> Self {
        Self(Arc::new(Mutex::new(now)))
    }

    fn callback(&self) -> Arc<dyn Fn() -> f64 + Send + Sync> {
        let clock = self.clone();
        Arc::new(move || *clock.0.lock().unwrap())
    }

    fn set(&self, now: f64) {
        *self.0.lock().unwrap() = now;
    }

    fn advance(&self, seconds: f64) {
        *self.0.lock().unwrap() += seconds;
    }
}

fn settings_for_data_directory(data: &Path) -> Arc<Settings> {
    Arc::new(Settings {
        database_url: "postgres://example.invalid/sub2ops".into(),
        bind: "127.0.0.1:0".into(),
        base_path: "/".into(),
        audit_path: data.join("audit.jsonl"),
        usage_path: data.join("oauth-state.json"),
        oauth_path: data.join("oauth-config.json"),
        bark_path: data.join("bark-config.json"),
        fallback_path: data.join("key-fallback.json"),
        connection_path: data.join("sub2api-config.json"),
        model_path: data.join("group-models.json"),
        keepalive_path: data.join("keepalive-state.json"),
        oauth: json!({}),
        bark: json!({}),
        bark_valid: false,
        base_url: String::new(),
        verify_url: String::new(),
        read_only: false,
    })
}

fn new_bank(directory: &Path, transport: Arc<dyn Transport>, clock: &Clock) -> FingerprintBank {
    new_bank_with_data_directory(&directory.join("data"), transport, clock)
}

fn new_bank_with_data_directory(
    data_directory: &Path,
    transport: Arc<dyn Transport>,
    clock: &Clock,
) -> FingerprintBank {
    FingerprintBank::with_transport(
        settings_for_data_directory(data_directory),
        transport,
        clock.callback(),
    )
    .expect("construct FingerprintBank with a synthetic transport and clock")
}

fn current_revision() -> String {
    modeltrace::bundled().unwrap().1["revision"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn http_plan(
    url_contains: impl Into<String>,
    status: StatusCode,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Plan {
    Plan::http(
        url_contains,
        status,
        headers
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect(),
        body,
    )
}

fn tree_payload(revision: &str, bank_blob: &str) -> Value {
    let manifest = modeltrace::manifest();
    let tree = ["core", "challenge", "bank"]
        .into_iter()
        .map(|name| {
            let file = &manifest["files"][name];
            json!({
                "path":file["path"],
                "type":"blob",
                "mode":"100644",
                "sha":if name == "bank" { json!(bank_blob) } else { file["gitBlob"].clone() }
            })
        })
        .collect::<Vec<_>>();
    json!({"sha":revision,"truncated":false,"tree":tree})
}

fn api_update_plans(
    revision: &str,
    raw_bank: &[u8],
    bank_blob_override: Option<&str>,
) -> Vec<Plan> {
    let manifest = modeltrace::manifest();
    let bank_blob = bank_blob_override
        .map(str::to_owned)
        .unwrap_or_else(|| fingerprint_bank::git_blob_digest(raw_bank));
    vec![
        Plan::json(COMMIT_PATH, StatusCode::OK, json!({"sha":revision})),
        Plan::json(
            format!("/git/trees/{revision}?recursive=1"),
            StatusCode::OK,
            tree_payload(revision, &bank_blob),
        ),
        Plan::http(
            format!(
                "/{revision}/{}",
                manifest["files"]["bank"]["path"].as_str().unwrap()
            ),
            StatusCode::OK,
            vec![("content-type".into(), "text/plain".into())],
            raw_bank.to_vec(),
        ),
    ]
}

fn commit_plan(revision: &str) -> Plan {
    Plan::json(COMMIT_PATH, StatusCode::OK, json!({"sha":revision}))
}

fn git_refs(revision: &str) -> Vec<u8> {
    let line = format!("{revision} refs/heads/hanmo\0report-status-v2 ofs-delta agent=fixture")
        .into_bytes();
    let length = line.len() + 4;
    let mut packet = format!("{length:04x}").into_bytes();
    packet.extend(line);
    packet.extend(b"0000");
    packet
}

fn fallback_plans(revision: &str, raw_bank: &[u8]) -> Vec<Plan> {
    vec![
        Plan::http(
            COMMIT_PATH,
            StatusCode::SERVICE_UNAVAILABLE,
            vec![],
            Vec::new(),
        ),
        Plan::http(
            COMMIT_PATH,
            StatusCode::SERVICE_UNAVAILABLE,
            vec![],
            Vec::new(),
        ),
        Plan::http(
            INFO_REFS_PATH,
            StatusCode::OK,
            vec![(
                "content-type".into(),
                "application/x-git-upload-pack-advertisement".into(),
            )],
            git_refs(revision),
        ),
        Plan::http(
            format!("/{revision}/static/fingerprint-core.js"),
            StatusCode::OK,
            vec![("content-type".into(), "text/plain".into())],
            PINNED_CORE.to_vec(),
        ),
        Plan::http(
            format!("/{revision}/static/challenge-browser.js"),
            StatusCode::OK,
            vec![("content-type".into(), "text/plain".into())],
            PINNED_CHALLENGE.to_vec(),
        ),
        Plan::http(
            format!("/{revision}/data/unified_bank.json"),
            StatusCode::OK,
            vec![("content-type".into(), "text/plain".into())],
            raw_bank.to_vec(),
        ),
    ]
}

fn non_finite_bank(value: &str) -> Vec<u8> {
    let (bank, _) = modeltrace::bundled().unwrap();
    let raw = serde_json::to_string(&bank).unwrap();
    let marker = "\"feature_mean\":[";
    let start = raw.find(marker).unwrap() + marker.len();
    let end = start
        + raw[start..]
            .find(',')
            .expect("feature mean contains an initial element");
    format!("{}{}{}", &raw[..start], value, &raw[end..]).into_bytes()
}

fn schema_variant(mutation: &str) -> Vec<u8> {
    let (mut bank, _) = modeltrace::bundled().unwrap();
    match mutation {
        "dimension" => {
            bank["robust"]["hellinger"]["feature_mean"]
                .as_array_mut()
                .unwrap()
                .pop();
        }
        "duplicate_model" => {
            let first = bank["models"][0]["id"].clone();
            bank["models"][1]["id"] = first;
        }
        "older_built_at" => bank["built_at"] = "2026-09-01T00:00:00+00:00".into(),
        other => panic!("unknown synthetic bank mutation {other}"),
    }
    serde_json::to_vec(&bank).unwrap()
}

async fn rejected_remote_bank(raw: Vec<u8>, expected: &str, tree_blob: Option<&str>) {
    let directory = tempfile::tempdir().unwrap();
    let clock = Clock::new(1000.0);
    let revision = "d".repeat(40);
    let transport = QueueTransport::new(api_update_plans(&revision, &raw, tree_blob));
    let bank = new_bank(directory.path(), transport.clone(), &clock);
    bank.sync(true).await;
    let public = bank.public();
    assert_eq!(public["error"], expected);
    let expected_status = if expected == "incompatible" {
        "upgrade-required"
    } else {
        "error"
    };
    assert_eq!(public["status"], expected_status);
    assert_eq!(public["result"], "failed");
    assert_eq!(bank.snapshot().1["revision"], current_revision());
    assert_eq!(transport.count(), 3);
}

#[test]
fn pinned_public_source_hashes_match_the_manifest_and_fixed_commit() {
    let manifest = modeltrace::manifest();
    assert_eq!(manifest["revision"], SOURCE_COMMIT);
    for (name, source, expected_sha) in [
        (
            "core",
            PINNED_CORE,
            "83fa5bd611e18f8339122582335123c8ea168ed242298bb31f4e363abeeb6e4a",
        ),
        (
            "challenge",
            PINNED_CHALLENGE,
            "4a3868c9fe237cabde92f0d2bffaf32118f558f7a43fcc336874fecd2bf6be39",
        ),
    ] {
        assert_eq!(sha256(source), expected_sha);
        assert_eq!(manifest["files"][name]["sha256"], expected_sha);
        assert_eq!(
            fingerprint_bank::git_blob_digest(source),
            manifest["files"][name]["gitBlob"]
        );
    }
}

#[tokio::test]
async fn remote_update_public_contract_hourly_gate_and_restart_cache_match_pinned_behavior() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Clock::new(1000.0);
    let revision = "b".repeat(40);
    let mut plans = api_update_plans(&revision, PINNED_BANK, None);
    plans.push(commit_plan(&revision));
    let transport = QueueTransport::new(plans);
    let bank = new_bank(directory.path(), transport.clone(), &clock);

    bank.sync(true).await;
    let public = bank.public();
    assert_eq!(public["source"], "remote");
    assert_eq!(public["status"], "idle");
    assert_eq!(public["result"], "updated");
    assert_eq!(public["error"], Value::Null);
    assert_eq!(public["checked_at"], 1000.0);
    assert_eq!(public["synced_at"], 1000.0);
    assert_eq!(public["cooldown_until"], Value::Null);
    assert_eq!(public["retrieval"], "api_tree");
    assert_eq!(public["version"]["revision"], revision);
    assert_eq!(transport.count(), 3);
    assert_eq!(
        transport
            .calls()
            .iter()
            .map(|call| call.accept.as_str())
            .collect::<Vec<_>>(),
        [
            "application/vnd.github+json",
            "application/vnd.github+json",
            "text/plain"
        ]
    );

    let snapshot = bank.snapshot();
    assert!(Arc::ptr_eq(&snapshot, &bank.snapshot()));
    let mut isolated = (*snapshot).clone();
    isolated.0["models"][0]["id"] = "fixture-mutated-model".into();
    isolated.1["revision"] = "fixture-mutated-revision".into();
    assert_ne!(
        bank.snapshot().0["models"][0]["id"],
        "fixture-mutated-model"
    );
    assert_eq!(snapshot.1["revision"], revision);
    assert_eq!(bank.snapshot().1["revision"], revision);
    let mut public_copy = bank.public();
    public_copy["version"]["revision"] = "fixture-public-mutation".into();
    assert_eq!(bank.public()["version"]["revision"], revision);

    clock.advance(3599.0);
    bank.sync(false).await;
    assert_eq!(transport.count(), 3, "the hourly check remains gated");
    clock.advance(1.0);
    bank.sync(false).await;
    assert_eq!(transport.count(), 4);
    assert_eq!(transport.count_matching(COMMIT_PATH), 2);
    assert_eq!(transport.count_matching("/git/trees/"), 1);
    assert_eq!(
        transport.count_matching("/data/unified_bank.json"),
        1,
        "same revision only refreshes the commit head"
    );
    assert_eq!(bank.public()["result"], "up-to-date");
    assert_eq!(bank.public()["checked_at"], 4600.0);

    let restarted_transport = QueueTransport::new([]);
    let restarted = new_bank(directory.path(), restarted_transport.clone(), &clock);
    let cached = restarted.public();
    assert_eq!(cached["source"], "cache");
    assert_eq!(cached["status"], "idle");
    assert_eq!(cached["error"], Value::Null);
    assert_eq!(cached["version"]["revision"], revision);
    assert_eq!(restarted.snapshot().0, modeltrace::bundled().unwrap().0);
    assert_eq!(restarted_transport.count(), 0);
}

#[tokio::test]
async fn failed_remote_update_after_restart_keeps_the_persisted_last_good_bank() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Clock::new(1000.0);
    let revision = "b".repeat(40);
    let transport = QueueTransport::new(api_update_plans(&revision, PINNED_BANK, None));
    let bank = new_bank(directory.path(), transport.clone(), &clock);
    bank.sync(true).await;
    let saved = bank.snapshot();
    assert_eq!(saved.1["revision"], revision);
    assert_eq!(transport.count(), 3);
    clock.advance(3600.0);
    let failures = QueueTransport::new([
        Plan::failure(COMMIT_PATH, "network"),
        Plan::failure(COMMIT_PATH, "network"),
        Plan::failure(INFO_REFS_PATH, "network"),
        Plan::failure(INFO_REFS_PATH, "network"),
    ]);
    let restarted = new_bank(directory.path(), failures.clone(), &clock);
    assert_eq!(restarted.snapshot().1, saved.1);
    restarted.sync(true).await;
    assert_eq!(failures.count(), 4);
    assert_eq!(restarted.public()["error"], "network");
    assert_eq!(restarted.snapshot().1, saved.1);
    assert_eq!(restarted.snapshot().0, saved.0);
}

#[tokio::test]
async fn network_and_timeout_retry_once_then_apply_five_minute_cooldown() {
    for code in ["network", "timeout"] {
        let directory = tempfile::tempdir().unwrap();
        let clock = Clock::new(1000.0);
        let mut plans = vec![
            Plan::failure(COMMIT_PATH, code),
            Plan::failure(COMMIT_PATH, code),
            Plan::failure(INFO_REFS_PATH, code),
            Plan::failure(INFO_REFS_PATH, code),
        ];
        let transport = QueueTransport::new(std::mem::take(&mut plans));
        let bank = new_bank(directory.path(), transport.clone(), &clock);

        bank.sync(true).await;
        assert_eq!(
            transport.count(),
            4,
            "{code} performs one retry per endpoint"
        );
        assert_eq!(transport.count_matching(COMMIT_PATH), 2);
        assert_eq!(transport.count_matching(INFO_REFS_PATH), 2);
        assert_eq!(bank.public()["error"], code);
        assert_eq!(bank.public()["status"], "error");
        assert_eq!(bank.public()["cooldown_until"], 1300.0);

        clock.set(1299.0);
        bank.sync(false).await;
        assert_eq!(transport.count(), 4, "{code} cooldown blocks early checks");
        transport.append([commit_plan(&current_revision())]);
        clock.set(1300.0);
        bank.sync(false).await;
        assert_eq!(transport.count(), 5);
        assert_eq!(bank.public()["result"], "up-to-date");
        assert_eq!(bank.public()["cooldown_until"], Value::Null);
    }
}

#[tokio::test]
async fn retry_after_and_rate_reset_cooldowns_gate_manual_fallback_checks() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Clock::new(1000.0);
    let revision = current_revision();
    let transport = QueueTransport::new([
        http_plan(
            COMMIT_PATH,
            StatusCode::TOO_MANY_REQUESTS,
            &[("retry-after", "600")],
            Vec::new(),
        ),
        http_plan(
            INFO_REFS_PATH,
            StatusCode::TOO_MANY_REQUESTS,
            &[("retry-after", "600")],
            Vec::new(),
        ),
    ]);
    let bank = new_bank(directory.path(), transport.clone(), &clock);
    bank.sync(true).await;
    assert_eq!(bank.public()["error"], "rate-limit");
    assert_eq!(bank.public()["cooldown_until"], 1600.0);
    assert_eq!(transport.count(), 2);

    clock.set(1599.0);
    bank.sync(true).await;
    assert_eq!(transport.count(), 2, "force cannot bypass Retry-After");
    transport.append([commit_plan(&revision)]);
    clock.set(1600.0);
    bank.sync(true).await;
    assert_eq!(transport.count(), 3);
    assert_eq!(bank.public()["result"], "up-to-date");
    assert_eq!(bank.public()["cooldown_until"], Value::Null);

    let reset_dir = tempfile::tempdir().unwrap();
    let reset_clock = Clock::new(1000.0);
    let reset_transport = QueueTransport::new([
        http_plan(
            COMMIT_PATH,
            StatusCode::FORBIDDEN,
            &[("x-ratelimit-reset", "2200")],
            Vec::new(),
        ),
        http_plan(
            INFO_REFS_PATH,
            StatusCode::FORBIDDEN,
            &[("x-ratelimit-reset", "2200")],
            Vec::new(),
        ),
    ]);
    let reset_bank = new_bank(reset_dir.path(), reset_transport.clone(), &reset_clock);
    reset_bank.sync(true).await;
    assert_eq!(reset_bank.public()["error"], "rate-limit");
    assert_eq!(reset_bank.public()["cooldown_until"], 2200.0);
    assert_eq!(reset_transport.count(), 2);
}

#[tokio::test]
async fn successful_git_refs_fallback_downloads_only_fixed_hashed_sources() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Clock::new(1000.0);
    let revision = "c".repeat(40);
    let transport = QueueTransport::new(fallback_plans(&revision, PINNED_BANK));
    let bank = new_bank(directory.path(), transport.clone(), &clock);

    bank.sync(true).await;
    let public = bank.public();
    assert_eq!(public["source"], "remote");
    assert_eq!(public["status"], "idle");
    assert_eq!(public["result"], "updated");
    assert_eq!(public["retrieval"], "git_refs");
    assert_eq!(public["version"]["revision"], revision);
    assert_eq!(transport.count(), 6);
    assert_eq!(transport.count_matching(COMMIT_PATH), 2);
    assert_eq!(transport.count_matching(INFO_REFS_PATH), 1);
    assert_eq!(transport.count_matching("/git/trees/"), 0);
    assert_eq!(transport.count_matching("raw.githubusercontent.com"), 3);
    assert_eq!(
        sha256(PINNED_CORE),
        modeltrace::manifest()["files"]["core"]["sha256"]
    );
    assert_eq!(
        sha256(PINNED_CHALLENGE),
        modeltrace::manifest()["files"]["challenge"]["sha256"]
    );
}

#[tokio::test]
async fn rate_limited_same_revision_fallback_preserves_manual_check_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Clock::new(1000.0);
    let revision = current_revision();
    let transport = QueueTransport::new([
        http_plan(
            COMMIT_PATH,
            StatusCode::TOO_MANY_REQUESTS,
            &[("retry-after", "600")],
            Vec::new(),
        ),
        Plan::http(INFO_REFS_PATH, StatusCode::OK, vec![], git_refs(&revision)),
    ]);
    let bank = new_bank(directory.path(), transport.clone(), &clock);
    bank.sync(true).await;
    assert_eq!(bank.public()["result"], "up-to-date");
    assert_eq!(bank.public()["cooldown_until"], 1600.0);
    assert_eq!(transport.count(), 2);

    clock.set(1599.0);
    bank.sync(true).await;
    assert_eq!(transport.count(), 2);
    transport.append([commit_plan(&revision)]);
    clock.set(1600.0);
    bank.sync(true).await;
    assert_eq!(transport.count(), 3);
    assert_eq!(bank.public()["result"], "up-to-date");
    assert_eq!(bank.public()["cooldown_until"], Value::Null);
}

#[tokio::test]
async fn malformed_nonfinite_dimension_duplicate_and_old_banks_are_rejected() {
    rejected_remote_bank(b"{".to_vec(), "invalid-data", None).await;
    rejected_remote_bank(non_finite_bank("NaN"), "invalid-data", None).await;
    rejected_remote_bank(non_finite_bank("Infinity"), "invalid-data", None).await;
    rejected_remote_bank(schema_variant("dimension"), "invalid-data", None).await;
    rejected_remote_bank(schema_variant("duplicate_model"), "invalid-data", None).await;
    rejected_remote_bank(schema_variant("older_built_at"), "incompatible", None).await;
}

#[tokio::test]
async fn remote_bank_git_blob_mismatch_is_invalid_data() {
    rejected_remote_bank(PINNED_BANK.to_vec(), "invalid-data", Some(&"0".repeat(40))).await;
}

#[tokio::test]
async fn cache_persist_failure_keeps_the_last_good_active_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Clock::new(1000.0);
    let first_revision = "b".repeat(40);
    let second_revision = "c".repeat(40);
    let mut plans = api_update_plans(&first_revision, PINNED_BANK, None);
    plans.extend(api_update_plans(&second_revision, PINNED_BANK, None));
    let transport = QueueTransport::new(plans);
    let active = new_bank(directory.path(), transport.clone(), &clock);
    active.sync(true).await;
    let last_good = active.snapshot();
    assert_eq!(last_good.1["revision"], first_revision);

    let data_dir = directory.path().join("data");
    let saved_dir = directory.path().join("saved-state");
    fs::rename(&data_dir, &saved_dir).expect("move the synthetic cache directory");
    fs::write(&data_dir, b"fixture directory blocker").expect("block cache persistence path");
    active.sync(true).await;
    assert_eq!(transport.count(), 6);
    assert_eq!(active.public()["error"], "cache");
    assert_eq!(active.public()["status"], "error");
    assert_eq!(active.public()["cooldown_until"], 1300.0);
    assert!(Arc::ptr_eq(&last_good, &active.snapshot()));

    let restarted = new_bank_with_data_directory(&saved_dir, QueueTransport::new([]), &clock);
    assert_eq!(restarted.public()["source"], "cache");
    assert_eq!(restarted.public()["version"]["revision"], first_revision);
    assert_eq!(restarted.snapshot().1["revision"], first_revision);
}

#[tokio::test]
async fn concurrent_sync_calls_share_one_check_and_publish_checking_state() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Clock::new(1000.0);
    let revision = "b".repeat(40);
    let transport = QueueTransport::new(api_update_plans(&revision, PINNED_BANK, None));
    let gate = transport.block_first(COMMIT_PATH);
    let bank = Arc::new(new_bank(directory.path(), transport.clone(), &clock));

    let entered = gate.entered.notified();
    let owner_bank = bank.clone();
    let owner = tokio::spawn(async move { owner_bank.sync(true).await });
    tokio::time::timeout(Duration::from_secs(2), entered)
        .await
        .expect("first sync reached the synthetic transport gate");
    assert_eq!(bank.public()["status"], "checking");
    assert_eq!(transport.count(), 1);

    bank.sync(true).await;
    assert_eq!(
        transport.count(),
        1,
        "concurrent caller joins the active check"
    );
    gate.release.add_permits(1);
    owner.await.unwrap();
    assert_eq!(transport.count(), 3);
    assert_eq!(bank.public()["status"], "idle");
    assert_eq!(bank.public()["result"], "updated");
}
