use axum::{extract::State, http::StatusCode, routing::get, Router};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use sub2ops_server::{
    account_locks::AccountLocks,
    account_templates::Templates,
    actions::Actions,
    capacity_alerts::CapacityStore,
    compat,
    db::{self, Database},
    execution::DiskLease,
    oauth_queries::{Coordinator, QueryOptions, Source},
    oauth_state::OAuthState,
    queries::Queries,
    runtime::Runtime,
    settings::Settings,
    store::Store,
    tasks::Tasks,
    upstream::Upstream,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{oneshot, Notify, Semaphore},
};

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../tests/contracts/state/fixtures.json"))
        .expect("pinned Python state fixture must parse")
}

fn contract_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("STATE_LIFECYCLE_CONTRACT_DIR")
            .expect("state contract runner must supply its owned output directory"),
    )
}

fn database_url() -> String {
    std::env::var("DATABASE_URL").expect("state contract runner must provide temporary PostgreSQL")
}

fn settings(root: &Path, database_url: String, read_only: bool) -> Settings {
    Settings {
        database_url,
        bind: "127.0.0.1:0".into(),
        base_path: String::new(),
        audit_path: root.join("audit.jsonl"),
        usage_path: root.join("usage-query-state.json"),
        oauth_path: root.join("oauth-config.json"),
        bark_path: root.join("bark-config.json"),
        fallback_path: root.join("key-fallback-config.json"),
        connection_path: root.join("sub2api-config.json"),
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
        base_url: String::new(),
        verify_url: String::new(),
        read_only,
    }
}

#[test]
fn store_preserves_permissions_identity_and_atomic_failure_invariants() {
    let temporary = tempfile::tempdir().expect("create owned Store fixture directory");
    let expected = fixture()["store"].clone();
    let path = temporary.path().join("state.json");
    let store = Store::policy(&path, json!({"payload":{"fixture":true}}), false);

    store
        .transaction(|_| Ok(()))
        .expect("first empty transaction must create a valid state file");
    let first = fs::metadata(&path).expect("first transaction wrote state");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
        json!({"payload":{"fixture":true},"version":1})
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let expected_state_mode =
            u32::from_str_radix(expected["new_state_mode_octal"].as_str().unwrap(), 8).unwrap();
        let expected_lock_mode =
            u32::from_str_radix(expected["new_lock_mode_octal"].as_str().unwrap(), 8).unwrap();
        assert_eq!(first.mode() & 0o777, expected_state_mode, "new state mode");
        let lock = fs::metadata(path.with_extension("lock")).expect("persistent lock exists");
        assert_eq!(lock.mode() & 0o777, expected_lock_mode, "new lock mode");
    }

    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(expected["no_change_preserves_inode_and_mtime"], true);
    store
        .transaction(|_| Ok(()))
        .expect("no-change transaction remains successful");
    let unchanged = fs::metadata(&path).expect("state remains present");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            first.ino(),
            unchanged.ino(),
            "no-change must not replace inode"
        );
    }
    assert_eq!(
        first.modified().unwrap(),
        unchanged.modified().unwrap(),
        "no-change must preserve mtime"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // The pinned Python fixture records its historical 0640 mode-preservation
        // behavior. Rust 0.1.43 intentionally tightens an existing state file to
        // 0600 whenever a successful write replaces it.
        let python_existing_mode = u32::from_str_radix(
            expected["preserved_existing_mode_octal"].as_str().unwrap(),
            8,
        )
        .unwrap();
        assert_eq!(
            python_existing_mode, 0o640,
            "pinned Python history stays fixed"
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(python_existing_mode)).unwrap();
        store
            .transaction(|value| {
                value["payload"]["changed"] = true.into();
                Ok(())
            })
            .expect("changed transaction atomically replaces state");
        let changed = fs::metadata(&path).unwrap();
        assert_eq!(
            changed.mode() & 0o777,
            0o600,
            "0.1.43 tightens legacy 0640 state to private mode"
        );
        assert_ne!(
            first.ino(),
            changed.ino(),
            "changed state is replaced atomically"
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        store
            .transaction(|value| {
                value["payload"]["second_change"] = true.into();
                Ok(())
            })
            .expect("changed transaction tightens a legacy 0644 state file");
        assert_eq!(
            fs::metadata(&path).unwrap().mode() & 0o777,
            0o600,
            "existing 0644 state converges to private mode"
        );
    }

    let target_dir = temporary.path().join("atomic-target");
    fs::create_dir(&target_dir).unwrap();
    let marker = target_dir.join("fixture-marker");
    fs::write(&marker, b"original fixture bytes").unwrap();
    assert_eq!(
        expected["atomic_failure_preserves_target_and_leaves_no_tempfile"],
        true
    );
    assert!(
        sub2ops_server::store::write_json(&target_dir, &json!({"replacement":true})).is_err(),
        "persisting over a non-empty directory must fail"
    );
    assert_eq!(fs::read(&marker).unwrap(), b"original fixture bytes");
    assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".sub2ops-state-")
    }));
}

