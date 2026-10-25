use crate::{
    compat::{datetime, integer, string, truthy},
    credit_ownership, daily_test,
    error::Result,
    oauth_admission,
    oauth_monitor::Monitor,
    oauth_queries::{QueryOptions, Source},
    quota, recovery_events, recovery_rules,
    security::safe_error,
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Instant};

impl Monitor {
    pub async fn run_daily(&self, batch: Value, rows: Vec<Value>) -> Result<()> {
        self.run_daily_with_recovery(batch, rows, BTreeMap::new())
            .await
    }
    pub(crate) async fn run_daily_with_recovery(
        &self,
        mut batch: Value,
        rows: Vec<Value>,
        mut cycle_results: BTreeMap<i64, tokio::sync::watch::Receiver<Option<Value>>>,
    ) -> Result<()> {
        let started = self.now();
        let model = self.model()?;
        let state = self.shared_state()?;
        let generations = rows
            .iter()
            .filter_map(|row| {
                integer(&row["id"]).map(|id| (id, credit_ownership::generation(&state, id)))
            })
            .collect::<BTreeMap<_, _>>();
        if !batch["accounts"].is_object() {
            batch["accounts"] = json!({});
        }
        let rows = rows
            .into_iter()
            .filter(|row| {
                integer(&row["id"])
                    .is_some_and(|id| id > 0 && batch["accounts"].get(id.to_string()).is_none())
            })
            .collect::<Vec<_>>();
        for row in &rows {
            batch["accounts"][integer(&row["id"]).unwrap().to_string()] =
                json!({"status":"claimed","claimed_at":datetime(started)});
        }
        self.schedule.save_batch(&batch, &json!({}))?;
        let concurrency = integer(&self.config.current()?["oauth_recovery_test_concurrency"])
            .unwrap_or(2)
            .clamp(1, 8) as usize;
        let mut results=stream::iter(rows.into_iter().map(|row|{let this=self.clone();let model=model.clone();let id=integer(&row["id"]).unwrap();let generation=generations[&id];let recovery=cycle_results.remove(&id);async move {let result=this.daily_account(id,&model,generation,recovery).await.unwrap_or_else(|_|json!({"success":false,"error_code":"daily_test_error","error":"每日测活未完成"}));(row,generation,result)}})).buffer_unordered(concurrency);
        while let Some((row, generation, mut result)) = results.next().await {
            let id = integer(&row["id"]).unwrap();
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
                        if credit_ownership::generation(state, id) != generation {
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
        if !daily_test::eligible(&row, self.now()) {
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
        if !daily_test::eligible(&latest, self.now()) {
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
