use crate::{
    actions,
    compat::{datetime, integer, parse_datetime, sha256, string, truthy},
    credit::CreditController,
    credit_ownership, credit_rules,
    error::{Error, Result},
    oauth_admission::{credential_fingerprint, AUTH_ERRORS},
    recovery_policy, recovery_rules,
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

        if !self.owned(row, task, false)? || !self.processing_eligible(row, task, at)? {
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
                let remaining = parse_datetime(&task["verification_deadline_at"])
                    .map(|t| (t - at).num_seconds().max(1) as u64)
                    .unwrap_or(1);
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
        if credit_rules::expired(&task, completed) {
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
            if attempts >= 2 || !credit_rules::verification_retryable(&test, method.unwrap_or("")) {
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
            if parse_datetime(&task["verification_deadline_at"]).is_none_or(|t| retry >= t) {
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
        self.release(&live, &task, completed).await
    }
    pub async fn release(&self, row: &Value, task: &Value, at: DateTime<Utc>) -> Result<()> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let (_control, fresh) = match self.actions.locks.try_control(id) {
            Ok(control) => (control, None),
            Err(_) => {
                let control = self.actions.locks.control(id).await;
                (control, Some(self.row(id).await?))
            }
        };
        let current = self.state(id)?;
        if current["episode"] != task["episode"] || current["revision"] != task["revision"] {
            return Ok(());
        }
        self.release_locked(fresh.as_ref().unwrap_or(row), task, at)
            .await
    }
    async fn release_locked(&self, row: &Value, task: &Value, at: DateTime<Utc>) -> Result<()> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let mut task = task.clone();
        let reason = self.guard_reason(id, row, &task, at)?;
        let reason = if !reason.is_empty() {
            reason
        } else if credit_rules::expired(&task, at) {
            "verification_deadline"
        } else if !self.available(row, &task, at)? {
            "incomplete_quota"
        } else {
            ""
        };
        if !reason.is_empty() {
            self.finish(id, &task, reason, at, row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        if task["test_success"] != true || parse_datetime(&task["test_completed_at"]).is_none() {
            return Ok(());
        }
        if recovery_rules::confirmed(row) {
            return self.finish_release(row, &task);
        }
        let mut cleared = row.clone();
        cleared["schedulable"] = true.into();
        let needs_clear =
            !truthy(&task["clear_requested_at"]) || !recovery_rules::confirmed(&cleared);
        let key = self.store.admin_token()?;
        let mut may_enable = !needs_clear;
        if needs_clear {
            if !truthy(&task["clear_requested_at"]) {
                task["clear_requested_at"] = datetime(self.now()).into();
            }
            let recovered = self.hooks.recover(id, key.clone()).await;
            if recovered
                .as_ref()
                .is_ok_and(|result| result["success"] == true)
            {
                // Upstream can disable or delete an account while recover-state is in flight.
                let current = self
                    .actions
                    .queries
                    .db
                    .one(
                        "SELECT status FROM accounts WHERE id=$1 AND deleted_at IS NULL",
                        &[id.into()],
                    )
                    .await;
                let current = match current {
                    Ok(current) => current,
                    Err(error) => {
                        self.save(id, &task, json!({"stage":"confirming","clear_requested_at":task["clear_requested_at"],"error_code":"recovery_failed","next_at":datetime(self.now()+Duration::seconds(2))}))?;
                        return Err(error);
                    }
                };
                if current.as_ref().is_none_or(|row| row["status"] != "active") {
                    let reason = match current.as_ref().map(|row| string(&row["status"])) {
                        None => "account_deleted",
                        Some("disabled") => "account_disabled",
                        _ => "account_error",
                    };
                    let mut stopped = row.clone();
                    stopped["status"] = current
                        .as_ref()
                        .map(|row| row["status"].clone())
                        .unwrap_or(Value::Null);
                    self.finish(id, &task, reason, self.now(), &stopped, true, &Value::Null)
                        .await?;
                    return Ok(());
                }
                may_enable = true;
            }
        }
        if may_enable {
            let _ = actions::set_schedulable(&self.actions, id, true, &key, 3).await;
        }
        let final_row = self.row(id).await;
        if let Ok(row) = &final_row {
            let at = self.now();
            let reason = self.guard_reason(id, row, &task, at)?;
            if !reason.is_empty() {
                return self
                    .finish(id, &task, reason, at, row, true, &Value::Null)
                    .await
                    .map(|_| ());
            }
            if recovery_rules::confirmed(row)
                && self.available(row, &task, at)?
                && !credit_rules::expired(&task, at)
            {
                return self.finish_release(row, &task);
            }
        }
        self.save(
            id,
            &task,
            json!({"stage":"confirming","clear_requested_at":task["clear_requested_at"],
            "error_code":"recovery_failed","next_at":datetime(self.now()+Duration::seconds(if may_enable {30} else {2}))}),
        )?;
        final_row.map(|_| ())
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
        if !row.is_object() || !self.eligible(row, at, true)? {
            return Ok(row.clone());
        }
        let _control = self.actions.locks.control(id).await;
        let latest = self.state(id)?;
        if latest["episode"] != task["episode"] || latest["revision"] != task["revision"] {
            return Ok(row.clone());
        }
        let mut actual = self.row(id).await?;
        if actual["schedulable"] != true {
            if !self.owned(&actual, task, false)? || !self.eligible(&actual, at, true)? {
                return Ok(actual);
            }
            let _ =
                actions::set_schedulable(&self.actions, id, true, &self.store.admin_token()?, 3)
                    .await;
            actual = self.row(id).await?;
        }
        if actual["schedulable"] == true
            && credit_ownership::signature(&actual) == task["signature"]
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
        if task["model_test_job_id"] != job_id {
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
