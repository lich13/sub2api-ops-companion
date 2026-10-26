use crate::{
    compat::{datetime, integer, parse_datetime, string, truthy},
    error::Result,
    oauth_admission,
    oauth_state::{OAuthState, Update},
    quota, recovery_policy, recovery_rules,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

pub fn auth_code(code: &str) -> &'static str {
    match code.trim().to_lowercase().as_str() {
        "401" | "http_401" => "http_401",
        "402" | "http_402" => "http_402",
        _ => "",
    }
}
pub fn known(key: &str, id: i64, state: &Value) -> bool {
    state["recovery_history"]
        .get(key)
        .is_some_and(|v| !truthy(&v["legacy"]))
        || state["pending_events"].get(key).is_some()
        || state["scheduler"][id.to_string()]["notified_keys"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == key))
}
pub fn recovery_transition(previous: &Value, refreshed: &Value, at: DateTime<Utc>) -> Value {
    if !previous.is_object() || !refreshed.is_object() {
        return Value::Null;
    }
    let plan = quota::plan_type(&refreshed["plan_type"]);
    let previous_windows = quota::windows_by_key(&previous["ui_windows"]);
    let refreshed_windows = quota::windows_by_key(&refreshed["ui_windows"]);
    let mut required = vec![];
    let mut full = vec![];
    for key in quota::required_windows(&json!(plan)) {
        let Some(refreshed) = refreshed_windows.get(*key) else {
            return Value::Null;
        };
        if !quota::percent(&refreshed["used_percent"]).is_some_and(|v| v < 100.0) {
            return Value::Null;
        }
        required.push(refreshed.clone());
        if let Some(previous) = previous_windows
            .get(*key)
            .filter(|v| quota::percent(&v["used_percent"]).is_some_and(|v| v >= 100.0))
        {
            full.push(previous.clone());
        }
    }
    if full.is_empty()
        || !quota::percent(
            &refreshed_windows.get("codex_7d").unwrap_or(&Value::Null)["used_percent"],
        )
        .is_some_and(|v| v < 100.0)
    {
        return Value::Null;
    }
    let metadata = quota::with_recovery_metadata(json!({"plan_type":plan,"ui_windows":full}));
    let mut fingerprint = string(&metadata["recovery_fingerprint"]).to_owned();
    if fingerprint.is_empty() {
        fingerprint = format!(
            "transition:{}@{}",
            full.iter()
                .map(|v| v["key"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .unwrap_or("unknown"))
                .collect::<Vec<_>>()
                .join(","),
            datetime(at)
        );
    }
    let seven_before = previous_windows.get("codex_7d").unwrap_or(&Value::Null);
    let seven_after = refreshed_windows.get("codex_7d").unwrap_or(&Value::Null);
    let old_used = quota::percent(&seven_before["used_percent"]);
    let new_used = quota::percent(&seven_after["used_percent"]);
    let old_reset = parse_datetime(&seven_before["reset_at"]);
    let early = old_used.is_some_and(|v| v >= 100.0)
        && new_used.is_some_and(|v| v < 100.0)
        && old_reset.is_some_and(|reset| at < reset);
    let mut candidate = json!({"plan_type":plan,"windows":required,"window_labels":required.iter().map(|v|v["label"].as_str().filter(|v|!v.is_empty()).unwrap_or("-")).collect::<Vec<_>>(),"window_keys":full.iter().map(|v|string(&v["key"])).collect::<Vec<_>>(),"trigger_window_labels":full.iter().map(|v|v["label"].as_str().filter(|v|!v.is_empty()).unwrap_or("-")).collect::<Vec<_>>(),"fingerprint":if early {format!("early:{fingerprint}")} else {fingerprint},"reset_at":if truthy(&metadata["recovery_due_at"]) {metadata["recovery_due_at"].clone()} else {json!(datetime(at))},"remaining_summary":required.iter().map(|v|format!("{} {}",v["label"].as_str().filter(|v|!v.is_empty()).unwrap_or("-"),quota::number(&v["remaining_percent"]).map(|n|format!("{}%",format!("{n:.2}").trim_end_matches('0').trim_end_matches('.'))).unwrap_or_else(||"-".into()))).collect::<Vec<_>>().join(" / ")});
    if truthy(&metadata["recovery_reset_source"]) {
        candidate["reset_source"] = metadata["recovery_reset_source"].clone();
    }
    if early {
        candidate.as_object_mut().unwrap().extend(json!({"early_reset_detected":true,"old_7d_used_percent":old_used,"new_7d_used_percent":new_used,"old_reset_at":old_reset.map(datetime),"detected_at":datetime(at)}).as_object().unwrap().clone());
    }
    candidate
}
pub fn new_intent(descriptor: &Value, status: &str, at: Option<DateTime<Utc>>) -> Value {
    let mut intent = descriptor
        .as_object()
        .cloned()
        .map(Value::Object)
        .unwrap_or(json!({}));
    intent.as_object_mut().unwrap().extend(json!({"status":status,"deferred_until":"","next_retry_at":"","attempt_count":0,"block_signature":"","confirmed_at":at.map(datetime).unwrap_or_default(),"tested_at":"","recovered_at":"","last_error":"","last_error_code":""}).as_object().unwrap().clone());
    intent
}
pub fn discover(
    store: &OAuthState,
    rows: &[Value],
    config: &Value,
    at: DateTime<Utc>,
) -> Result<()> {
    let previous = store.shared_snapshot()?;
    let mut changes = vec![];
    for row in rows {
        let id = integer(&row["id"]).unwrap_or(0);
        if id <= 0 || recovery_policy::method(config, id).is_none() {
            continue;
        }
        let meta = &previous["scheduler"][id.to_string()];
        let saved = quota::latest_result(row, &previous["oauth_results"][id.to_string()], at);
        if recovery_rules::obsolete_healthy_intent(row, &saved, meta, at) {
            changes.push((
                id,
                recovery_rules::cancel_healthy_intent(&meta["recovery_intent"]),
            ));
            continue;
        }
        if !recovery_rules::control_allows(row, &saved, meta, at) {
            continue;
        }
        let (blocked, _) = oauth_admission::automatic_gate(row, &saved, meta, at, true, false, 1)?;
        if matches!(blocked, "account_ineligible" | "auth_paused") {
            continue;
        }
        let summary = quota::summary_from_result(row, &saved);
        let windows = quota::required_windows(&summary["plan_type"])
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>();
        if !recovery_rules::eligible(row, &windows, at) {
            continue;
        }
        let old = &meta["recovery_intent"];
        if recovery_rules::early_quota_release(row, &saved, meta, at)
            && integer(&old["connection_attempt_count"]).unwrap_or(0) == 0
            && !truthy(&old["verification_dispatched"])
            && !truthy(&old["verification_receipt"])
            && !truthy(&old["recovery_write_dispatched"])
        {
            let mut intent = old.clone();
            intent["status"] = "ready".into();
            intent["due_at"] = saved["queried_at"].clone();
            for key in [
                "next_retry_at",
                "waiting_reason",
                "waiting_until",
                "deferred_until",
                "last_error",
                "last_error_code",
            ] {
                intent[key] = "".into();
            }
            if intent != *old {
                changes.push((id, intent));
            }
            continue;
        }
        if recovery_rules::obsolete_rate_intent(row, &summary, old, at) {
            changes.push((id, recovery_rules::cancel_obsolete_rate_intent(old)));
            continue;
        }
        if truthy(&old["verification_dispatched"])
            && !matches!(string(&old["status"]), "cancelled" | "recovered")
        {
            continue;
        }
        let descriptor = recovery_rules::quota_descriptor(&summary, at);
        let descriptor = if truthy(&descriptor) {
            descriptor
        } else {
            recovery_rules::threshold_descriptor(row, &summary, at)
        };
        if old["status"] == "retry_exhausted"
            && !truthy(&descriptor)
            && parse_datetime(&row["rate_limited_at"]).is_none_or(|limited| {
                parse_datetime(&old["test_completed_at"])
                    .is_some_and(|completed| limited <= completed)
            })
        {
            continue;
        }
        if !truthy(&descriptor)
            && matches!(
                string(&old["status"]),
                "pending" | "ready" | "retry" | "waiting_quota" | "testing"
            )
        {
            continue;
        }
        let descriptor = if truthy(&descriptor) {
            descriptor
        } else {
            recovery_rules::rate_descriptor(row, &summary, at)
        };
        if !truthy(&descriptor)
            || descriptor["fingerprint"] == old["fingerprint"] && old["status"] != "cancelled"
        {
            continue;
        }
        if parse_datetime(&old["recovered_at"])
            .zip(parse_datetime(&row["rate_limited_at"]))
            .is_some_and(|(recovered, blocked)| blocked <= recovered)
            && descriptor["kind"] == "rate_limit_block"
        {
            continue;
        }
        let unfinished = old["retry_policy"].is_null()
            && matches!(
                string(&old["status"]),
                "pending" | "ready" | "retry" | "waiting_quota"
            );
        let mut intent = if unfinished { old.clone() } else { json!({}) };
        intent
            .as_object_mut()
            .unwrap()
            .extend(descriptor.as_object().unwrap().clone());
        for (field, default) in [
            ("status", json!("pending")),
            ("attempt_count", json!(0)),
            ("next_retry_at", json!("")),
            ("last_error", json!("")),
            ("last_error_code", json!("")),
        ] {
            intent[field] = if unfinished {
                old.get(field).cloned().unwrap_or(default)
            } else {
                default
            };
        }
        if intent != *old {
            changes.push((id, intent));
        }
    }
    if !changes.is_empty() {
        store.transaction(|state| {
            for (id, intent) in changes {
                let old = &previous["scheduler"][id.to_string()];
                let meta = &mut state["scheduler"][id.to_string()];
                if !meta.is_object() {
                    *meta = json!({});
                }
                if meta["recovery_intent"] == old["recovery_intent"]
                    && meta["manual_control"] == old["manual_control"]
                {
                    meta["recovery_intent"] = intent;
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}
pub fn quota_update(
    item: &Value,
    result: &Value,
    state: &Value,
    at: DateTime<Utc>,
    recovery_enabled: bool,
    reused: bool,
) -> (Update, Option<Value>, bool) {
    let (mut update, mut runnable, depleted) =
        quota_update_inner(item, result, state, at, recovery_enabled, reused);
    let id = integer(&item["account_id"]).unwrap_or(0);
    let previous = &state["scheduler"][id.to_string()]["recovery_intent"];
    if integer(&previous["connection_attempt_count"]).unwrap_or(0) > 0 {
        if let Some(intent) = update
            .scheduler
            .get_mut(&id)
            .and_then(|meta| meta.get_mut("recovery_intent"))
        {
            if intent["fingerprint"] == previous["fingerprint"] {
                // Quota refreshes cannot restart a connection verification round or
                // replace its failure deadline with the quota-query backoff.
                if previous["status"] == "retry_exhausted" {
                    *intent = previous.clone();
                    runnable = None;
                } else if !matches!(
                    string(&intent["status"]),
                    "auth_failed" | "blocked" | "cancelled" | "recovered"
                ) {
                    intent["next_retry_at"] = previous["next_retry_at"].clone();
                    if crate::compat::parse_datetime(&intent["next_retry_at"])
                        .is_some_and(|due| at < due)
                    {
                        intent["status"] = "retry".into();
                        runnable = None;
                    } else if runnable.is_some() {
                        runnable = Some(intent.clone());
                    }
                }
            }
        }
    }
    (update, runnable, depleted)
}

fn quota_update_inner(
    item: &Value,
    result: &Value,
    state: &Value,
    at: DateTime<Utc>,
    recovery_enabled: bool,
    reused: bool,
) -> (Update, Option<Value>, bool) {
    let id = integer(&item["account_id"]).unwrap_or(0);
    let row = &item["row"];
    let meta = &state["scheduler"][id.to_string()];
    let mut update = Update::default();
    let mut intent = meta["recovery_intent"]
        .as_object()
        .cloned()
        .map(Value::Object)
        .unwrap_or(json!({}));
    if truthy(&result["skipped"]) {
        if truthy(&intent) {
            intent["waiting_reason"] = string(&result["error_code"]).into();
            intent["waiting_until"] = result["next_query_at"].clone();
            update
                .scheduler
                .insert(id, json!({"recovery_intent":intent}));
        }
        return (update, None, false);
    }
    if !recovery_rules::control_allows(row, result, meta, at) {
        return (update, None, false);
    }
    let mut metadata = json!({"last_attempt_at":datetime(at),"last_reason":item["reason"]});
    if reused {
        metadata
            .as_object_mut()
            .unwrap()
            .shift_remove("last_attempt_at");
    }
    if item["reason"] == "exact_reset" {
        metadata["last_exact_at"] = datetime(at).into();
        metadata["last_exact_fingerprint"] = string(&item["exact_fingerprint"]).into();
    }
    if !truthy(&result["success"]) {
        let code = string(&result["error_code"]);
        let auth = auth_code(code);
        metadata["last_error_code"] = code.into();
        metadata["last_error_at"] = datetime(at).into();
        if truthy(&intent) {
            if !auth.is_empty() {
                intent["status"] = "auth_failed".into();
                intent["next_retry_at"] = "".into();
                intent["last_error_code"] = auth.into();
            } else {
                let attempts = integer(&intent["attempt_count"]).unwrap_or(0) + 1;
                intent["status"] = "retry".into();
                intent["attempt_count"] = attempts.into();
                intent["next_retry_at"] = recovery_rules::retry_at(at, attempts).into();
                intent["last_error_code"] = if code.is_empty() {
                    "oauth_usage_query_error"
                } else {
                    code
                }
                .into();
            }
            intent["last_error"] = string(&result["error"]).into();
            metadata["recovery_intent"] = intent;
        }
        if !auth.is_empty() {
            let key = format!("auth:{id}:active_usage:{auth}");
            if !known(&key, id, state) {
                update.pending.insert(key.clone(),json!({"account_id":id,"account_name":row["name"].as_str().filter(|v|!v.is_empty()).unwrap_or("-"),"plan_type":quota::account_plan(row),"status":"auth_failed","stage":"active_usage","error_code":auth,"error":string(&result["error"]),"checked_at":datetime(at),"dedupe_key":key}));
            }
        }
        update.scheduler.insert(id, metadata);
        return (update, None, false);
    }
    update.results.insert(id, result.clone());
    metadata["last_success_at"] = datetime(at).into();
    metadata["last_error_code"] = "".into();
    let prefix = format!("auth:{id}:active_usage:");
    metadata["notified_keys"] = json!(meta["notified_keys"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| !string(v).starts_with(&prefix))
        .cloned()
        .collect::<Vec<_>>());
    update.remove_pending.extend(
        state["pending_events"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(key, _)| key.clone()),
    );
    let summary = quota::summary_from_result(row, result);
    if recovery_rules::obsolete_healthy_intent(row, result, meta, at) {
        metadata["recovery_intent"] = recovery_rules::cancel_healthy_intent(&intent);
        update.scheduler.insert(id, metadata);
        return (update, None, false);
    }
    let quota_descriptor = recovery_rules::quota_descriptor(&summary, at);
    let threshold = recovery_rules::threshold_descriptor(row, &summary, at);
    let descriptor = if truthy(&quota_descriptor) {
        &quota_descriptor
    } else {
        &threshold
    };
    let depleted = truthy(&quota_descriptor);
    let released_early = recovery_rules::early_quota_release(row, result, meta, at);
    let transition = recovery_transition(&item["previous_summary"], &summary, at);
    if !truthy(descriptor)
        && !truthy(&transition)
        && recovery_rules::obsolete_rate_intent(row, &summary, &intent, at)
    {
        metadata["recovery_intent"] = recovery_rules::cancel_obsolete_rate_intent(&intent);
        update.scheduler.insert(id, metadata);
        return (update, None, false);
    }
    if truthy(descriptor) {
        if descriptor["fingerprint"] != intent["fingerprint"] {
            intent = new_intent(descriptor, "pending", None);
        } else {
            intent
                .as_object_mut()
                .unwrap()
                .extend(descriptor.as_object().unwrap().clone());
        }
        let due = parse_datetime(&intent["due_at"]);
        let threshold_blocked =
            truthy(&threshold) && !recovery_rules::threshold_available(&summary, &threshold);
        if due.is_none() || due.is_some_and(|t| at >= t) && (depleted || threshold_blocked) {
            intent["status"] = "waiting_quota".into();
            intent["next_retry_at"] = datetime(at + Duration::seconds(3600)).into();
            intent["last_error"] = "到点后必要额度窗口仍未释放".into();
            intent["last_error_code"] = "quota_not_released".into();
        }
        metadata["recovery_intent"] = intent.clone();
        if !released_early
            && (due.is_none() || due.is_some_and(|t| at < t) || depleted || threshold_blocked)
        {
            update.scheduler.insert(id, metadata);
            return (update, None, depleted);
        }
    }
    if !recovery_enabled || !recovery_rules::all_available(&summary) {
        if truthy(&intent)
            && !matches!(
                string(&intent["status"]),
                "recovered" | "auth_failed" | "blocked"
            )
        {
            let attempts = integer(&intent["attempt_count"]).unwrap_or(0) + 1;
            intent["status"] = "retry".into();
            intent["attempt_count"] = attempts.into();
            intent["next_retry_at"] = recovery_rules::retry_at(at, attempts).into();
            intent["last_error"] = "新鲜额度响应缺少必要窗口".into();
            intent["last_error_code"] = "incomplete_quota".into();
            metadata["recovery_intent"] = intent;
        }
        update.scheduler.insert(id, metadata);
        return (update, None, depleted);
    }
    if released_early {
        intent["due_at"] = result["queried_at"].clone();
        intent["next_retry_at"] = "".into();
        intent["waiting_reason"] = "".into();
        intent["waiting_until"] = "".into();
    }
    if !released_early
        && truthy(&transition)
        && truthy(&transition["fingerprint"])
        && transition["fingerprint"] != intent["fingerprint"]
    {
        intent = new_intent(
            &json!({"fingerprint":transition["fingerprint"],"due_at":transition["reset_at"],"source":transition.get("reset_source").unwrap_or(&json!("server_exact")),"window_keys":transition["window_keys"]}),
            "ready",
            Some(at),
        );
        for key in [
            "early_reset_detected",
            "old_7d_used_percent",
            "new_7d_used_percent",
            "old_reset_at",
            "detected_at",
        ] {
            if let Some(value) = transition.get(key) {
                intent[key] = value.clone();
            }
        }
    }
    if !truthy(&intent)
        || matches!(
            string(&intent["status"]),
            "recovered" | "auth_failed" | "blocked"
        )
    {
        let descriptor = recovery_rules::rate_descriptor(row, &summary, at);
        if truthy(&descriptor) && descriptor["fingerprint"] != intent["fingerprint"] {
            intent = new_intent(&descriptor, "ready", Some(at));
        }
    }
    if !truthy(&intent)
        || matches!(
            string(&intent["status"]),
            "recovered" | "auth_failed" | "blocked"
        )
    {
        update.scheduler.insert(id, metadata);
        return (update, None, depleted);
    }
    intent["confirmed_at"] = datetime(at).into();
    if reused && intent["status"] == "waiting_quota" {
        intent["next_retry_at"] = "".into();
    }
    let runnable = if parse_datetime(&intent["next_retry_at"]).is_none_or(|t| at >= t) {
        intent["status"] = "ready".into();
        metadata["recovery_intent"] = intent.clone();
        Some(intent)
    } else {
        None
    };
    update.scheduler.insert(id, metadata);
    (update, runnable, depleted)
}
impl OAuthState {
    pub fn mark_delivered(&self, events: &[Value], suppressed: bool) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        self.transaction(|state| {
            let at = crate::compat::now();
            for event in events {
                let key = string(&event["dedupe_key"]).trim();
                let id = integer(&event["account_id"]).unwrap_or(0);
                if key.is_empty() || id <= 0 {
                    continue;
                }
                let meta = &mut state["scheduler"][id.to_string()];
                if !meta.is_object() {
                    *meta = json!({});
                }
                let mut notified = meta["notified_keys"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| !string(v).is_empty())
                    .cloned()
                    .collect::<Vec<_>>();
                if !notified.iter().any(|v| v == key) {
                    notified.push(key.into());
                }
                let start = notified.len().saturating_sub(40);
                meta["notified_keys"] = json!(notified[start..]);
                meta["last_notification_at"] = at.clone().into();
                meta["last_notification_suppressed"] = suppressed.into();
                state["pending_events"]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove(key);
            }
            Ok(())
        })
    }
}
pub fn project(meta: &Value) -> Value {
    let intent = &meta["recovery_intent"];
    if !truthy(intent) || matches!(string(&intent["status"]), "cancelled" | "recovered") {
        return Value::Null;
    }
    let reason = if truthy(&intent["waiting_reason"]) {
        string(&intent["waiting_reason"])
    } else {
        string(&intent["last_error_code"])
    };
    let label = if truthy(&intent["verification_dispatched"]) && reason != "verification_uncertain"
    {
        "验证中"
    } else {
        match reason {
            "waiting_reset" => "等待额度恢复时间",
            "query_cooldown" => "额度查询冷却中",
            "query_budget" => "等待额度查询预算",
            "query_backoff" => "额度查询退避中",
            "query_slots" => "等待额度查询执行槽",
            "test_slots" => "等待验证执行槽",
            "worker_slots" => "等待恢复执行槽",
            "account_busy" => "等待账号操作完成",
            "incomplete_quota" => "缺少跨恢复点的完整额度证据",
            "verification_backoff" => "等待验证重试",
            "retry_exhausted" => "连接测试重试次数已用尽，请重新启用恢复或等待新恢复事件",
            "model_verification_pending" => "等待模型验证",
            "model_cooldown" => "模型验证冷却中",
            "model_busy" => "等待当前模型测试结束",
            "auth_paused" | "http_401" | "http_402" => "等待凭据更新",
            "verification_uncertain" => "上次验证结果未保存，需人工复核",
            _ => "等待恢复验证",
        }
    };
    let mut projected = json!({"status":if truthy(&intent["status"]) {string(&intent["status"])} else {"pending"},"label":label,"reason":reason,"next_at":if truthy(&intent["waiting_until"]) {&intent["waiting_until"]} else if truthy(&intent["next_retry_at"]) {&intent["next_retry_at"]} else {&Value::Null}});
    if let Some(attempts) = intent.get("connection_attempt_count") {
        projected["attempt_count"] = attempts.clone();
        projected["next_retry_at"] = if truthy(&intent["next_retry_at"]) {
            intent["next_retry_at"].clone()
        } else {
            Value::Null
        };
    }
    projected
}
