//! Saved quota evidence and the window rules used by both UI and scheduling.
use crate::compat::{datetime, float, parse_datetime, string, truthy};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};

pub fn number(value: &Value) -> Option<f64> {
    float(value).filter(|v| v.is_finite())
}
pub fn nonnegative(value: &Value) -> Option<f64> {
    number(value).filter(|v| *v >= 0.0)
}
pub fn percent(value: &Value) -> Option<f64> {
    if let Some(value) = value.as_str() {
        value
            .trim()
            .trim_end_matches('%')
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
    } else {
        number(value)
    }
}
pub fn plan_type(value: &Value) -> String {
    let mut text = string(value).trim();
    for prefix in ["计划 ", "Codex ", "codex "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
        }
    }
    let lower = text.to_lowercase();
    if text.is_empty() {
        "oauth".into()
    } else if matches!(
        lower.as_str(),
        "free" | "plus" | "pro" | "team" | "enterprise" | "oauth"
    ) {
        lower
    } else {
        text.into()
    }
}
pub fn account_plan(row: &Value) -> String {
    [
        &row["credentials"]["plan_type"],
        &row["credentials"]["chatgpt_plan_type"],
        &row["extra"]["plan_type"],
    ]
    .into_iter()
    .find(|v| !string(v).trim().is_empty())
    .map(plan_type)
    .unwrap_or_else(|| "oauth".into())
}
pub fn required_windows(plan: &Value) -> &'static [&'static str] {
    if plan_type(plan) == "free" {
        &["codex_7d"]
    } else {
        &["codex_5h", "codex_7d"]
    }
}

pub fn reset_details(
    explicit: &Value,
    remaining: &Value,
    queried: &Value,
    at: Option<DateTime<Utc>>,
) -> (String, String) {
    if let Some(time) = parse_datetime(explicit) {
        return (datetime(time), "server_exact".into());
    }
    let Some(seconds) = number(remaining) else {
        return (String::new(), String::new());
    };
    let Some(base) = parse_datetime(queried).or(at) else {
        return (String::new(), String::new());
    };
    if seconds.max(0.0) > i64::MAX as f64 {
        return (String::new(), String::new());
    }
    if let Some(time) =
        Duration::try_seconds(seconds.max(0.0) as i64).and_then(|v| base.checked_add_signed(v))
    {
        (datetime(time), "estimated_from_remaining".into())
    } else {
        (String::new(), String::new())
    }
}
fn reset_source(window: &Value) -> &str {
    match string(&window["reset_source"]) {
        "server_exact" => "server_exact",
        "estimated_from_remaining" => "estimated_from_remaining",
        _ if number(&window["reset_after_seconds"]).is_some() => "estimated_from_remaining",
        _ => "server_exact",
    }
}
fn window_key(window: &Value) -> String {
    let key = string(&window["key"]).trim();
    if !key.is_empty() {
        key.into()
    } else {
        match string(&window["label"]).trim().to_lowercase().as_str() {
            "5h" => "codex_5h".into(),
            "7d" => "codex_7d".into(),
            _ => String::new(),
        }
    }
}
pub fn windows_by_key(value: &Value) -> Map<String, Value> {
    let mut windows = Map::new();
    for raw in value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v.is_object())
    {
        let key = window_key(raw);
        if key.is_empty() {
            continue;
        }
        let mut item = raw.clone();
        item["key"] = key.clone().into();
        windows.insert(key, item);
    }
    windows
}
pub fn with_recovery_metadata(mut summary: Value) -> Value {
    for key in [
        "recovery_due_at",
        "recovery_fingerprint",
        "recovery_reset_source",
        "recovery_window_keys",
    ] {
        summary.as_object_mut().unwrap().shift_remove(key);
    }
    let windows = windows_by_key(&summary["ui_windows"]);
    let depleted: Vec<&Value> = required_windows(&summary["plan_type"])
        .iter()
        .filter_map(|key| windows.get(*key))
        .filter(|v| percent(&v["used_percent"]).is_some_and(|v| v >= 100.0))
        .collect();
    if depleted.is_empty() {
        return summary;
    }
    let Some(times): Option<Vec<_>> = depleted
        .iter()
        .map(|v| parse_datetime(&v["reset_at"]))
        .collect()
    else {
        return summary;
    };
    summary["recovery_due_at"] = datetime(*times.iter().max().unwrap()).into();
    summary["recovery_fingerprint"] = depleted
        .iter()
        .zip(&times)
        .map(|(w, t)| format!("{}@{}", string(&w["key"]), datetime(*t)))
        .collect::<Vec<_>>()
        .join("|")
        .into();
    summary["recovery_reset_source"] =
        if depleted.iter().all(|v| reset_source(v) == "server_exact") {
            "server_exact"
        } else {
            "estimated_from_remaining"
        }
        .into();
    summary["recovery_window_keys"] = json!(depleted
        .iter()
        .map(|v| v["key"].clone())
        .collect::<Vec<_>>());
    summary
}

