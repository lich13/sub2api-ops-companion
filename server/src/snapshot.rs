use crate::{
    compat::{datetime, integer, string, truthy},
    db::Param,
    error::{Error, Result},
    queries,
    runtime::Runtime,
    usage,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    sync::{atomic::Ordering, LazyLock},
    time::{Duration, Instant},
};

static STATS_SQL: LazyLock<String> =
    LazyLock::new(|| crate::sql::STATS_STATS_SQL.replace("%(windows)s", "$1"));

#[derive(Clone)]
struct BuiltSnapshot {
    build_id: u64,
    etag: String,
    observed_at: String,
    bytes: axum::body::Bytes,
    gzip: Option<axum::body::Bytes>,
}

type InflightReceiver = tokio::sync::watch::Receiver<Option<Result<BuiltSnapshot>>>;

#[derive(Default)]
pub(crate) struct Cache {
    epoch: u64,
    stats_epoch: u64,
    build_id: u64,
    observed: Option<Instant>,
    observed_at: String,
    etag: String,
    snapshot_bytes: axum::body::Bytes,
    snapshot_gzip: Option<axum::body::Bytes>,
    account_digests: HashMap<i64, ([u8; 32], String, Value)>,
    stats_at: Option<Instant>,
    stats_signature: Option<[u8; 32]>,
    stats: std::sync::Arc<usage::Stats>,
    stats_failed: bool,
    inflight: Option<(u64, u64, InflightReceiver)>,
    next_flight_id: u64,
}

impl Cache {
    fn try_cache_gzip(&mut self, epoch: u64, built: &BuiltSnapshot, gzip: axum::body::Bytes) {
        if self.epoch == epoch
            && self.build_id == built.build_id
            && self.observed_at == built.observed_at
            && self.etag == built.etag
            && self.snapshot_gzip.is_none()
        {
            self.snapshot_gzip = Some(gzip);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn try_commit_stats(
        &mut self,
        current_cache_epoch: u64,
        current_stats_epoch: u64,
        epoch: u64,
        stats_epoch: u64,
        query_started: Instant,
        signature: [u8; 32],
        stats: std::sync::Arc<usage::Stats>,
        stats_failed: bool,
    ) {
        if current_cache_epoch == epoch
            && current_stats_epoch == stats_epoch
            && stats_epoch >= self.stats_epoch
            && (stats_epoch > self.stats_epoch
                || self.stats_at.is_none_or(|at| at <= query_started))
        {
            self.stats_failed = stats_failed;
            self.stats = stats;
            self.stats_epoch = stats_epoch;
            self.stats_at = Some(Instant::now());
            self.stats_signature = Some(signature);
        }
    }
}

pub(crate) enum SnapshotReply {
    NotModified {
        etag: String,
        observed_at: String,
    },
    Full {
        etag: String,
        observed_at: String,
        body: axum::body::Bytes,
        gzipped: bool,
    },
}

#[derive(serde::Serialize)]
struct SnapshotContent<'a> {
    schema_version: u8,
    accounts: &'a [Value],
    groups: &'a [Value],
    errors: &'a Value,
    recoveries: &'a Value,
}

fn hash_value_into(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hasher.update([0u8]),
        Value::Bool(b) => hasher.update([if *b { 2u8 } else { 1u8 }]),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                hasher.update([3u8]);
                hasher.update(i.to_le_bytes());
            } else if let Some(u) = n.as_u64() {
                hasher.update([4u8]);
                hasher.update(u.to_le_bytes());
            } else if let Some(f) = n.as_f64() {
                hasher.update([5u8]);
                hasher.update(f.to_bits().to_le_bytes());
            }
        }
        Value::String(s) => {
            hasher.update([6u8]);
            hasher.update((s.len() as u64).to_le_bytes());
            hasher.update(s.as_bytes());
        }
        Value::Array(arr) => {
            hasher.update([7u8]);
            hasher.update((arr.len() as u64).to_le_bytes());
            for item in arr {
                hash_value_into(hasher, item);
            }
        }
        Value::Object(map) => {
            hasher.update([8u8]);
            hasher.update((map.len() as u64).to_le_bytes());
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
            for (k, v) in entries {
                hasher.update((k.len() as u64).to_le_bytes());
                hasher.update(k.as_bytes());
                hash_value_into(hasher, v);
            }
        }
    }
}

