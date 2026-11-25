use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

fn harness() -> (Candidates, Arc<Mutex<Instant>>) {
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock = now.clone();
    (
        Candidates::with_clock(Default::default(), Arc::new(move || *clock.lock().unwrap())),
        now,
    )
}
fn advance(clock: &Mutex<Instant>, seconds: u64) {
    *clock.lock().unwrap() += Duration::from_secs(seconds);
}
fn models() -> Vec<Value> {
    vec![json!({"id":"fixture-text"})]
}
async fn settled(cache: &Candidates) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while cache
            .state
            .lock()
            .unwrap()
            .entries
            .values()
            .any(|e| e.flight.is_some())
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn directories_survive_age_and_refresh_replaces_including_empty() {
    let (cache, clock) = harness();
    assert_eq!(
        cache
            .get("a".into(), false, async { Ok(models()) })
            .await
            .unwrap(),
        models()
    );
    advance(&clock, 86_400);
    assert_eq!(
        cache
            .get("a".into(), false, async {
                panic!("cached reads must not fetch")
            })
            .await
            .unwrap(),
        models()
    );
    assert!(cache
        .get("a".into(), true, async { Ok(vec![]) })
        .await
        .unwrap()
        .is_empty());
    assert!(cache
        .get("a".into(), false, async {
            panic!("empty success is cached")
        })
        .await
        .unwrap()
        .is_empty());
    cache.close().await;
}

#[tokio::test]
async fn explicit_refresh_reports_failure_retains_transient_and_revokes_permanent() {
    let (cache, _) = harness();
    cache
        .get("a".into(), false, async { Ok(models()) })
        .await
        .unwrap();
    assert_eq!(
        cache
            .get("a".into(), true, async {
                Err(Failure::temporary(Error::new(504, "fixture timeout")))
            })
            .await
            .unwrap_err()
            .status
            .as_u16(),
        504
    );
    assert_eq!(
        cache
            .get("a".into(), false, async {
                panic!("retain last successful directory")
            })
            .await
            .unwrap(),
        models()
    );
    assert!(cache
        .get("a".into(), true, async {
            Err(Failure::permanent(Error::new(401, "fixture revoked")))
        })
        .await
        .is_err());
    assert!(cache
        .get("a".into(), false, async {
            panic!("revoked failure cooldown")
        })
        .await
        .is_err());
    assert_eq!(
        cache
            .get("a".into(), true, async { Ok(models()) })
            .await
            .unwrap(),
        models()
    );
    cache.close().await;
}

