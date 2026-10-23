use crate::{
    compat::{datetime, integer, parse_datetime, string, truthy},
    error::Result,
    oauth_admission::{self, MIN_QUERY_SECONDS},
    oauth_state::query_metadata,
    quota,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
pub fn change_is_safe(before: &Value, after: &Value) -> bool {
    if [
        "id",
        "platform",
        "type",
        "status",
        "schedulable",
        "concurrency",
    ]
    .iter()
    .any(|key| before[key] != after[key])
    {
        return false;
    }
    if [
        "rate_limited_at",
        "rate_limit_reset_at",
        "overload_until",
        "temp_unschedulable_until",
        "temp_unschedulable_reason",
    ]
    .iter()
    .any(|key| !after[key].is_null() && after[key] != "" && before[key] != after[key])
    {
        return false;
    }
    ["model_rate_limits", "antigravity_quota_scopes"]
        .iter()
        .all(|key| !truthy(&after["extra"][key]) || after["extra"][key] == before["extra"][key])
}
pub fn threshold(value: &Value) -> Value {
    let payload: Value = serde_json::from_str(string(value).trim()).unwrap_or(Value::Null);
    if !payload.is_object()
        || string(&payload["source"]).trim() != "account_scheduling_threshold"
        || !string(&payload["platform"])
            .trim()
            .eq_ignore_ascii_case("openai")
    {
        return json!({});
    }
    let window = string(&payload["window"]).trim().to_lowercase();
    let window = window.strip_prefix("codex_").unwrap_or(&window);
    let Some(percent) =
        quota::percent(&payload["threshold_percent"]).filter(|v| *v > 0.0 && *v <= 100.0)
    else {
        return json!({});
    };
    if !matches!(window, "5h" | "7d")
        || payload["until_unix"].is_boolean()
        || quota::percent(&payload["until_unix"]).is_none()
    {
        return json!({});
    }
    let Some(until) = parse_datetime(&payload["until_unix"]) else {
        return json!({});
    };
    json!({"window_key":format!("codex_{window}"),"threshold_percent":percent,"until_at":datetime(until)})
}
pub fn eligible(row: &Value, windows: &[String], at: DateTime<Utc>) -> bool {
    if integer(&row["id"]).unwrap_or(0) <= 0
        || !string(&row["platform"]).eq_ignore_ascii_case("openai")
        || !string(&row["type"]).eq_ignore_ascii_case("oauth")
        || (!row["deleted_at"].is_null() && row["deleted_at"] != "")
        || !string(&row["status"]).eq_ignore_ascii_case("active")
        || row["schedulable"] != true
    {
        return false;
    }
    let reason = string(&row["temp_unschedulable_reason"]).trim();
    let info = threshold(&json!(reason));
    if !oauth_admission::temporary_block_expired(row, at)
        && (!reason.is_empty() && !truthy(&info)
            || !row["temp_unschedulable_until"].is_null()
                && row["temp_unschedulable_until"] != ""
                && !truthy(&info))
    {
        return false;
    }
    let until = parse_datetime(&row["temp_unschedulable_until"]);
    if until
        .zip(parse_datetime(&info["until_at"]))
        .is_some_and(|(a, b)| (a - b).num_milliseconds().abs() > 1000)
    {
        return false;
    }
    if truthy(&row["rate_limited_at"]) || truthy(&row["rate_limit_reset_at"]) {
        return true;
    }
    until.is_some()
        && truthy(&info)
        && !windows.is_empty()
        && windows
            .iter()
            .all(|v| matches!(v.as_str(), "codex_5h" | "codex_7d"))
        && windows.iter().any(|v| v == string(&info["window_key"]))
}
pub fn confirmed(row: &Value) -> bool {
    integer(&row["id"]).is_some_and(|id| id > 0)
        && string(&row["platform"]).eq_ignore_ascii_case("openai")
        && string(&row["type"]).eq_ignore_ascii_case("oauth")
        && (row["deleted_at"].is_null() || row["deleted_at"] == "")
        && string(&row["status"]).eq_ignore_ascii_case("active")
        && row["schedulable"] == true
        && [
            "rate_limited_at",
            "rate_limit_reset_at",
            "overload_until",
            "temp_unschedulable_until",
            "temp_unschedulable_reason",
        ]
        .iter()
        .all(|key| !truthy(&row[key]))
}
pub fn quota_descriptor(summary: &Value, at: DateTime<Utc>) -> Value {
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    let depleted = quota::required_windows(&summary["plan_type"])
        .iter()
        .filter_map(|key| {
            windows
                .get(*key)
                .filter(|win| quota::percent(&win["used_percent"]).is_some_and(|v| v >= 100.0))
                .map(|win| (*key, win, parse_datetime(&win["reset_at"])))
        })
        .collect::<Vec<_>>();
    if depleted.is_empty() {
        return Value::Null;
    }
    let parsed = depleted
        .iter()
        .filter_map(|(_, _, t)| *t)
        .collect::<Vec<_>>();
    let due = if parsed.len() == depleted.len() {
        *parsed.iter().max().unwrap()
    } else {
        at
    };
    json!({"fingerprint":depleted.iter().map(|(key,_,t)|format!("{key}@{}",t.map(datetime).unwrap_or_else(||"unknown".into()))).collect::<Vec<_>>().join("|"),"due_at":datetime(due),"source":if depleted.iter().all(|(_,win,_)|!truthy(&win["reset_source"]) || win["reset_source"]=="server_exact") {"server_exact"} else {"estimated_from_remaining"},"window_keys":depleted.iter().map(|(key,_,_)|*key).collect::<Vec<_>>(),"kind":"quota_exhausted"})
}
pub fn threshold_descriptor(row: &Value, summary: &Value, at: DateTime<Utc>) -> Value {
    let info = threshold(&row["temp_unschedulable_reason"]);
    let key = string(&info["window_key"]);
    let Some(due) = parse_datetime(&row["temp_unschedulable_until"]) else {
        return Value::Null;
    };
    if key.is_empty() || !eligible(row, &[key.into()], at) {
        return Value::Null;
    }
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    let win = windows.get(key).unwrap_or(&Value::Null);
    json!({"fingerprint":format!("{key}@{}",datetime(due)),"due_at":datetime(due),"source":if truthy(&win["reset_source"]) {string(&win["reset_source"])} else {"server_exact"},"window_keys":[key],"kind":"account_scheduling_threshold","threshold_percent":info["threshold_percent"]})
}
pub fn threshold_available(summary: &Value, descriptor: &Value) -> bool {
    let Some(keys) = descriptor["window_keys"]
        .as_array()
        .filter(|a| a.len() == 1)
    else {
        return false;
    };
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    quota::percent(&windows.get(string(&keys[0])).unwrap_or(&Value::Null)["used_percent"])
        .is_some_and(|used| {
            used < quota::percent(&descriptor["threshold_percent"]).unwrap_or(100.0)
        })
}
pub fn all_available(summary: &Value) -> bool {
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    quota::required_windows(&summary["plan_type"])
        .iter()
        .all(|key| {
            quota::percent(&windows.get(*key).unwrap_or(&Value::Null)["used_percent"])
                .is_some_and(|used| used < 100.0)
        })
}
pub fn rate_descriptor(row: &Value, summary: &Value, at: DateTime<Utc>) -> Value {
    let windows = quota::required_windows(&summary["plan_type"]);
    if !(truthy(&row["rate_limited_at"]) || truthy(&row["rate_limit_reset_at"]))
        || !all_available(summary)
        || !eligible(
            row,
            &windows.iter().map(|v| (*v).into()).collect::<Vec<_>>(),
            at,
        )
    {
        return Value::Null;
    }
    let Some(blocked) = parse_datetime(&row["rate_limited_at"])
        .or_else(|| parse_datetime(&row["rate_limit_reset_at"]))
    else {
        return Value::Null;
    };
    let deadline = parse_datetime(&row["rate_limit_reset_at"]).unwrap_or(blocked);
    json!({"fingerprint":windows.iter().map(|key|format!("{key}@{}:{}",datetime(blocked),datetime(deadline))).collect::<Vec<_>>().join("|"),"due_at":datetime(deadline),"source":"server_exact","window_keys":windows,"kind":"rate_limit_block"})
}
pub fn retry_at(at: DateTime<Utc>, attempt: i64) -> String {
    datetime(
        at + Duration::seconds(
            [60, 300, 900, 1800][attempt.saturating_sub(1).clamp(0, 3) as usize],
        ),
    )
}
pub fn quota_ready(row: &Value, result: &Value, meta: &Value, at: DateTime<Utc>) -> bool {
    let Some(observed) = parse_datetime(&result["queried_at"]) else {
        return false;
    };
    if !truthy(&result["success"])
        || observed > at
        || (at - observed).num_milliseconds() > MIN_QUERY_SECONDS * 1000
        || !oauth_admission::quota_complete(row, result)
    {
        return false;
    }
    let summary = quota::summary_from_result(row, result);
    if !all_available(&summary) {
        return false;
    }
    if [
        &row["rate_limit_reset_at"],
        &row["temp_unschedulable_until"],
        &meta["recovery_intent"]["due_at"],
    ]
    .into_iter()
    .filter_map(parse_datetime)
    .any(|boundary| observed < boundary)
    {
        return false;
    }
    if parse_datetime(&meta["last_error_at"]).is_some_and(|failed| observed <= failed)
        && truthy(&meta["last_error_code"])
    {
        return false;
    }
    let threshold = threshold_descriptor(row, &summary, at);
    !truthy(&threshold) || threshold_available(&summary, &threshold)
}
pub fn control_allows(row: &Value, result: &Value, meta: &Value, at: DateTime<Utc>) -> bool {
    let control = &meta["manual_control"];
    let Some(changed) = parse_datetime(&control["at"]) else {
        return true;
    };
    if row["schedulable"] != true {
        return false;
    }
    if parse_datetime(&row["rate_limited_at"]).is_some_and(|blocked| blocked > changed) {
        return true;
    }
    parse_datetime(&result["queried_at"]).is_some_and(|observed| {
        observed > changed
            && parse_datetime(&control["evidence_at"]).is_none_or(|base| observed > base)
    }) && quota_ready(row, result, meta, at)
}
pub fn candidates(
    rows: &[Value],
    results: &Value,
    scheduler: &Value,
    at: DateTime<Utc>,
    enabled: bool,
) -> Result<Vec<Value>> {
    if !enabled {
        return Ok(vec![]);
    }
    let mut candidates = vec![];
    for row in rows {
        let id = integer(&row["id"]).unwrap_or(0);
        if id <= 0 {
            continue;
        }
        let result = quota::latest_result(row, &results[id.to_string()], at);
        let meta = &scheduler[id.to_string()];
        if !control_allows(row, &result, meta, at) {
            continue;
        }
        let (blocked, _) = oauth_admission::automatic_gate(row, &result, meta, at, true, false, 1)?;
        if matches!(blocked, "account_ineligible" | "auth_paused") {
            continue;
        }
        let summary = quota::summary_from_result(row, &result);
        let intent = &meta["recovery_intent"];
        if truthy(&intent["verification_dispatched"])
            && (!truthy(&intent["verification_receipt"]) || intent["status"] == "blocked")
        {
            continue;
        }
        let status = string(&intent["status"]);
        let due = parse_datetime(
            &intent[if matches!(status, "retry" | "waiting_quota" | "testing") {
                "next_retry_at"
            } else {
                "due_at"
            }],
        );
        let descriptor = quota_descriptor(&summary, at);
        let descriptor = if descriptor.is_null() {
            threshold_descriptor(row, &summary, at)
        } else {
            descriptor
        };
        if oauth_admission::reset_not_before(row, &result).is_some_and(|due| at < due) {
            continue;
        }
        let reusable = quota_ready(row, &result, meta, at);
        let due = if status == "waiting_quota" && reusable {
            Some(at)
        } else {
            due
        };
        let (reason, priority) = if matches!(
            status,
            "pending" | "ready" | "retry" | "waiting_quota" | "testing"
        ) && due.is_none_or(|due| at >= due)
        {
            ("recovery_intent", 0)
        } else if truthy(&descriptor) {
            ("exact_reset", 0)
        } else if !truthy(&result["success"]) {
            ("bootstrap", 1)
        } else if truthy(&meta["quota_query"]["auth_fingerprint"])
            && meta["quota_query"]["auth_fingerprint"]
                != oauth_admission::credential_fingerprint(row)
        {
            ("credential_changed", 1)
        } else {
            ("", 99)
        };
        if reason.is_empty() || !(blocked.is_empty() || reason == "recovery_intent" && reusable) {
            continue;
        }
        candidates.push(json!({"account_id":id,"row":row,"reason":reason,"priority":priority,"exact_fingerprint":string(&if truthy(&descriptor) {&descriptor} else {intent}["fingerprint"]),"previous_summary":summary,"reuse_quota":if reason=="recovery_intent" && reusable {result} else {Value::Null}}));
    }
    let mut sort_values = std::collections::BTreeMap::new();
    for candidate in &candidates {
        let id = integer(&candidate["account_id"]).unwrap();
        let query = query_metadata(&scheduler[id.to_string()], &results[id.to_string()])?;
        sort_values.insert(id, string(&query["last_query_at"]).to_owned());
    }
    candidates.sort_by_key(|v| {
        let id = integer(&v["account_id"]).unwrap();
        (
            integer(&v["priority"]).unwrap_or(99),
            sort_values.get(&id).cloned().unwrap_or_default(),
            id,
        )
    });
    Ok(candidates)
}