fn version_input_digest(row: &Value) -> [u8; 32] {
    let mut hasher = Sha256::new();
    const TOP_FIELDS: &[&str] = &[
        "id",
        "name",
        "platform",
        "type",
        "status",
        "schedulable",
        "updated_at",
        "priority",
        "concurrency",
        "load_factor",
        "proxy_id",
        "parent_account_id",
        "credential_version",
        "credential_settings_version",
        "model_mapping_version",
        "model_catalog_version",
        "model_sync_project",
        "error_message",
        "overload_until",
        "rate_limit_reset_at",
        "temp_unschedulable_until",
        "group_ids",
    ];
    for key in TOP_FIELDS {
        match row.get(*key) {
            Some(v) => {
                hasher.update([1u8]);
                hash_value_into(&mut hasher, v);
            }
            None => hasher.update([0u8]),
        }
    }
    if !truthy(&row["model_mapping_version"]) {
        match row.get("model_mapping") {
            Some(m) => {
                hasher.update([1u8]);
                hash_value_into(&mut hasher, m);
            }
            None => hasher.update([0u8]),
        }
    }
    hasher.update([0x1eu8]);
    let extra = &row["extra"];
    for key in [
        "parent_account_id",
        "openai_passthrough",
        "openai_oauth_passthrough",
    ] {
        match extra.get(key) {
            Some(v) => {
                hasher.update([1u8]);
                hash_value_into(&mut hasher, v);
            }
            None => hasher.update([0u8]),
        }
    }
    if let Some(map) = extra.as_object() {
        let mut reset_keys: Vec<(&String, &Value)> = map
            .iter()
            .filter(|(k, _)| k.contains("reset_credit") || k.contains("reset_quota"))
            .collect();
        reset_keys.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (k, v) in reset_keys {
            hasher.update((k.len() as u64).to_le_bytes());
            hasher.update(k.as_bytes());
            hash_value_into(&mut hasher, v);
        }
    }
    hasher.finalize().into()
}

fn weak_opaque_tag(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let tag = trimmed.strip_prefix("W/").unwrap_or(trimmed);
    tag.strip_prefix('"')?.strip_suffix('"')
}

pub(crate) fn etag_matches(if_none_match: Option<&str>, current_etag: &str) -> bool {
    let Some(header) = if_none_match.map(str::trim).filter(|h| !h.is_empty()) else {
        return false;
    };
    if header == "*" {
        return true;
    }
    let Some(expected) = weak_opaque_tag(current_etag) else {
        return false;
    };
    header
        .split(',')
        .any(|candidate| weak_opaque_tag(candidate) == Some(expected))
}

fn compress_gzip(raw: &[u8]) -> Result<axum::body::Bytes> {
    let mut encoder = flate2::write::GzEncoder::new(
        Vec::with_capacity(raw.len() / 4),
        flate2::Compression::fast(),
    );
    encoder.write_all(raw).map_err(|_| Error::internal())?;
    let compressed = encoder.finish().map_err(|_| Error::internal())?;
    Ok(axum::body::Bytes::from(compressed))
}

async fn compress_gzip_async(bytes: axum::body::Bytes) -> Result<axum::body::Bytes> {
    crate::supervision::join(tokio::task::spawn_blocking(move || compress_gzip(&bytes)))
        .await
        .map_err(|_| Error::internal())?
}

