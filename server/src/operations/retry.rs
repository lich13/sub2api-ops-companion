use super::*;

impl Operations {
    pub(super) fn needs_key(job: &Value) -> bool {
        matches!(string(&job["status"]), "queued" | "running" | "checking")
            || job["status"] == "completed" && job["repeat_state"] == "active"
    }

    pub(super) fn retry_scheduled(&self, job: &Value, result: &Value) -> Result<bool> {
        if !crate::connection_retry::retryable(result) {
            return Ok(false);
        }
        let policy = crate::connection_retry::Policy::saved(job)?.ok_or_else(Error::internal)?;
        let attempts = integer(&job["attempt_count"]).unwrap_or(1);
        let Some(next) = policy.next(attempts, self.backend.now()) else {
            return Ok(false);
        };
        self.update(
            string(&job["id"]),
            json!({"status":"queued","result":result,
            "next_retry_at":next,"reason":"等待连接测试重试"}),
        )?;
        Ok(true)
    }
}
