use crate::{
    compat::{config_revision, datetime, parse_datetime, pick, string, truthy},
    error::Result,
    oauth_state::query_metadata,
    quota,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
pub const MIN_QUERY_SECONDS: i64 = 3600;
pub const AUTO_WINDOW_SECONDS: i64 = 86400;
pub const AUTO_QUERY_LIMIT: usize = 6;
pub const BACKOFF: [i64; 4] = [3600, 10800, 21600, 43200];
pub const AUTH_ERRORS: [&str; 4] = ["401", "402", "http_401", "http_402"];
pub fn credential_fingerprint(row: &Value) -> String {
    config_revision(&pick(
        &row["credentials"],
        &["access_token", "refresh_token", "id_token"],
    ))
}
pub fn quota_complete(row: &Value, result: &Value) -> bool {
    if !truthy(&result["success"]) {
        return false;
    }
    let summary = quota::summary_from_result(row, result);
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    quota::required_windows(&summary["plan_type"])
        .iter()
        .all(|key| {
            quota::percent(&windows.get(*key).unwrap_or(&Value::Null)["used_percent"]).is_some()
        })
}
pub fn fresh_quota(row: &Value, result: &Value, at: DateTime<Utc>) -> bool {
    if !parse_datetime(&result["queried_at"])
        .is_some_and(|t| t <= at && (at - t).num_milliseconds() <= MIN_QUERY_SECONDS * 1000)
        || !quota_complete(row, result)
    {
        return false;
    }
    let summary = quota::summary_from_result(row, result);
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    quota::required_windows(&summary["plan_type"])
        .iter()
        .all(|key| {
            parse_datetime(&windows.get(*key).unwrap_or(&Value::Null)["reset_at"])
                .is_none_or(|t| t > at)
        })
}
pub fn automatic_eligible(row: &Value, at: DateTime<Utc>) -> bool {
    if row["platform"] != "openai"
        || row["type"] != "oauth"
        || (!row["deleted_at"].is_null() && row["deleted_at"] != "")
        || row["status"] != "active"
        || row["schedulable"] != true
    {
        return false;
    }
    let reason = string(&row["temp_unschedulable_reason"]);
    let threshold: Value = serde_json::from_str(reason).unwrap_or(json!({}));
    if !reason.is_empty()
        && (!threshold.is_object()
            || threshold["source"] != "account_scheduling_threshold"
            || threshold["platform"] != "openai")
    {
        return false;
    }
    if truthy(&row["temp_unschedulable_until"]) && reason.is_empty() {
        return false;
    }
    !(truthy(&row["auto_pause_on_expired"])
        && truthy(&row["expires_at"])
        && parse_datetime(&row["expires_at"]).is_none_or(|expiry| expiry <= at))
}
pub fn reset_not_before(row: &Value, result: &Value) -> Option<DateTime<Utc>> {
    let summary = quota::summary_from_result(row, result);
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    let mut deadlines = [
        "rate_limit_reset_at",
        "temp_unschedulable_until",
        "overload_until",
    ]
    .into_iter()
    .filter_map(|k| parse_datetime(&row[k]))
    .collect::<Vec<_>>();
    for key in quota::required_windows(&summary["plan_type"]) {
        let win = windows.get(*key).unwrap_or(&Value::Null);
        if quota::percent(&win["used_percent"]).is_some_and(|v| v >= 100.0) {
            if let Some(t) = parse_datetime(&win["reset_at"]) {
                deadlines.push(
                    t + Duration::seconds(if win["reset_source"] == "estimated_from_remaining" {
                        60
                    } else {
                        0
                    }),
                );
            }
        }
    }
    deadlines.into_iter().max()
}
pub fn automatic_gate(
    row: &Value,
    saved: &Value,
    meta: &Value,
    at: DateTime<Utc>,
    wait_reset: bool,
    bypass_interval: bool,
    budget: usize,
) -> Result<(&'static str, Option<DateTime<Utc>>)> {
    if !automatic_eligible(row, at) {
        return Ok(("account_ineligible", None));
    }
    let query = query_metadata(meta, saved)?;
    if truthy(&query["auth_fingerprint"])
        && query["auth_fingerprint"] == credential_fingerprint(row)
        || !truthy(&query["auth_fingerprint"])
            && AUTH_ERRORS.contains(&string(&meta["last_error_code"]))
    {
        return Ok(("auth_paused", None));
    }
    let mut bounds = vec![];
    if !bypass_interval {
        if let Some(t) = parse_datetime(&query["last_query_at"]) {
            bounds.push((t + Duration::seconds(MIN_QUERY_SECONDS), "query_cooldown"));
        }
    }
    if let Some(t) = parse_datetime(&query["retry_at"]) {
        bounds.push((t, "query_backoff"));
    }
    if wait_reset {
        if let Some(t) = reset_not_before(row, saved) {
            bounds.push((t, "waiting_reset"));
        }
    }
    let mut attempts = query["automatic_attempts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_datetime)
        .filter(|t| *t > at - Duration::seconds(AUTO_WINDOW_SECONDS))
        .collect::<Vec<_>>();
    attempts.sort();
    if attempts.len() + budget > AUTO_QUERY_LIMIT {
        if let Some(t) = attempts.get(
            (attempts.len() + budget - AUTO_QUERY_LIMIT - 1).min(attempts.len().saturating_sub(1)),
        ) {
            bounds.push((*t + Duration::seconds(AUTO_WINDOW_SECONDS), "query_budget"));
        } else {
            return Ok((
                "query_budget",
                Some(at + Duration::seconds(AUTO_WINDOW_SECONDS)),
            ));
        }
    }
    Ok(bounds
        .into_iter()
        .max()
        .filter(|(t, _)| *t > at)
        .map(|(t, reason)| (reason, Some(t)))
        .unwrap_or(("", None)))
}
pub fn gate_result(reason: &str, deadline: Option<DateTime<Utc>>) -> Value {
    json!({"success":false,"skipped":true,"error_code":reason,"next_query_at":deadline.map(datetime)})
}
