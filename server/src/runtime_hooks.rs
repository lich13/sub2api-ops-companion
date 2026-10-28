use crate::{
    compat::{integer, string, text, truthy},
    credit,
    error::{Error, Result},
    model_tests, oauth_monitor, operations,
    runtime::Runtime,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock, Weak};

pub struct Hooks(pub OnceLock<Weak<Runtime>>);
impl Hooks {
    fn runtime(&self) -> Result<Arc<Runtime>> {
        self.0
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(Error::internal)
    }
    fn cycle_saved(row: &Value, state: &Value, at: chrono::DateTime<chrono::Utc>) -> Result<Value> {
        let id = integer(&row["id"]).unwrap_or(0).to_string();
        let meta = &state["scheduler"][&id];
        if meta["quota_query"]["auth_fingerprint"]
            == crate::oauth_admission::credential_fingerprint(row)
            || !truthy(&meta["quota_query"]["auth_fingerprint"])
                && crate::oauth_admission::AUTH_ERRORS.contains(&string(&meta["last_error_code"]))
        {
            return Err(Error::new(422, "账号授权失效，请更新授权后重新预约"));
        }
        Ok(crate::quota::latest_result(
            row,
            &state["oauth_results"][&id],
            at,
        ))
    }
    fn cycle_needs_evidence(row: &Value, saved: &Value) -> bool {
        let summary = crate::quota::summary_from_result(row, saved);
        let windows = crate::quota::windows_by_key(&summary["ui_windows"]);
        crate::quota::percent(&windows.get("codex_7d").unwrap_or(&Value::Null)["used_percent"])
            .is_some_and(|used| used >= 100.0)
    }
}
impl credit::Hooks for Hooks {
    fn config(&self) -> Result<Value> {
        self.runtime()?.config.current()
    }
    fn detection_held(&self, id: i64) -> Result<bool> {
        Ok(self.runtime()?.detection.recovery_held(id))
    }
    fn detection_version(&self, id: i64) -> Result<Value> {
        self.runtime()?.detection.credit_control_version(id)
    }
    fn model_verify(
        &self,
        id: i64,
        episode: String,
        attempt: i64,
        consume: bool,
    ) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            self.runtime()?
                .detection
                .recovery_job(id, "credit", &episode, attempt, consume)
                .await
        })
    }
    fn cancel_verification(&self, job_id: String, episode: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let runtime = self.runtime()?;
            let job = runtime.tests.get(&job_id)?;
            if job["recovery_context"]["kind"] == "credit"
                && job["recovery_context"]["fingerprint"] == episode
            {
                runtime.tests.cancel(&job_id).await?;
            }
            Ok(())
        })
    }
    fn connection_test(
        &self,
        id: i64,
        model: String,
        generation: i64,
        seconds: u64,
        reserved: bool,
    ) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let runtime = self.runtime()?;
            runtime.writable()?;
            let _slot = if reserved {
                None
            } else {
                match runtime.slots.acquire() {
                    Some(slot) => Some(slot),
                    None => {
                        return Ok(json!({"success":false,"deferred":true,
                    "error_code":"test_slots","error":"等待测活并发空闲","next_at":crate::compat::datetime(chrono::Utc::now()+chrono::Duration::seconds(2))}))
                    }
                }
            };
            if runtime.monitor.generation(id)? != generation {
                return Ok(
                    json!({"success":false,"error_code":"manual_intervention","error":"人工操作已优先处理"}),
                );
            }
            let key = runtime.actions.queries.oauth.admin_token()?;
            Ok(crate::oauth_native::test(&runtime.actions, id, &model, &key, seconds).await)
        })
    }
    fn recover(&self, id: i64, key: String) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let runtime = self.runtime()?;
            runtime.writable()?;
            Ok(crate::oauth_native::recover(&runtime.actions, id, &key, 5).await)
        })
    }
}
impl oauth_monitor::RecoveryHooks for Hooks {
    fn model_notification(&self, job_id: &str, suppressed: bool) -> Result<()> {
        self.runtime()?
            .detection
            .log_notification(job_id, suppressed)
    }
    fn model_recovery(
        &self,
        id: i64,
        fingerprint: String,
        attempt: i64,
    ) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            self.runtime()?
                .detection
                .recovery_job(id, "recovery", &fingerprint, attempt, false)
                .await
        })
    }
}
impl model_tests::ModelHooks for Hooks {
    fn guard(&self, job: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move { self.runtime()?.detection.guard(&job).await })
    }
    fn next_allowed_at(&self, id: i64) -> Result<Value> {
        self.runtime()?.detection.next_allowed_at(id)
    }
    fn register_dispatch(&self, job: &Value) -> Result<()> {
        self.runtime()?.detection.register_dispatch(job)
    }
    fn prepare_recovery(&self, job: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move { self.runtime()?.detection.prepare_recovery(&job).await })
    }
    fn verdict(&self, job: Value, report: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move { self.runtime()?.detection.verdict(&job, &report).await })
    }
}
impl operations::Backend for Hooks {
    fn retry_config(&self) -> Result<crate::connection_retry::Policy> {
        crate::connection_retry::Policy::config(&self.runtime()?.config.current()?)
    }
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.runtime()
            .map(|runtime| runtime.monitor.now())
            .unwrap_or_else(|_| chrono::Utc::now())
    }
    fn mark(&self, id: i64) -> Result<Value> {
        self.runtime()?.detection.mark(id)
    }
    fn cycle_observation<'a>(
        &'a self,
        id: i64,
        key: &'a str,
        refresh: bool,
    ) -> BoxFuture<'a, Result<operations::CycleObservation>> {
        Box::pin(async move {
            let runtime = self.runtime()?;
            let mut row = runtime.monitor.row(id).await?;
            if row["platform"] != "openai" || row["type"] != "oauth" {
                return Err(Error::new(422, "持续预约仅支持 OpenAI OAuth 账号"));
            }
            if refresh {
                let account = runtime.actions.account(id, None).await?;
                runtime.usage_action(id, &json!({"action":"query_usage","expected_version":crate::compat::account_version(&account)}), key, None).await?;
                row = runtime.monitor.row(id).await?;
            }
            let at = runtime.monitor.now();
            let state = runtime.monitor.store.shared_snapshot()?;
            let saved = Self::cycle_saved(&row, &state, at)?;
            let evidence = if Self::cycle_needs_evidence(&row, &saved) {
                crate::credit_rules::upstream_evidence(&runtime.db, &row, at).await?
            } else {
                Value::Null
            };
            operations::observe_cycle(&row, &saved, &evidence, at)
        })
    }
    fn cycle_observations<'a>(
        &'a self,
        accounts: &'a [i64],
    ) -> BoxFuture<'a, Result<std::collections::BTreeMap<i64, Result<operations::CycleObservation>>>>
    {
        Box::pin(async move {
            let runtime = self.runtime()?;
            let rows = runtime
                .db
                .all(
                    &format!("{} AND id=ANY($1)", crate::credit_ownership::ACCOUNT_SELECT),
                    &[crate::db::Param::Ints(accounts.to_vec())],
                )
                .await?;
            let at = runtime.monitor.now();
            let state = runtime.monitor.store.shared_snapshot()?;
            let mut observations = std::collections::BTreeMap::new();
            let mut saved_rows = Vec::new();
            for row in rows {
                let id = integer(&row["id"]).unwrap_or(0);
                match Self::cycle_saved(&row, &state, at) {
                    Ok(saved) => saved_rows.push((row, saved)),
                    Err(error) => {
                        observations.insert(id, Err(error));
                    }
                }
            }
            let evidence_rows = saved_rows
                .iter()
                .filter(|(row, saved)| Self::cycle_needs_evidence(row, saved))
                .map(|(row, _)| row.clone())
                .collect::<Vec<_>>();
            let evidence =
                crate::credit_rules::upstream_evidence_batch(&runtime.db, &evidence_rows, at)
                    .await?;
            for (row, saved) in saved_rows {
                let id = integer(&row["id"]).unwrap_or(0);
                observations.insert(
                    id,
                    operations::observe_cycle(
                        &row,
                        &saved,
                        evidence.get(&id).unwrap_or(&Value::Null),
                        at,
                    ),
                );
            }
            Ok(observations)
        })
    }
    fn dispatch<'a>(
        &'a self,
        row: &'a Value,
        job: &'a Value,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            let runtime = self.runtime()?;
            let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
            let mut payload = job["payload"].clone();
            payload["expected_version"] = crate::compat::account_version(row).into();
            let result = match string(&job["action"]) {
                "schedulable" => runtime.set_schedulable(id, &payload, key).await,
                "recover" => runtime.recover_account(id, &payload, key).await,
                "degradation_mark" => {
                    payload["expected_mark_version"] = job["expected_version"].clone();
                    runtime.set_degradation(id, &payload).await
                }
                "delete" => runtime.delete_account(id, &payload, key).await,
                "usage" | "reset_quota" => {
                    let mut result = runtime.usage_action(id, &payload, key, None).await?;
                    // Billing samples live only in the quota service's memory.
                    // Durable operation receipts retain the ordinary usage result.
                    if let Some(usage) = result["account"]["usage"].as_object_mut() {
                        usage.remove("key_quota");
                    }
                    Ok(result)
                }
                _ => Err(Error::new(422, "未知账号操作")),
            };
            runtime.invalidate_snapshot().await;
            result
        })
    }
    fn model_get(&self, id: &str) -> Result<Value> {
        self.runtime()?.tests.get(id)
    }
    fn account_deleted(&self, id: i64) {
        if let Ok(runtime) = self.runtime() {
            runtime.key_quota.forget(id);
            let _ = runtime.key_quota.remove_account_settings(id);
            runtime.monitor.queries.forget(id);
        }
    }
    fn model_jobs(&self) -> Result<Vec<Value>> {
        Ok(self.runtime()?.tests.shared_data()?["jobs"]
            .as_object()
            .unwrap()
            .values()
            .filter(|job| {
                matches!(
                    string(&job["status"]),
                    "queued" | "running" | "retrying" | "needs_confirmation"
                )
            })
            .map(|job| {
                Value::Object(
                    [
                        "id",
                        "account_id",
                        "account_name",
                        "status",
                        "error",
                        "requested_model",
                        "concurrency",
                    ]
                    .into_iter()
                    .filter_map(|key| job.get(key).map(|value| (key.to_owned(), value.clone())))
                    .collect(),
                )
            })
            .collect())
    }
    fn model_cancel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move { self.runtime()?.tests.cancel(id).await })
    }
    fn test_config(&self) -> Result<(String, usize)> {
        let config = self.runtime()?.config.current()?;
        let configured = &config["oauth_recovery_test_model_id"];
        let model = if truthy(configured) {
            text(configured).trim().to_owned()
        } else {
            crate::model_downgrade::DEFAULT_TEST_MODEL.to_owned()
        };
        Ok((
            model,
            integer(&config["oauth_recovery_test_concurrency"])
                .unwrap_or(2)
                .clamp(1, 8) as usize,
        ))
    }
    fn test_slots(&self) -> Option<crate::execution::TestSlots> {
        self.runtime().ok().map(|runtime| runtime.slots.clone())
    }
}
