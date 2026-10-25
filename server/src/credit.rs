use crate::{
    actions::Actions,
    audit,
    bark::Bark,
    compat::{datetime, integer, parse_datetime, string, truthy},
    credit_ownership, credit_rules,
    error::{Error, Result},
    execution::{DiskLease, TestSlots},
    oauth_admission::{self, credential_fingerprint, AUTH_ERRORS},
    oauth_queries::Coordinator,
    oauth_state::{available_credits, OAuthState},
    quota, recovery_policy, recovery_rules,
};
use chrono::{DateTime, Duration, Utc};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::Arc;

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
        match credit_ownership::account(&self.actions.queries.db, id).await {
            Err(e) if e.status.as_u16() == 404 => Ok(Value::Null),
            other => other,
        }
    }
    pub fn save(&self, id: i64, task: &Value, changes: Value) -> Result<Value> {
        credit_ownership::save(&self.store, id, task, changes)
    }
    pub fn audit(&self, id: i64, action: &str, fields: Value) {
        let mut payload = json!({"account_id":id,"action":action});
        if let Some(fields) = fields.as_object() {
            payload.as_object_mut().unwrap().extend(fields.clone());
        }
        audit::write(
            &self.actions.settings.audit_path,
            "oauth_auto_reset_credit",
            payload,
            self.actions.settings.read_only,
        );
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
            || recovery_policy::method(&config, id).is_none()
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
    pub fn processing_eligible(
        &self,
        row: &Value,
        task: &Value,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let mut candidate = row.clone();
        if row["status"] == "error"
            && truthy(&task["auth_recheck"])
            && task["auth_error"] == crate::compat::sha256(string(&row["error_message"]))
        {
            candidate["status"] = "active".into();
        }
        self.eligible(&candidate, at, true)
    }
    pub fn owned(&self, row: &Value, task: &Value, same_version: bool) -> Result<bool> {
        Ok(credit_ownership::owned(
            row,
            task,
            self.store.shared_snapshot()?.as_ref(),
            same_version,
        ))
    }
    pub fn available(&self, row: &Value, task: &Value, at: DateTime<Utc>) -> Result<bool> {
        Ok(credit_rules::available(
            row,
            &self.saved_quota(row, at)?,
            task,
            at,
        ))
    }
    pub async fn depletion(&self, row: &Value, at: DateTime<Utc>, held: bool) -> Result<Value> {
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
        if parse_datetime(&row["rate_limit_reset_at"]).is_none_or(|t| t <= at)
            || !truthy(&saved["success"])
            || !quota::percent(&seven["used_percent"]).is_some_and(|p| p >= 100.0)
        {
            return Ok(Value::Null);
        }
        let evidence = credit_rules::upstream_evidence(&self.actions.queries.db, row, at).await?;
        Ok(credit_rules::depletion_evidence(row, &saved, &evidence, at))
    }
    pub fn positive_credit_reason(
        &self,
        row: &Value,
        evidence: &Value,
        at: DateTime<Utc>,
    ) -> Result<&'static str> {
        let Some(count) = available_credits(&row["extra"]["codex_reset_credit_snapshot"], at)
        else {
            return Ok("credit_unconfirmed");
        };
        if count <= 0 {
            return Ok("no_credit");
        }
        let state = self.store.shared_snapshot()?;
        let meta = &state["scheduler"][integer(&row["id"]).unwrap_or(0).to_string()];
        Ok(
            if credit_rules::credit_reason(row, evidence, meta, &self.saved_quota(row, at)?, at)?
                .is_empty()
            {
                ""
            } else {
                "credit_unconfirmed"
            },
        )
    }
    pub fn admit_episode(&self, row: &Value, evidence: &Value, at: DateTime<Utc>) -> Result<Value> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let reason = self.positive_credit_reason(row, evidence, at)?;
        let Some(observed) = parse_datetime(&evidence["evidence_at"]) else {
            return Ok(Value::Null);
        };
        let mut changed = false;
        let result=self.store.transaction(|state| {let meta=&mut state["scheduler"][id.to_string()];if !meta.is_object() {*meta=json!({});}let previous=&meta["credit_admission"];let recovered=parse_datetime(&previous["recovered_at"]);let is_new=!truthy(previous) || recovered.is_some_and(|t|observed>t);
            let trigger=if is_new {Some(observed)} else {parse_datetime(&previous["trigger_at"])};let checked=parse_datetime(&previous["checked_at"]).unwrap_or(at).max(at);
            let why=if parse_datetime(&previous["checked_at"]).is_some_and(|t|at<t-Duration::seconds(1)) {"clock_untrusted"} else if trigger.is_none_or(|t|checked>=t+Duration::seconds(300)) {"consume_deadline"} else if recovered.is_some_and(|t|observed<=t) {"block_cleared"} else {reason};
            let trigger=trigger.ok_or_else(||Error::new(503,"自动用卡触发时间无效"))?;let mut value=json!({"trigger_at":datetime(trigger),"checked_at":datetime(checked),"reason":why,"evidence_fingerprint":if is_new {&evidence["evidence_fingerprint"]} else {&previous["evidence_fingerprint"]}});if !is_new {if let Some(recovered)=recovered {value["recovered_at"]=datetime(recovered).into();}}
            let without_clock=|v:&Value|v.as_object().into_iter().flatten().filter(|(key,_)|key.as_str()!="checked_at").map(|(k,v)|(k.clone(),v.clone())).collect::<serde_json::Map<_,_>>();changed=without_clock(previous)!=without_clock(&value);meta["credit_admission"]=value;
            if !why.is_empty() {Ok(Value::Null)} else {let mut evidence=evidence.clone();evidence["evidence_at"]=datetime(trigger).into();Ok(evidence)}
        })?;
        if changed {
            self.audit(id,if result.is_null() {"credit_skipped"} else {"credit_admitted"},json!({"reason":self.store.shared_snapshot()?["scheduler"][id.to_string()]["credit_admission"]["reason"]}));
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
        let task = self.state(integer(&row["id"]).ok_or_else(Error::invalid)?)?;
        if truthy(&task) && (!truthy(&task["credit_gate_skipped"]) || truthy(&task["owns_pause"])) {
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
        Ok(truthy(&self.admit_episode(row, &evidence, at)?))
    }
    pub async fn begin_episode(&self, id: i64, old: &Value, evidence: &Value) -> Result<Value> {
        let row = self.row(id).await?;
        let at = self.now();
        let evidence = self.admit_episode(&row, evidence, at)?;
        if !truthy(&evidence) {
            return Ok(json!({}));
        }
        let mut task = evidence;
        task.as_object_mut().unwrap().extend(json!({"episode":uuid::Uuid::new_v4().simple().to_string(),"stage":"waiting","account_name":string(&row["name"]),"credit_admitted_at":datetime(at),"credit_admitted_count":available_credits(&row["extra"]["codex_reset_credit_snapshot"],at)}).as_object().unwrap().clone());
        self.store.transaction(|state| {
            let meta = &mut state["scheduler"][id.to_string()];
            if !meta.is_object() {
                *meta = json!({});
            }
            let current = &meta["auto_reset_credit"];
            if current["episode"] != old["episode"] || current["revision"] != old["revision"] {
                return Err(Error::new(409, "自动用卡任务已变化"));
            }
            if truthy(&old["attempt_at"]) {
                if !meta["reset_credit_episodes"].is_object() {
                    meta["reset_credit_episodes"] = json!({});
                }
                meta["reset_credit_episodes"][string(&old["episode"])] = old.clone();
            }
            meta["auto_reset_credit"] = task.clone();
            Ok(())
        })?;
        Ok(task)
    }
    pub fn migrate_admission(
        &self,
        id: i64,
        row: &Value,
        task: &Value,
        at: DateTime<Utc>,
    ) -> Result<Value> {
        if !row.is_object()
            || !truthy(task)
            || truthy(&task["attempt_at"])
            || truthy(&task["consumed"])
            || truthy(&task["credit_admitted_at"])
            || credit_rules::manual(task)
            || matches!(string(&task["stage"]), "recovered" | "closed")
            || truthy(&task["credit_gate_skipped"])
        {
            return Ok(task.clone());
        }
        let reason = self.positive_credit_reason(row, task, at)?;
        if reason.is_empty() {
            return Ok(task.clone());
        }
        let mut updated = task.clone();
        updated["revision"] = json!(integer(&task["revision"]).unwrap_or(0) + 1);
        updated["credit_gate_skipped"] = true.into();
        if !credit_rules::LEGACY_ENDED.contains(&string(&task["stage"])) {
            updated.as_object_mut().unwrap().extend(json!({"stage":"finished","outcome":"not_executed","result_reason":reason,"error_code":reason,"completed_at":datetime(at),"next_at":null}).as_object().unwrap().clone());
        }
        self.store.transaction(|state| {let meta=&mut state["scheduler"][id.to_string()];let current=&meta["auto_reset_credit"];if current["episode"]!=task["episode"] || current["revision"]!=task["revision"] {return Err(Error::new(409,"用卡任务已变化"));}meta["auto_reset_credit"]=updated.clone();let trigger=if truthy(&task["trigger_at"]) {&task["trigger_at"]} else {&task["evidence_at"]};if parse_datetime(trigger).is_some() && meta.get("credit_admission").is_none() {meta["credit_admission"]=json!({"trigger_at":trigger,"checked_at":datetime(at),"reason":reason,"evidence_fingerprint":string(&task["evidence_fingerprint"])});}
            if let Some(notice)=meta.get_mut("credit_result_notifications").and_then(|v|v.get_mut(string(&task["episode"]))) {if notice["status"]=="pending" && notice["outcome"]=="not_executed" {notice["status"]="suppressed".into();notice["next_at"]=Value::Null;}}Ok(())})?;
        self.audit(id, "credit_admission_migrated", json!({"reason":reason}));
        Ok(updated)
    }
    pub async fn policy(&self, id: i64, task: &Value, at: DateTime<Utc>) -> Result<Value> {
        if !truthy(task)
            || credit_rules::manual(task)
            || truthy(&task["policy_version"])
            || credit_rules::LEGACY_ENDED.contains(&string(&task["stage"]))
        {
            return Ok(task.clone());
        }
        let trigger = parse_datetime(&task["evidence_at"]);
        let completed = parse_datetime(&task["reset_completed_at"]);
        let state = self.store.shared_snapshot()?;
        let meta = &state["scheduler"][id.to_string()];
        self.save(id,task,json!({"policy_version":1,"trigger_at":trigger.map(datetime),"consume_deadline_at":trigger.map(|t|datetime(t+Duration::seconds(300))),"verification_deadline_at":completed.map(|t|datetime(t+Duration::seconds(300))),"policy_credential_fingerprint":credential_fingerprint(&self.row(id).await?),"policy_control_generation":task.get("control_generation").filter(|v|!v.is_null()).cloned().unwrap_or(json!(credit_ownership::generation(&state,id))),"policy_detection_version":self.hooks.detection_version(id)?,"policy_method":recovery_policy::method(&self.hooks.config()?,id),"policy_method_generation":integer(&meta["recovery_method_generation"]).unwrap_or(0),"verification_dispatched":truthy(&task["verification_dispatched"]) || task["stage"]=="testing" && integer(&task["test_attempts"]).unwrap_or(0)>0 && !truthy(&task["test_completed_at"]),"policy_checked_at":datetime(at)}))
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
        if parse_datetime(&task["policy_checked_at"])
            .is_some_and(|checked| self.now() < checked - Duration::seconds(1))
        {
            return Ok("clock_untrusted");
        }
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
        if json!(credit_ownership::generation(&state, id)) != task["policy_control_generation"]
            || json!(integer(&meta["recovery_method_generation"]).unwrap_or(0))
                != task["policy_method_generation"]
            || json!(recovery_policy::method(&config, id)) != task["policy_method"]
        {
            return Ok("manual_intervention");
        }
        if task["policy_credential_fingerprint"] != credential_fingerprint(row) {
            return Ok("ownership_changed");
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
            && task["stage"] != "releasing"
            && !self.owned(row, task, false)?
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
        json!({"job_id":task["model_test_job_id"],"episode":task["episode"],"account_id":id,"account_name":if truthy(&row["name"]) {string(&row["name"])} else {string(&task["account_name"])},"outcome":task["outcome"],"reason":credit_rules::reason(code),"reason_code":code,"trigger_at":task["trigger_at"],"attempt_at":task["attempt_at"],"reset_completed_at":task["reset_completed_at"],"completed_at":datetime(at),"verification_method":if truthy(&task["verification_method"]) {&task["verification_method"]} else {&task["policy_method"]},"model_id":task["model_id"],"status":"pending","attempts":0,"next_at":datetime(at)})
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
        if task["stage"] == "finished"
            || credit_rules::LEGACY_ENDED.contains(&string(&task["stage"]))
                && !truthy(&task["policy_version"])
        {
            return Ok(task.clone());
        }
        let mut finished = task.clone();
        finished.as_object_mut().ok_or_else(Error::internal)?.extend(json!({"revision":integer(&task["revision"]).unwrap_or(0)+1,"stage":"finished","outcome":credit_rules::outcome(task),"result_reason":reason,"error_code":reason,"completed_at":datetime(at),"next_at":null}).as_object().unwrap().clone());
        if reason == "ownership_changed" {
            finished["owns_pause"] = false.into();
        }
        let mut notice = self.notice(id, &finished, at, row);
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
    pub async fn deliver_results(&self, notifier: &Bark) -> Result<()> {
        if self.actions.settings.read_only {
            return Ok(());
        }
        let Ok(_guard) = self.notice_lock.try_lock() else {
            return Ok(());
        };
        let config = notifier.runtime();
        if !config.valid {
            return Ok(());
        }
        let state = self.store.shared_snapshot()?;
        for (id, meta) in state["scheduler"].as_object().into_iter().flatten() {
            for (episode, notice) in meta["credit_result_notifications"]
                .as_object()
                .into_iter()
                .flatten()
            {
                let at = self.now();
                if notice["status"] != "pending"
                    || parse_datetime(&notice["next_at"]).is_some_and(|t| t > at)
                {
                    continue;
                }
                let aid = id.parse::<i64>().map_err(|_| Error::internal())?;
                let task = &meta["auto_reset_credit"];
                if task["episode"] == episode.as_str()
                    && !truthy(&task["attempt_at"])
                    && !truthy(&task["credit_admitted_at"])
                {
                    self.migrate_admission(aid, &self.row(aid).await?, task, at)?;
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
                        at + Duration::seconds([5, 30, 120, 600, 600][count.clamp(0, 4) as usize]),
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
    pub fn disk_lease(&self, id: i64) -> Result<DiskLease> {
        DiskLease::try_acquire(
            self.store.0.path(),
            &format!("credit-operation-{id}"),
            self.actions.settings.read_only,
        )
    }
}
