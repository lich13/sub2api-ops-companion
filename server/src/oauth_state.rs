use crate::{
    compat::{datetime, integer, parse_datetime, pick, py_json, sha256, string, truthy},
    error::{Error, Result},
    quota,
    store::Store,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn invalid(message: &str) -> Error {
    Error::new(503, message)
}
pub fn hex_digest(value: &Value) -> bool {
    string(value).len() == 64
        && string(value)
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn optional_time(value: &Value) -> bool {
    !truthy(value) || parse_datetime(value).is_some()
}

pub fn validate_credit(value: &Value) -> Result<()> {
    if !value.is_object()
        || !matches!(
            string(&value["stage"]),
            "waiting"
                | "pausing"
                | "resetting"
                | "uncertain"
                | "testing"
                | "retry"
                | "confirming"
                | "releasing"
                | "recovered"
                | "manual"
                | "blocked"
                | "closed"
                | "finished"
        )
        || !truthy(&value["episode"])
    {
        return Err(invalid("自动用卡状态无效"));
    }
    for key in [
        "attempt_at",
        "reset_at",
        "next_at",
        "test_completed_at",
        "recovered_at",
        "evidence_at",
        "reset_completed_at",
        "release_requested_at",
        "manual_at",
        "trigger_at",
        "consume_deadline_at",
        "verification_deadline_at",
        "completed_at",
        "policy_checked_at",
        "supplement_attempt_at",
        "clear_requested_at",
        "pause_undo_requested_at",
        "pause_undo_confirmed_at",
        "recovered_evidence_at",
    ] {
        if !optional_time(&value[key]) {
            return Err(invalid("自动用卡时间无效"));
        }
    }
    if !value["owns_pause"].is_null() && !value["owns_pause"].is_boolean() {
        return Err(invalid("自动用卡暂停归属无效"));
    }
    if !value["policy_version"].is_null() {
        if value["policy_version"].as_i64() != Some(1) {
            return Err(invalid("自动用卡策略版本无效"));
        }
        for key in [
            "test_attempts",
            "policy_control_generation",
            "policy_method_generation",
        ] {
            if value
                .get(key)
                .is_some_and(|v| v.as_i64().is_none_or(|v| v < 0))
            {
                return Err(invalid("自动用卡控制状态无效"));
            }
        }
        if !value["outcome"].is_null()
            && !matches!(
                string(&value["outcome"]),
                "restored" | "not_executed" | "consume_failed" | "unrecovered" | "unknown"
            )
        {
            return Err(invalid("自动用卡结果无效"));
        }
        if !value["policy_detection_version"].is_null()
            && !hex_digest(&value["policy_detection_version"])
        {
            return Err(invalid("自动用卡检测控制版本无效"));
        }
    }
    Ok(())
}

pub fn normalized_credits(value: &Value) -> Option<Value> {
    let count = value["available_count"].as_i64().filter(|v| *v >= 0)?;
    let empty = vec![];
    let cards = if value.get("credits").is_none() {
        &empty
    } else {
        value["credits"].as_array()?
    };
    if cards.len() > 10_000 {
        return None;
    }
    let mut expires: Vec<String> = cards
        .iter()
        .map(|v| parse_datetime(&v["expires_at"]).map(datetime))
        .collect::<Option<_>>()?;
    if count as usize > expires.len() {
        return None;
    }
    expires.sort();
    Some(json!({"available_count":count,"expires_at":expires}))
}
pub fn available_credits(value: &Value, at: DateTime<Utc>) -> Option<i64> {
    let content = normalized_credits(value)?;
    let expired = content["expires_at"]
        .as_array()?
        .iter()
        .filter(|v| parse_datetime(v).is_some_and(|v| v <= at))
        .count();
    Some((content["available_count"].as_i64()? - expired as i64).max(0))
}
pub fn compact_digest(value: &Value) -> String {
    sha256(py_json(value, true, true))
}
pub fn limit_fingerprint(row: &Value) -> String {
    compact_digest(&json!([
        crate::compat::text(&row["id"]),
        parse_datetime(&row["rate_limited_at"]).map(datetime),
        parse_datetime(&row["rate_limit_reset_at"]).map(datetime)
    ]))
}
pub fn validate_observation(value: &Value) -> Result<()> {
    let keys: BTreeSet<&str> = [
        "version",
        "observed_at",
        "available_count",
        "expires_at",
        "snapshot_sha256",
        "credential_fingerprint",
        "window_reset_at",
        "window_minutes",
        "limit_fingerprint",
    ]
    .into();
    if value
        .as_object()
        .is_none_or(|v| v.keys().map(String::as_str).collect::<BTreeSet<_>>() != keys)
        || value["version"].as_i64() != Some(1)
    {
        return Err(invalid("重置卡观测状态无效"));
    }
    if [
        "snapshot_sha256",
        "credential_fingerprint",
        "limit_fingerprint",
    ]
    .into_iter()
    .any(|k| !hex_digest(&value[k]))
    {
        return Err(invalid("重置卡观测摘要无效"));
    }
    if ["observed_at", "window_reset_at"]
        .into_iter()
        .any(|k| parse_datetime(&value[k]).is_none())
    {
        return Err(invalid("重置卡观测时间无效"));
    }
    if !value["window_minutes"].is_number()
        || value["window_minutes"]
            .as_f64()
            .is_none_or(|v| v <= 0.0 || v > 5_256_000.0)
    {
        return Err(invalid("重置卡观测窗口无效"));
    }
    let expires = value["expires_at"]
        .as_array()
        .ok_or_else(|| invalid("重置卡到期信息无效"))?;
    let content = normalized_credits(&json!({"available_count":value["available_count"],"credits":expires.iter().map(|v|json!({"expires_at":v})).collect::<Vec<_>>()})).ok_or_else(||invalid("重置卡观测内容无效"))?;
    if content["expires_at"] != value["expires_at"]
        || compact_digest(&content) != value["snapshot_sha256"]
    {
        return Err(invalid("重置卡观测内容无效"));
    }
    Ok(())
}

pub fn query_metadata(meta: &Value, saved: &Value) -> Result<Value> {
    if meta.get("quota_query").is_some_and(|v| !v.is_object()) {
        return Err(invalid("OAuth 查询状态无效"));
    }
    let mut value = meta["quota_query"]
        .as_object()
        .cloned()
        .map(Value::Object)
        .unwrap_or(json!({}));
    if value.get("automatic_attempts").is_some_and(|v| {
        v.as_array()
            .is_none_or(|v| v.iter().any(|v| parse_datetime(v).is_none()))
    }) {
        return Err(invalid("OAuth 查询预算无效"));
    }
    if value
        .get("failure_count")
        .is_some_and(|v| v.as_i64().is_none_or(|v| v < 0))
    {
        return Err(invalid("OAuth 查询退避状态无效"));
    }
    for key in ["last_query_at", "retry_at"] {
        if !value[key].is_null() && value[key] != "" && parse_datetime(&value[key]).is_none() {
            return Err(invalid("OAuth 查询时间无效"));
        }
    }
    if !truthy(&value["last_query_at"]) {
        value["last_query_at"] = json!([
            &meta["last_attempt_at"],
            &meta["last_success_at"],
            if saved["source"] != "passive" {
                &saved["queried_at"]
            } else {
                &Value::Null
            }
        ]
        .into_iter()
        .filter_map(parse_datetime)
        .max()
        .map(datetime));
    }
    Ok(value)
}

pub fn validate(value: &Value) -> Result<()> {
    if !value.is_object() || value["version"].as_i64().is_some_and(|v| v > 5) {
        return Err(invalid("OAuth 状态文件无效"));
    }
    for section in [
        "settings",
        "scheduler",
        "oauth_results",
        "pending_events",
        "daily_test",
        "recovery_history",
    ] {
        if value.get(section).is_some_and(|v| !v.is_object()) {
            return Err(invalid("OAuth 状态分区无效"));
        }
    }
    for meta in value["scheduler"]
        .as_object()
        .into_iter()
        .flat_map(|v| v.values())
    {
        if !meta.is_object() {
            return Err(invalid("OAuth 调度状态无效"));
        }
        if meta.get("quota_query").is_some() {
            query_metadata(meta, &Value::Null)?;
        }
        if let Some(credit) = meta.get("auto_reset_credit") {
            validate_credit(credit)?;
        }
        if let Some(notices) = meta.get("credit_result_notifications") {
            let notices = notices
                .as_object()
                .ok_or_else(|| invalid("用卡通知状态无效"))?;
            for (episode, notice) in notices {
                if !notice.is_object()
                    || !matches!(
                        string(&notice["status"]),
                        "pending" | "delivered" | "failed" | "suppressed"
                    )
                    || notice["episode"] != *episode
                    || !matches!(
                        string(&notice["outcome"]),
                        "restored" | "not_executed" | "consume_failed" | "unrecovered" | "unknown"
                    )
                    || notice["account_id"].as_i64().is_none_or(|v| v <= 0)
                    || notice["attempts"]
                        .as_i64()
                        .is_none_or(|v| !(0..=5).contains(&v))
                    || parse_datetime(&notice["completed_at"]).is_none()
                    || (notice["status"] == "pending"
                        && parse_datetime(&notice["next_at"]).is_none())
                    || !optional_time(&notice["next_at"])
                {
                    return Err(invalid("用卡通知状态无效"));
                }
            }
        }
        if let Some(observation) = meta.get("reset_credit_observation") {
            validate_observation(observation)?;
        }
        if let Some(admission) = meta.get("credit_admission").filter(|v| !v.is_null()) {
            if !admission.is_object()
                || parse_datetime(&admission["trigger_at"]).is_none()
                || parse_datetime(&admission["checked_at"]).is_none()
                || !admission["reason"].is_string()
                || !admission["evidence_fingerprint"].is_string()
                || !optional_time(&admission["recovered_at"])
            {
                return Err(invalid("重置卡准入状态无效"));
            }
        }
        if let Some(control) = meta.get("manual_control") {
            if !control.is_object()
                || control["generation"].as_i64().is_none_or(|v| v < 1)
                || !control["enabled"].is_boolean()
                || parse_datetime(&control["at"]).is_none()
            {
                return Err(invalid("人工调度控制状态无效"));
            }
        }
        let intent = &meta["recovery_intent"];
        if intent.is_object() {
            crate::connection_retry::Policy::saved(intent)?;
            if intent
                .get("connection_attempt_count")
                .is_some_and(|value| value.as_u64().is_none())
            {
                return Err(invalid("连接测试尝试次数无效"));
            }
            if intent
                .get("verification_dispatched")
                .is_some_and(|v| !v.is_boolean())
            {
                return Err(invalid("恢复验证派发状态无效"));
            }
            if let Some(receipt) = intent.get("verification_receipt").filter(|v| !v.is_null()) {
                if !receipt.is_object()
                    || !receipt["result"].is_object()
                    || !receipt["block_state"].is_object()
                    || string(&receipt["credential_fingerprint"]).len() != 64
                {
                    return Err(invalid("恢复验证回执无效"));
                }
            }
        }
    }
    Ok(())
}

pub fn normalize(raw: &Value) -> Value {
    let mut settings = json!({});
    let token = string(&raw["settings"]["sub2api_admin_token"]).trim();
    if !token.is_empty() {
        settings["sub2api_admin_token"] = token.into();
    }
    if raw["settings"]["legacy_recovery_cleanup_completed"] == true {
        settings["legacy_recovery_cleanup_completed"] = true.into();
        if truthy(&raw["settings"]["legacy_recovery_cleanup_completed_at"]) {
            settings["legacy_recovery_cleanup_completed_at"] =
                raw["settings"]["legacy_recovery_cleanup_completed_at"].clone();
        }
    }
    let mut results = Map::new();
    for source in ["results", "oauth_results"] {
        for (key, value) in raw[source].as_object().into_iter().flatten() {
            if let Ok(id) = key.parse::<i64>() {
                if id > 0
                    && value.is_object()
                    && (string(&value["template_type"]).to_lowercase() == "oauth"
                        || value["source"] == "sub2api_admin_usage"
                        || value["oauth_quota"].is_object())
                {
                    results.insert(id.to_string(), value.clone());
                }
            }
        }
    }
    let mut scheduler = Map::new();
    for (key, value) in raw["scheduler"].as_object().into_iter().flatten() {
        let Ok(id) = key.parse::<i64>() else {
            continue;
        };
        if id <= 0 || !value.is_object() {
            continue;
        }
        let mut value = value.clone();
        for retired in ["last_regular_at", "last_7d_probe_at"] {
            value.as_object_mut().unwrap().shift_remove(retired);
        }
        if matches!(
            string(&value["last_reason"]),
            "regular_refresh" | "seven_day_probe"
        ) {
            value.as_object_mut().unwrap().shift_remove("last_reason");
        }
        if let Some(intent) = value.get_mut("recovery_intent") {
            if !intent.is_object() {
                *intent = json!({});
            }
            if intent["status"] == "deferred" {
                intent["status"] = "ready".into();
                intent["deferred_until"] = "".into();
                intent["next_retry_at"] = "".into();
            }
            if intent["status"] == "testing" {
                intent["status"] = "retry".into();
                intent["next_retry_at"] = ["next_retry_at", "tested_at", "confirmed_at", "due_at"]
                    .into_iter()
                    .map(|k| &intent[k])
                    .find(|v| truthy(v))
                    .cloned()
                    .unwrap_or(json!(""));
            }
        }
        scheduler.insert(id.to_string(), value);
    }
    let pending: Map<_, _> = raw["pending_events"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, v)| !k.trim().is_empty() && v.is_object())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut history: Map<_, _> = raw["recovery_history"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, v)| v.is_object())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut sequence = history
        .values()
        .filter_map(|v| integer(&v["id"]))
        .max()
        .unwrap_or(0);
    for (account, meta) in &scheduler {
        let Ok(account_id) = account.parse::<i64>() else {
            continue;
        };
        let intent = &meta["recovery_intent"];
        if intent["status"] != "recovered" {
            continue;
        }
        let key = format!(
            "recovery:{account}:{}:success",
            crate::compat::text(&intent["fingerprint"])
        );
        if !history.contains_key(&key) {
            sequence += 1;
            history.insert(key.clone(),json!({"id":sequence,"dedupe_key":key,"account_id":account_id,"account_name":"","model_id":"","test_completed_at":null,"recovered_at":null,"legacy":true}));
        }
    }
    json!({"version":5,"settings":settings,"oauth_results":results,"scheduler":scheduler,"pending_events":pending,"daily_test":raw["daily_test"].as_object().cloned().unwrap_or_default(),"recovery_history":history})
}

