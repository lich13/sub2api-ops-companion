use crate::{
    actions,
    compat::{datetime, integer, parse_datetime, sha256, string, truthy},
    credit::CreditController,
    credit_ownership, credit_rules,
    error::{Error, Result},
    oauth_admission::{credential_fingerprint, AUTH_ERRORS},
    quota, recovery_policy, recovery_rules,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

impl CreditController {
    pub async fn test(
        &self,
        row: &Value,
        task: &Value,
        at: DateTime<Utc>,
        reserved: bool,
    ) -> Result<()> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        if truthy(&task["policy_version"]) {
            let reason = self.guard_reason(id, row, task, at)?;
            if !reason.is_empty() {
                self.finish(id, task, reason, at, row, true, &Value::Null)
                    .await?;
                return Ok(());
            }
            if task["stage"] == "finished" || credit_rules::expired(task, at) {
                self.finish(
                    id,
                    task,
                    "verification_deadline",
                    at,
                    row,
                    true,
                    &Value::Null,
                )
                .await?;
                return Ok(());
            }
        }
        if !self.owned(row, task, true)? || !self.processing_eligible(row, task, at)? {
            self.save(
                id,
                task,
                json!({"stage":"blocked","error_code":"ownership_changed","owns_pause":false}),
            )?;
            return Ok(());
        }
        let config = self.hooks.config()?;
        let mut model = string(&config["oauth_recovery_test_model_id"]).to_owned();
        let attempts = integer(&task["test_attempts"]).unwrap_or(0) + 1;
        let mut task=self.save(id,task,json!({"stage":"testing","verification_dispatched":true,"test_attempts":attempts,"next_at":recovery_rules::retry_at(at,attempts)}))?;
        let state = self.store.shared_snapshot()?;
        let generation = credit_ownership::generation(&state, id);
        let method = recovery_policy::method(&config, id);
        let selection =
            integer(&state["scheduler"][id.to_string()]["recovery_method_generation"]).unwrap_or(0);
        let test = match method {
            Some("model") => {
                self.hooks
                    .model_verify(id, string(&task["episode"]).into(), attempts, false)
                    .await?
            }
            Some("connection") => {
                let remaining = if truthy(&task["policy_version"]) {
                    parse_datetime(&task["verification_deadline_at"])
                        .map(|t| (t - at).num_seconds().max(1) as u64)
                        .unwrap_or(1)
                } else {
                    30
                };
                self.hooks
                    .connection_test(id, model.clone(), generation, remaining.min(30), reserved)
                    .await?
            }
            _ => return Ok(()),
        };
        let state = self.store.shared_snapshot()?;
        if credit_ownership::generation(&state, id) != generation
            || method != recovery_policy::method(&self.hooks.config()?, id)
            || selection
                != integer(&state["scheduler"][id.to_string()]["recovery_method_generation"])
                    .unwrap_or(0)
        {
            return Ok(());
        }
        let current = self.state(id)?;
        if current["stage"] == "finished" || current["revision"] != task["revision"] {
            return Ok(());
        }
        if truthy(&test["deferred"]) {
            let next = if truthy(&test["next_at"]) {
                test["next_at"].clone()
            } else {
                json!(datetime(at + Duration::seconds(5)))
            };
            let code = if truthy(&test["waiting_reason"]) {
                string(&test["waiting_reason"])
            } else if truthy(&test["error_code"]) {
                string(&test["error_code"])
            } else {
                "model_verification_pending"
            };
            task=self.save(id,&task,json!({"stage":"retry","verification_dispatched":false,"test_attempts":attempts-1,"next_at":next,"model_test_job_id":test["job_id"],"error_code":code}))?;
            self.bounded_wait(id, &task, &test, self.now()).await?;
            return Ok(());
        }
        if truthy(&test["model_id"]) {
            model = string(&test["model_id"]).into();
        }
        let completed = self.now();
        self.audit(
            id,
            "test",
            json!({"success":truthy(&test["success"]),"error_code":string(&test["error_code"])}),
        );
        if credit_ownership::generation(self.store.shared_snapshot()?.as_ref(), id) != generation {
            return Ok(());
        }
        let live = self.row(id).await?;
        let code = if truthy(&test["error_code"]) {
            string(&test["error_code"])
        } else {
            "test_failed"
        };
        let auth_error =
            (AUTH_ERRORS.contains(&code) || code == "http_403") && !truthy(&test["success"]);
        let mut candidate = live.clone();
        if auth_error && candidate["status"] == "error" {
            candidate["status"] = "active".into();
        }
        if !self.owned(&live, &task, false)?
            || !self.processing_eligible(&candidate, &task, completed)?
        {
            self.save(
                id,
                &task,
                json!({"stage":"blocked","owns_pause":false,"error_code":"ownership_changed"}),
            )?;
            return Ok(());
        }
        // A lost receipt authorizes read-only reconciliation, never another test.
        if truthy(&test["uncertain"]) {
            self.finish(
                id,
                &task,
                "verification_interrupted",
                completed,
                &live,
                true,
                &Value::Null,
            )
            .await?;
            return Ok(());
        }
        task=self.save(id,&task,json!({"stage":if truthy(&test["success"]) {json!("confirming")} else {task["stage"].clone()},"owned_version":live["updated_at"],"test_completed_at":datetime(completed),"test_success":truthy(&test["success"]),"verification_dispatched":false,"model_id":model,"verification_method":method,"verification_generation":selection}))?;
        if truthy(&task["policy_version"]) && credit_rules::expired(&task, completed) {
            self.finish(
                id,
                &task,
                "verification_deadline",
                completed,
                &live,
                true,
                &Value::Null,
            )
            .await?;
            return Ok(());
        }
        if !truthy(&test["success"]) {
            if truthy(&task["policy_version"])
                && (attempts >= 2
                    || !credit_rules::verification_retryable(&test, method.unwrap_or("")))
            {
                self.finish(
                    id,
                    &task,
                    if attempts >= 2 {
                        "verification_limit"
                    } else {
                        code
                    },
                    completed,
                    &live,
                    true,
                    &Value::Null,
                )
                .await?;
                return Ok(());
            }
            let retry = completed + Duration::seconds(60);
            if truthy(&task["policy_version"])
                && parse_datetime(&task["verification_deadline_at"]).is_none_or(|t| retry >= t)
            {
                self.finish(
                    id,
                    &task,
                    "verification_deadline",
                    completed,
                    &live,
                    true,
                    &Value::Null,
                )
                .await?;
                return Ok(());
            }
            self.save(id,&task,json!({"stage":"retry","error_code":if AUTH_ERRORS.contains(&code) {"auth_paused"} else {"test_failed"},"auth_fingerprint":if AUTH_ERRORS.contains(&code) {json!(credential_fingerprint(&live))} else {Value::Null},"auth_recheck":false,"auth_error":if AUTH_ERRORS.contains(&code) {json!(sha256(string(&live["error_message"])))} else {Value::Null},"next_at":if AUTH_ERRORS.contains(&code) {Value::Null} else {json!(datetime(retry))}}))?;
            return Ok(());
        }
        if !self.available(&live, &task, completed)? {
            self.finish(
                id,
                &task,
                "incomplete_quota",
                completed,
                &live,
                true,
                &Value::Null,
            )
            .await?;
            return Ok(());
        }
        self.release(&live, &task, completed).await
    }
    pub async fn release(&self, row: &Value, task: &Value, at: DateTime<Utc>) -> Result<()> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let _control = self.actions.locks.control(id).await;
        let current = self.state(id)?;
        if current["episode"] != task["episode"] || current["revision"] != task["revision"] {
            return Ok(());
        }
        self.release_locked(row, task, at).await
    }
    async fn release_locked(&self, row: &Value, task: &Value, at: DateTime<Utc>) -> Result<()> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let mut task = task.clone();
        let fresh = self.row(id).await?;
        let row = &fresh;
        if truthy(&task["policy_version"]) {
            let reason = self.guard_reason(id, row, &task, at)?;
            if !reason.is_empty() {
                self.finish(id, &task, reason, at, row, true, &Value::Null)
                    .await?;
                return Ok(());
            }
        }
        let config = self.hooks.config()?;
        let state = self.store.shared_snapshot()?;
        if truthy(&task["verification_method"])
            && task["verification_method"] != json!(recovery_policy::method(&config, id))
            || integer(&task["verification_generation"]).unwrap_or(0)
                != integer(&state["scheduler"][id.to_string()]["recovery_method_generation"])
                    .unwrap_or(0)
        {
            return Ok(());
        }
        if truthy(&task["policy_version"]) && credit_rules::expired(&task, at) {
            self.finish(
                id,
                &task,
                "verification_deadline",
                at,
                row,
                true,
                &Value::Null,
            )
            .await?;
            return Ok(());
        }
        if self.release_confirmed(&fresh, &task, at)? {
            return self.finish_release(&fresh, &task);
        }
        if !self.owned(&fresh, &task, !task["owned_version"].is_null())?
            || !self.processing_eligible(&fresh, &task, at)?
            || !self.available(&fresh, &task, at)?
        {
            self.save(
                id,
                &task,
                json!({"stage":"blocked","error_code":"ownership_changed","owns_pause":false}),
            )?;
            return Ok(());
        }
        let attempt = parse_datetime(&task["attempt_at"]).unwrap_or(at);
        if [
            "rate_limited_at",
            "temp_unschedulable_until",
            "overload_until",
        ]
        .iter()
        .any(|key| parse_datetime(&fresh[key]).is_some_and(|t| t > attempt))
        {
            self.finish(
                id,
                &task,
                "quota_unavailable",
                at,
                &fresh,
                true,
                &Value::Null,
            )
            .await?;
            return Ok(());
        }
        let cleared = if !truthy(&task["clear_requested_at"]) {
            task["clear_requested_at"] = datetime(self.now()).into();
            let _ = self.hooks.recover(id, self.store.admin_token()?).await;
            match self.row(id).await {
                Ok(row) => row,
                Err(error) => {
                    self.save(id, &task, json!({"stage":"confirming","clear_requested_at":task["clear_requested_at"],"owned_version":null,"error_code":"recovery_failed","next_at":datetime(self.now()+Duration::seconds(2))}))?;
                    return Err(error);
                }
            }
        } else {
            fresh
        };
        let mut confirmed = cleared.clone();
        if confirmed.is_object() {
            confirmed["schedulable"] = true.into();
        }
        if !self.owned(&cleared, &task, false)? || !recovery_rules::confirmed(&confirmed) {
            self.save(id,&task,json!({"stage":"confirming","clear_requested_at":task["clear_requested_at"],"error_code":"recovery_failed","next_at":datetime(at+Duration::seconds(2))}))?;
            return Ok(());
        }
        if !self.processing_eligible(&cleared, &task, self.now())? {
            self.save(
                id,
                &task,
                json!({"stage":"blocked","error_code":"ownership_changed","owns_pause":false}),
            )?;
            return Ok(());
        }
        if !self.available(&cleared, &task, self.now())? {
            self.finish(
                id,
                &task,
                "incomplete_quota",
                self.now(),
                &cleared,
                true,
                &Value::Null,
            )
            .await?;
            return Ok(());
        }
        if truthy(&task["policy_version"]) && credit_rules::expired(&task, self.now()) {
            self.finish(
                id,
                &task,
                "verification_deadline",
                self.now(),
                &cleared,
                true,
                &Value::Null,
            )
            .await?;
            return Ok(());
        }
        task["owned_version"] = cleared["updated_at"].clone();
        let _ =
            actions::set_schedulable(&self.actions, id, true, &self.store.admin_token()?, 3).await;
        let final_row = self.row(id).await;
        if let Ok(row) = &final_row {
            if self.release_confirmed(row, &task, self.now())? {
                return self.finish_release(row, &task);
            }
        }
        self.save(id, &task, json!({"stage":"confirming","error_code":"recovery_failed","next_at":datetime(self.now()+Duration::seconds(30))}))?;
        final_row.map(|_| ())
    }
    pub fn release_confirmed(&self, row: &Value, task: &Value, at: DateTime<Utc>) -> Result<bool> {
        if !row.is_object() {
            return Ok(false);
        }
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let state = self.store.shared_snapshot()?;
        let meta = &state["scheduler"][id.to_string()];
        Ok(truthy(&task["owns_pause"])
            && truthy(&task["clear_requested_at"])
            && task["test_success"] == true
            && parse_datetime(&task["test_completed_at"]).is_some()
            && task["control_generation"] == json!(credit_ownership::generation(&state, id))
            && (!truthy(&task["policy_version"])
                || task["policy_credential_fingerprint"] == credential_fingerprint(row))
            && task["verification_method"]
                == json!(recovery_policy::method(&self.hooks.config()?, id))
            && integer(&task["verification_generation"]).unwrap_or(0)
                == integer(&meta["recovery_method_generation"]).unwrap_or(0)
            && task["signature"] == credit_ownership::signature(row)
            && self.eligible(row, at, false)?
            && recovery_rules::confirmed(row)
            && self.available(row, task, at)?)
    }
    pub fn finish_release(&self, row: &Value, task: &Value) -> Result<()> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let at = self.now();
        let completed = datetime(at);
        let mut finished = task.clone();
        finished.as_object_mut().unwrap().extend(json!({"revision":integer(&task["revision"]).unwrap_or(0)+1,"stage":"recovered","owns_pause":false,"recovered_at":completed,"error_code":"","next_at":null,"outcome":"restored","completed_at":completed,"result_reason":""}).as_object().unwrap().clone());
        let key = format!("reset-credit:{id}:{}", string(&task["episode"]));
        let mut history = json!({"account_id":id,"account_name":string(&row["name"]),"model_id":task["model_id"],"legacy":false,"kind":"quota_recovery","test_completed_at":task["test_completed_at"],"recovered_at":completed});
        if let Some(attempts) = task["test_attempts"].as_u64() {
            history["attempt_count"] = attempts.into();
        }
        if task["model_test_job_id"].is_string() {
            history["model_test_job_id"] = task["model_test_job_id"].clone();
        }
        if task["consumed"] == true && truthy(&task["reset_completed_at"]) {
            history["kind"] = "reset_credit".into();
            history["reset_credit"] = json!({"consumed":true,"completed_at":task["reset_completed_at"],"verification_method":if truthy(&task["verification_method"]) {string(&task["verification_method"])} else {"connection"}});
        }
        let mut event = history.clone();
        event.as_object_mut().unwrap().extend(json!({"status":"recovered","stage":"recovery","checked_at":completed,"test_success":true,"window_labels":["7d"],"fingerprint":task["episode"],"dedupe_key":key,"plan_type":quota::summary_from_result(row,&self.saved_quota(row,at)?)["plan_type"]}).as_object().unwrap().clone());
        self.store.transaction(|state| {
            let current = &state["scheduler"][id.to_string()]["auto_reset_credit"];
            if current["episode"] != task["episode"]
                || current["stage"] != task["stage"]
                || current["revision"] != task["revision"]
                || json!(credit_ownership::generation(state, id)) != task["control_generation"]
            {
                return Err(Error::new(409, "自动恢复状态已变化"));
            }
            state["scheduler"][id.to_string()]["auto_reset_credit"] = finished.clone();
            if state["recovery_history"].get(&key).is_none() {
                let sequence = state["recovery_history"]
                    .as_object()
                    .unwrap()
                    .values()
                    .filter_map(|v| integer(&v["id"]))
                    .max()
                    .unwrap_or(0)
                    + 1;
                history["id"] = sequence.into();
                history["dedupe_key"] = key.clone().into();
                state["recovery_history"][&key] = history;
                if truthy(&task["policy_version"]) {
                    let notices =
                        &mut state["scheduler"][id.to_string()]["credit_result_notifications"];
                    if !notices.is_object() {
                        *notices = json!({});
                    }
                    notices
                        .as_object_mut()
                        .unwrap()
                        .entry(string(&task["episode"]).to_owned())
                        .or_insert_with(|| self.notice(id, &finished, at, row));
                } else {
                    state["pending_events"][&key] = event;
                }
            }
            Ok(())
        })?;
        self.audit(
            id,
            "recovered",
            json!({"consumed":truthy(&task["consumed"])}),
        );
        Ok(())
    }
    pub async fn undo_unissued_pause(
        &self,
        id: i64,
        row: &Value,
        task: &Value,
        at: DateTime<Utc>,
    ) -> Result<Value> {
        if !row.is_object()
            || !self.eligible(row, at, true)?
            || task["policy_credential_fingerprint"] != credential_fingerprint(row)
        {
            return Ok(row.clone());
        }
        let _control = self.actions.locks.control(id).await;
        let latest = self.state(id)?;
        if latest["episode"] != task["episode"] || latest["revision"] != task["revision"] {
            return Ok(row.clone());
        }
        let mut actual = self.row(id).await?;
        if actual["schedulable"] != true {
            if !self.owned(&actual, task, true)? || !self.eligible(&actual, at, true)? {
                return Ok(actual);
            }
            let _ =
                actions::set_schedulable(&self.actions, id, true, &self.store.admin_token()?, 3)
                    .await;
            actual = self.row(id).await?;
        }
        if actual["schedulable"] == true
            && credit_ownership::signature(&actual) == task["signature"]
            && credential_fingerprint(&actual) == task["policy_credential_fingerprint"]
            && json!(credit_ownership::generation(
                self.store.shared_snapshot()?.as_ref(),
                id
            )) == task["control_generation"]
        {
            self.save(id, task, json!({"owns_pause":false}))?;
        }
        Ok(actual)
    }
    pub async fn merge_disposition(
        &self,
        id: i64,
        job_id: &str,
        disposition: &Value,
        row: &Value,
    ) -> Result<bool> {
        let task = self.state(id)?;
        if !truthy(&task["policy_version"]) || task["model_test_job_id"] != job_id {
            return Ok(false);
        }
        let fields = json!({"job_id":job_id,"degradation_confirmed":truthy(&disposition["marked"]) && truthy(&disposition["schedule_verified"]),"model_id":disposition["model_id"],"prediction":disposition["prediction"],"probability":disposition["probability"]});
        if task["stage"] != "finished" {
            self.finish(id, &task, "detection_held", self.now(), row, false, &fields)
                .await?;
            return Ok(true);
        }
        self.store.transaction(|state| {
            if let Some(notice) = state["scheduler"][id.to_string()]
                .get_mut("credit_result_notifications")
                .and_then(|v| v.get_mut(string(&task["episode"])))
            {
                if notice["status"] == "pending" && !truthy(&notice["attempts"]) {
                    notice
                        .as_object_mut()
                        .unwrap()
                        .extend(fields.as_object().unwrap().clone());
                }
            }
            Ok(())
        })?;
        Ok(true)
    }
}
