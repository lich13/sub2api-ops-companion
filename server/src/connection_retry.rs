use crate::{
    compat::{datetime, integer, string, truthy},
    error::{Error, Result},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct Policy {
    pub interval_seconds: i64,
    pub max_retries: i64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            interval_seconds: 10,
            max_retries: 3,
        }
    }
}
impl Policy {
    pub fn config(config: &Value) -> Result<Self> {
        let defaults = Self::default();
        let policy = Self {
            interval_seconds: integer(&config["connection_test_retry_interval_seconds"])
                .unwrap_or(defaults.interval_seconds),
            max_retries: integer(&config["connection_test_max_retries"])
                .unwrap_or(defaults.max_retries),
        };
        policy.validate()?;
        Ok(policy)
    }
    pub fn saved(value: &Value) -> Result<Option<Self>> {
        value
            .get("retry_policy")
            .map(|raw| {
                let policy: Self = serde_json::from_value(raw.clone())
                    .map_err(|_| Error::new(503, "连接测试重试配置无效"))?;
                policy.validate()?;
                Ok(policy)
            })
            .transpose()
    }
    fn validate(self) -> Result<()> {
        if (1..=3600).contains(&self.interval_seconds) && (0..=10).contains(&self.max_retries) {
            Ok(())
        } else {
            Err(Error::new(503, "连接测试重试配置无效"))
        }
    }
    pub fn next(self, completed_attempts: i64, at: DateTime<Utc>) -> Option<String> {
        (completed_attempts <= self.max_retries)
            .then(|| datetime(at + Duration::seconds(self.interval_seconds)))
    }
}

pub fn retryable(result: &Value) -> bool {
    let code = string(&result["error_code"]).trim().to_ascii_lowercase();
    result["success"] == false
        && !truthy(&result["uncertain"])
        && !truthy(&result["skipped"])
        && crate::recovery_events::auth_code(&code).is_empty()
        && !matches!(
            code.as_str(),
            "403"
                | "http_403"
                | "account_not_found"
                | "manual_intervention"
                | "invalid_grant"
                | "unauthorized"
        )
}