#[cfg(unix)]
#[test]
fn existing_lock_lease_audit_and_capacity_account_files_converge_to_private_modes() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().mode() & 0o777
    }

    let temporary = tempfile::tempdir().expect("create owned permission fixture directory");
    let root = temporary.path();

    let state_path = root.join("store-state.json");
    let store = Store::policy(&state_path, json!({"payload":{}}), false);
    store
        .transaction(|_| Ok(()))
        .expect("create synthetic Store state and lock");
    let store_lock_path = state_path.with_extension("lock");
    fs::set_permissions(&store_lock_path, fs::Permissions::from_mode(0o644)).unwrap();
    store
        .transaction(|state| {
            state["payload"]["lock_repaired"] = true.into();
            Ok(())
        })
        .expect("write while converging the existing Store lock");
    assert_eq!(mode(&store_lock_path), 0o600, "existing Store lock");

    let lease_state = root.join("lease-state.json");
    let lease_path = PathBuf::from(format!("{}.fixture", lease_state.display()));
    fs::write(&lease_path, b"synthetic lease fixture").unwrap();
    fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o644)).unwrap();
    let held_lease = DiskLease::try_acquire(&lease_state, "fixture", false)
        .expect("acquire and restrict the existing lease file");
    assert_eq!(mode(&lease_path), 0o600, "existing DiskLease");
    assert!(
        DiskLease::try_acquire(&lease_state, "fixture", false).is_err(),
        "a second instance cannot acquire the held disk lease"
    );
    drop(held_lease);
    let released_lease = DiskLease::try_acquire(&lease_state, "fixture", false)
        .expect("dropping the instance lease releases its exclusive lock");
    drop(released_lease);

    let audit_path = root.join("audit.jsonl");
    fs::write(&audit_path, b"{}\n").unwrap();
    fs::set_permissions(&audit_path, fs::Permissions::from_mode(0o644)).unwrap();
    sub2ops_server::audit::write(&audit_path, "fixture_event", json!({"fixture":true}), false);
    assert_eq!(mode(&audit_path), 0o600, "existing audit append target");
    assert_eq!(
        fs::read_to_string(&audit_path).unwrap().lines().count(),
        2,
        "audit hardening preserves append behavior"
    );

    let capacity_lock_dir = root.join("capacity-alert-locks");
    fs::create_dir_all(&capacity_lock_dir).unwrap();
    let capacity_lock_path = capacity_lock_dir.join("71001.lock");
    fs::write(&capacity_lock_path, b"synthetic account lock fixture").unwrap();
    fs::set_permissions(&capacity_lock_path, fs::Permissions::from_mode(0o644)).unwrap();
    let capacity = CapacityStore::new(root.join("capacity-state.json"), false);
    let guard = capacity
        .account_guard(71001)
        .expect("acquire and restrict the existing account guard file");
    assert_eq!(mode(&capacity_lock_path), 0o600, "existing account guard");
    drop(guard);
}

#[test]
fn read_only_store_does_not_create_parent_lock_or_state() {
    let temporary = tempfile::tempdir().expect("create owned Store fixture directory");
    let absent_parent = temporary.path().join("not-created");
    let path = absent_parent.join("state.json");
    assert_eq!(
        fixture()["store"]["read_only_missing_state_creates_no_parent_lock_or_state"],
        true
    );
    let store = Store::policy(&path, json!({"fixture":true}), true);
    assert_eq!(store.read().unwrap(), json!({"fixture":true,"version":1}));
    assert!(store.transaction(|_| Ok(())).is_err());
    assert!(!absent_parent.exists());
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 0);
}