pub fn block_signature(row: &Value) -> String {
    let mut value = pick(
        row,
        &[
            "id",
            "platform",
            "type",
            "status",
            "schedulable",
            "concurrency",
            "rate_limited_at",
            "rate_limit_reset_at",
            "overload_until",
            "temp_unschedulable_until",
            "temp_unschedulable_reason",
        ],
    );
    // Database timestamp columns are represented internally with Python str's
    // space separator. This particular hash historically uses datetime ISO.
    for key in [
        "rate_limited_at",
        "rate_limit_reset_at",
        "overload_until",
        "temp_unschedulable_until",
    ] {
        if string(&value[key]).contains(' ') {
            if let Some(time) = parse_datetime(&value[key]) {
                value[key] = datetime(time).into();
            }
        }
    }
    value["model_rate_limits"] = row["extra"]["model_rate_limits"].clone();
    value["antigravity_quota_scopes"] = row["extra"]["antigravity_quota_scopes"].clone();
    sha256(py_json(&value, false, true))
}

const MAX_TERMINAL_ACCOUNT_CREDIT_HISTORY: usize = 200;
const MAX_RECOVERY_HISTORY: usize = 10_000;

fn prune_credit_history(state: &mut Value) {
    if let Some(scheduler) = state["scheduler"].as_object_mut() {
        for meta in scheduler.values_mut() {
            let active_episode = meta["auto_reset_credit"]["episode"]
                .as_str()
                .map(str::to_owned);
            let mut pending_notice_episodes = std::collections::HashSet::new();
            if let Some(notices) = meta["credit_result_notifications"].as_object() {
                for (ep, notice) in notices {
                    if notice["status"] == "pending" {
                        pending_notice_episodes.insert(ep.clone());
                    }
                }
            }
            if meta["credit_result_notifications"]
                .as_object()
                .is_some_and(|m| m.len() > MAX_TERMINAL_ACCOUNT_CREDIT_HISTORY)
            {
                let mut terminals = meta["credit_result_notifications"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .enumerate()
                    .filter(|(_, (ep, notice))| {
                        notice["status"] != "pending"
                            && active_episode.as_deref() != Some(ep.as_str())
                    })
                    .map(|(idx, (ep, notice))| {
                        (
                            notice["completed_at"].as_str().unwrap_or("").to_owned(),
                            idx,
                            ep.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                if terminals.len() > MAX_TERMINAL_ACCOUNT_CREDIT_HISTORY {
                    terminals.sort();
                    let drop_count = terminals.len() - MAX_TERMINAL_ACCOUNT_CREDIT_HISTORY;
                    let map = meta["credit_result_notifications"].as_object_mut().unwrap();
                    for (_, _, ep) in terminals.into_iter().take(drop_count) {
                        map.shift_remove(&ep);
                    }
                }
            }
            if meta["reset_credit_episodes"]
                .as_object()
                .is_some_and(|m| m.len() > MAX_TERMINAL_ACCOUNT_CREDIT_HISTORY)
            {
                let mut terminals = meta["reset_credit_episodes"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .enumerate()
                    .filter(|(_, (ep, item))| {
                        let stage = string(&item["stage"]);
                        let is_terminal = matches!(
                            stage,
                            "recovered" | "manual" | "blocked" | "closed" | "finished"
                        ) && !(stage == "finished"
                            && truthy(&item["owns_pause"]));
                        is_terminal
                            && active_episode.as_deref() != Some(ep.as_str())
                            && !pending_notice_episodes.contains(*ep)
                    })
                    .map(|(idx, (ep, item))| {
                        let stamp = item["completed_at"]
                            .as_str()
                            .or_else(|| item["manual_at"].as_str())
                            .or_else(|| item["recovered_at"].as_str())
                            .or_else(|| item["attempt_at"].as_str())
                            .unwrap_or("")
                            .to_owned();
                        (stamp, idx, ep.clone())
                    })
                    .collect::<Vec<_>>();
                if terminals.len() > MAX_TERMINAL_ACCOUNT_CREDIT_HISTORY {
                    terminals.sort();
                    let drop_count = terminals.len() - MAX_TERMINAL_ACCOUNT_CREDIT_HISTORY;
                    let map = meta["reset_credit_episodes"].as_object_mut().unwrap();
                    for (_, _, ep) in terminals.into_iter().take(drop_count) {
                        map.shift_remove(&ep);
                    }
                }
            }
        }
    }
    if state["recovery_history"]
        .as_object()
        .is_some_and(|m| m.len() > MAX_RECOVERY_HISTORY)
    {
        let mut entries = state["recovery_history"]
            .as_object()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(idx, (key, item))| (integer(&item["id"]).unwrap_or(0), idx, key.clone()))
            .collect::<Vec<_>>();
        entries.sort();
        let drop_count = entries.len() - MAX_RECOVERY_HISTORY;
        let map = state["recovery_history"].as_object_mut().unwrap();
        for (_, _, key) in entries.into_iter().take(drop_count) {
            map.shift_remove(&key);
        }
    }
}

#[derive(Clone)]
pub struct OAuthState(pub Store, Arc<Mutex<Option<Normalized>>>);

struct Normalized {
    source: Arc<Value>,
    value: Arc<Value>,
}

#[derive(Default)]
pub struct Update {
    pub results: BTreeMap<i64, Value>,
    pub scheduler: BTreeMap<i64, Value>,
    pub pending: BTreeMap<String, Value>,
    pub remove_pending: BTreeSet<String>,
    pub daily: Option<Value>,
    pub history: BTreeMap<String, Value>,
    pub generations: Option<BTreeMap<i64, i64>>,
    pub methods: Option<BTreeMap<i64, i64>>,
}

impl OAuthState {
    pub fn new(path: PathBuf, read_only: bool) -> Self {
        let lock = PathBuf::from(format!("{}.lock", path.display()));
        Self(
            Store::with_lock(path, lock, json!({}), validate, read_only),
            Arc::new(Mutex::new(None)),
        )
    }
    pub fn snapshot(&self) -> Result<Value> {
        self.shared_snapshot().map(|value| (*value).clone())
    }
    pub fn shared_snapshot(&self) -> Result<Arc<Value>> {
        // Always consult Store first: its file stamp also catches external
        // replacements and corrupt writes even when this projection is cached.
        let source = self.0.read_shared()?;
        Ok(self.normalized_snapshot(source))
    }
    pub(crate) fn normalized_snapshot(&self, source: Arc<Value>) -> Arc<Value> {
        let mut cache = self.1.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(cached) = cache
            .as_ref()
            .filter(|cached| Arc::ptr_eq(&cached.source, &source))
        {
            return cached.value.clone();
        }
        let value = Arc::new(normalize(&source));
        *cache = Some(Normalized {
            source,
            value: value.clone(),
        });
        value
    }
    pub fn transaction<T>(&self, action: impl FnOnce(&mut Value) -> Result<T>) -> Result<T> {
        self.0.transaction(|value| {
            *value = normalize(value);
            let out = action(value)?;
            prune_credit_history(value);
            Ok(out)
        })
    }
    pub fn admin_token(&self) -> Result<String> {
        Ok(
            string(&self.0.read_shared()?["settings"]["sub2api_admin_token"])
                .trim()
                .to_owned(),
        )
    }
    pub fn save_admin_token(&self, key: &str) -> Result<()> {
        let key = key.trim();
        if key.is_empty() {
            return Ok(());
        }
        if self.0.is_read_only() {
            return Err(Error::read_only());
        }
        if self.admin_token()? == key {
            return Ok(());
        }
        self.transaction(|v| {
            v["settings"]["sub2api_admin_token"] = key.into();
            Ok(())
        })
    }
    pub fn commit(&self, update: Update) -> Result<()> {
        self.transaction(|state| {
            let matches = |id: i64, state: &Value| {
                let meta = &state["scheduler"][id.to_string()];
                update.generations.as_ref().is_none_or(|v| {
                    v.get(&id).copied().unwrap_or(0)
                        == integer(&meta["manual_control"]["generation"]).unwrap_or(0)
                }) && update.methods.as_ref().is_none_or(|v| {
                    v.get(&id).copied().unwrap_or(0)
                        == integer(&meta["recovery_method_generation"]).unwrap_or(0)
                })
            };
            for (id, value) in &update.results {
                if matches(*id, state) {
                    state["oauth_results"][id.to_string()] = value.clone();
                }
            }
            for (id, value) in &update.scheduler {
                if !matches(*id, state) {
                    continue;
                }
                let meta = &mut state["scheduler"][id.to_string()];
                if !meta.is_object() {
                    *meta = json!({});
                }
                meta.as_object_mut().unwrap().extend(
                    value
                        .as_object()
                        .cloned()
                        .ok_or_else(|| invalid("OAuth 调度状态无效"))?,
                );
            }
            for (key, event) in &update.pending {
                if matches(integer(&event["account_id"]).unwrap_or(0), state) {
                    state["pending_events"][key] = event.clone();
                }
            }
            let mut sequence = state["recovery_history"]
                .as_object()
                .unwrap()
                .values()
                .filter_map(|v| integer(&v["id"]))
                .max()
                .unwrap_or(0);
            for (key, event) in &update.history {
                if matches(integer(&event["account_id"]).unwrap_or(0), state)
                    && state["recovery_history"].get(key).is_none()
                {
                    sequence += 1;
                    let mut event = event.clone();
                    event["id"] = sequence.into();
                    event["dedupe_key"] = key.clone().into();
                    state["recovery_history"][key] = event;
                }
            }
            for key in &update.remove_pending {
                state["pending_events"]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove(key);
            }
            if let Some(daily) = &update.daily {
                state["daily_test"] = daily.clone();
            }
            Ok(())
        })
    }
    pub fn manual_control(
        &self,
        row: &Value,
        enabled: bool,
        at: DateTime<Utc>,
        origin: &str,
    ) -> Result<i64> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        self.transaction(|state| {
            let evidence = quota::latest_result(row,&state["oauth_results"][id.to_string()],at);
            let meta = &mut state["scheduler"][id.to_string()]; if !meta.is_object() { *meta = json!({}); }
            let generation = integer(&meta["manual_control"]["generation"]).unwrap_or(0)+1;
            meta["manual_control"] = json!({"generation":generation,"at":datetime(at),"enabled":enabled,"event_fingerprint":block_signature(row),"evidence_at":evidence["queried_at"],"origin":if origin == "model_detection" {"model_detection"} else {"manual"}});
            if truthy(&meta["recovery_intent"]) { meta["recovery_intent"]["status"] = "cancelled".into(); meta["recovery_intent"]["next_retry_at"] = "".into(); }
            let card = &mut meta["auto_reset_credit"];
            if truthy(card) && (!matches!(string(&card["stage"]),"recovered"|"closed"|"finished") || card["stage"] == "finished" && truthy(&card["owns_pause"])) {
                if card["stage"] != "finished" { card["stage"] = "manual".into(); card["next_at"] = Value::Null; }
                card["owns_pause"] = false.into(); card["manual_at"] = datetime(at).into(); card["revision"] = (integer(&card["revision"]).unwrap_or(0)+1).into();
            }
            // Do not materialize an absent optional control record as null.
            if meta["auto_reset_credit"].is_null() { meta.as_object_mut().unwrap().shift_remove("auto_reset_credit"); }
            state["pending_events"].as_object_mut().unwrap().retain(|_,v|integer(&v["account_id"]) != Some(id) || v["stage"] == "active_usage" || v["kind"] == "connection_test");
            Ok(generation)
        })
    }
}

#[cfg(test)]
mod shared_tests {
    use super::*;

    #[test]
    fn normalized_snapshots_reuse_reads_and_observe_external_changes_and_damage() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("oauth.json");
        let legacy = json!({"results":{"1":{"template_type":"oauth","success":true}},"scheduler":{"1":{"last_regular_at":"legacy"}}});
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let state = OAuthState::new(path.clone(), false);
        let first = state.shared_snapshot().unwrap();
        let original_source = state.0.read_shared().unwrap();
        assert_eq!(*first, normalize(&legacy));
        assert!(Arc::ptr_eq(
            &first,
            &state.clone().shared_snapshot().unwrap()
        ));
        let mut owned = state.snapshot().unwrap();
        owned["oauth_results"] = json!({});
        assert!(state.shared_snapshot().unwrap()["oauth_results"]["1"].is_object());
        state
            .transaction(|value| {
                value["scheduler"]["1"]["last_reason"] = "fixture".into();
                Ok(())
            })
            .unwrap();
        let second = state.shared_snapshot().unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(first["scheduler"]["1"]["last_reason"].is_null());
        assert_eq!(*state.normalized_snapshot(original_source), *first);
        assert_eq!(*state.shared_snapshot().unwrap(), *second);
        let replacement = root.path().join("replacement.json");
        std::fs::write(&replacement, b"{}").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert_eq!(state.shared_snapshot().unwrap()["oauth_results"], json!({}));
        std::fs::write(&path, b"invalid").unwrap();
        assert!(state.shared_snapshot().is_err());
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(*state.shared_snapshot().unwrap(), *first);
    }
}