async fn reply_from_built(
    runtime: &Runtime,
    epoch: u64,
    built: BuiltSnapshot,
    if_none_match: Option<&str>,
    accepts_gzip: bool,
) -> Result<SnapshotReply> {
    if etag_matches(if_none_match, &built.etag) {
        return Ok(SnapshotReply::NotModified {
            etag: built.etag,
            observed_at: built.observed_at,
        });
    }
    let body = if accepts_gzip {
        match built.gzip {
            Some(gzip) => gzip,
            None => {
                let gzip = compress_gzip_async(built.bytes.clone()).await?;
                let mut cache = runtime.snapshot.lock().await;
                cache.try_cache_gzip(epoch, &built, gzip.clone());
                gzip
            }
        }
    } else {
        built.bytes
    };
    Ok(SnapshotReply::Full {
        etag: built.etag,
        observed_at: built.observed_at,
        body,
        gzipped: accepts_gzip,
    })
}
impl Runtime {
    pub fn invalidate(&self) {
        self.stats_epoch.fetch_add(1, Ordering::AcqRel);
        self.cache_epoch.fetch_add(1, Ordering::AcqRel);
    }
    pub(crate) fn invalidate_snapshot(&self) {
        self.cache_epoch.fetch_add(1, Ordering::AcqRel);
    }
    pub async fn snapshot_bytes(&self) -> Result<axum::body::Bytes> {
        match self.snapshot_http(None, false).await? {
            SnapshotReply::Full { body, .. } => Ok(body),
            SnapshotReply::NotModified { .. } => Err(Error::internal()),
        }
    }
    pub(crate) async fn snapshot_http(
        &self,
        if_none_match: Option<&str>,
        accepts_gzip: bool,
    ) -> Result<SnapshotReply> {
        loop {
            let epoch = self.cache_epoch.load(Ordering::Acquire);
            let stats_epoch = self.stats_epoch.load(Ordering::Acquire);
            let (flight_id, tx) = {
                let mut cache = self.snapshot.lock().await;
                if cache.epoch == epoch
                    && !cache.snapshot_bytes.is_empty()
                    && cache
                        .observed
                        .is_some_and(|at| at.elapsed() < Duration::from_millis(1500))
                {
                    let built = BuiltSnapshot {
                        build_id: cache.build_id,
                        etag: cache.etag.clone(),
                        observed_at: cache.observed_at.clone(),
                        bytes: cache.snapshot_bytes.clone(),
                        gzip: cache.snapshot_gzip.clone(),
                    };
                    drop(cache);
                    return reply_from_built(self, epoch, built, if_none_match, accepts_gzip).await;
                }
                if let Some((inflight_epoch, _, mut rx)) = cache.inflight.clone() {
                    if inflight_epoch == epoch && rx.has_changed().is_ok() {
                        drop(cache);
                        let waited = loop {
                            if let Some(outcome) = rx.borrow().clone() {
                                break Some(outcome);
                            }
                            if rx.changed().await.is_err() {
                                break rx.borrow().clone();
                            }
                        };
                        if let Some(outcome) = waited {
                            let built = outcome?;
                            return reply_from_built(
                                self,
                                epoch,
                                built,
                                if_none_match,
                                accepts_gzip,
                            )
                            .await;
                        }
                        continue;
                    }
                }
                cache.next_flight_id = cache.next_flight_id.wrapping_add(1);
                let flight_id = cache.next_flight_id;
                let (tx, rx) = tokio::sync::watch::channel(None);
                cache.inflight = Some((epoch, flight_id, rx));
                (flight_id, tx)
            };

            let cleanup_snapshot = &self.snapshot;
            let inflight_guard = crate::supervision::on_drop(move || {
                if let Ok(mut cache) = cleanup_snapshot.try_lock() {
                    if cache
                        .inflight
                        .as_ref()
                        .is_some_and(|(_, id, _)| *id == flight_id)
                    {
                        cache.inflight = None;
                    }
                }
            });

            let build_result: Result<BuiltSnapshot> = self
                .build_snapshot_flight(flight_id, epoch, stats_epoch, if_none_match, accepts_gzip)
                .await;
            drop(inflight_guard);

            {
                let mut cache = self.snapshot.lock().await;
                if cache
                    .inflight
                    .as_ref()
                    .is_some_and(|(_, id, _)| *id == flight_id)
                {
                    cache.inflight = None;
                }
                if let Ok(built) = &build_result {
                    if self.cache_epoch.load(Ordering::Acquire) == epoch {
                        cache.epoch = epoch;
                        cache.build_id = built.build_id;
                        cache.observed = Some(Instant::now());
                        cache.observed_at = built.observed_at.clone();
                        cache.etag = built.etag.clone();
                        cache.snapshot_bytes = built.bytes.clone();
                        cache.snapshot_gzip = built.gzip.clone();
                    } else if cache.epoch <= epoch && cache.stats_epoch <= stats_epoch {
                        cache.stats_at = None;
                    }
                }
            }
            let _ = tx.send(Some(build_result.clone()));
            let built = build_result?;
            return reply_from_built(self, epoch, built, if_none_match, accepts_gzip).await;
        }
    }

