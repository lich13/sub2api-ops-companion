use crate::{
    compat::{datetime, integer, parse_datetime, string, truthy},
    credit,
    error::Result,
    oauth_admission,
    oauth_monitor::Monitor,
    oauth_queries::{QueryOptions, Source},
    oauth_state::OAuthState,
    quota, recovery_events, recovery_rules,
    security::safe_error,
    settings::daily_time,
};
use chrono::{DateTime, Duration, Timelike, Utc};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Instant};

pub fn next_time(at: DateTime<Utc>, time: &str) -> Result<DateTime<Utc>> {
    let value = daily_time(&json!(time))?;
    let hour = value[..2].parse::<u32>().unwrap();
    let minute = value[3..].parse::<u32>().unwrap();
    let local = at.with_timezone(&crate::settings::BUSINESS_TZ);
    let mut target = local
        .with_hour(hour)
        .unwrap()
        .with_minute(minute)
        .unwrap()
        .with_second(0)
        .unwrap()
        .with_nanosecond(0)
        .unwrap();
    if target <= local {
        target += Duration::days(1);
    }
    Ok(target.with_timezone(&Utc))
}

pub fn eligible(row: &Value, at: DateTime<Utc>) -> bool {
    if !row.is_object()
        || (!row["deleted_at"].is_null() && row["deleted_at"] != "")
        || !string(&row["platform"]).eq_ignore_ascii_case("openai")
        || !string(&row["type"]).eq_ignore_ascii_case("oauth")
        || row["status"] != "active"
        || row["schedulable"] != true
    {
        return false;
    }
    if [
        "temp_unschedulable_until",
        "rate_limit_reset_at",
        "overload_until",
    ]
    .iter()
    .any(|field| {
        !row[field].is_null()
            && row[field] != ""
            && parse_datetime(&row[field]).is_none_or(|t| t > at)
    }) {
        return false;
    }
    let reason = string(&row["temp_unschedulable_reason"]).to_lowercase();
    if ["manual", "telegram", "人工", "手动"]
        .iter()
        .any(|word| reason.contains(word))
    {
        return false;
    }
    if truthy(&row["rate_limited_at"]) && parse_datetime(&row["rate_limit_reset_at"]).is_none() {
        return false;
    }
    !(truthy(&row["auto_pause_on_expired"])
        && truthy(&row["expires_at"])
        && parse_datetime(&row["expires_at"]).is_none_or(|t| t <= at))
}

#[derive(Clone)]
pub struct Schedule {
    store: OAuthState,
}

fn daily(state: &mut Value) -> &mut Value {
    if !state["daily_test"].is_object() {
        state["daily_test"] = json!({});
    }
    if !state["daily_test"]["batches"].is_object() {
        state["daily_test"]["batches"] = json!({});
    }
    &mut state["daily_test"]
}

fn configuration(state: &mut Value, config: &Value, at: DateTime<Utc>) -> Result<()> {
    let enabled = config["oauth_daily_test_enabled"].as_bool().unwrap_or(true);
    let time = daily_time(
        config
            .get("oauth_daily_test_time")
            .unwrap_or(&json!("05:00")),
    )?;
    state["enabled"] = enabled.into();
    state["time"] = time.clone().into();
    state["next_run_at"] = if enabled {
        datetime(next_time(at, &time)?)
    } else {
        String::new()
    }
    .into();
    Ok(())
}