#[test]
fn pinned_python_state_is_readable_and_rust_state_is_old_python_compatible() {
    let root = contract_dir().join("cross-language");
    fs::create_dir_all(&root).expect("create runner-owned cross-language directory");
    let reference = fixture()["cross_language"].clone();
    let python_path = root.join("python-state.json");
    let python_state = Store::policy(&python_path, json!({}), true)
        .read()
        .expect("Rust reads state emitted by pinned Python 6be");
    assert_eq!(python_state, reference["python_input"]);

    let rust_path = root.join("rust-state.json");
    let rust_store = Store::policy(&rust_path, json!({}), false);
    rust_store
        .transaction(|state| {
            *state = reference["rust_output"].clone();
            Ok(())
        })
        .expect("Rust writes state for the pinned Python reader");
    assert_eq!(rust_store.read().unwrap(), reference["rust_output"]);
}

#[tokio::test]
async fn postgres_numeric_wire_scale_and_python_digest_match() {
    let database = Database::open(&database_url(), false)
        .await
        .expect("connect to runner-owned PostgreSQL");
    let values = fixture()["numeric"]["amounts"].as_array().unwrap().clone();
    let expressions = values
        .iter()
        .enumerate()
        .map(|(index, amount)| {
            let literal = amount["sql_literal"].as_str().unwrap();
            assert!(literal.bytes().all(|byte| {
                byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'E')
            }));
            format!("'{literal}'::numeric AS amount_{index}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let row = sqlx::query(&format!("SELECT {expressions}"))
        .fetch_one(&database.pool)
        .await
        .expect("fetch synthetic PostgreSQL NUMERIC values over SQLx wire protocol");
    let row = db::row_value(row).expect("convert PostgreSQL row through production boundary");
    let actual = values
        .iter()
        .enumerate()
        .map(|(index, amount)| {
            let field = format!("amount_{index}");
            let value = row
                .get(field.as_str())
                .unwrap()
                .as_str()
                .expect("NUMERIC converts to a Python-compatible string");
            assert_eq!(
                value,
                amount["expected"].as_str().unwrap(),
                "NUMERIC {index}"
            );
            value.to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        compat::digest(&json!({"amounts":actual})),
        fixture()["numeric"]["python_operation_digest"]
            .as_str()
            .unwrap(),
        "Rust digest must match the pinned Python Decimal/default=str digest"
    );
    database.close().await;
}

#[derive(Clone)]
struct GateState {
    started: Arc<Notify>,
    release: Arc<Semaphore>,
    calls: Arc<AtomicUsize>,
}

async fn gated_upstream(State(gate): State<GateState>) -> StatusCode {
    gate.calls.fetch_add(1, Ordering::SeqCst);
    gate.started.notify_one();
    gate.release
        .acquire()
        .await
        .expect("fixture gate remains open")
        .forget();
    StatusCode::OK
}

#[derive(Clone)]
struct BackgroundRequest {
    tasks: Tasks,
    target: String,
    done: Arc<StdMutex<Option<oneshot::Sender<()>>>>,
}

async fn spawn_background_http(State(state): State<BackgroundRequest>) -> StatusCode {
    let target = state.target.clone();
    let done = state
        .done
        .lock()
        .unwrap()
        .take()
        .expect("background completion sender is used once");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build loopback-only test client");
    state
        .tasks
        .spawn(async move {
            if client.get(target).send().await.is_ok() {
                let _ = done.send(());
            }
        })
        .expect("service accepts request before shutdown");
    StatusCode::ACCEPTED
}

#[tokio::test]
async fn owned_http_request_finishes_after_inbound_client_disconnect() {
    let gate = GateState {
        started: Arc::new(Notify::new()),
        release: Arc::new(Semaphore::new(0)),
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let upstream_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream_listener.local_addr().unwrap();
    let upstream_state = gate.clone();
    let upstream_server = tokio::spawn(async move {
        let app = Router::new()
            .route("/pending", get(gated_upstream))
            .with_state(upstream_state);
        let _ = axum::serve(upstream_listener, app).await;
    });

    let tasks = Tasks::default();
    let (done_tx, mut done_rx) = oneshot::channel();
    let request_state = BackgroundRequest {
        tasks: tasks.clone(),
        target: format!("http://{upstream_address}/pending"),
        done: Arc::new(StdMutex::new(Some(done_tx))),
    };
    let inbound_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let inbound_address = inbound_listener.local_addr().unwrap();
    let inbound_server = tokio::spawn(async move {
        let app = Router::new()
            .route("/background", get(spawn_background_http))
            .with_state(request_state);
        let _ = axum::serve(inbound_listener, app).await;
    });

    let mut client = TcpStream::connect(inbound_address).await.unwrap();
    client
        .write_all(b"GET /background HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(3), gate.started.notified())
        .await
        .expect("owned request reached loopback upstream before client socket closed");
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
    drop(client);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut done_rx)
            .await
            .is_err(),
        "upstream gate keeps the owned request in flight"
    );
    gate.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), &mut done_rx)
        .await
        .expect("owned request completes after upstream release")
        .expect("background completion signal is delivered");

    tasks.close(1).await;
    inbound_server.abort();
    upstream_server.abort();
    let _ = inbound_server.await;
    let _ = upstream_server.await;
}

#[tokio::test]
async fn tasks_close_zero_and_one_abort_release_permits_and_reject_new_work() {
    for seconds in fixture()["tasks"]["close_timeouts_seconds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_u64().unwrap())
    {
        let tasks = Tasks::default();
        let resources = Arc::new(Semaphore::new(1));
        let permit = resources.clone().acquire_owned().await.unwrap();
        let started = Arc::new(Notify::new());
        let task_started = started.clone();
        tasks
            .spawn(async move {
                let _permit = permit;
                task_started.notify_one();
                std::future::pending::<()>().await;
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("owned task acquired resource");
        assert_eq!(resources.available_permits(), 0);
        tasks.close(seconds).await;
        assert!(tasks.is_closed());
        assert_eq!(
            resources.available_permits(),
            1,
            "aborted task releases permit"
        );
        assert!(
            tasks.spawn(async {}).is_err(),
            "closed collection rejects new work"
        );
    }
}

struct NotifyOnDrop(Option<oneshot::Sender<()>>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[tokio::test]
async fn coordinator_drain_closes_aborts_owned_query_and_releases_state_lock() {
    let temporary = tempfile::tempdir().expect("create owned coordinator directory");
    let database = Database::open(&database_url(), false)
        .await
        .expect("connect to runner-owned PostgreSQL");
    let settings = Arc::new(settings(temporary.path(), database_url(), false));
    let oauth = OAuthState::new(settings.usage_path.clone(), false);
    let queries = Queries {
        db: database.clone(),
        oauth: oauth.clone(),
    };
    let upstream = Upstream::new(settings.clone()).unwrap();
    let actions = Actions::new(
        queries,
        upstream,
        AccountLocks::default(),
        Templates::new(settings.model_path.clone(), false),
        settings,
    )
    .unwrap();
    let coordinator = Coordinator::new(actions, Arc::new(|| 2));
    let row = json!({"id":71998,"platform":"openai","type":"oauth","credentials":{}});
    let started = Arc::new(Notify::new());
    let called = Arc::new(AtomicUsize::new(0));
    let called_first = called.clone();
    let started_first = started.clone();
    let (drop_tx, mut drop_rx) = oneshot::channel();
    let call_coordinator = coordinator.clone();
    let in_flight = tokio::spawn(async move {
        call_coordinator
            .external_read(
                row.clone(),
                QueryOptions {
                    source: Source::Manual,
                    reason: "state-lifecycle-fixture".into(),
                    ..QueryOptions::default()
                },
                false,
                1,
                |_| true,
                move || {
                    called_first.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let _drop_notice = NotifyOnDrop(Some(drop_tx));
                        started_first.notify_one();
                        std::future::pending::<Value>().await
                    }
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("owned external query reached its pending operation");
    assert_eq!(called.load(Ordering::SeqCst), 1);

    let duplicate_count = Arc::new(AtomicUsize::new(0));
    let duplicate_count_for_op = duplicate_count.clone();
    let duplicate = coordinator
        .external_read(
            json!({"id":71998,"platform":"openai","type":"oauth","credentials":{}}),
            QueryOptions {
                source: Source::Manual,
                reason: "duplicate-fixture".into(),
                ..QueryOptions::default()
            },
            false,
            1,
            |_| true,
            move || async move {
                duplicate_count_for_op.fetch_add(1, Ordering::SeqCst);
                json!({"success":true})
            },
        )
        .await;
    assert_eq!(
        duplicate["error_code"],
        fixture()["coordinator"]["pending_duplicate_error_code"]
    );
    assert_eq!(
        duplicate_count.load(Ordering::SeqCst),
        0,
        "duplicate was not dispatched"
    );

    coordinator.drain(0).await;
    tokio::time::timeout(Duration::from_secs(1), &mut drop_rx)
        .await
        .expect("drain abort drops the owned query future")
        .expect("owned query drop signal delivered");
    let uncertain = tokio::time::timeout(Duration::from_secs(1), in_flight)
        .await
        .expect("aborted owned task releases external caller")
        .expect("external query waiter joined");
    assert_eq!(uncertain["error_code"], "result_uncertain");

    let rejected_count = Arc::new(AtomicUsize::new(0));
    let rejected_count_for_op = rejected_count.clone();
    let rejected = tokio::time::timeout(
        Duration::from_secs(1),
        coordinator.external_read(
            json!({"id":71997,"platform":"openai","type":"oauth","credentials":{}}),
            QueryOptions {
                source: Source::Manual,
                reason: "after-drain-fixture".into(),
                ..QueryOptions::default()
            },
            false,
            1,
            |_| true,
            move || async move {
                rejected_count_for_op.fetch_add(1, Ordering::SeqCst);
                json!({"success":true})
            },
        ),
    )
    .await
    .expect("closed Coordinator mutex remains available");
    assert_eq!(
        rejected["error_code"],
        fixture()["coordinator"]["drain_rejects_new_work_error_code"]
    );
    assert_eq!(rejected_count.load(Ordering::SeqCst), 0);
    oauth
        .transaction(|state| {
            state["settings"]["after_drain_fixture"] = true.into();
            Ok(())
        })
        .expect("OAuth state lock remains available after query abort");
    database.close().await;
}

#[derive(Debug, PartialEq, Eq)]
struct DiskEntry {
    directory: bool,
    bytes: Option<Vec<u8>>,
    modified: Option<SystemTime>,
    inode: u64,
    mode: u32,
}

#[cfg(unix)]
fn metadata_identity(metadata: &fs::Metadata) -> (u64, u32) {
    use std::os::unix::fs::MetadataExt;
    (metadata.ino(), metadata.mode() & 0o777)
}

#[cfg(not(unix))]
fn metadata_identity(_metadata: &fs::Metadata) -> (u64, u32) {
    (0, 0)
}

fn disk_snapshot(root: &Path) -> std::io::Result<std::collections::BTreeMap<PathBuf, DiskEntry>> {
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut std::collections::BTreeMap<PathBuf, DiskEntry>,
    ) -> std::io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            let is_directory = metadata.is_dir();
            let (inode, mode) = metadata_identity(&metadata);
            let relative = path.strip_prefix(root).unwrap().to_owned();
            entries.insert(
                relative,
                DiskEntry {
                    directory: is_directory,
                    bytes: if is_directory {
                        None
                    } else {
                        Some(fs::read(&path)?)
                    },
                    modified: metadata.modified().ok(),
                    inode,
                    mode,
                },
            );
            if is_directory {
                visit(root, &path, entries)?;
            }
        }
        Ok(())
    }
    let mut entries = std::collections::BTreeMap::new();
    visit(root, root, &mut entries)?;
    Ok(entries)
}

#[tokio::test]
async fn readonly_runtime_preflight_and_check_state_leave_directory_unchanged() {
    let root = contract_dir().join("runtime-readonly");
    fs::create_dir_all(&root).expect("create runner-owned read-only directory");
    let old_config = fixture()["runtime"]["legacy_config"].clone();
    fs::write(
        root.join("oauth-config.json"),
        serde_json::to_vec_pretty(&old_config).unwrap(),
    )
    .unwrap();
    let before = disk_snapshot(&root).expect("capture existing synthetic state");
    let runtime = Runtime::new(settings(&root, database_url(), true))
        .await
        .expect("construct read-only Runtime against temporary PostgreSQL");
    assert_eq!(runtime.preflight().await.unwrap()["read_only"], true);
    runtime
        .start()
        .await
        .expect("--read-only and --check-state path skips writable startup");
    runtime.close().await;
    assert_eq!(
        before,
        disk_snapshot(&root).expect("capture state after read-only preflight"),
        "read-only preflight must preserve every existing byte/metadata and create no files"
    );
}

fn seed_fingerprint_cache(root: &Path) {
    let raw = include_str!("../resources/modeltrace/unified_bank.json");
    let (bank, mut version) = sub2ops_server::modeltrace::bundled().unwrap();
    assert_eq!(serde_json::from_str::<Value>(raw).unwrap(), bank);
    let manifest = sub2ops_server::modeltrace::manifest();
    for key in ["core", "challenge"] {
        version[format!("{key}_blob")] = manifest["files"][key]["gitBlob"].clone();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let state = json!({
        "version": 1,
        "active": {"raw_bank": raw, "version": version},
        "source": "bundled",
        "checked_at": now,
        "synced_at": now,
        "cooldown_until": null,
        "last_error": null,
        "result": "fixture-cached"
    });
    fs::write(
        root.join("modeltrace-bank-state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .expect("seed a valid recent bundled bank to keep startup offline");
}

async fn insert_legacy_accounts(database: &Database) {
    for (id, name, mapping) in [
        (
            71001i64,
            "fixture-account-applied",
            json!({"model":"fixture-native"}),
        ),
        (
            71002i64,
            "fixture-account-mismatch",
            json!({"model":"fixture-current"}),
        ),
    ] {
        sqlx::query(
            "INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra) \
             VALUES ($1,$2,'anthropic','apikey','active',false,$3,$4)",
        )
        .bind(id)
        .bind(name)
        .bind(sqlx::types::Json(json!({"model_mapping":mapping})))
        .bind(sqlx::types::Json(json!({})))
        .execute(&database.pool)
        .await
        .expect("insert synthetic non-OAuth account for database readback");
    }
    sqlx::query(
        "INSERT INTO scheduled_test_plans (id,account_id,cron_expression,auto_recover) \
         VALUES (72001,71001,'* * * * *',true),(72002,71002,'@hourly',true)",
    )
    .execute(&database.pool)
    .await
    .expect("seed matching and nonmatching legacy recovery plan rows");
}

#[tokio::test]
async fn runtime_new_directory_startup_creates_valid_state_without_missing_intents() {
    let root = contract_dir().join("runtime-new-directory");
    fs::create_dir_all(&root).expect("create runner-owned fresh runtime directory");
    seed_fingerprint_cache(&root);
    let runtime = Runtime::new(settings(&root, database_url(), false))
        .await
        .expect("construct Runtime in fresh writable directory");
    runtime
        .start()
        .await
        .expect("new directory startup must tolerate absent legacy profile_intents");
    let capacity_path = root.join("capacity-alert-state.json");
    let capacity: Value = serde_json::from_slice(&fs::read(&capacity_path).unwrap()).unwrap();
    assert!(
        capacity.get("profile_intents").is_none(),
        "missing legacy profile_intents stays absent rather than becoming null"
    );
    assert_eq!(
        runtime.monitor.state().unwrap()["settings"]["legacy_recovery_cleanup_completed"],
        true
    );
    runtime.close().await;
}

#[tokio::test]
async fn runtime_start_migrates_legacy_state_by_readback_and_keeps_old_python_reader_compatible() {
    let contract = fixture();
    let root = contract_dir().join("runtime-migration");
    fs::create_dir_all(&root).expect("create runner-owned migration directory");
    seed_fingerprint_cache(&root);
    let oauth_config_path = root.join("oauth-config.json");
    fs::write(
        &oauth_config_path,
        serde_json::to_vec_pretty(&contract["runtime"]["legacy_config"]).unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&oauth_config_path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    fs::write(
        root.join("account-model-profiles.json"),
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "jobs": contract["runtime"]["legacy_profile_jobs"]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("capacity-alert-state.json"),
        serde_json::to_vec_pretty(&contract["runtime"]["legacy_capacity_without_profile_intents"])
            .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("guard-state.json"),
        br#"{"synthetic_legacy_guard":true}"#,
    )
    .unwrap();

    let database = Database::open(&database_url(), false)
        .await
        .expect("connect to runner-owned PostgreSQL");
    insert_legacy_accounts(&database).await;
    let runtime = Runtime::new(settings(&root, database_url(), false))
        .await
        .expect("construct Runtime with synthetic legacy files");
    runtime
        .start()
        .await
        .expect("writable startup runs legacy config/recovery/template migrations");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&oauth_config_path).unwrap().mode() & 0o777,
            0o600,
            "migration write tightens an existing 0644 OAuth config"
        );
    }

    assert!(
        !root.join("guard-state.json").exists(),
        "legacy guard file must be retired"
    );
    assert_eq!(
        runtime.monitor.state().unwrap()["settings"]["legacy_recovery_cleanup_completed"],
        true
    );
    assert!(
        runtime.monitor.state().unwrap()["settings"]["legacy_recovery_cleanup_completed_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    let remaining_matching_plans: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_test_plans WHERE cron_expression='* * * * *' AND auto_recover=true",
    )
    .fetch_one(&runtime.db.pool)
    .await
    .unwrap();
    let retained_hourly_plans: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_test_plans WHERE cron_expression='@hourly' AND auto_recover=true",
    )
    .fetch_one(&runtime.db.pool)
    .await
    .unwrap();
    assert_eq!(remaining_matching_plans, 0);
    assert_eq!(retained_hourly_plans, 1);

    let profiles_path = root.join("account-model-profiles.json");
    let profiles = Store::policy(&profiles_path, json!({}), false)
        .read()
        .expect("read back retired legacy profile jobs");
    let expected_statuses = contract["runtime"]["expected_statuses"]
        .as_object()
        .unwrap();
    for item in profiles["jobs"]["fixture-job"]["items"].as_array().unwrap() {
        let id = item["id"].as_str().unwrap();
        assert_eq!(item["status"], expected_statuses[id], "profile item {id}");
        if id == "writing-profile-applied" {
            assert_eq!(item["error"], "");
        }
        if id == "already-applied-profile" {
            assert_eq!(item["error"], "fixture-terminal");
        }
    }

    let capacity_path = root.join("capacity-alert-state.json");
    let mut capacity: Value = serde_json::from_slice(&fs::read(&capacity_path).unwrap()).unwrap();
    assert!(capacity.get("profile_intents").is_none());
    capacity["profile_intents"] = contract["runtime"]["capacity_profile_intents"].clone();
    fs::write(
        &capacity_path,
        serde_json::to_vec_pretty(&capacity).unwrap(),
    )
    .unwrap();
    runtime
        .retire_legacy_templates()
        .await
        .expect("retire optional legacy capacity intents by database readback");
    let capacity = CapacityStore::new(capacity_path, false).snapshot().unwrap();
    for (id, item) in capacity["profile_intents"].as_object().unwrap() {
        assert_eq!(
            item["status"], expected_statuses[id],
            "capacity intent {id}"
        );
        if id == "writing-capacity-applied" {
            assert_eq!(item["error"], "");
        }
        if id == "already-applied-capacity" {
            assert_eq!(item["error"], "fixture-terminal");
        }
    }

    for (id, expected) in [
        (71001i64, json!({"model":"fixture-native"})),
        (71002i64, json!({"model":"fixture-current"})),
    ] {
        let row = runtime.actions.account(id, None).await.unwrap();
        assert_eq!(row["model_mapping"], expected, "migration is readback-only");
    }

    let config: Value =
        serde_json::from_slice(&fs::read(root.join("oauth-config.json")).unwrap()).unwrap();
    let actual_keys = config
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let expected_keys = contract["runtime"]["legacy_config_retained_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        actual_keys
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        expected_keys,
        "legacy OAuth cleanup retains active fields, metadata, and SEEN_IDS only"
    );
    assert_eq!(
        runtime.monitor.state().unwrap()["settings"]["legacy_recovery_cleanup_completed"],
        true
    );
    runtime.migrate_legacy_recovery().await.unwrap();
    runtime.close().await;
    database.close().await;
}
