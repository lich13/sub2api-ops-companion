mod execution;
pub mod rules;

use crate::{
    actions::Actions,
    bark::Bark,
    compat::{config_revision, datetime, integer, parse_datetime, pick, string, truthy},
    db::Database,
    error::{Error, Result},
    oauth_admission::{self, credential_fingerprint, AUTH_ERRORS},
    oauth_queries::Coordinator,
    oauth_state::{available_credits, validate_credit, OAuthState},
    operations::TestSlots,
    quota, recovery_rules,
};
use chrono::{DateTime, Duration, Utc};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::Arc;

pub const ACCOUNT_SELECT: &str = "SELECT id,name,platform,type,credentials,nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id,extra,status,schedulable,priority AS account_priority,concurrency,load_factor,proxy_id,temp_unschedulable_until,temp_unschedulable_reason,rate_limited_at,rate_limit_reset_at,overload_until,error_message,expires_at,auto_pause_on_expired,updated_at,(SELECT created_at FROM usage_logs WHERE account_id=accounts.id ORDER BY created_at DESC,id DESC LIMIT 1) AS last_success_at FROM accounts WHERE deleted_at IS NULL";

pub async fn account(db: &Database, id: i64) -> Result<Value> {
    db.one(&format!("{ACCOUNT_SELECT} AND id=$1"), &[id.into()])
        .await?
        .ok_or_else(|| Error::new(404, "账号不存在"))
}

pub fn signature(row: &Value) -> String {
    let mut stable = pick(
        row,
        &[
            "id",
            "name",
            "platform",
            "type",
            "parent_account_id",
            "account_priority",
            "priority",
            "concurrency",
            "expires_at",
            "auto_pause_on_expired",
        ],
    );
    stable["account_priority"] = priority(row).clone();
    stable["priority"] = Value::Null;
    stable["identity"] = identity(row);
    stable["extra"] = row["extra"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| !quota_extra(key))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<serde_json::Map<_, _>>()
        .into();
    config_revision(&stable)
}

fn quota_extra(key: &str) -> bool {
    [
        "codex_5h_",
        "codex_7d_",
        "codex_primary_",
        "codex_secondary_",
        "codex_spark_",
        "codex_usage_",
        "codex_reset_credit_",
        "codex_credits_",
        "codex_turn_ticket:",
        "passive_usage_",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix))
        || matches!(
            key,
            "codex_auto_reset_credit_state"
                | "codex_referral_snapshot"
                | "codex_degradation_check"
                | "codex_fingerprint_seed"
                | "model_rate_limits"
                | "antigravity_quota_scopes"
                | "session_window_utilization"
                | "upstream_billing_probe"
                | "upstream_billing_rate_sync"
                | "ollama_cloud_usage"
                | "ollama_cloud_usage_snapshot"
                | "ollama_cloud_usage_session"
                | "opencode_go_usage_snapshot"
                | "grok_billing_snapshot"
                | "grok_usage_snapshot"
                | "grok_quota_snapshot"
                | "grok_sched_reset_at"
                | "privacy_mode"
                | "openai_compact_supported"
                | "openai_compact_last_status"
                | "openai_compact_last_error"
                | "upstream_model_metadata"
                | "quota_used"
                | "quota_daily_used"
                | "quota_daily_start"
                | "quota_daily_reset_at"
                | "quota_weekly_used"
                | "quota_weekly_start"
                | "quota_weekly_reset_at"
                | "crs_synced_at"
        )
}

fn priority(row: &Value) -> &Value {
    row.get("account_priority").unwrap_or(&row["priority"])
}

pub fn identity(row: &Value) -> Value {
    row.get("account_identity").cloned().unwrap_or_else(|| {
        pick(
            &row["credentials"],
            &["plan_type", "chatgpt_account_id", "email"],
        )
    })
}

pub fn generation(state: &Value, id: i64) -> i64 {
    integer(&state["scheduler"][id.to_string()]["manual_control"]["generation"]).unwrap_or(0)
}

pub fn owned(row: &Value, task: &Value, state: &Value) -> bool {
    truthy(task)
        && task["owns_pause"] == true
        && row["schedulable"] == false
        && task["signature"] == signature(row)
        && (task["control_generation"].is_null()
            || integer(&task["control_generation"])
                == Some(generation(state, integer(&row["id"]).unwrap_or(0))))
}