impl Schedule {
    pub fn new(store: OAuthState) -> Self {
        Self { store }
    }
    pub fn startup(&self, config: &Value, at: DateTime<Utc>) -> Result<()> {
        self.store.transaction(|state| {
            let state = daily(state);
            for batch in state["batches"].as_object_mut().unwrap().values_mut() {
                if matches!(string(&batch["status"]), "queued" | "running") {
                    batch["status"] = "interrupted".into();
                    batch["completed_at"] = datetime(at).into();
                }
            }
            configuration(state, config, at)
        })
    }
    pub fn tick(&self, config: &Value, at: DateTime<Utc>) -> Result<()> {
        let enabled = config["oauth_daily_test_enabled"].as_bool().unwrap_or(true);
        let time = daily_time(
            config
                .get("oauth_daily_test_time")
                .unwrap_or(&json!("05:00")),
        )?;
        let snapshot = self.store.shared_snapshot()?;
        let state = &snapshot["daily_test"];
        if state["enabled"] == enabled
            && state["time"] == time
            && (!enabled || parse_datetime(&state["next_run_at"]).is_none_or(|t| at < t))
        {
            return Ok(());
        }
        self.store.transaction(|state| {
            let state = daily(state);
            if state["enabled"] != enabled || state["time"] != time {
                return configuration(state, config, at);
            }
            let Some(target) = parse_datetime(&state["next_run_at"]) else {
                return Ok(());
            };
            if !enabled || at < target {
                return Ok(());
            }
            let date = target
                .with_timezone(&crate::settings::BUSINESS_TZ)
                .date_naive()
                .to_string();
            let batches = &mut state["batches"];
            let scheduled = datetime(target);
            if batches.get(&date).is_none()
                || batches[&date]["status"] == "missed"
                    && batches[&date]["scheduled_at"] != scheduled
            {
                batches[&date] = json!({
                    "date": date,
                    "scheduled_at": scheduled,
                    "status": if at < target + Duration::minutes(1) { "queued" } else { "missed" },
                    "accounts": {}
                });
            }
            while batches.as_object().unwrap().len() > 60 {
                let key = batches.as_object().unwrap().keys().min().unwrap().clone();
                batches.as_object_mut().unwrap().shift_remove(&key);
            }
            state["next_run_at"] = datetime(next_time(at, &time)?).into();
            Ok(())
        })
    }
    pub fn claim(&self, config: &Value, at: DateTime<Utc>) -> Result<Option<Value>> {
        if !self.store.shared_snapshot()?["daily_test"]["batches"]
            .as_object()
            .into_iter()
            .flat_map(|batches| batches.values())
            .any(|batch| batch["status"] == "queued")
        {
            return Ok(None);
        }
        self.store.transaction(|state| {
            let state = daily(state);
            if config["oauth_daily_test_enabled"] == false {
                for batch in state["batches"].as_object_mut().unwrap().values_mut() {
                    if batch["status"] == "queued" {
                        batch["status"] = "cancelled".into();
                        batch["completed_at"] = datetime(at).into();
                    }
                }
                return Ok(None);
            }
            for batch in state["batches"].as_object_mut().unwrap().values_mut() {
                if batch["status"] == "queued" {
                    batch["status"] = "running".into();
                    batch["started_at"] = datetime(at).into();
                    return Ok(Some(batch.clone()));
                }
            }
            Ok(None)
        })
    }
    pub fn save_batch(&self, batch: &Value, pending: &Value) -> Result<()> {
        self.store.transaction(|state| {
            daily(state)["batches"][string(&batch["date"])] = batch.clone();
            if let Some(events) = pending.as_object() {
                state["pending_events"]
                    .as_object_mut()
                    .unwrap()
                    .extend(events.clone());
            }
            Ok(())
        })
    }
}