#[tokio::test]
async fn successful_directories_survive_restart_and_revocation_is_persistent() {
    let temp = tempfile::tempdir().unwrap();
    let settings = Settings::from_env(
        &[
            (
                "DATABASE_URL".into(),
                "postgresql://fixture@127.0.0.1/fixture".into(),
            ),
            (
                "USAGE_QUERY_STATE_PATH".into(),
                temp.path()
                    .join("usage.json")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]
        .into(),
    )
    .unwrap();
    let cache = Candidates::new(Default::default())
        .persisted(&settings)
        .unwrap();
    cache
        .get("fixture-a".into(), false, async { Ok(models()) })
        .await
        .unwrap();
    cache
        .get("fixture-b".into(), false, async {
            Ok(vec![json!({"id":"fixture-other"})])
        })
        .await
        .unwrap();
    cache.close().await;
    let restored = Candidates::new(Default::default())
        .persisted(&settings)
        .unwrap();
    assert_eq!(
        restored
            .get("fixture-a".into(), false, async {
                panic!("restart must not fetch")
            })
            .await
            .unwrap(),
        models()
    );
    assert_eq!(
        restored
            .get("fixture-b".into(), false, async {
                panic!("isolated persisted account")
            })
            .await
            .unwrap()[0]["id"],
        "fixture-other"
    );
    assert!(restored
        .get("fixture-a".into(), true, async {
            Err(Failure::permanent(Error::new(403, "fixture denied")))
        })
        .await
        .is_err());
    restored.close().await;
    let restored = Candidates::new(Default::default())
        .persisted(&settings)
        .unwrap();
    assert!(restored
        .get("fixture-a".into(), false, async {
            Err(Failure::permanent(Error::new(403, "fixture denied")))
        })
        .await
        .is_err());
    restored.close().await;
}

#[tokio::test]
async fn concurrent_reads_survive_client_cancellation_without_duplicate_fetch() {
    let (cache, _) = harness();
    let calls = Arc::new(AtomicUsize::new(0));
    let (send, receive) = tokio::sync::oneshot::channel();
    let owned = cache.clone();
    let count = calls.clone();
    let first = tokio::spawn(async move {
        owned
            .get("a".into(), false, async move {
                count.fetch_add(1, Ordering::SeqCst);
                receive.await.unwrap();
                Ok(models())
            })
            .await
    });
    while calls.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    first.abort();
    let _ = first.await;
    let owned = cache.clone();
    let second = tokio::spawn(async move {
        owned
            .get("a".into(), false, async { panic!("single flight") })
            .await
    });
    send.send(()).unwrap();
    assert_eq!(second.await.unwrap().unwrap(), models());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    settled(&cache).await;
    assert_eq!(cache.slots.available_permits(), 8);
    cache.close().await;
}

#[tokio::test]
async fn concurrency_is_bounded_and_shutdown_releases_waiters_and_permits() {
    let (cache, _) = harness();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut readers = vec![];
    for id in 0..8 {
        let owned = cache.clone();
        let count = calls.clone();
        readers.push(tokio::spawn(async move {
            owned
                .get(id.to_string(), false, async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    std::future::pending().await
                })
                .await
        }));
    }
    while calls.load(Ordering::SeqCst) < 8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        cache
            .get("overflow".into(), false, async {
                panic!("no waiting queue")
            })
            .await
            .unwrap_err()
            .status
            .as_u16(),
        503
    );
    cache.close().await;
    for reader in readers {
        assert!(reader.await.unwrap().is_err());
    }
    assert!(cache.state.lock().unwrap().entries.is_empty());
    assert_eq!(cache.slots.available_permits(), 8);
    assert!(cache
        .get("closed".into(), false, async { panic!("closed") })
        .await
        .is_err());
}

#[tokio::test]
async fn entries_and_normalized_bytes_are_bounded_and_identity_isolated() {
    let (cache, clock) = harness();
    for id in 0..=MAX_ENTRIES {
        cache
            .get(id.to_string(), false, async { Ok(models()) })
            .await
            .unwrap();
        advance(&clock, 1);
    }
    assert_eq!(cache.state.lock().unwrap().entries.len(), MAX_ENTRIES);
    assert!(!cache.state.lock().unwrap().entries.contains_key("0"));
    let calls = Arc::new(AtomicUsize::new(0));
    for source in [
        "account-a-key-a",
        "account-b-key-a",
        "account-a-key-b",
        "account-a-new-version",
    ] {
        let count = calls.clone();
        cache
            .get(source.into(), false, async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(vec![
                    json!({"id":"fixture","display_name":"x".repeat(3*1024*1024)}),
                ])
            })
            .await
            .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(
        cache
            .state
            .lock()
            .unwrap()
            .entries
            .values()
            .filter_map(|e| e.success.as_ref())
            .map(|s| s.bytes)
            .sum::<usize>()
            <= MAX_BYTES
    );
    cache.close().await;
}

#[tokio::test]
async fn refresh_has_an_eight_second_budget_and_can_retry_after_timeout() {
    let (cache, clock) = harness();
    let start = Instant::now();
    let error = cache
        .get("a".into(), false, std::future::pending())
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 504);
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(cache.slots.available_permits(), 8);
    advance(&clock, 5);
    assert_eq!(
        cache
            .get("a".into(), false, async { Ok(models()) })
            .await
            .unwrap(),
        models()
    );
    cache.close().await;
}