pub fn quota_windows(row: &Value, at: Option<DateTime<Utc>>) -> Value {
    let extra = &row["extra"];
    let plan = account_plan(row);
    let updated = string(&extra["codex_usage_updated_at"]);
    let mut windows = vec![];
    for (key, label) in [("codex_5h", "5h"), ("codex_7d", "7d")] {
        if plan == "free" && key == "codex_5h" {
            continue;
        }
        let Some(used) = percent(&extra[format!("{key}_used_percent")]) else {
            continue;
        };
        let remaining = number(&extra[format!("{key}_reset_after_seconds")]);
        let minutes = number(&extra[format!("{key}_window_minutes")])
            .filter(|v| (1.0..=5_256_000.0).contains(v) && v.fract() == 0.0);
        let (reset, source) = reset_details(
            &extra[format!("{key}_reset_at")],
            &json!(remaining),
            &json!(updated),
            at,
        );
        let mut value = json!({"key":key,"label":label,"used_percent":used.clamp(0.0,100.0),"remaining_percent":(100.0-used).clamp(0.0,100.0),"depleted":used >= 100.0});
        if !reset.is_empty() {
            value["reset_at"] = reset.into();
            value["reset_source"] = source.into();
        }
        if let Some(v) = remaining {
            value["reset_after_seconds"] = (v as i64).into();
        }
        if let Some(v) = minutes {
            value["window_minutes"] = (v as i64).into();
        }
        windows.push(value);
    }
    with_recovery_metadata(
        json!({"plan_type":plan,"updated_at":parse_datetime(&json!(updated)).map(datetime).unwrap_or_else(||updated.to_owned()),"ui_windows":windows}),
    )
}

pub fn sanitize_summary(row: &Value, summary: &Value) -> Value {
    let mut summary = summary.clone();
    let plan = account_plan(row);
    summary["plan_type"] = plan.clone().into();
    let updated = parse_datetime(&summary["updated_at"])
        .map(datetime)
        .unwrap_or_default();
    if !updated.is_empty() {
        summary["updated_at"] = updated.clone().into();
    }
    let mut windows = vec![];
    for raw in summary["ui_windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v.is_object())
    {
        let mut value = raw.clone();
        let key = window_key(raw);
        if !key.is_empty() {
            value["key"] = key.clone().into();
        }
        if plan == "free" && key == "codex_5h" {
            continue;
        }
        let (reset, source) = reset_details(
            &raw["reset_at"],
            &raw["reset_after_seconds"],
            &json!(updated),
            None,
        );
        if !reset.is_empty() {
            value["reset_at"] = reset.into();
            value["reset_source"] = if parse_datetime(&raw["reset_at"]).is_some() {
                reset_source(raw).to_owned()
            } else {
                source
            }
            .into();
        } else {
            value.as_object_mut().unwrap().shift_remove("reset_at");
            value.as_object_mut().unwrap().shift_remove("reset_source");
        }
        windows.push(value);
    }
    summary["ui_windows"] = windows.into();
    with_recovery_metadata(summary)
}

pub fn from_usage_data(data: &Value, row: &Value, at: DateTime<Utc>) -> Value {
    let mut row = row.clone();
    let mut extra = row["extra"].as_object().cloned().unwrap_or_default();
    for (source, prefix) in [("five_hour", "codex_5h"), ("seven_day", "codex_7d")] {
        for suffix in [
            "used_percent",
            "reset_at",
            "reset_after_seconds",
            "window_minutes",
            "reset_source",
        ] {
            extra.shift_remove(&format!("{prefix}_{suffix}"));
        }
        let source = &data[source];
        if !source.is_object() {
            continue;
        }
        let used = source
            .get("utilization")
            .or_else(|| source.get("used_percent"))
            .or_else(|| source.get("used"));
        if let Some(used) = used.filter(|v| !v.is_null()) {
            extra.insert(format!("{prefix}_used_percent"), used.clone());
        }
        if let Some(reset) =
            parse_datetime(&source["resets_at"]).or_else(|| parse_datetime(&source["reset_at"]))
        {
            extra.insert(format!("{prefix}_reset_at"), datetime(reset).into());
        }
        if let Some(remaining) =
            number(&source["remaining_seconds"]).or_else(|| number(&source["reset_after_seconds"]))
        {
            extra.insert(format!("{prefix}_reset_after_seconds"), json!(remaining));
        }
        let minutes = source
            .get("window_minutes")
            .unwrap_or(&source["window_stats"]["window_minutes"]);
        if !minutes.is_null() {
            extra.insert(format!("{prefix}_window_minutes"), minutes.clone());
        }
    }
    extra.insert("codex_usage_updated_at".into(), datetime(at).into());
    row["extra"] = extra.into();
    quota_windows(&row, Some(at))
}

pub fn summary_from_result(row: &Value, result: &Value) -> Value {
    if truthy(&result["success"]) {
        let data = &result["data"];
        if data.is_object() && (data["five_hour"].is_object() || data["seven_day"].is_object()) {
            return from_usage_data(
                data,
                row,
                parse_datetime(&result["queried_at"]).unwrap_or_else(Utc::now),
            );
        }
        if result["oauth_quota"].is_object() {
            return sanitize_summary(row, &result["oauth_quota"]);
        }
    }
    quota_windows(row, None)
}

pub fn latest_result(row: &Value, result: &Value, at: DateTime<Utc>) -> Value {
    let summary = quota_windows(row, None);
    let Some(observed) = parse_datetime(&summary["updated_at"]) else {
        return result.clone();
    };
    if observed > at
        || !truthy(&summary["ui_windows"])
        || parse_datetime(&result["queried_at"]).is_some_and(|v| v >= observed)
    {
        return result.clone();
    }
    json!({"success":true,"queried_at":datetime(observed),"oauth_quota":summary,"source":"passive"})
}

#[expect(
    clippy::too_many_arguments,
    reason = "The window projection preserves each independent legacy API field"
)]
pub fn window(
    key: &str,
    label: &str,
    used: Option<f64>,
    reset: &Value,
    observed: &Value,
    at: DateTime<Utc>,
    source: &str,
    failed: bool,
) -> Value {
    let used = used.filter(|v| v.is_finite() && *v >= 0.0);
    let stamp = parse_datetime(observed);
    let deadline = parse_datetime(reset);
    let status = if failed {
        "error"
    } else if used.is_none() || stamp.is_none_or(|v| v > at) {
        "unknown"
    } else if stamp.is_some_and(|v| (at - v).num_milliseconds() > 3_600_000)
        || deadline.is_some_and(|v| v <= at)
    {
        "stale"
    } else {
        "known"
    };
    json!({"key":key,"label":label,"used_percent":used,"reset_at":deadline.map(datetime),"observed_at":stamp.map(datetime),"status":status,"source":source})
}

