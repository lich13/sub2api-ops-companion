use crate::{
    compat::{parse_datetime, string},
    quota,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::types::BigDecimal;
use std::str::FromStr;
fn number(value: &Value) -> Option<BigDecimal> {
    if value.is_boolean() || value.is_null() {
        return None;
    }
    let parsed = BigDecimal::from_str(&crate::compat::text(value)).ok()?;
    (parsed >= 0).then_some(parsed)
}
fn fresh(value: &Value, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    parse_datetime(value)
        .filter(|at| *at <= now && now.signed_duration_since(*at).num_milliseconds() <= 3_600_000)
}
pub fn credits(row: &Value, now: DateTime<Utc>) -> &'static str {
    let snapshot = &row["extra"]["codex_credits_snapshot"];
    if !snapshot.is_object() || fresh(&snapshot["fetched_at"], now).is_none() {
        return "unknown";
    }
    let credits = &snapshot["credits"];
    let (Some(has), Some(unlimited)) = (
        credits["has_credits"].as_bool(),
        credits["unlimited"].as_bool(),
    ) else {
        return "unknown";
    };
    let balance = number(&credits["balance"]);
    if !credits["balance"].is_null() && balance.is_none() {
        return "unknown";
    }
    if unlimited {
        return "available";
    }
    if balance.is_some_and(|value| (value > 0) != has) {
        return "unknown";
    }
    if has {
        "available"
    } else {
        "empty"
    }
}
pub fn reason(row: &Value, now: DateTime<Utc>, result: &Value) -> &'static str {
    for (field, reason) in [
        ("rate_limit_reset_at", "账号仍在上游限流中"),
        ("overload_until", "账号仍在上游过载等待中"),
        ("temp_unschedulable_until", "账号暂不可调度"),
    ] {
        if parse_datetime(&row[field]).is_some_and(|until| until > now) {
            return reason;
        }
    }
    let extra = &row["extra"];
    if row["type"] == "apikey" {
        for (prefix, days) in [("quota_daily", 1), ("quota_weekly", 7), ("quota", 0)] {
            let limit = number(&extra[format!("{prefix}_limit")]);
            let used = number(
                extra
                    .get(format!("{prefix}_used"))
                    .unwrap_or(&Value::from(0)),
            );
            let (Some(limit), Some(used)) = (limit, used) else {
                continue;
            };
            if limit == 0 || used < limit {
                continue;
            }
            if days > 0 {
                let reset = if extra[format!("{prefix}_reset_mode")] == "fixed" {
                    parse_datetime(&extra[format!("{prefix}_reset_at")])
                } else {
                    parse_datetime(&extra[format!("{prefix}_start")])
                        .map(|start| start + Duration::days(days))
                };
                if reset.is_some_and(|reset| reset <= now) {
                    continue;
                }
            }
            return "已确认额度耗尽";
        }
        return "";
    }
    let observations = [
        parse_datetime(&row["last_success_at"]),
        parse_datetime(&extra["codex_usage_updated_at"]),
        parse_datetime(&extra["codex_credits_snapshot"]["fetched_at"]),
    ];
    let mut rejections = vec![(&row["last_error_code"], &row["last_error_at"])];
    if result["success"] == false {
        rejections.push((&result["error_code"], &result["queried_at"]));
    }
    for (code, at) in rejections {
        let Some(at) = fresh(at, now) else {
            continue;
        };
        if parse_datetime(&row["rate_limit_reset_at"])
            .is_some_and(|reset| at <= reset && reset <= now)
        {
            continue;
        }
        if matches!(
            string(code).to_lowercase().as_str(),
            "insufficient_quota" | "quota_exceeded" | "usage_limit_reached"
        ) && !observations
            .iter()
            .flatten()
            .any(|value| *value > at && *value <= now)
        {
            return "上游已拒绝额度请求";
        }
    }
    if credits(row, now) == "empty"
        && quota::usage_windows(row, now, result).iter().any(|window| {
            window["status"] == "known"
                && window["used_percent"].as_f64().is_some_and(|v| v >= 100.0)
        })
    {
        "已确认额度耗尽"
    } else {
        ""
    }
}