pub fn save(store: &OAuthState, id: i64, task: &Value, changes: Value) -> Result<Value> {
    let mut updated = task.clone();
    updated
        .as_object_mut()
        .ok_or_else(Error::internal)?
        .extend(changes.as_object().cloned().ok_or_else(Error::internal)?);
    updated["revision"] = (integer(&task["revision"]).unwrap_or(0) + 1).into();
    crate::oauth_state::retire_credit_fields(&mut updated);
    validate_credit(&updated)?;
    store.transaction(|state| {
        let meta = &mut state["scheduler"][id.to_string()];
        if !meta.is_object() {
            *meta = json!({});
        }
        let current = &meta["auto_reset_credit"];
        if !current["episode"].is_null() && current["episode"] != task["episode"]
            || integer(&current["revision"]).unwrap_or(0) != integer(&task["revision"]).unwrap_or(0)
        {
            return Err(Error::new(409, "自动用卡任务已变化"));
        }
        if changes.get("model_test_job_id").is_none() && truthy(&current["model_test_job_id"]) {
            updated["model_test_job_id"] = current["model_test_job_id"].clone();
        }
        meta["auto_reset_credit"] = updated.clone();
        Ok(updated.clone())
    })
}

pub(crate) fn parameter_change(store: &OAuthState, row: &Value) -> Result<Option<Value>> {
    let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
    let state = store.shared_snapshot()?;
    let task = &state["scheduler"][id.to_string()]["auto_reset_credit"];
    if task["owns_pause"] != true {
        return Ok(None);
    }
    if !owned(row, task, &state) {
        return Err(Error::new(409, "用卡暂停归属待核对，未修改参数"));
    }
    Ok(Some(task.clone()))
}

pub(crate) fn finish_parameter_change(store: &OAuthState, row: &Value, task: &Value) -> Result<()> {
    let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
    let state = store.shared_snapshot()?;
    if row["schedulable"] != false
        || task["control_generation"]
            .as_i64()
            .is_some_and(|expected| expected != generation(&state, id))
    {
        return Err(Error::new(409, "改参后的用卡暂停归属已变化，请重新核对"));
    }
    save(store, id, task, json!({"signature":signature(row)}))?;
    Ok(())
}

pub(crate) fn cancel(store: &OAuthState, id: i64, at: DateTime<Utc>) -> Result<()> {
    let state = store.shared_snapshot()?;
    let task = &state["scheduler"][id.to_string()]["auto_reset_credit"];
    if truthy(task) && string(&task["stage"]) != "recovered" {
        save(
            store,
            id,
            task,
            json!({"stage":"manual","owns_pause":false,"next_at":null,"error_code":"","manual_at":datetime(at)}),
        )?;
    }
    Ok(())
}
pub trait Hooks: Send + Sync {
    fn config(&self) -> Result<Value>;
    fn detection_held(&self, id: i64) -> Result<bool>;
    fn detection_version(&self, id: i64) -> Result<Value>;
    fn model_verify(
        &self,
        id: i64,
        episode: String,
        attempt: i64,
        consume: bool,
    ) -> BoxFuture<'_, Result<Value>>;
    fn cancel_verification(&self, job_id: String, episode: String) -> BoxFuture<'_, Result<()>>;
    fn connection_test(
        &self,
        id: i64,
        model: String,
        generation: i64,
        seconds: u64,
        reserved: bool,
    ) -> BoxFuture<'_, Result<Value>>;
    fn recover(&self, id: i64, key: String) -> BoxFuture<'_, Result<Value>>;
}

#[derive(Clone)]
pub struct CreditController {
    pub actions: Actions,
    pub store: OAuthState,
    pub queries: Coordinator,
    pub hooks: Arc<dyn Hooks>,
    pub slots: TestSlots,
    pub budget: Arc<tokio::sync::Semaphore>,
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    pub notice_lock: Arc<tokio::sync::Mutex<()>>,
}

