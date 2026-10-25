use crate::{
    compat::{datetime, integer, parse_datetime, string, truthy},
    quota::{nonnegative, usage_windows, window},
};
use chrono::{DateTime, Duration, FixedOffset, Timelike, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn reset_credits(extra: &Value, at: DateTime<Utc>) -> Value {
    let snapshot = &extra["codex_reset_credit_snapshot"];
    let mut count = nonnegative(&snapshot["available_count"]);
    let mut expires = vec![];
    let mut alive = 0;
    if let Some(credits) = snapshot["credits"].as_array() {
        for credit in credits.iter().filter(|v| v.is_object()) {
            let expiration = parse_datetime(&credit["expires_at"]);
            if expiration.is_some_and(|v| v <= at) {
                continue;
            }
            alive += 1;
            if let Some(expiration) = expiration {
                expires.push(datetime(expiration));
            }
        }
        if !credits.is_empty() {
            count = count.map(|v| v.min(alive as f64));
        }
    }
    expires.sort();
    expires.dedup();
    let observed = if truthy(&snapshot["fetched_at"]) {
        &snapshot["fetched_at"]
    } else {
        &snapshot["updated_at"]
    };
    json!({"available":count.map(|v|v as i64),"expires_at":expires,"observed_at":parse_datetime(observed).map(datetime)})
}

fn first(values: &[Value]) -> Value {
    values
        .iter()
        .find(|v| truthy(v))
        .cloned()
        .unwrap_or(Value::Null)
}
fn grok_free(extra: &Value, billing: &Value) -> bool {
    let free = |v: &str| v.contains("free") || v.contains("basic");
    let paid = |v: &str| !v.is_empty() && !free(v) && !v.contains("unknown");
    let tier = string(&extra["subscription_tier"]).trim().to_lowercase();
    let plan = string(&billing["plan"]).trim().to_lowercase();
    if free(&tier) {
        return true;
    }
    if paid(&tier)
        || ["usage_percent", "used_percent"]
            .into_iter()
            .any(|k| nonnegative(&billing[k]).is_some())
        || nonnegative(&billing["monthly_limit_cents"]).is_some_and(|v| v > 0.0)
    {
        return false;
    }
    if paid(&plan) {
        return false;
    }
    free(&plan)
        || free(&string(&extra["grok_entitlement_status"]).to_lowercase())
        || truthy(billing)
}

pub fn project(row: &Value, at: DateTime<Utc>, result: &Value) -> Value {
    let extra = &row["extra"];
    let mut value =
        json!({"branch":"none","windows":[],"today":null,"actions":[],"reset_credits":null});
    let mut windows = vec![];
    let stamp = json!(datetime(at));
    if matches!(
        string(&row["type"]),
        "apikey" | "upstream" | "bedrock" | "service_account"
    ) {
        value["branch"] = "apikey".into();
        let midnight = at
            .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
            .with_hour(0)
            .unwrap()
            .with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap();
        value["today_start"] = midnight.to_rfc3339().into();
        for (key, label, prefix, days, color) in [
            ("daily", "1d", "quota_daily", 1, "indigo"),
            ("weekly", "7d", "quota_weekly", 7, "emerald"),
            ("total", "总", "quota", 0, "purple"),
        ] {
            let Some(limit) = nonnegative(&extra[format!("{prefix}_limit")]).filter(|v| *v > 0.0)
            else {
                continue;
            };
            let used = extra
                .get(format!("{prefix}_used"))
                .map(nonnegative)
                .unwrap_or(Some(0.0));
            let reset = if days == 0 {
                None
            } else if extra[format!("{prefix}_reset_mode")] == "fixed" {
                parse_datetime(&extra[format!("{prefix}_reset_at")])
            } else {
                parse_datetime(&extra[format!("{prefix}_start")])
                    .and_then(|v| v.checked_add_signed(Duration::days(days)))
            };
            let mut item = window(
                key,
                label,
                used.map(|v| v / limit * 100.0),
                &json!(reset.map(datetime)),
                &stamp,
                at,
                "configured_quota",
                false,
            );
            item["color"] = color.into();
            item["used"] = if extra.get(format!("{prefix}_used")).is_none() {
                json!(0)
            } else {
                json!(used)
            };
            item["limit"] = json!(limit);
            windows.push(item);
        }
    } else if row["platform"] == "openai" && row["type"] == "oauth" {
        value["branch"] = "openai_oauth".into();
        value["actions"] = json!(["query_usage", "query_reset_credits"]);
        value["reset_credits"] = reset_credits(extra, at);
        if !truthy(&row["parent_account_id"]) {
            value["actions"]
                .as_array_mut()
                .unwrap()
                .push("reset_quota".into());
        }
        for mut item in usage_windows(row, at, result) {
            let short = item["key"] == "codex_5h";
            let duration = item["window_minutes"]
                .as_i64()
                .map(Duration::minutes)
                .unwrap_or_else(|| {
                    if short {
                        Duration::hours(5)
                    } else {
                        Duration::days(7)
                    }
                });
            let start = parse_datetime(&item["reset_at"])
                .filter(|v| *v > at)
                .and_then(|v| v.checked_sub_signed(duration))
                .filter(|v| *v <= at)
                .unwrap_or(at - duration);
            item["color"] = if short { "indigo" } else { "emerald" }.into();
            item["stats_start"] = datetime(start).into();
            windows.push(item);
        }
    } else if row["platform"] == "grok" && row["type"] == "oauth" {
        let billing = if extra["grok_billing_snapshot"].is_object() {
            extra["grok_billing_snapshot"].clone()
        } else {
            json!({})
        };
        let snapshot = &extra["grok_usage_snapshot"];
        let mut extra = extra
            .as_object()
            .cloned()
            .map(Value::Object)
            .unwrap_or(json!({}));
        extra["subscription_tier"] = first(&[
            row["quota_grok_tier"].clone(),
            snapshot["subscription_tier"].clone(),
            extra["subscription_tier"].clone(),
            billing["plan"].clone(),
        ]);
        extra["grok_entitlement_status"] = first(&[
            row["quota_grok_entitlement"].clone(),
            snapshot["entitlement_status"].clone(),
            extra["grok_entitlement_status"].clone(),
        ]);
        let free = grok_free(&extra, &billing);
        value["branch"] = if free { "grok_free" } else { "grok_paid" }.into();
        value["actions"] = json!(["probe_quota"]);
        if free {
            let mut item = window(
                "grok_24h",
                "24h",
                None,
                &Value::Null,
                &stamp,
                at,
                "local_24h",
                false,
            );
            item["color"] = "emerald".into();
            item["stats_start"] = datetime(at - Duration::hours(24)).into();
            windows.push(item);
        } else {
            let limit = nonnegative(&billing["monthly_limit_cents"]);
            let used = nonnegative(&billing["used_cents"]);
            let mut monthly = nonnegative(&billing["used_percent"]).or_else(|| {
                limit
                    .zip(used)
                    .filter(|(l, _)| *l > 0.0)
                    .map(|(l, u)| u / l * 100.0)
            });
            let weekly_period = string(&billing["period_type"]).to_lowercase() == "weekly";
            let weekly = if weekly_period {
                nonnegative(&billing["usage_percent"])
            } else {
                None
            };
            if weekly_period && limit.is_none() {
                monthly = None;
            }
            let failed = billing["failed_windows"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for (key, label, percent, start, end) in [
                (
                    "weekly",
                    "7d",
                    weekly,
                    billing["period_start"].clone(),
                    billing["period_end"].clone(),
                ),
                (
                    "monthly",
                    "30d",
                    monthly,
                    billing["billing_period_start"].clone(),
                    first(&[
                        billing["billing_period_end"].clone(),
                        billing["period_end"].clone(),
                    ]),
                ),
            ] {
                if percent.is_none() && !failed.contains(&json!(key)) {
                    continue;
                }
                let code = nonnegative(&first(&[
                    billing[format!("{key}_status_code")].clone(),
                    billing["status_code"].clone(),
                ]));
                let observed = first(&[
                    billing[format!("{key}_updated_at")].clone(),
                    billing["fetched_at"].clone(),
                    billing["updated_at"].clone(),
                ]);
                let failure = failed.contains(&json!(key))
                    || truthy(&billing["partial"]) && failed.is_empty()
                    || code.is_some_and(|v| v != 0.0 && !(200.0..300.0).contains(&v));
                let mut item = window(
                    &format!("grok_{key}"),
                    label,
                    percent.map(|v| v.min(100.0)),
                    &end,
                    &observed,
                    at,
                    "billing_probe",
                    failure,
                );
                item["color"] = "indigo".into();
                item["stats_start"] = json!(parse_datetime(&start)
                    .zip(parse_datetime(&end))
                    .filter(|(s, e)| *s <= at && at < *e)
                    .map(|(s, _)| datetime(s)));
                windows.push(item);
            }
            value["prepaid_balance"] = json!(nonnegative(&billing["prepaid_balance"]));
            value["monthly_limit"] =
                json!(nonnegative(&billing["monthly_limit"]).or_else(|| limit.map(|v| v / 100.0)));
            value["monthly_used"] =
                json!(nonnegative(&billing["monthly_used"]).or_else(|| used.map(|v| v / 100.0)));
        }
    }
    value["windows"] = windows.into();
    if row["type"] == "apikey" {
        value["native_read"] = true.into();
        let actions = value["actions"].as_array_mut().expect("usage actions");
        if !actions.iter().any(|a| a == "query_usage") {
            actions.insert(0, json!("query_usage"));
        }
    }
    crate::account_usage::extend(row, &mut value, at);
    value
}

pub fn stats_specs(id: i64, usage: &Value) -> Vec<Value> {
    let mut specs: Vec<Value> = usage["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| truthy(&v["stats_start"]))
        .map(|v| json!({"account_id":id,"key":v["key"],"start_at":v["stats_start"]}))
        .collect();
    if truthy(&usage["today_start"]) {
        specs.push(json!({"account_id":id,"key":"today","start_at":usage["today_start"]}));
    }
    specs
}
pub type Stats = BTreeMap<(i64, String), Value>;
pub fn stats_rows(rows: Vec<Value>) -> Stats {
    rows.into_iter().filter_map(|row|Some(((integer(&row["account_id"])?,string(&row["key"]).into()),json!({
        "requests":integer(&row["requests"]),"tokens":integer(&row["tokens"]),
        "cost":crate::compat::float(&row["cost"]),"standard_cost":crate::compat::float(&row["standard_cost"]),"user_cost":crate::compat::float(&row["user_cost"])
    })))).collect()
}
pub fn attach_stats(id: i64, usage: &mut Value, stats: &Stats, free_limit: i64) {
    usage.as_object_mut().unwrap().shift_remove("today_start");
    usage["today"] = stats
        .get(&(id, "today".into()))
        .cloned()
        .unwrap_or(Value::Null);
    for item in usage["windows"].as_array_mut().unwrap() {
        item.as_object_mut().unwrap().shift_remove("stats_start");
        let data = stats
            .get(&(id, string(&item["key"]).to_owned()))
            .cloned()
            .unwrap_or(Value::Null);
        item["stats"] = data.clone();
        if item["key"] == "grok_24h" && !data.is_null() {
            item["used_percent"] = json!((crate::compat::float(&data["tokens"]).unwrap_or(0.0)
                / free_limit as f64
                * 100.0)
                .min(100.0));
            item["status"] = "known".into();
            item["limit"] = free_limit.into();
            item["used"] = data["tokens"].clone();
        }
        if item["key"] == "codex_7d" {
            if let Some((cost, percent)) = crate::compat::float(&data["cost"])
                .zip(crate::compat::float(&item["used_percent"]))
                .filter(|(c, p)| *c > 0.0 && *p > 0.0)
            {
                item["estimated_total_cost"] = json!(cost * 100.0 / percent);
            }
        }
    }
}
