use crate::{
    actions::Actions,
    compat::{config_revision, datetime, parse_datetime, sha256, string, text, truthy},
    db::Database,
    error::{Error, Result},
    error_evidence,
    oauth_admission::{self, credential_fingerprint},
    oauth_state::{compact_digest, limit_fingerprint, normalized_credits, validate_observation},
    quota, reset_credit_observation, sql,
    upstream::read_limited,
    usage,
};
use chrono::{DateTime, Duration as TimeDelta, Timelike, Utc};
use serde_json::{json, Value};
use std::{sync::LazyLock, time::Duration};
static MESSAGES: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../resources/credit-messages.json"))
        .expect("credit messages")
});
pub const LEGACY_ENDED: [&str; 4] = ["finished", "recovered", "closed", "manual"];
pub const TERMINAL: [&str; 5] = ["finished", "recovered", "closed", "manual", "blocked"];
pub fn reason(code: &str) -> &'static str {
    if code.is_empty() {
        return "";
    }
    MESSAGES["REASONS"][code]
        .as_str()
        .or_else(|| MESSAGES["ERRORS"][code].as_str())
        .unwrap_or("操作未确认")
}
pub fn label(outcome: &str) -> &'static str {
    MESSAGES["RESULT_LABELS"][outcome]
        .as_str()
        .unwrap_or("最近用卡结果")
}
pub fn evidence_error(code: &str) -> &'static str {
    MESSAGES["CREDIT_EVIDENCE_ERRORS"][code]
        .as_str()
        .unwrap_or("卡证据无效")
}
pub fn project(value: &Value, notices: &Value) -> Value {
    if !truthy(value) {
        return Value::Null;
    }
    let mut value = value.clone();
    if truthy(&value["credit_gate_skipped"]) && !truthy(&value["attempt_at"]) {
        return if truthy(&value["owns_pause"]) {
            json!({"stage":"blocked","label":"调度暂停待核对","error":"用卡已跳过；原临时暂停尚未确认解除","next_at":null,"evidence_source":value["evidence_source"],"evidence_at":value["evidence_at"]})
        } else {
            Value::Null
        };
    }
    if value["stage"] == "manual"
        && truthy(&value["attempt_at"])
        && !truthy(&value["reset_completed_at"])
        && value["consumed"] != true
        && value["consumption_outcome"] != "not_consumed"
    {
        value["stage"] = "uncertain".into();
        value["error_code"] = "result_uncertain".into();
        value["next_at"] = Value::Null;
    }
    if !truthy(&value["policy_version"])
        && matches!(string(&value["stage"]), "manual" | "recovered" | "closed")
    {
        return Value::Null;
    }
    let code = if truthy(&value["result_reason"]) {
        string(&value["result_reason"])
    } else {
        string(&value["error_code"])
    };
    let finished = matches!(string(&value["stage"]), "finished" | "recovered");
    let notice = &notices[string(&value["episode"])];
    let mut out = json!({"stage":value["stage"],"label":if finished {label(string(&value["outcome"]))} else {MESSAGES["STATE_LABELS"][string(&value["stage"])].as_str().unwrap_or("用卡处理中")},"error":reason(code),"notification_status":notice["status"],"notification_attempts":notice.get("attempts").unwrap_or(&json!(0))});
    for key in [
        "trigger_at",
        "consume_deadline_at",
        "verification_deadline_at",
        "outcome",
        "completed_at",
        "next_at",
        "attempt_at",
        "evidence_source",
        "evidence_at",
        "test_completed_at",
        "recovered_at",
    ] {
        out[key] = value[key].clone();
    }
    out
}
pub fn manual(task: &Value) -> bool {
    task["source"] == "manual"
        || !truthy(&task["policy_version"])
            && matches!(string(&task["stage"]), "manual" | "uncertain")
            && !truthy(&task["evidence_at"])
            && !truthy(&task["owns_pause"])
}
pub fn policy_now(task: &Value, at: DateTime<Utc>) -> DateTime<Utc> {
    parse_datetime(&task["policy_checked_at"])
        .unwrap_or(at)
        .max(at)
}
pub fn expired(task: &Value, at: DateTime<Utc>) -> bool {
    if truthy(&task["attempt_at"]) && !truthy(&task["consumed"]) {
        return false;
    }
    let at = policy_now(task, at);
    parse_datetime(
        &task[if truthy(&task["consumed"]) {
            "verification_deadline_at"
        } else {
            "consume_deadline_at"
        }],
    )
    .is_none_or(|deadline| at >= deadline)
}
pub fn outcome(task: &Value) -> &'static str {
    if truthy(&task["consumed"]) {
        "unrecovered"
    } else if truthy(&task["attempt_at"]) {
        if task["consumption_outcome"] == "not_consumed" {
            "consume_failed"
        } else {
            "unknown"
        }
    } else {
        "not_executed"
    }
}
pub fn verification_retryable(result: &Value, method: &str) -> bool {
    if let Some(value) = result["retryable"].as_bool() {
        return value;
    }
    if method == "model" {
        return false;
    }
    let code = string(&result["error_code"]);
    matches!(
        code,
        "network_error" | "timeout" | "missing_success_event" | "account_test_error"
    ) || code
        .strip_prefix("http_")
        .unwrap_or(code)
        .parse::<u16>()
        .is_ok_and(|status| (500..=599).contains(&status))
}
pub fn quota_result(
    data: &Value,
    row: &Value,
    at: DateTime<Utc>,
    requested: Option<DateTime<Utc>>,
) -> Value {
    let payload = if data["quota"].is_object() {
        &data["quota"]
    } else {
        data
    };
    let windows = &payload["rate_limit"];
    let mut normal = json!({});
    for value in [&windows["primary_window"], &windows["secondary_window"]] {
        if !value.is_object() {
            continue;
        }
        let duration = &value["limit_window_seconds"];
        let key = if quota::number(duration) == Some(18000.0) {
            Some("five_hour")
        } else if duration.is_number() && duration.as_f64().is_some_and(|v| v > 18000.0) {
            Some("seven_day")
        } else {
            None
        };
        if let Some(key) = key {
            normal[key] = json!({"utilization":value["used_percent"],"resets_at":value["reset_at"],"remaining_seconds":value["reset_after_seconds"],"window_minutes":duration.as_f64().unwrap_or(0.0)/60.0});
        }
    }
    if !truthy(windows) {
        for key in ["five_hour", "seven_day"] {
            if payload[key].is_object() {
                normal[key] = payload[key].clone();
            }
        }
    }
    let observed = if !payload["fetched_at"].is_null() {
        parse_datetime(&payload["fetched_at"])
    } else {
        Some(at)
    };
    let mut out = json!({"account_id":row["id"],"template_type":"oauth","success":truthy(&normal) && observed.is_some_and(|v|v<=at),"source":"sub2api_admin_usage","queried_at":datetime(observed.unwrap_or(at)),"oauth_quota":quota::from_usage_data(&normal,row,observed.unwrap_or(at))});
    if requested.zip(observed).is_some_and(|(start, observed)| {
        start.with_nanosecond(0).unwrap() <= observed && observed <= at
    }) {
        out["read_started_at"] = datetime(requested.unwrap()).into();
        out["read_completed_at"] = datetime(at).into();
    }
    out
}
pub async fn upstream_evidence(db: &Database, row: &Value, at: DateTime<Utc>) -> Result<Value> {
    let (Some(start), Some(reset)) = (
        parse_datetime(&row["rate_limited_at"]),
        parse_datetime(&row["rate_limit_reset_at"]),
    ) else {
        return Ok(Value::Null);
    };
    if start > at || reset <= at || reset <= start {
        return Ok(Value::Null);
    }
    let params = [
        row["id"].as_i64().unwrap_or(0).into(),
        (start - TimeDelta::seconds(5)).into(),
        at.into(),
    ];
    let sql=format!("SELECT e.id,e.created_at,e.error_owner,e.error_phase,to_jsonb(e)->>'upstream_error_message' AS upstream_error_message,to_jsonb(e)->>'error_message' AS error_message,to_jsonb(e)->'error_body' AS error_body,to_jsonb(e)->'upstream_error_detail' AS upstream_error_detail,to_jsonb(e)->'upstream_errors' AS upstream_errors FROM ops_error_logs e WHERE e.account_id=$1 AND e.upstream_status_code=429 AND coalesce(e.error_owner,'') <> 'client' AND NOT {} AND e.created_at >= $2 AND e.created_at <= $3 AND NOT EXISTS (SELECT 1 FROM ops_error_logs x WHERE x.account_id=e.account_id AND x.created_at>e.created_at AND x.upstream_status_code IN (401,402)) ORDER BY e.created_at ASC,e.id ASC LIMIT 1",sql::ERRORS_LOCAL_THROTTLE);
    let candidate = db
        .one(&sql, &params)
        .await
        .map_err(|_| Error::new(503, "限流证据无法可靠读取"))?;
    if let Some(mut candidate) = candidate.filter(|v| !error_evidence::local_throttle(v)) {
        candidate["source"] = "upstream_error".into();
        return Ok(candidate);
    }
    let sql=format!("SELECT e.id FROM ops_error_logs e WHERE e.account_id=$1 AND e.created_at >= $2 AND e.created_at <= $3 AND (e.upstream_status_code IN (401,402) OR {} OR (e.error_owner='client' AND (e.status_code=429 OR e.upstream_status_code=429))) ORDER BY e.created_at DESC,e.id DESC LIMIT 1",sql::ERRORS_LOCAL_THROTTLE);
    if db
        .one(&sql, &params)
        .await
        .map_err(|_| Error::new(503, "限流证据无法可靠读取"))?
        .is_some()
    {
        return Ok(Value::Null);
    }
    let fingerprint = sha256(format!(
        "{}:{}:{}",
        text(&row["id"]),
        datetime(start),
        datetime(reset)
    ));
    Ok(
        json!({"id":format!("account-rate-limit:{fingerprint}"),"created_at":datetime(start),"source":"account_rate_limit"}),
    )
}
pub fn depletion_evidence(
    row: &Value,
    saved: &Value,
    evidence: &Value,
    at: DateTime<Utc>,
) -> Value {
    let Some(block) = parse_datetime(&row["rate_limit_reset_at"]).filter(|t| *t > at) else {
        return Value::Null;
    };
    let Some(observed) = parse_datetime(&saved["queried_at"]).filter(|t| *t <= at) else {
        return Value::Null;
    };
    if !truthy(&saved["success"]) {
        return Value::Null;
    }
    let summary = quota::summary_from_result(row, saved);
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    let seven = windows.get("codex_7d").unwrap_or(&Value::Null);
    let Some(reset) = parse_datetime(&seven["reset_at"]).filter(|t| *t > at && observed < *t)
    else {
        return Value::Null;
    };
    if !quota::percent(&seven["used_percent"]).is_some_and(|v| v >= 100.0) {
        return Value::Null;
    }
    let Some(stamp) = parse_datetime(&evidence["created_at"]).filter(|t| *t <= at) else {
        return Value::Null;
    };
    let duration = seven
        .get("window_minutes")
        .filter(|v| truthy(v))
        .cloned()
        .unwrap_or(json!(10080));
    let Some(milliseconds) = quota::number(&duration)
        .filter(|v| *v > 0.0 && *v <= 5256000.0)
        .map(|v| (v * 60000.0) as i64)
    else {
        return Value::Null;
    };
    if observed < reset - TimeDelta::milliseconds(milliseconds)
        || stamp < reset - TimeDelta::milliseconds(milliseconds)
    {
        return Value::Null;
    }
    let fingerprint = config_revision(
        &json!({"account_id":row["id"],"limited_at":text(&row["rate_limited_at"]),"block_until":datetime(block),"quota_reset":datetime(reset)}),
    );
    json!({"reset_at":datetime(reset),"window_minutes":duration,"evidence_at":datetime(stamp),"evidence_id":text(&evidence["id"]),"evidence_source":evidence.get("source").unwrap_or(&json!("upstream_error")),"evidence_fingerprint":fingerprint})
}
pub fn available(row: &Value, saved: &Value, task: &Value, at: DateTime<Utc>) -> bool {
    let Some(observed) = parse_datetime(&saved["queried_at"]) else {
        return false;
    };
    if !oauth_admission::fresh_quota(row, saved, at) {
        return false;
    }
    if let Some(attempt) = parse_datetime(&task["attempt_at"]).filter(|t| observed < *t) {
        let (Some(start), Some(completed)) = (
            parse_datetime(&saved["read_started_at"]),
            parse_datetime(&saved["read_completed_at"]),
        ) else {
            return false;
        };
        if !(attempt <= start
            && start <= completed
            && completed <= at
            && start.with_nanosecond(0).unwrap() <= observed
            && observed <= completed)
        {
            return false;
        }
    }
    let summary = quota::summary_from_result(row, saved);
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    quota::required_windows(&summary["plan_type"])
        .iter()
        .all(|key| {
            quota::percent(&windows.get(*key).unwrap_or(&Value::Null)["used_percent"])
                .unwrap_or(0.0)
                < 100.0
        })
}
pub fn credit_reason(
    row: &Value,
    evidence: &Value,
    meta: &Value,
    saved: &Value,
    at: DateTime<Utc>,
) -> Result<&'static str> {
    let Some(reset) = parse_datetime(&evidence["reset_at"]) else {
        return Ok("outside_window");
    };
    let Some(minutes) =
        quota::number(&evidence["window_minutes"]).filter(|v| *v > 0.0 && *v <= 5256000.0)
    else {
        return Ok("outside_window");
    };
    let raw = &row["extra"]["codex_reset_credit_snapshot"];
    let Some(content) = normalized_credits(raw) else {
        return Ok("invalid_credit_snapshot");
    };
    let observation = &meta["reset_credit_observation"];
    let observed = if !observation.is_null() {
        validate_observation(observation)?;
        if compact_digest(&content) != observation["snapshot_sha256"] {
            return Ok("content_changed");
        }
        if credential_fingerprint(row) != observation["credential_fingerprint"] {
            return Ok("credentials_changed");
        }
        if limit_fingerprint(row) != observation["limit_fingerprint"] {
            return Ok("limit_changed");
        }
        if parse_datetime(&observation["window_reset_at"]) != Some(reset)
            || quota::number(&observation["window_minutes"]) != Some(minutes)
        {
            return Ok("window_changed");
        }
        parse_datetime(&observation["observed_at"])
    } else {
        parse_datetime(&usage::reset_credits(&row["extra"], at)["observed_at"])
    };
    let Some(observed) = observed else {
        return Ok("missing");
    };
    if observed < reset - TimeDelta::milliseconds((minutes * 60000.0) as i64)
        || observed > at
        || observed >= reset
    {
        return Ok("outside_window");
    }
    Ok(if parse_datetime(&saved["queried_at"]).is_some() {
        ""
    } else {
        "quota_missing"
    })
}
pub fn credit_observation(
    before: &Value,
    after: &Value,
    result: &Value,
    started: DateTime<Utc>,
    completed: DateTime<Utc>,
) -> Option<Value> {
    let payload = &result["data"];
    if !truthy(&result["success"])
        || payload["cache_persisted"] != true
        || !after.is_object()
        || credential_fingerprint(before) != credential_fingerprint(after)
        || limit_fingerprint(before) != limit_fingerprint(after)
    {
        return None;
    }
    let content = normalized_credits(&payload["rate_limit_reset_credits"])?;
    if Some(&content) != normalized_credits(&after["extra"]["codex_reset_credit_snapshot"]).as_ref()
    {
        return None;
    }
    let observed = if payload["fetched_at"].is_null() {
        completed
    } else {
        parse_datetime(&payload["fetched_at"])?
    };
    if observed < started - TimeDelta::seconds(1) || observed > completed {
        return None;
    }
    let summary = quota::summary_from_result(after, &result["quota_result"]);
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    let win = windows.get("codex_7d")?;
    let reset = parse_datetime(&win["reset_at"])?;
    if reset <= observed {
        return None;
    }
    reset_credit_observation::create(
        after,
        &content,
        observed,
        &credential_fingerprint(after),
        reset,
        win.get("window_minutes")
            .filter(|v| truthy(v))
            .unwrap_or(&json!(10080)),
    )
    .ok()
}
pub async fn execute(actions: &Actions, action: &str, id: i64, key: &str, seconds: u64) -> Value {
    if actions.settings.read_only {
        return json!({"success":false,"uncertain":false,"error_code":"read_only"});
    }
    let uncertain = action == "reset";
    let path = if uncertain {
        "reset-quota"
    } else {
        "quota/refresh"
    };
    let diagnostic = reset_credit_observation::diagnostics(None, &Value::Null);
    let response = match actions
        .request(
            reqwest::Method::POST,
            &format!("openai/accounts/{id}/{path}"),
            key,
            Some(&json!({})),
            Duration::from_secs(seconds),
            false,
        )
        .await
    {
        Ok(v) => v,
        Err(_) => {
            return json!({"success":false,"uncertain":uncertain,"error_code":"result_uncertain","diagnostics":diagnostic})
        }
    };
    let status = response.status().as_u16();
    let body = read_limited(response, 2_000_000)
        .await
        .ok()
        .and_then(|v| serde_json::from_slice::<Value>(&v).ok())
        .unwrap_or(Value::Null);
    let data = &body["data"];
    if !(200..300).contains(&status) {
        return json!({"success":false,"uncertain":uncertain,"error_code":format!("http_{status}"),"diagnostics":reset_credit_observation::diagnostics(Some(status),if data.is_object() {data} else {&body})});
    }
    let envelope = body.is_object() && body["code"].is_i64() && body["code"] == 0;
    let diagnostic = reset_credit_observation::diagnostics(
        Some(status),
        if envelope && data.is_object() {
            data
        } else {
            &body
        },
    );
    if !envelope || !data.is_object() {
        return json!({"success":false,"uncertain":uncertain,"error_code":"result_uncertain","diagnostics":diagnostic});
    }
    let mut outcome = if uncertain {
        reset_credit_observation::consumption_result(data)
    } else {
        json!({"success":true,"consumed":false,"uncertain":false,"error_code":""})
    };
    outcome["data"] = data.clone();
    outcome["diagnostics"] = diagnostic;
    outcome
}