    async fn build_snapshot_flight(
        &self,
        build_id: u64,
        epoch: u64,
        stats_epoch: u64,
        if_none_match: Option<&str>,
        accepts_gzip: bool,
    ) -> Result<BuiltSnapshot> {
        let now = self.monitor.now();
        let state = self.actions.queries.oauth.shared_snapshot()?;
        let (rows, group_rows, errors) = tokio::try_join!(
            self.actions.queries.accounts(),
            self.db.all(crate::sql::DESKTOP_GROUP_SQL, &[]),
            self.actions.queries.errors(None, None, 20, None)
        )?;
        self.key_quota.retain_accounts(&rows);
        self.monitor.queries.retain_accounts(&rows);

        let mut specs = vec![];
        let mut sig_hasher = Sha256::new();
        let mut accounts = Vec::with_capacity(rows.len());
        let mut live_names = BTreeMap::new();
        let need_stats_query = {
            let mut cache = self.snapshot.lock().await;
            for row in &rows {
                let id = integer(&row["id"]).ok_or_else(Error::internal)?;
                live_names.insert(id, row["name"].clone());
                let row_digest = version_input_digest(row);
                let versions = match cache.account_digests.get(&id) {
                    Some((cached_digest, ver, op_ver)) if *cached_digest == row_digest => {
                        (ver.clone(), op_ver.clone())
                    }
                    _ => {
                        let ver = crate::compat::account_version(row);
                        let op_ver = crate::compat::operation_versions(row);
                        cache
                            .account_digests
                            .insert(id, (row_digest, ver.clone(), op_ver.clone()));
                        (ver, op_ver)
                    }
                };
                let meta = &state["scheduler"][id.to_string()];
                let result =
                    queries::latest_completed_result(&state["oauth_results"][id.to_string()], meta);
                let mut account = queries::account_dto_with_versions(row, now, Some(versions));
                account["usage"] = usage::project(row, now, &result);
                account["auto_reset_credit"] = crate::credit::rules::project(
                    &meta["auto_reset_credit"],
                    &meta["credit_result_notifications"],
                );
                account["recovery_monitor"] = crate::recovery_events::project(meta);
                specs.extend(usage::stats_specs(id, &account["usage"]));
                usage::hash_stats_signature(id, &account["usage"], now, &mut sig_hasher);
                accounts.push(account);
            }
            cache
                .account_digests
                .retain(|id, _| live_names.contains_key(id));
            let signature: [u8; 32] = sig_hasher.finalize().into();
            let stale = stats_epoch != cache.stats_epoch
                || cache.stats_failed
                || cache
                    .stats_at
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(15))
                || cache.stats_signature != Some(signature);
            if stale {
                Err(signature)
            } else {
                Ok((cache.stats.clone(), cache.stats_failed))
            }
        };

        let (local_stats, stats_failed) = match need_stats_query {
            Ok((cached_stats, cached_failed)) => (cached_stats, cached_failed),
            Err(signature) => {
                let query_started = Instant::now();
                let stats = if specs.is_empty() {
                    Ok(usage::Stats::new())
                } else {
                    self.db
                        .all(&STATS_SQL, &[Param::Json(json!(specs))])
                        .await
                        .map(usage::stats_rows)
                };
                let failed = stats.is_err();
                let stats_arc = std::sync::Arc::new(stats.unwrap_or_default());
                let mut cache = self.snapshot.lock().await;
                cache.try_commit_stats(
                    self.cache_epoch.load(Ordering::Acquire),
                    self.stats_epoch.load(Ordering::Acquire),
                    epoch,
                    stats_epoch,
                    query_started,
                    signature,
                    stats_arc.clone(),
                    failed,
                );
                (stats_arc, failed)
            }
        };
        for account in &mut accounts {
            let id = integer(&account["id"]).unwrap();
            usage::attach_stats(id, &mut account["usage"], &local_stats, 500_000);
        }

