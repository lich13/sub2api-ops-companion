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
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct Cache {
    epoch: u64,
    observed: Option<Instant>,
    snapshot: Value,
    stats_at: Option<Instant>,
    stats_signature: Value,
    stats: usage::Stats,
}

impl Runtime {
    pub async fn invalidate(&self) {
        self.cache_epoch.fetch_add(1, Ordering::AcqRel);
    }
    pub async fn snapshot(&self) -> Result<Value> {
        let mut cache = self.snapshot.lock().await;
        let epoch = self.cache_epoch.load(Ordering::Acquire);
        if cache.epoch == epoch
            && cache
                .observed
                .is_some_and(|at| at.elapsed() < Duration::from_millis(1500))
        {
            return Ok(cache.snapshot.clone());
        }
        let now = chrono::Utc::now();
        let state = self.actions.queries.oauth.snapshot()?;
        let rows = self.actions.queries.accounts().await?;
        let mut specs = vec![];
        let mut accounts = Vec::with_capacity(rows.len());
        for row in &rows {
            let id = integer(&row["id"]).ok_or_else(Error::internal)?;
            let meta = &state["scheduler"][id.to_string()];
            let result =
                queries::latest_completed_result(&state["oauth_results"][id.to_string()], meta);
            let mut account = queries::account_dto(row, now, &result);
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
        if epoch != cache.epoch
            || cache
                .stats_at
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(15))
            || cache.stats_signature != signature
        {
            cache.stats = if specs.is_empty() {
                usage::Stats::new()
            } else {
                self.db
                    .all(
                        &crate::sql::STATS_STATS_SQL.replace("%(windows)s", "$1"),
                        &[Param::Json(json!(specs))],
                    )
                    .await
                    .map(usage::stats_rows)
                    .unwrap_or_default()
            };
            cache.stats_at = Some(Instant::now());
            cache.stats_signature = signature;
        }
        let marks = self.detection.capacity.store.snapshot();
        for account in &mut accounts {
            let id = integer(&account["id"]).unwrap();
            usage::attach_stats(id, &mut account["usage"], &cache.stats, 500_000);
            account["usage_windows"] = account["usage"]["windows"].clone();
            if account["platform"] == "openai"
                && matches!(string(&account["type"]), "oauth" | "apikey")
            {
                account["degradation_mark"] = match &marks {
                    Ok(state) => {
                        crate::capacity_alerts::mark_view(id, &state["marks"][id.to_string()])
                    }
                    Err(_) => json!({"error":"降智标记暂不可读取"}),
                };
                if marks.is_ok() {
                    account["model_detection"] = self
                        .detection
                        .view(id)
                        .unwrap_or(json!({"status":"error","reason":"检测状态暂不可读取"}));
                }
            }
        }
        let groups = self.actions.queries.groups(&rows).await?;
        let errors = self.actions.queries.errors(None, None, 20, None).await?;
        let recoveries = self.actions.queries.recoveries(None, 20).await?;
        let result = json!({"schema_version":1,"observed_at":datetime(now),"accounts":accounts,"groups":groups,
            "errors":errors["items"],"recoveries":recoveries["items"]});
        if self.cache_epoch.load(Ordering::Acquire) == epoch {
            cache.epoch = epoch;
            cache.observed = Some(Instant::now());
            cache.snapshot = result.clone();
        } else {
            cache.stats_at = None;
        }
        Ok(result)
    }
    pub async fn account_snapshot(&self, id: i64) -> Result<Value> {
        Ok(self.snapshot().await?["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .cloned()
            .unwrap_or(Value::Null))
    }
    pub async fn error_detail(&self, id: i64, category: Option<&str>) -> Result<Value> {
        let mut result = self.actions.queries.error_detail(id, category).await?;
        match self.detection.capacity.store.snapshot() {
            Ok(state) => {
                if let Some(notification) = state["notifications"].get(id.to_string()) {
                    result["notification"] = notification.clone();
                    match self.detection.store.read() {
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