impl CreditController {
    pub fn new(
        actions: Actions,
        queries: Coordinator,
        hooks: Arc<dyn Hooks>,
        slots: TestSlots,
        budget: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            store: actions.queries.oauth.clone(),
            actions,
            queries,
            hooks,
            slots,
            budget,
            clock: Arc::new(Utc::now),
            notice_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }
    pub fn state(&self, id: i64) -> Result<Value> {
        Ok(
            self.store.shared_snapshot()?["scheduler"][id.to_string()]["auto_reset_credit"]
                .as_object()
                .cloned()
                .map(Value::Object)
                .unwrap_or(json!({})),
        )
    }
    pub async fn row(&self, id: i64) -> Result<Value> {
        match account(&self.actions.queries.db, id).await {
            Err(e) if e.status.as_u16() == 404 => Ok(Value::Null),
            other => other,
        }
    }
    pub fn save(&self, id: i64, task: &Value, changes: Value) -> Result<Value> {
        save(&self.store, id, task, changes)
    }
    pub fn audit(&self, id: i64, action: &str, fields: Value) {
        let mut payload = json!({"account_id":id,"action":action});
        if let Some(fields) = fields.as_object() {
            payload.as_object_mut().unwrap().extend(fields.clone());
        }
        self.actions.audit("oauth_auto_reset_credit", payload);
    }
    pub fn saved_quota(&self, row: &Value, at: DateTime<Utc>) -> Result<Value> {
        Ok(quota::latest_result(
            row,
            &self.store.shared_snapshot()?["oauth_results"]
                [integer(&row["id"]).unwrap_or(0).to_string()],
            at,
        ))
    }
    pub fn eligible(&self, row: &Value, at: DateTime<Utc>, held: bool) -> Result<bool> {
        let id = integer(&row["id"]).unwrap_or(0);
        let config = self.hooks.config()?;
        if !row.is_object()
            || recovery_rules::method(&config, id).is_none()
            || config["oauth_recovery_monitor_enabled"] == false
            || self.hooks.detection_held(id)?
            || truthy(&row["parent_account_id"])
            || truthy(&row["extra"]["parent_account_id"])
        {
            return Ok(false);
        }
        let mut candidate = row.clone();
        if held {
            candidate["schedulable"] = true.into();
        }
        Ok(oauth_admission::automatic_eligible(&candidate, at))
    }
    pub fn owned(&self, row: &Value, task: &Value) -> Result<bool> {
        Ok(owned(row, task, self.store.shared_snapshot()?.as_ref()))
    }
    pub fn available(&self, row: &Value, task: &Value, at: DateTime<Utc>) -> Result<bool> {
        Ok(rules::available(row, &self.saved_quota(row, at)?, task, at))
    }
    pub async fn depletion(&self, row: &Value, at: DateTime<Utc>, held: bool) -> Result<Value> {
        if parse_datetime(&row["rate_limit_reset_at"]).is_none_or(|t| t <= at) {
            return Ok(Value::Null);
        }
        if !self.eligible(row, at, held)? {
            return Ok(Value::Null);
        }
        let state = self.store.shared_snapshot()?;
        let meta = &state["scheduler"][integer(&row["id"]).unwrap_or(0).to_string()];
        let query = &meta["quota_query"];
        if query["auth_fingerprint"] == credential_fingerprint(row)
            || !truthy(&query["auth_fingerprint"])
                && AUTH_ERRORS.contains(&string(&meta["last_error_code"]))
        {
            return Ok(Value::Null);
        }
        let saved = self.saved_quota(row, at)?;
        let summary = quota::summary_from_result(row, &saved);
        let windows = quota::windows_by_key(&summary["ui_windows"]);
        let seven = windows.get("codex_7d").unwrap_or(&Value::Null);
        if !truthy(&saved["success"])
            || !quota::percent(&seven["used_percent"]).is_some_and(|p| p >= 100.0)
        {
            return Ok(Value::Null);
        }
        let evidence = rules::upstream_evidence(&self.actions.queries.db, row, at).await?;
        if evidence["source"] != "upstream_error" {
            return Ok(Value::Null);
        }
        Ok(rules::depletion_evidence(row, &saved, &evidence, at))
    }
    pub(crate) fn positive_credit_reason(&self, row: &Value, at: DateTime<Utc>) -> &'static str {
        match available_credits(&row["extra"]["codex_reset_credit_snapshot"], at) {
            None => "credit_unconfirmed",
            Some(0) => "no_credit",
            Some(_) => "",
        }
    }
    pub(crate) fn admit_episode(
        &self,
        row: &Value,
        evidence: &Value,
        at: DateTime<Utc>,
    ) -> Result<Value> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let reason = self.positive_credit_reason(row, at);
        let Some(observed) = parse_datetime(&evidence["evidence_at"]) else {
            return Ok(Value::Null);
        };
        let window_start = parse_datetime(&evidence["reset_at"])
            .zip(quota::number(&evidence["window_minutes"]))
            .map(|(reset, minutes)| reset - Duration::milliseconds((minutes * 60000.0) as i64));
        let mut changed = false;
        let result = self.store.transaction(|state| {
            let meta = &mut state["scheduler"][id.to_string()];
            if !meta.is_object() {
                *meta = json!({});
            }
            let previous = &meta["credit_admission"];
            let recovered = parse_datetime(&previous["recovered_at"]);
            let is_new = !truthy(previous)
                || recovered.is_some_and(|t| observed > t)
                || parse_datetime(&previous["trigger_at"])
                    .zip(window_start)
                    .is_some_and(|(old, start)| old < start);
            let trigger = if is_new {
                Some(observed)
            } else {
                parse_datetime(&previous["trigger_at"])
            };
            let checked = at;
            let why = if trigger.is_none_or(|t| checked >= t + Duration::seconds(300)) {
                "consume_deadline"
            } else if recovered.is_some_and(|t| observed <= t) {
                "block_cleared"
            } else {
                reason
            };
            let trigger = trigger.ok_or_else(|| Error::new(503, "自动用卡触发时间无效"))?;
            let mut value = json!({
                "trigger_at": datetime(trigger),
                "checked_at": datetime(checked),
                "reason": why,
                "evidence_fingerprint": if is_new { &evidence["evidence_fingerprint"] } else { &previous["evidence_fingerprint"] }
            });
            if !is_new {
                if let Some(recovered) = recovered {
                    value["recovered_at"] = datetime(recovered).into();
                }
            }
            let without_clock = |v: &Value| {
                v.as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(key, _)| key.as_str() != "checked_at")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<serde_json::Map<_, _>>()
            };
            changed = without_clock(previous) != without_clock(&value);
            if changed {
                meta["credit_admission"] = value;
            }
            if !why.is_empty() {
                Ok(Value::Null)
            } else {
                let mut evidence = evidence.clone();
                evidence["evidence_at"] = datetime(trigger).into();
                Ok(evidence)
            }
        })?;
        if changed {
            self.audit(
                id,
                if result.is_null() {
                    "credit_skipped"
                } else {
                    "credit_admitted"
                },
                json!({"reason":self.store.shared_snapshot()?["scheduler"][id.to_string()]["credit_admission"]["reason"]}),
            );
        }
        Ok(result)
    }
    pub fn observe_recovery(&self, row: &Value, at: DateTime<Utc>) -> Result<()> {
        if !row.is_object()
            || !recovery_rules::confirmed(row)
            || !self.available(row, &json!({}), at)?
        {
            return Ok(());
        }
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?.to_string();
        let value = self.store.shared_snapshot()?["scheduler"][&id]["credit_admission"].clone();
        if truthy(&value) && !truthy(&value["recovered_at"]) {
            self.store.transaction(|state| {
                let current = &mut state["scheduler"][&id]["credit_admission"];
                if *current == value {
                    current["recovered_at"] = datetime(at).into();
                }
                Ok(())
            })?;
        }
        Ok(())
    }
    pub async fn candidate_ready(&self, row: &Value, at: DateTime<Utc>) -> Result<bool> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let task = self.state(id)?;
        let ended = matches!(string(&task["stage"]), "finished" | "recovered" | "closed");
        if ended {
            if task["stage"] == "finished" {
                if truthy(&task["owns_pause"]) && !truthy(&task["attempt_at"]) {
                    return Ok(true);
                }
                if !truthy(&task["recovered_evidence_at"])
                    && self.eligible(row, at, false)?
                    && recovery_rules::confirmed(row)
                    && self.available(row, &task, at)?
                {
                    return Ok(true);
                }
                if !truthy(&task["recovered_evidence_at"]) {
                    return Ok(false);
                }
            }
        } else if rules::manual(&task) {
            if row.is_object()
                && self.eligible(row, at, false)?
                && recovery_rules::confirmed(row)
                && self.available(row, &task, at)?
            {
                return Ok(true);
            }
            if self.hooks.config()?["oauth_auto_reset_credit_enabled"] != true {
                return Ok(false);
            }
            let control = self.store.shared_snapshot()?["scheduler"][id.to_string()]
                ["manual_control"]
                .clone();
            let Some(changed) =
                parse_datetime(&control["at"]).filter(|_| control["enabled"] == true)
            else {
                return Ok(false);
            };
            if parse_datetime(&row["rate_limit_reset_at"]).is_none_or(|t| t <= at) {
                self.observe_recovery(row, at)?;
                return Ok(false);
            }
            if parse_datetime(&row["rate_limited_at"]).is_some_and(|rl| rl <= changed)
                && parse_datetime(&row["updated_at"]).is_some_and(|u| u <= changed)
                && parse_datetime(&row["rate_limit_reset_at"])
                    .map(datetime)
                    .as_deref()
                    == task["reset_at"].as_str()
            {
                return Ok(false);
            }
            let evidence = self.depletion(row, at, false).await?;
            if !truthy(&evidence) {
                self.observe_recovery(row, at)?;
                return Ok(false);
            }
            if parse_datetime(&evidence["evidence_at"]).is_none_or(|observed| observed <= changed)
                || evidence["evidence_fingerprint"] == task["evidence_fingerprint"]
            {
                return Ok(false);
            }
            return Ok(truthy(&self.admit_episode(row, &evidence, at)?));
        } else if truthy(&task) {
            if parse_datetime(&task["next_at"]).is_some_and(|due| due > at)
                && self.guard_reason(id, row, &task, at)?.is_empty()
                && !rules::expired(&task, at)
                && !truthy(&row["extra"]["auto_reset_credit_enabled"])
            {
                if truthy(&task["attempt_at"]) {
                    return Ok(false);
                }
                if task["stage"] != "pausing" && !truthy(&task["owns_pause"]) {
                    if parse_datetime(&row["rate_limit_reset_at"])
                        .map(datetime)
                        .as_deref()
                        == task["reset_at"].as_str()
                        && task["signature"] == signature(row)
                    {
                        return Ok(false);
                    }
                    let evidence = self.depletion(row, at, false).await?;
                    if truthy(&evidence)
                        && ["reset_at", "evidence_fingerprint", "evidence_source"]
                            .iter()
                            .all(|key| task[*key] == evidence[*key])
                    {
                        return Ok(false);
                    }
                }
            }
            return Ok(true);
        }
        if self.hooks.config()?["oauth_auto_reset_credit_enabled"] != true {
            return Ok(false);
        }
        let evidence = self.depletion(row, at, false).await?;
        if !truthy(&evidence) {
            self.observe_recovery(row, at)?;
            return Ok(false);
        }
        if ended {
            let observed = parse_datetime(&evidence["evidence_at"]);
            let state = self.store.shared_snapshot()?;
            if [
                &task["recovered_evidence_at"],
                &task["recovered_at"],
                &state["scheduler"][id.to_string()]["manual_control"]["at"],
            ]
            .into_iter()
            .filter_map(parse_datetime)
            .any(|boundary| observed.is_none_or(|at| at <= boundary))
            {
                return Ok(false);
            }
        }
        Ok(truthy(&self.admit_episode(row, &evidence, at)?))
    }
    pub fn begin_episode(
        &self,
        row: &Value,
        old: &Value,
        evidence: &Value,
        at: DateTime<Utc>,
    ) -> Result<Value> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let evidence = self.admit_episode(row, evidence, at)?;
        if !truthy(&evidence) {
            return Ok(json!({}));
        }
        let mut task = evidence;
        task.as_object_mut().unwrap().extend(
            json!({
                "episode": uuid::Uuid::new_v4().simple().to_string(),
                "stage": "waiting",
                "account_name": string(&row["name"])
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let policy = self.policy_fields(row, &task)?;
        task.as_object_mut()
            .unwrap()
            .extend(policy.as_object().unwrap().clone());
        self.store.transaction(|state| {
            let meta = &mut state["scheduler"][id.to_string()];
            if !meta.is_object() {
                *meta = json!({});
            }
            let current = &meta["auto_reset_credit"];
            if current["episode"] != old["episode"] || current["revision"] != old["revision"] {
                return Err(Error::new(409, "自动用卡任务已变化"));
            }
            meta["auto_reset_credit"] = task.clone();
            Ok(())
        })?;
        Ok(task)
    }
    fn policy_fields(&self, row: &Value, task: &Value) -> Result<Value> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let trigger = parse_datetime(&task["evidence_at"]);
        let state = self.store.shared_snapshot()?;
        let meta = &state["scheduler"][id.to_string()];
        Ok(json!({
            "policy_version": 1,
            "trigger_at": trigger.map(datetime),
            "consume_deadline_at": trigger.map(|t| datetime(t + Duration::seconds(300))),
            "verification_deadline_at": null,
            "policy_control_generation": generation(&state, id),
            "policy_detection_version": self.hooks.detection_version(id)?,
            "policy_method": recovery_rules::method(&self.hooks.config()?, id),
            "policy_method_generation": integer(&meta["recovery_method_generation"]).unwrap_or(0),
            "verification_dispatched": false
        }))
    }
    pub fn guard_reason(
        &self,
        id: i64,
        row: &Value,
        task: &Value,
        at: DateTime<Utc>,
    ) -> Result<&'static str> {
        let state = self.store.shared_snapshot()?;
        let meta = &state["scheduler"][id.to_string()];
        if !row.is_object() || truthy(&row["deleted_at"]) {
            return Ok("account_deleted");
        }
        if row["status"] == "disabled" {
            return Ok("account_disabled");
        }
        if row["status"] == "error" {
            return Ok("account_error");
        }
        let query = &meta["quota_query"];
        if query["auth_fingerprint"] == credential_fingerprint(row)
            || !truthy(&query["auth_fingerprint"])
                && AUTH_ERRORS.contains(&string(&meta["last_error_code"]))
        {
            return Ok("auth_paused");
        }
        let config = self.hooks.config()?;
        if json!(generation(&state, id)) != task["policy_control_generation"]
            || json!(integer(&meta["recovery_method_generation"]).unwrap_or(0))
                != task["policy_method_generation"]
            || json!(recovery_rules::method(&config, id)) != task["policy_method"]
        {
            return Ok("manual_intervention");
        }
        if task["policy_detection_version"] != self.hooks.detection_version(id)? {
            return Ok("manual_intervention");
        }
        if self.hooks.detection_held(id)? {
            return Ok("detection_held");
        }
        if config["oauth_auto_reset_credit_enabled"] != true
            || !self.eligible(
                row,
                at,
                truthy(&task["owns_pause"]) || task["stage"] == "pausing",
            )?
        {
            return Ok("account_ineligible");
        }
        if truthy(&task["owns_pause"])
            && !self.owned(row, task)?
            && !(task["stage"] == "confirming"
                && task["test_success"] == true
                && truthy(&task["clear_requested_at"])
                && row["schedulable"] == true
                && task["signature"] == signature(row))
        {
            return Ok("ownership_changed");
        }
        Ok("")
    }
    pub fn notice(&self, id: i64, task: &Value, at: DateTime<Utc>, row: &Value) -> Value {
        let code = if truthy(&task["result_reason"]) {
            string(&task["result_reason"])
        } else {
            string(&task["error_code"])
        };
        json!({"job_id":task["model_test_job_id"],"episode":task["episode"],"account_id":id,"account_name":if truthy(&row["name"]) {string(&row["name"])} else {string(&task["account_name"])},"outcome":task["outcome"],"reason":rules::reason(code),"reason_code":code,"trigger_at":task["trigger_at"],"attempt_at":task["attempt_at"],"reset_completed_at":task["reset_completed_at"],"completed_at":datetime(at),"verification_method":if truthy(&task["verification_method"]) {&task["verification_method"]} else {&task["policy_method"]},"model_id":task["model_id"],"status":"pending","attempts":0,"next_at":datetime(at)})
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "Keep the persisted task, live row, completion metadata and notification cancellation explicit"
    )]
    pub async fn finish(
        &self,
        id: i64,
        task: &Value,
        reason: &str,
        at: DateTime<Utc>,
        row: &Value,
        cancel: bool,
        fields: &Value,
    ) -> Result<Value> {
        if task["stage"] == "finished" {
            return Ok(task.clone());
        }
        let mut finished = task.clone();
        finished.as_object_mut().ok_or_else(Error::internal)?.extend(json!({"revision":integer(&task["revision"]).unwrap_or(0)+1,"stage":"finished","outcome":rules::outcome(task),"result_reason":reason,"error_code":reason,"completed_at":datetime(at),"next_at":null}).as_object().unwrap().clone());
        if matches!(reason, "ownership_changed" | "account_deleted") {
            finished["owns_pause"] = false.into();
        }
        let mut notice = self.notice(id, &finished, at, row);
        if reason == "account_deleted" {
            notice["status"] = "suppressed".into();
            notice["next_at"] = Value::Null;
        }
        if let Some(fields) = fields.as_object() {
            notice.as_object_mut().unwrap().extend(fields.clone());
        }
        self.store.transaction(|state| {
            let meta = &mut state["scheduler"][id.to_string()];
            let current = &meta["auto_reset_credit"];
            if current["episode"] != task["episode"] || current["revision"] != task["revision"] {
                return Err(Error::new(409, "用卡任务已变化"));
            }
            meta["auto_reset_credit"] = finished.clone();
            if !meta["credit_result_notifications"].is_object() {
                meta["credit_result_notifications"] = json!({});
            }
            meta["credit_result_notifications"]
                .as_object_mut()
                .unwrap()
                .entry(string(&task["episode"]).to_owned())
                .or_insert(notice);
            if reason == "account_deleted" {
                suppress_pending_notices(meta);
            }
            Ok(())
        })?;
        self.audit(
            id,
            "finished",
            json!({"outcome":finished["outcome"],"reason":reason}),
        );
        if cancel && truthy(&task["model_test_job_id"]) {
            self.hooks
                .cancel_verification(
                    string(&task["model_test_job_id"]).into(),
                    string(&task["episode"]).into(),
                )
                .await?;
        }
        Ok(finished)
    }
    pub async fn bounded_wait(
        &self,
        id: i64,
        task: &Value,
        result: &Value,
        at: DateTime<Utc>,
    ) -> Result<()> {
        if task["stage"] == "finished" {
            return Ok(());
        }
        let code = if truthy(&result["waiting_reason"]) {
            string(&result["waiting_reason"])
        } else if truthy(&result["error_code"]) {
            string(&result["error_code"])
        } else {
            "incomplete_quota"
        };
        let mut until = parse_datetime(if truthy(&result["next_at"]) {
            &result["next_at"]
        } else {
            &result["next_query_at"]
        });
        if until.is_none() && code == "query_busy" {
            until = Some(at + Duration::seconds(2));
        }
        let end = parse_datetime(
            &task[if truthy(&task["consumed"]) {
                "verification_deadline_at"
            } else {
                "consume_deadline_at"
            }],
        );
        if AUTH_ERRORS.contains(&code)
            || matches!(
                code,
                "no_credit" | "query_budget" | "account_ineligible" | "reset_state_changed"
            )
            || until.zip(end).is_none_or(|(until, end)| until >= end)
        {
            self.finish(id, task, code, at, &Value::Null, true, &Value::Null)
                .await?;
        } else {
            self.save(
                id,
                task,
                json!({"next_at":until.map(datetime),"error_code":code}),
            )?;
        }
        Ok(())
    }
    pub fn account_deleted(&self, id: i64, at: DateTime<Utc>) -> Result<()> {
        self.store.transaction(|state| {
            let Some(meta) = state["scheduler"].get_mut(id.to_string()) else {
                return Ok(());
            };
            if let Some(task) = meta.get_mut("auto_reset_credit").filter(|task| {
                task.is_object()
                    && !rules::manual(task)
                    && !matches!(string(&task["stage"]), "finished" | "recovered" | "closed")
            }) {
                let outcome = rules::outcome(task);
                let revision = integer(&task["revision"]).unwrap_or(0) + 1;
                task.as_object_mut().unwrap().extend(
                    json!({
                        "stage": "finished",
                        "outcome": outcome,
                        "result_reason": "account_deleted",
                        "error_code": "account_deleted",
                        "completed_at": datetime(at),
                        "owns_pause": false,
                        "next_at": null,
                        "revision": revision
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                );
            }
            suppress_pending_notices(meta);
            Ok(())
        })
    }
    pub async fn deliver_results(&self, notifier: &Bark) -> Result<()> {
        let Ok(_guard) = self.notice_lock.try_lock() else {
            return Ok(());
        };
        let config = notifier.runtime();
        if !config.valid {
            return Ok(());
        }
        let state = self.store.shared_snapshot()?;
        let due_at = self.now();
        let ready = |notice: &Value| {
            notice["status"] == "pending"
                && (!config.enabled
                    || parse_datetime(&notice["next_at"]).is_none_or(|time| time <= due_at))
        };
        let pending: Vec<i64> = state["scheduler"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, meta)| {
                meta["credit_result_notifications"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .any(|(_, notice)| ready(notice))
            })
            .filter_map(|(id, _)| id.parse().ok())
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        let live: std::collections::BTreeSet<i64> = self
            .actions
            .queries
            .db
            .all(
                "SELECT id FROM accounts WHERE deleted_at IS NULL AND id=ANY($1)",
                &[crate::db::Param::Ints(pending)],
            )
            .await?
            .iter()
            .filter_map(|row| integer(&row["id"]))
            .collect();
        for (id, meta) in state["scheduler"].as_object().into_iter().flatten() {
            for (episode, notice) in meta["credit_result_notifications"]
                .as_object()
                .into_iter()
                .flatten()
            {
                let at = self.now();
                if !ready(notice) {
                    continue;
                }
                let aid = id.parse::<i64>().map_err(|_| Error::internal())?;
                if !live.contains(&aid) {
                    self.account_deleted(aid, at)?;
                    break;
                }
                let reserved = self.store.transaction(|state| {
                    let item = &mut state["scheduler"][id]["credit_result_notifications"][episode];
                    if item != notice {
                        return Ok(Value::Null);
                    }
                    if !config.enabled {
                        item["status"] = "suppressed".into();
                        item["next_at"] = Value::Null;
                        return Ok(Value::Null);
                    }
                    let count = integer(&item["attempts"]).unwrap_or(0);
                    if count >= 5 {
                        item["status"] = "failed".into();
                        item["next_at"] = Value::Null;
                        return Ok(Value::Null);
                    }
                    item["attempts"] = json!(count + 1);
                    item["next_at"] = datetime(
                        at + Duration::seconds(crate::operations::retry_delay(count.max(0) as u64)),
                    )
                    .into();
                    Ok(item.clone())
                })?;
                if reserved.is_null() {
                    continue;
                }
                let delivered = notifier.notify_credit(&reserved, Some(&config)).await;
                self.store.transaction(|state| {
                    let item = &mut state["scheduler"][id]["credit_result_notifications"][episode];
                    if item["attempts"] != reserved["attempts"] {
                        return Ok(());
                    }
                    if delivered {
                        item["status"] = "delivered".into();
                        item["next_at"] = Value::Null;
                    } else if integer(&reserved["attempts"]).unwrap_or(0) >= 5 {
                        item["status"] = "failed".into();
                        item["next_at"] = Value::Null;
                    }
                    Ok(())
                })?;
                self.audit(
                    aid,
                    if delivered {
                        "result_delivered"
                    } else {
                        "result_retry"
                    },
                    json!({"attempt":reserved["attempts"]}),
                );
            }
        }
        Ok(())
    }
}

fn suppress_pending_notices(meta: &mut Value) {
    for notice in meta
        .get_mut("credit_result_notifications")
        .and_then(Value::as_object_mut)
        .into_iter()
        .flat_map(|notices| notices.values_mut())
    {
        if notice["status"] == "pending" {
            notice["status"] = "suppressed".into();
            notice["next_at"] = Value::Null;
        }
    }
}