pub fn usage_windows(row: &Value, at: DateTime<Utc>, result: &Value) -> Vec<Value> {
    let mut row = row.clone();
    if truthy(&row["quota_plan_type"]) {
        row["credentials"] = json!({"plan_type":row["quota_plan_type"]});
    }
    let extra = &row["extra"];
    if row["platform"] == "openai" && row["type"] == "oauth" {
        let latest = latest_result(&row, result, at);
        let summary = summary_from_result(&row, &latest);
        let windows = windows_by_key(&summary["ui_windows"]);
        let observed = if truthy(&summary["updated_at"]) {
            &summary["updated_at"]
        } else {
            &latest["queried_at"]
        };
        return required_windows(&summary["plan_type"])
            .iter()
            .map(|key| {
                let value = windows.get(*key).unwrap_or(&Value::Null);
                let minutes = number(&value["window_minutes"])
                    .filter(|v| (1.0..=5_256_000.0).contains(v) && v.fract() == 0.0)
                    .map(|v| v as i64);
                let label = match minutes {
                    Some(v) if v % 1440 == 0 => format!("{}d", v / 1440),
                    Some(v) if v % 60 == 0 => format!("{}h", v / 60),
                    Some(v) => format!("{v}m"),
                    None => if *key == "codex_5h" { "5h" } else { "7d" }.into(),
                };
                let mut item = window(
                    key,
                    &label,
                    nonnegative(&value["used_percent"]),
                    &value["reset_at"],
                    observed,
                    at,
                    "passive",
                    truthy(&latest) && !truthy(&latest["success"]),
                );
                if let Some(minutes) = minutes {
                    item["window_minutes"] = minutes.into();
                }
                item
            })
            .collect();
    }
    if row["platform"] == "grok" && matches!(string(&row["type"]), "oauth" | "apikey") {
        let snapshot = if extra["grok_usage_snapshot"].is_object() {
            &extra["grok_usage_snapshot"]
        } else {
            &extra["grok_quota_snapshot"]
        };
        let observed = if truthy(&snapshot["last_headers_seen_at"]) {
            &snapshot["last_headers_seen_at"]
        } else {
            &snapshot["updated_at"]
        };
        let failed = nonnegative(&snapshot["status_code"])
            .is_some_and(|v| v != 0.0 && !(200.0..300.0).contains(&v));
        return [("requests", "请求"), ("tokens", "Token")]
            .into_iter()
            .map(|(key, label)| {
                let raw = &snapshot[key];
                let limit = nonnegative(&raw["limit"]);
                let remaining = nonnegative(&raw["remaining"]);
                let used = limit
                    .zip(remaining)
                    .filter(|(l, r)| *l > 0.0 && r <= l)
                    .map(|(l, r)| l - r);
                let reset = if truthy(&raw["reset_at"]) {
                    &raw["reset_at"]
                } else {
                    &raw["resets_at"]
                };
                let mut value = window(
                    key,
                    label,
                    used.zip(limit).map(|(u, l)| u / l * 100.0),
                    reset,
                    observed,
                    at,
                    "upstream_headers",
                    failed,
                );
                value["limit"] = json!(limit);
                value["remaining"] = json!(remaining);
                value["used"] = json!(used);
                value
            })
            .collect();
    }
    vec![]
}
