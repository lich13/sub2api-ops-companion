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
    collections::BTreeMap,
    sync::{atomic::Ordering, Arc, LazyLock},
    time::{Duration, Instant},
};

static STATS_SQL: LazyLock<String> =
    LazyLock::new(|| crate::sql::STATS_STATS_SQL.replace("%(windows)s", "$1"));

#[derive(Default)]
pub(crate) struct Cache {
    epoch: u64,
    stats_epoch: u64,
    observed: Option<Instant>,
    snapshot: Arc<Value>,
    snapshot_bytes: axum::body::Bytes,
    stats_at: Option<Instant>,
    stats_signature: Value,
    stats: usage::Stats,
    stats_failed: bool,
}

impl Runtime {
    pub async fn invalidate(&self) {
        self.stats_epoch.fetch_add(1, Ordering::AcqRel);
        self.cache_epoch.fetch_add(1, Ordering::AcqRel);
    }
    pub(crate) async fn invalidate_snapshot(&self) {
        self.cache_epoch.fetch_add(1, Ordering::AcqRel);
    }
    pub async fn snapshot(&self) -> Result<Value> {
        self.shared_snapshot().await.map(|value| (*value).clone())
    }
    pub async fn snapshot_bytes(&self) -> Result<axum::body::Bytes> {
        self.shared_snapshot_with_bytes()
            .await
            .map(|(_, bytes)| bytes)
    }
    async fn shared_snapshot(&self) -> Result<Arc<Value>> {
        self.shared_snapshot_with_bytes()
            .await
            .map(|(value, _)| value)
    }
    async fn shared_snapshot_with_bytes(&self) -> Result<(Arc<Value>, axum::body::Bytes)> {
        let mut cache = self.snapshot.lock().await;
        let epoch = self.cache_epoch.load(Ordering::Acquire);
        let stats_epoch = self.stats_epoch.load(Ordering::Acquire);
        if cache.epoch == epoch
            && !cache.snapshot_bytes.is_empty()
            && cache
                .observed
                .is_some_and(|at| at.elapsed() < Duration::from_millis(1500))
        {
            return Ok((cache.snapshot.clone(), cache.snapshot_bytes.clone()));
        }
        let now = chrono::Utc::now();
        let state = self.actions.queries.oauth.shared_snapshot()?;
        let rows = self.actions.queries.accounts().await?;
        self.key_quota.retain_accounts(&rows);
        self.monitor.queries.retain_accounts(&rows);
        let mut specs = vec![];
        let mut accounts = Vec::with_capacity(rows.len());
        let mut live_names = BTreeMap::new();
        for row in &rows {
            let id = integer(&row["id"]).ok_or_else(Error::internal)?;
            live_names.insert(id, row["name"].clone());
            let meta = &state["scheduler"][id.to_string()];
            let result =
                queries::latest_completed_result(&state["oauth_results"][id.to_string()], meta);
            let mut account = queries::account_dto_without_windows(row, now);
            account["usage"] = usage::project(row, now, &result);
            account["auto_reset_credit"] = crate::credit_rules::project(
                &meta["auto_reset_credit"],
                &meta["credit_result_notifications"],
            );
            account["recovery_monitor"] = crate::recovery_events::project(meta);
            specs.extend(usage::stats_specs(id, &account["usage"]));
            accounts.push(account);
        }
        let signature = json!(specs
            .iter()
            .map(|s| json!([
                s["account_id"],
                s["key"],
                string(&s["start_at"]).chars().take(16).collect::<String>()
            ]))
            .collect::<Vec<_>>());
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
            .and_then(|state| self.config.current().map(|config| (state, config)));
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
        let (groups, errors) = tokio::try_join!(
            self.actions.queries.groups(&rows),
            self.actions.queries.errors(None, None, 20, None)
        )?;
        let recoveries = self
            .actions
            .queries
            .recoveries_with_names(None, 20, &live_names)?;
        let result = Arc::new(
            json!({"schema_version":1,"observed_at":datetime(now),"accounts":accounts,"groups":groups,
            "errors":errors["items"],"recoveries":recoveries["items"]}),
        );
        let bytes =
            axum::body::Bytes::from(serde_json::to_vec(&*result).map_err(|_| Error::internal())?);
        if self.cache_epoch.load(Ordering::Acquire) == epoch {
            cache.epoch = epoch;
            cache.observed = Some(Instant::now());
            cache.snapshot = result.clone();
            cache.snapshot_bytes = bytes.clone();
        } else {
            cache.stats_at = None;
        }
        Ok((result, bytes))
    }
    pub async fn account_snapshot(&self, id: i64) -> Result<Value> {
        Ok(self.shared_snapshot().await?["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .cloned()
            .unwrap_or(Value::Null))
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
