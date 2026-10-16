//! Private, durable control and admission rules for account keepalive.
use crate::{
    compat::{digest, parse_datetime, string, truthy},
    error::{Error, Result},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const LIMIT: usize = 5;
pub const CONCURRENCY: usize = 8;

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Control {
    pub enabled: bool,
    pub generation: u64,
    #[serde(default)]
    pub last_restore_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub restores: Vec<DateTime<Utc>>,
    #[serde(default)]
    pub paused_reason: String,
    #[serde(default)]
    pub pending: Option<Intent>,
    // Unknown outcomes reserve capacity too, so uncertainty cannot evade the
    // hourly budget when a new upstream incident replaces the previous one.
    #[serde(default)]
    pub reservations: Vec<Reservation>,
    #[serde(default)]
    pub failures: u32,
    #[serde(default)]
    pub retry_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub notifications: Vec<Notice>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Reservation {
    pub id: String,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Intent {
    pub id: String,
    pub generation: u64,
    pub fingerprint: String,
    pub before_until: DateTime<Utc>,
    pub reason: String,
    pub dispatched_at: DateTime<Utc>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Notice {
    pub id: String,
    pub generation: u64,
    pub status: String,
    pub attempts: u32,
    pub next_at: Option<DateTime<Utc>>,
}

pub fn invalid() -> Error {
    Error::new(503, "账号保活状态无效")
}

pub fn validate(value: &Value) -> Result<()> {
    if value["version"] != 1 {
        return Err(invalid());
    }
    let accounts = value["accounts"].as_object().ok_or_else(invalid)?;
    for (id, value) in accounts {
        if !id
            .parse::<i64>()
            .is_ok_and(|number| number > 0 && number.to_string() == *id)
        {
            return Err(invalid());
        }
        let control: Control = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
        if control.generation > 9_007_199_254_740_991
            || !matches!(control.paused_reason.as_str(), "" | "恢复过于频繁")
            || control.restores.len() > LIMIT
            || control.reservations.len() > LIMIT
            || control.notifications.iter().any(|n| {
                n.attempts > 5
                    || n.id.is_empty()
                    || !matches!(
                        n.status.as_str(),
                        "pending"
                            | "sending"
                            | "delivered"
                            | "failed"
                            | "suppressed"
                            | "needs_confirmation"
                    )
            })
            || control.pending.as_ref().is_some_and(|p| {
                p.id.is_empty()
                    || p.fingerprint.len() != 64
                    || !matches!(p.reason.as_str(), "临时错误策略" | "流响应超时")
                    || !control.reservations.iter().any(|r| r.id == p.id)
            })
        {
            return Err(invalid());
        }
    }
    Ok(())
}

impl Control {
    pub fn version(&self) -> String {
        digest(&json!({"enabled":self.enabled,"generation":self.generation}))
    }
    pub fn prune(&mut self, at: DateTime<Utc>) {
        self.restores
            .retain(|v| at.signed_duration_since(*v) < Duration::hours(1));
        self.reservations.retain(|v| {
            at.signed_duration_since(v.at) < Duration::hours(1)
                || self.pending.as_ref().is_some_and(|p| p.id == v.id)
        });
        // Keep all unsent notices and a small delivered tail. A notice belongs
        // to a unique generation, never to an account name or an upstream body.
        let mut completed = 0;
        self.notifications.reverse();
        self.notifications.retain(|n| {
            if matches!(
                n.status.as_str(),
                "pending" | "sending" | "needs_confirmation"
            ) {
                true
            } else {
                completed += 1;
                completed <= 32
            }
        });
        self.notifications.reverse();
    }
    pub fn used(&self, at: DateTime<Utc>) -> usize {
        self.restores
            .iter()
            .filter(|t| at.signed_duration_since(**t) < Duration::hours(1))
            .count()
            + self
                .reservations
                .iter()
                .filter(|r| at.signed_duration_since(r.at) < Duration::hours(1))
                .count()
    }
    pub fn next_allowed(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let mut next = self
            .last_restore_at
            .into_iter()
            .chain(self.reservations.iter().map(|r| r.at))
            .filter_map(|t| t.checked_add_signed(Duration::seconds(60)))
            .max();
        if self.used(at) >= LIMIT {
            let oldest = self
                .restores
                .iter()
                .copied()
                .chain(self.reservations.iter().map(|r| r.at))
                .filter(|t| at.signed_duration_since(*t) < Duration::hours(1))
                .min();
            next = next
                .into_iter()
                .chain(oldest.and_then(|t| t.checked_add_signed(Duration::hours(1))))
                .max();
        }
        next.into_iter()
            .chain(self.retry_at)
            .max()
            .filter(|t| *t > at)
    }
    pub fn waiting(&self, at: DateTime<Utc>) -> bool {
        self.used(at) >= LIMIT
            || self.retry_at.is_some_and(|t| t > at)
            || self
                .last_restore_at
                .into_iter()
                .chain(self.reservations.iter().map(|r| r.at))
                .any(|t| at.signed_duration_since(t) < Duration::seconds(60))
    }
}

pub fn fingerprint(row: &Value) -> String {
    digest(&json!([
        row["temp_unschedulable_until"],
        row["temp_unschedulable_reason"]
    ]))
}

/// Only recognized native error-policy records are eligible. A threshold source
/// is excluded before inspecting its optional details, even if those are broken.
pub fn eligible_reason(row: &Value) -> std::result::Result<&'static str, &'static str> {
    let raw = string(&row["temp_unschedulable_reason"]);
    if raw.len() > 16_384 {
        return Err("暂停原因无法识别");
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| "暂停原因无法识别")?;
    if value["source"] == "account_scheduling_threshold" {
        return Err("额度阈值暂停不参与保活");
    }
    if !value.is_object() || truthy(&value["source"]) {
        return Err("暂停原因无法识别");
    }
    let code = value["status_code"].as_i64().ok_or("暂停原因无法识别")?;
    let rule = value["rule_index"].as_i64().ok_or("暂停原因无法识别")?;
    let keyword = value["matched_keyword"]
        .as_str()
        .ok_or("暂停原因无法识别")?;
    let message = value["error_message"].as_str().ok_or("暂停原因无法识别")?;
    let until = value["until_unix"].as_i64().ok_or("暂停原因无法识别")?;
    let triggered = value["triggered_at_unix"]
        .as_i64()
        .ok_or("暂停原因无法识别")?;
    if triggered <= 0
        || until < triggered
        || parse_datetime(&row["temp_unschedulable_until"]).is_none_or(|t| t.timestamp() != until)
        || !(rule >= 0 && (400..=599).contains(&code)
            || rule == -1 && code == 0 && keyword == "stream_timeout")
    {
        return Err("暂停原因无法识别");
    }
    let text = format!("{keyword} {message}").to_lowercase();
    if matches!(code, 401..=403)
        || [
            "banned",
            "deactivated",
            "suspended",
            "organization has been disabled",
            "account disabled",
            "invalid_api_key",
            "unauthorized",
            "authentication",
            "credential",
            "token_revoked",
            "token_invalidated",
            "token expired",
            "invalid_grant",
            "needs_reauth",
            "identity verification",
            "usage_limit_reached",
            "usage limit has been reached",
            "insufficient_quota",
            "quota_exceeded",
            "quota exhausted",
            "quota exceeded",
            "credit balance",
            "credits exhausted",
            "weekly limit reached",
            "monthly limit reached",
            "额度耗尽",
            "额度已用尽",
            "余额不足",
            "封禁",
            "凭证失效",
        ]
        .iter()
        .any(|term| text.contains(term))
    {
        return Err("认证、封禁或额度异常不参与保活");
    }
    Ok(if code == 0 {
        "流响应超时"
    } else {
        "临时错误策略"
    })
}

pub fn row_reason(row: &Value, at: DateTime<Utc>) -> &'static str {
    if !row.is_object() || truthy(&row["deleted_at"]) {
        return "账号已删除";
    }
    if row["status"] != "active" {
        return "账号停用或认证异常";
    }
    if row["schedulable"] != true {
        return "调度已关闭";
    }
    if !row["rate_limited_at"].is_null() || !row["overload_until"].is_null() {
        return "账号限流或过载不参与保活";
    }
    if parse_datetime(&row["temp_unschedulable_until"]).is_none_or(|t| t <= at) {
        return "等待临时不可调度";
    }
    eligible_reason(row).err().unwrap_or("")
}
