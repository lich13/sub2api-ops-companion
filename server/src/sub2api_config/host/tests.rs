use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    restarts: AtomicUsize,
    fail_first: bool,
    fail_all: bool,
    config: PathBuf,
    external: bool,
}
impl Driver for Fixture {
    fn observe(&self) -> BoxFuture<'_, Result<Observed>> {
        Box::pin(async move {
            let n = self.restarts.load(Ordering::SeqCst);
            Ok(Observed {
                version: "0.2.18".into(),
                commit: string(&CATALOG["revision"]).into(),
                process: sha256(n.to_string()),
                healthy: !self.fail_all && (!self.fail_first || n != 1),
                env: BTreeMap::new(),
            })
        })
    }
    fn restart(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.restarts.fetch_add(1, Ordering::SeqCst);
            if self.external {
                std::fs::write(&self.config, b"external: true\n")?;
            }
            Ok(())
        })
    }
}
fn fixture(
    fail_first: bool,
    fail_all: bool,
    external: bool,
) -> (tempfile::TempDir, Arc<Host>, Arc<Fixture>, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let bytes=b"jwt:\n  secret: fixture-configuration-key-for-tests-only\nlog:\n  level: info\nunknown:\n  keep: [false, 0, null]\n".to_vec();
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let driver = Arc::new(Fixture {
        restarts: AtomicUsize::new(0),
        fail_first,
        fail_all,
        config: path.clone(),
        external,
    });
    let mut host = Host::new(path, driver.clone(), vec!["http://127.0.0.1".into()]);
    host.health_wait = Duration::from_millis(5);
    (dir, Arc::new(host), driver, bytes)
}
#[tokio::test]
async fn saves_and_restarts_preserving_owner_unknown_values_and_permissions() {
    let (_dir, host, driver, before) = fixture(false, false, false);
    let o = driver.observe().await.unwrap();
    let request = json!({"expected_revision":sha256(&before),"runtime_revision":o.process,"changes":[{"path":"log.level","remove":false,"value":"debug"}]});
    let (_, _, after) = host.checked(&request).await.unwrap();
    let lock = host.gate.clone().lock_owned().await;
    host.clone().apply(o, before, after, lock).await;
    assert_eq!(host.operation.lock().unwrap()["status"], "succeeded");
    assert_eq!(driver.restarts.load(Ordering::SeqCst), 1);
    let config = parse_yaml(&host.read().unwrap()).unwrap();
    assert_eq!(config["unknown"]["keep"], json!([false, 0, null]));
    assert_eq!(config["log"]["level"], "debug");
    assert_eq!(
        std::fs::metadata(&host.config).unwrap().mode() & 0o777,
        0o600
    );
    let owner = std::fs::metadata(&host.config).unwrap();
    let parent = std::fs::metadata(host.config.parent().unwrap()).unwrap();
    assert_eq!(owner.uid(), parent.uid());
    assert_eq!(owner.gid(), parent.gid());
}
#[tokio::test]
async fn failed_health_restores_once_and_reports_failed_restoration() {
    for fail_all in [false, true] {
        let (_dir, host, driver, before) = fixture(true, fail_all, false);
        let o = driver.observe().await.unwrap();
        let lock = host.gate.clone().lock_owned().await;
        host.clone()
            .apply(o, before.clone(), b"log:\n  level: debug\n".to_vec(), lock)
            .await;
        assert_eq!(
            host.operation.lock().unwrap()["status"],
            if fail_all {
                "restore_failed"
            } else {
                "restored"
            }
        );
        assert_eq!(host.read().unwrap(), before);
        assert_eq!(driver.restarts.load(Ordering::SeqCst), 2);
    }
}
#[tokio::test]
async fn external_changes_are_never_rolled_back() {
    let (_dir, host, driver, before) = fixture(false, false, true);
    let o = driver.observe().await.unwrap();
    let lock = host.gate.clone().lock_owned().await;
    host.clone()
        .apply(o, before, b"log:\n  level: debug\n".to_vec(), lock)
        .await;
    assert_eq!(host.operation.lock().unwrap()["status"], "conflict");
    assert_eq!(host.read().unwrap(), b"external: true\n");
    assert_eq!(driver.restarts.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn request_disconnect_does_not_cancel_apply_and_target_is_bound() {
    let (_dir, host, driver, before) = fixture(false, false, false);
    let o = driver.observe().await.unwrap();
    let request = json!({"target":"http://127.0.0.1","action":"apply","payload":{"expected_revision":sha256(&before),"runtime_revision":o.process,"changes":[{"path":"log.level","remove":false,"value":"debug"}]}});
    assert!(host
        .handle(&json!({"target":"http://example.invalid","action":"read"}))
        .await
        .is_err());
    let response = host.handle(&request).await.unwrap();
    drop(response);
    let _lock = host.gate.lock().await;
    assert_eq!(host.operation.lock().unwrap()["status"], "succeeded");
}
#[tokio::test]
async fn stale_file_or_process_is_rejected_before_write() {
    let (_dir, host, driver, before) = fixture(false, false, false);
    let o = driver.observe().await.unwrap();
    let mut request = json!({"expected_revision":sha256(&before),"runtime_revision":o.process,"changes":[{"path":"log.level","remove":false,"value":"debug"}]});
    std::fs::write(&host.config, b"external: true\n").unwrap();
    assert_eq!(
        host.checked(&request).await.err().unwrap().status.as_u16(),
        409
    );
    std::fs::write(&host.config, &before).unwrap();
    request["runtime_revision"] = sha256("other").into();
    assert!(host.checked(&request).await.is_err());
    assert_eq!(driver.restarts.load(Ordering::SeqCst), 0);
    request["runtime_revision"] = driver.observe().await.unwrap().process.into();
    let backup = host.config.with_extension("yaml.sub2ops-rollback");
    std::fs::write(&backup, b"previous recovery").unwrap();
    assert_eq!(
        host.handle(&json!({"target":"http://127.0.0.1","action":"apply","payload":request}))
            .await
            .unwrap_err()
            .status
            .as_u16(),
        409
    );
    assert_eq!(std::fs::read(backup).unwrap(), b"previous recovery");
}

#[tokio::test]
async fn unrecognized_running_version_is_read_only_even_with_a_valid_configuration() {
    struct Unsupported;
    impl Driver for Unsupported {
        fn observe(&self) -> BoxFuture<'_, Result<Observed>> {
            Box::pin(async {
                Ok(Observed {
                    version: "0.2.11".into(),
                    commit: "fixture-unknown-commit".into(),
                    process: sha256("fixture-process"),
                    healthy: true,
                    env: BTreeMap::new(),
                })
            })
        }
        fn restart(&self) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { panic!("read-only target must not restart") })
        }
    }
    let (_dir, fixture, _, before) = fixture(false, false, false);
    let host = Host::new(
        fixture.config.clone(),
        Arc::new(Unsupported),
        vec!["http://127.0.0.1".into()],
    );
    let snapshot = host.snapshot().await.unwrap();
    assert_eq!(snapshot["version"], "0.2.11");
    assert_eq!(snapshot["editable"], false);
    assert_eq!(snapshot["values"]["log.level"]["editable"], false);
    let request = json!({"expected_revision":sha256(before),"runtime_revision":snapshot["runtime_revision"],"changes":[{"path":"log.level","remove":false,"value":"debug"}]});
    assert_eq!(
        host.checked(&request).await.err().unwrap().status.as_u16(),
        409
    );
}
