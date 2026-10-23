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
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.runtime()
            .map(|runtime| runtime.monitor.now())
            .unwrap_or_else(|_| chrono::Utc::now())
    }
    fn mark(&self, id: i64) -> Result<Value> {
        self.runtime()?.detection.mark(id)
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
                "usage" | "reset_quota" => runtime.usage_action(id, &payload, key, None).await,
                _ => Err(Error::new(422, "未知账号操作")),
            };
            runtime.invalidate().await;
            result
        })
    }
    fn model_get(&self, id: &str) -> Result<Value> {
        self.runtime()?.tests.get(id)
    }
    fn model_jobs(&self) -> Result<Vec<Value>> {
        Ok(self.runtime()?.tests.data()?["jobs"]
            .as_object()
            .unwrap()
            .values()
            .map(model_tests::ModelTests::public)
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