        let marks = self.detection.capacity.store.shared_snapshot();
        let detection = self
            .detection
            .store
            .read_shared()
            .and_then(|state| self.config.current_shared().map(|config| (state, config)));
        for account in &mut accounts {
            let id = integer(&account["id"]).unwrap();
            account["usage"]["stats_status"] = if stats_failed { "error" } else { "ok" }.into();
            account["usage_windows"] = account["usage"]["windows"].clone();
            if crate::model_target::supported(account) {
                account["degradation_mark"] = match &marks {
                    Ok(state) => {
                        crate::capacity_alerts::mark_view(id, &state["marks"][id.to_string()])
                    }
                    Err(_) => json!({"error":"降智标记暂不可读取"}),
                };
                if marks.is_ok() {
                    account["model_detection"] = detection
                        .as_ref()
                        .map_err(Clone::clone)
                        .and_then(|(state, config)| {
                            crate::model_detection::Detection::project_account(
                                account, state, config,
                            )
                        })
                        .unwrap_or(json!({"status":"error","reason":"检测状态暂不可读取"}));
                }
            }
        }
        let groups = queries::Queries::groups_from_rows(&group_rows, &rows);
        let recoveries = self
            .actions
            .queries
            .recoveries_with_names(None, 20, &live_names)?;
        let observed_at = datetime(now);
        let if_none_match_owned = if_none_match.map(str::to_owned);
        crate::supervision::join(tokio::task::spawn_blocking(
            move || -> Result<BuiltSnapshot> {
                let content_bytes = serde_json::to_vec(&SnapshotContent {
                    schema_version: 1,
                    accounts: &accounts,
                    groups: &groups,
                    errors: &errors["items"],
                    recoveries: &recoveries["items"],
                })
                .map_err(|_| Error::internal())?;
                let etag = format!("W/\"{}\"", crate::compat::sha256(&content_bytes));
                let obs_prefix = format!(
                    "{{\"observed_at\":{},",
                    serde_json::to_string(&observed_at).map_err(|_| Error::internal())?
                );
                let mut raw = Vec::with_capacity(obs_prefix.len() + content_bytes.len());
                raw.extend_from_slice(obs_prefix.as_bytes());
                raw.extend_from_slice(&content_bytes[1..]);
                let bytes = axum::body::Bytes::from(raw);
                let not_modified = etag_matches(if_none_match_owned.as_deref(), &etag);
                let gzip = if !not_modified && accepts_gzip {
                    Some(compress_gzip(&bytes)?)
                } else {
                    None
                };
                Ok(BuiltSnapshot {
                    build_id,
                    etag,
                    observed_at,
                    bytes,
                    gzip,
                })
            },
        ))
        .await
        .map_err(|_| Error::internal())?
    }

    pub async fn account_snapshot_from_row(
        &self,
        row: &Value,
        usage: Option<Value>,
    ) -> Result<Value> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let now = self.monitor.now();
        let state = self.actions.queries.oauth.shared_snapshot()?;
        let meta = &state["scheduler"][id.to_string()];
        let mut account = queries::account_dto_without_windows(row, now);
        account["usage"] = match usage {
            Some(usage) => usage,
            None => self.local_account_usage(row).await?,
        };
        account["usage_windows"] = account["usage"]["windows"].clone();
        account["auto_reset_credit"] = crate::credit::rules::project(
            &meta["auto_reset_credit"],
            &meta["credit_result_notifications"],
        );
        account["recovery_monitor"] = crate::recovery_events::project(meta);
        if crate::model_target::supported(row) {
            match self.detection.capacity.store.shared_snapshot() {
                Ok(marks) => {
                    account["degradation_mark"] =
                        crate::capacity_alerts::mark_view(id, &marks["marks"][id.to_string()]);
                    account["model_detection"] = self
                        .detection
                        .store
                        .read_shared()
                        .and_then(|state| {
                            self.config.current_shared().and_then(|config| {
                                crate::model_detection::Detection::project_account(
                                    row, &state, &config,
                                )
                            })
                        })
                        .unwrap_or(json!({"status":"error","reason":"检测状态暂不可读取"}));
                }
                Err(_) => account["degradation_mark"] = json!({"error":"降智标记暂不可读取"}),
            }
        }
        Ok(account)
    }

    pub async fn error_detail(&self, id: i64, category: Option<&str>) -> Result<Value> {
        let mut result = self.actions.queries.error_detail(id, category).await?;
        match self.detection.capacity.store.shared_snapshot() {
            Ok(state) => {
                if let Some(notification) = state["notifications"].get(id.to_string()) {
                    result["notification"] = notification.clone();
                    match self.detection.store.read_shared() {
                        Ok(data) => {
                            if let Some(notice) = data["disposition_notifications"]
                                .get(string(&notification["job_id"]))
                            {
                                result["disposition_notification"] = crate::compat::pick(
                                    notice,
                                    &[
                                        "status",
                                        "attempts",
                                        "delivered_at",
                                        "next_at",
                                        "error_code",
                                    ],
                                );
                            }
                        }
                        Err(_) => result["notification"] = json!({"status":"unavailable"}),
                    }
                }
            }
            Err(_) => result["notification"] = json!({"status":"unavailable"}),
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::{etag_matches, BuiltSnapshot, Cache};
    use axum::body::Bytes;
    use serde_json::json;
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    #[test]
    fn weak_etag_matching_covers_rfc_cases_and_malformed_headers() {
        let current = "W/\"abc123\"";
        assert!(!etag_matches(None, current));
        assert!(!etag_matches(Some(""), current));
        assert!(!etag_matches(Some("   "), current));
        assert!(etag_matches(Some("*"), current));
        assert!(etag_matches(Some("W/\"abc123\""), current));
        assert!(etag_matches(Some("\"abc123\""), current));
        assert!(etag_matches(Some("W/\"old\", W/\"abc123\""), current));
        assert!(!etag_matches(Some("w/\"abc123\""), current));
        assert!(!etag_matches(Some("abc123"), current));
        assert!(!etag_matches(Some("W/\"other\""), current));
    }

    #[test]
    fn gzip_writeback_requires_matching_build_id_and_observed_at() {
        let mut cache = Cache {
            epoch: 3,
            build_id: 11,
            observed_at: "2026-11-26T05:00:02Z".into(),
            etag: "W/\"same-etag\"".into(),
            snapshot_bytes: Bytes::from_static(b"{\"observed_at\":\"2026-11-26T05:00:02Z\"}"),
            ..Default::default()
        };

        let stale_built = BuiltSnapshot {
            build_id: 10,
            etag: "W/\"same-etag\"".into(),
            observed_at: "2026-11-26T05:00:00Z".into(),
            bytes: Bytes::from_static(b"{\"observed_at\":\"2026-11-26T05:00:00Z\"}"),
            gzip: None,
        };
        cache.try_cache_gzip(3, &stale_built, Bytes::from_static(b"stale-gzip"));
        assert!(cache.snapshot_gzip.is_none());

        let current_built = BuiltSnapshot {
            build_id: 11,
            etag: "W/\"same-etag\"".into(),
            observed_at: "2026-11-26T05:00:02Z".into(),
            bytes: cache.snapshot_bytes.clone(),
            gzip: None,
        };
        cache.try_cache_gzip(3, &current_built, Bytes::from_static(b"fresh-gzip"));
        assert_eq!(cache.snapshot_gzip, Some(Bytes::from_static(b"fresh-gzip")));
    }

    #[test]
    fn stats_commit_rejects_stale_epochs_and_older_concurrent_queries() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        let mut newer_map = crate::usage::Stats::new();
        newer_map.insert((1, "five_hour".into()), json!({"requests": 9}));
        cache.try_commit_stats(
            2,
            5,
            2,
            5,
            t0,
            [7u8; 32],
            Arc::new(newer_map.clone()),
            false,
        );
        assert_eq!(cache.stats_epoch, 5);
        assert_eq!(*cache.stats, newer_map);

        let mut stale_map = crate::usage::Stats::new();
        stale_map.insert((1, "five_hour".into()), json!({"requests": 1}));
        // Older stats_epoch cannot overwrite newer stats_epoch
        cache.try_commit_stats(2, 5, 2, 4, t0, [1u8; 32], Arc::new(stale_map.clone()), true);
        // Mismatched cache_epoch cannot overwrite cache.stats
        cache.try_commit_stats(3, 5, 2, 5, t0, [1u8; 32], Arc::new(stale_map.clone()), true);
        // Same epoch query started before cache.stats_at cannot clobber newer result
        cache.try_commit_stats(
            2,
            5,
            2,
            5,
            t0 - Duration::from_millis(10),
            [1u8; 32],
            Arc::new(stale_map),
            true,
        );
        assert_eq!(cache.stats_epoch, 5);
        assert!(!cache.stats_failed);
        assert_eq!(*cache.stats, newer_map);
    }
}