impl Monitor {
    pub async fn run_daily_with_recovery(
        &self,
        mut batch: Value,
        rows: std::sync::Arc<Vec<Value>>,
        mut cycle_results: BTreeMap<i64, tokio::sync::watch::Receiver<Option<Value>>>,
    ) -> Result<()> {
        let started = self.now();
        let model = self.model()?;
        let state = self.shared_state()?;
        let concurrency = self.config.concurrency()?.recovery;
        if !batch["accounts"].is_object() {
            batch["accounts"] = json!({});
        }
        let pending: Vec<_> = rows
            .iter()
            .filter_map(|row| {
                let id = integer(&row["id"]).filter(|id| *id > 0)?;
                if batch["accounts"].get(id.to_string()).is_some() {
                    return None;
                }
                let generation = credit::generation(&state, id);
                Some((id, row.clone(), generation))
            })
            .collect();
        for (id, _, _) in &pending {
            batch["accounts"][id.to_string()] =
                json!({"status": "claimed", "claimed_at": datetime(started)});
        }
        self.schedule.save_batch(&batch, &json!({}))?;
        let mut results = stream::iter(pending.into_iter().map(|(id, row, generation)| {
            let model = &model;
            let recovery = cycle_results.remove(&id);
            async move {
                let result = self.daily_account(id, model, generation, recovery).await.unwrap_or_else(|_| {
                    json!({"success": false, "error_code": "daily_test_error", "error": "每日测活未完成"})
                });
                (id, row, generation, result)
            }
        })).buffer_unordered(concurrency);
        while let Some((id, row, generation, mut result)) = results.next().await {
            if self.generation(id)? != generation {
                result = json!({"skipped":true,"error_code":"manual_intervention"});
            }
            let checked = result
                .get("completed_at")
                .filter(|v| truthy(v))
                .cloned()
                .unwrap_or(json!(datetime(self.now())));
            let status = if truthy(&result["skipped"]) || truthy(&result["deferred"]) {
                "skipped"
            } else if truthy(&result["success"]) {
                "success"
            } else {
                "failed"
            };
            let reused = result["reused_recovery_test"] == true;
            let record = json!({"status":status,"checked_at":checked,"model_id":model,"reused_recovery_test":reused,"error_code":string(&result["error_code"]),"error":safe_error(&result["error"],800)});
            batch["accounts"][id.to_string()] = record.clone();
            let mut pending = json!({});
            if status == "failed" && !reused {
                let auth = recovery_events::auth_code(string(&record["error_code"]));
                if !auth.is_empty() {
                    self.store.transaction(|state| {
                        if credit::generation(state, id) != generation {
                            return Ok(());
                        }
                        let saved = state["oauth_results"][id.to_string()].clone();
                        let meta = &mut state["scheduler"][id.to_string()];
                        if !meta.is_object() {
                            *meta = json!({});
                        }
                        let mut query = crate::oauth_state::query_metadata(meta, &saved)?;
                        query["auth_fingerprint"] =
                            oauth_admission::credential_fingerprint(&row).into();
                        meta["quota_query"] = query;
                        meta["last_error_code"] = auth.into();
                        meta["last_error_at"] = record["checked_at"].clone();
                        Ok(())
                    })?;
                }
                let key = format!("daily:{}:{id}", string(&batch["date"]));
                pending[&key] = json!({"account_id":id,"account_name":row["name"].as_str().filter(|v|!v.is_empty()).unwrap_or("-"),"plan_type":quota::account_plan(&row),"window_labels":["每日测活"],"status":if auth.is_empty() {"daily_test_failed"} else {"auth_failed"},"stage":"daily_test","checked_at":record["checked_at"],"model_id":model,"error_code":if auth.is_empty() {string(&record["error_code"])} else {auth},"error":record["error"],"dedupe_key":key});
            }
            self.schedule.save_batch(&batch, &pending)?;
        }
        batch["status"] = "completed".into();
        batch["completed_at"] = datetime(self.now()).into();
        self.schedule.save_batch(&batch, &json!({}))?;
        let counts = ["success", "failed", "skipped"]
            .into_iter()
            .map(|status| {
                (
                    status,
                    batch["accounts"]
                        .as_object()
                        .unwrap()
                        .values()
                        .filter(|v| v["status"] == status)
                        .count(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.audit(
            "oauth_daily_test",
            json!({"date":batch["date"],"scheduled_at":batch["scheduled_at"],"counts":counts}),
        );
        Ok(())
    }
    async fn daily_account(
        &self,
        id: i64,
        model: &str,
        generation: i64,
        recovery: Option<tokio::sync::watch::Receiver<Option<Value>>>,
    ) -> Result<Value> {
        let cycle = if let Some(mut receive) = recovery {
            loop {
                if let Some(result) = receive.borrow().clone() {
                    break Some(result);
                }
                if receive.changed().await.is_err() {
                    return Ok(json!({"skipped":true,"error_code":"recovery_uncertain"}));
                }
            }
        } else {
            None
        };
        if self.is_closed() {
            return Ok(json!({"skipped":true,"error_code":"service_stopping"}));
        }
        let row = self.row(id).await?;
        if !eligible(&row, self.now()) {
            return Ok(json!({"skipped":true,"error_code":"daily_ineligible"}));
        }
        if self.generation(id)? != generation {
            return Ok(json!({"skipped":true,"error_code":"manual_intervention"}));
        }
        if cycle
            .as_ref()
            .is_some_and(|result| !result["usage"].is_object())
        {
            return Ok(json!({"skipped":true,"error_code":"recovery_uncertain"}));
        }
        let Ok(_permit) = self.budget.clone().try_acquire_owned() else {
            return Ok(json!({"skipped":true,"error_code":"worker_slots"}));
        };
        let _lease = match self.lease(&row) {
            Ok(lease) => lease,
            Err(e) if e.status.as_u16() == 409 => {
                return Ok(json!({"skipped":true,"error_code":"account_busy"}))
            }
            Err(e) => return Err(e),
        };
        let state = self.shared_state()?;
        let saved = quota::latest_result(&row, &state["oauth_results"][id.to_string()], self.now());
        let metadata = &state["scheduler"][id.to_string()];
        let result = if let Some(cycle) = &cycle {
            cycle["usage"].clone()
        } else if oauth_admission::fresh_quota(&row, &saved, self.now())
            && recovery_rules::control_allows(&row, &saved, metadata, self.now())
            && metadata["quota_query"]["auth_fingerprint"]
                != oauth_admission::credential_fingerprint(&row)
        {
            saved
        } else {
            self.queries
                .query(
                    id,
                    &self.store.admin_token()?,
                    QueryOptions {
                        source: Source::Automatic,
                        reason: "daily_test".into(),
                        at: self.now(),
                        requested_at: Some(Instant::now()),
                        timeout: 10,
                        generation: Some(generation),
                    },
                )
                .await
        };
        if !truthy(&result["success"]) {
            return Ok(result);
        }
        let latest = self.row(id).await?;
        if !eligible(&latest, self.now()) {
            return Ok(json!({"skipped":true,"error_code":"daily_state_changed"}));
        }
        if !oauth_admission::quota_complete(&latest, &result) {
            return Ok(json!({"error_code":"incomplete_quota","error":"额度检查缺少必要窗口"}));
        }
        if !recovery_rules::all_available(&quota::summary_from_result(&latest, &result)) {
            return Ok(json!({"skipped":true,"error_code":"quota_insufficient"}));
        }
        if self.generation(id)? != generation {
            return Ok(json!({"skipped":true,"error_code":"manual_intervention"}));
        }
        if let Some(cycle) = cycle {
            if cycle["test"].is_object() {
                let mut result = cycle["test"].clone();
                if result
                    .get("manual_generation")
                    .is_some_and(|value| integer(value) != Some(generation))
                    || result.get("credential_fingerprint").is_some_and(|value| {
                        *value != oauth_admission::credential_fingerprint(&latest)
                    })
                    || result
                        .get("model_id")
                        .is_some_and(|value| truthy(value) && value != model)
                {
                    return Ok(json!({"skipped":true,"error_code":"daily_state_changed"}));
                }
                result["reused_recovery_test"] = true.into();
                return Ok(result);
            }
        }
        self.hooks
            .connection_test(id, model.to_owned(), generation, 30, false)
            .await
    }
}