#[tokio::test]
async fn concurrent_persisted_refreshes_track_sizes_atomically_within_max_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let settings = Settings::from_env(
        &[
            (
                "DATABASE_URL".into(),
                "postgresql://fixture@127.0.0.1/fixture".into(),
            ),
            (
                "USAGE_QUERY_STATE_PATH".into(),
                temp.path()
                    .join("usage.json")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]
        .into(),
    )
    .unwrap();
    let cache = Candidates::new(Default::default())
        .persisted(&settings)
        .unwrap();
    // Seed account-1 with a tiny entry so in-memory `self.state` initially records <100 bytes.
    cache
        .get("account-1".into(), false, async { Ok(models()) })
        .await
        .unwrap();

    let (ready_a_tx, ready_a_rx) = tokio::sync::oneshot::channel();
    let (ready_b_tx, ready_b_rx) = tokio::sync::oneshot::channel();
    let (go_a_tx, go_a_rx) = tokio::sync::oneshot::channel();
    let (go_b_tx, go_b_rx) = tokio::sync::oneshot::channel();

    let c1 = cache.clone();
    let t1 = tokio::spawn(async move {
        c1.get("account-1".into(), true, async move {
            let _ = ready_a_tx.send(());
            let _ = go_a_rx.await;
            Ok(vec![json!({
                "id": "fixture-large-1",
                "display_name": "a".repeat(5 * 1024 * 1024),
            })])
        })
        .await
    });
    let c2 = cache.clone();
    let t2 = tokio::spawn(async move {
        c2.get("account-2".into(), true, async move {
            let _ = ready_b_tx.send(());
            let _ = go_b_rx.await;
            Ok(vec![json!({
                "id": "fixture-large-2",
                "display_name": "b".repeat(5 * 1024 * 1024),
            })])
        })
        .await
    });

    ready_a_rx.await.unwrap();
    ready_b_rx.await.unwrap();
    go_a_tx.send(()).unwrap();
    go_b_tx.send(()).unwrap();

    t1.await.unwrap().unwrap();
    t2.await.unwrap().unwrap();

    let raw_disk: Value = serde_json::from_slice(
        &std::fs::read(temp.path().join("account-model-cache.json")).unwrap(),
    )
    .unwrap();
    let persisted_entries = raw_disk["entries"].as_object().unwrap();
    let persisted_total: usize = persisted_entries
        .values()
        .map(|v| json_byte_len(v).unwrap())
        .sum();
    assert!(
        persisted_total <= MAX_BYTES,
        "persisted entries exceeded MAX_BYTES: {persisted_total} > {MAX_BYTES}"
    );
    assert_eq!(persisted_entries.len(), 1);
    cache.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_persist_leaves_prior_disk_sizes_intact_for_subsequent_eviction() {
    let temp = tempfile::tempdir().unwrap();
    let settings = Settings::from_env(
        &[
            (
                "DATABASE_URL".into(),
                "postgresql://fixture@127.0.0.1/fixture".into(),
            ),
            (
                "USAGE_QUERY_STATE_PATH".into(),
                temp.path()
                    .join("usage.json")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]
        .into(),
    )
    .unwrap();
    let cache = Candidates::new(Default::default())
        .persisted(&settings)
        .unwrap();
    cache
        .get("account-1".into(), false, async {
            Ok(vec![json!({
                "id": "fixture-5mb",
                "display_name": "x".repeat(5 * 1024 * 1024),
            })])
        })
        .await
        .unwrap();

    // Corrupting the on-disk entries field makes the next refresh's disk.transaction fail
    // before committing, so disk_sizes must not record a tiny replacement size for account-1.
    let cache_path = temp.path().join("account-model-cache.json");
    let valid_bytes = std::fs::read(&cache_path).unwrap();
    std::fs::write(&cache_path, br#"{"version":1,"entries":"broken"}"#).unwrap();
    cache.disk.as_ref().unwrap().evict_cache();
    assert!(cache
        .get("account-1".into(), true, async { Ok(models()) })
        .await
        .is_err());

    // Restore valid 5 MiB file and verify persisting another 5 MiB entry still knows account-1 is 5 MiB and evicts it.
    std::fs::write(&cache_path, &valid_bytes).unwrap();
    cache.disk.as_ref().unwrap().evict_cache();
    cache
        .get("account-2".into(), true, async {
            Ok(vec![json!({
                "id": "fixture-5mb-2",
                "display_name": "y".repeat(5 * 1024 * 1024),
            })])
        })
        .await
        .unwrap();

    let raw_disk: Value = serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    let persisted_entries = raw_disk["entries"].as_object().unwrap();
    let persisted_total: usize = persisted_entries
        .values()
        .map(|v| json_byte_len(v).unwrap())
        .sum();
    assert!(persisted_total <= MAX_BYTES);
    assert_eq!(persisted_entries.len(), 1);
    assert!(persisted_entries.contains_key("account-2"));
    cache.close().await;
}
