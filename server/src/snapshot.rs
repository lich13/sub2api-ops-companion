use crate::{
    compat::{datetime, integer, string},
    db::Param,
    error::{Error, Result},
    queries,
    runtime::Runtime,
    usage,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    sync::{atomic::Ordering, LazyLock},
    time::{Duration, Instant},
};

static STATS_SQL: LazyLock<String> =
    LazyLock::new(|| crate::sql::STATS_STATS_SQL.replace("%(windows)s", "$1"));

#[derive(Default)]
pub(crate) struct Cache {
    epoch: u64,
    stats_epoch: u64,
    observed: Option<Instant>,
    observed_at: String,
    etag: String,
    snapshot_bytes: axum::body::Bytes,
    snapshot_gzip: Option<axum::body::Bytes>,
    account_digests: HashMap<i64, (Value, String, Value)>,
    stats_at: Option<Instant>,
    stats_signature: Value,
    stats: usage::Stats,
    stats_failed: bool,
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
        let mut cache = self.snapshot.lock().await;
        let epoch = self.cache_epoch.load(Ordering::Acquire);
        let stats_epoch = self.stats_epoch.load(Ordering::Acquire);
        if cache.epoch == epoch
            && !cache.snapshot_bytes.is_empty()
            && cache
                .observed
                .is_some_and(|at| at.elapsed() < Duration::from_millis(1500))
        {
            if etag_matches(if_none_match, &cache.etag) {
                return Ok(SnapshotReply::NotModified {
                    etag: cache.etag.clone(),
                    observed_at: cache.observed_at.clone(),
                });
            }
            let body = if accepts_gzip {
                match &cache.snapshot_gzip {
                    Some(gzip) => gzip.clone(),
                    None => {
                        let gzip = compress_gzip(&cache.snapshot_bytes)?;
                        cache.snapshot_gzip = Some(gzip.clone());
                        gzip
                    }
                }
            } else {
                cache.snapshot_bytes.clone()
            };
            return Ok(SnapshotReply::Full {
                etag: cache.etag.clone(),
                observed_at: cache.observed_at.clone(),
                body,
                gzipped: accepts_gzip,
            });
        }
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
        let mut sig_entries = vec![];
        let mut accounts = Vec::with_capacity(rows.len());
        let mut live_names = BTreeMap::new();
        for row in &rows {
            let id = integer(&row["id"]).ok_or_else(Error::internal)?;
            live_names.insert(id, row["name"].clone());
            let versions = match cache.account_digests.get(&id) {
                Some((cached_row, ver, op_ver)) if cached_row == row => {
                    (ver.clone(), op_ver.clone())
                }
                _ => {
                    let ver = crate::compat::account_version(row);
                    let op_ver = crate::compat::operation_versions(row);
                    cache
                        .account_digests
                        .insert(id, (row.clone(), ver.clone(), op_ver.clone()));
                    (ver, op_ver)
                }
            };
            let meta = &state["scheduler"][id.to_string()];
            let result =
                queries::latest_completed_result(&state["oauth_results"][id.to_string()], meta);
            let mut account = queries::account_dto_with_versions(row, now, Some(versions));
            account["usage"] = usage::project(row, now, &result);
            account["auto_reset_credit"] = crate::credit_rules::project(
                &meta["auto_reset_credit"],
                &meta["credit_result_notifications"],
            );
            account["recovery_monitor"] = crate::recovery_events::project(meta);
            specs.extend(usage::stats_specs(id, &account["usage"]));
            sig_entries.extend(usage::stats_signature(id, &account["usage"], now));
            accounts.push(account);
        }
        cache
            .account_digests
            .retain(|id, _| live_names.contains_key(id));
        let signature = Value::Array(sig_entries);
        if stats_epoch != cache.stats_epoch
            || cache.stats_failed
            || cache
                .stats_at
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(15))
            || cache.stats_signature != signature
        {
            let stats = if specs.is_empty() {
                Ok(usage::Stats::new())
            } else {
                self.db
                    .all(&STATS_SQL, &[Param::Json(json!(specs))])
                    .await
                    .map(usage::stats_rows)
            };
            cache.stats_failed = stats.is_err();
            cache.stats = stats.unwrap_or_default();
            cache.stats_epoch = stats_epoch;
            cache.stats_at = Some(Instant::now());
            cache.stats_signature = signature;
        }
        let marks = self.detection.capacity.store.shared_snapshot();
        let detection = self
            .detection
            .store
            .read_shared()
            .and_then(|state| self.config.current_shared().map(|config| (state, config)));
        for account in &mut accounts {
            let id = integer(&account["id"]).unwrap();
            usage::attach_stats(id, &mut account["usage"], &cache.stats, 500_000);
            account["usage"]["stats_status"] =
                if cache.stats_failed { "error" } else { "ok" }.into();
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
        let not_modified = etag_matches(if_none_match, &etag);
        let gzip = if !not_modified && accepts_gzip {
            Some(compress_gzip(&bytes)?)
        } else {
            None
        };
        if self.cache_epoch.load(Ordering::Acquire) == epoch {
            cache.epoch = epoch;
            cache.observed = Some(Instant::now());
            cache.observed_at = observed_at.clone();
            cache.etag = etag.clone();
            cache.snapshot_bytes = bytes.clone();
            cache.snapshot_gzip = gzip.clone();
        } else {
            cache.stats_at = None;
        }
        if not_modified {
            return Ok(SnapshotReply::NotModified { etag, observed_at });
        }
        Ok(SnapshotReply::Full {
            etag,
            observed_at,
            body: match gzip {
                Some(gzip) => gzip,
                None => bytes,
            },
            gzipped: accepts_gzip,
        })
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
        account["auto_reset_credit"] = crate::credit_rules::project(
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
    use super::etag_matches;

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
}
