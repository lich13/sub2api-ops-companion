use crate::{
    actions,
    compat::{datetime, integer, string, truthy},
    credit::CreditController,
    credit_ownership, credit_rules,
    error::{Error, Result},
    oauth_admission::quota_complete,
    oauth_queries::{QueryOptions, Source},
    reset_credit_observation, usage,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

fn failed(code: &str) -> Value {
    json!({"success":false,"error_code":code})
}
impl CreditController {
    pub async fn request(
        &self,
        row: &Value,
        action: &str,
        task: &Value,
        at: DateTime<Utc>,
        source: Source,
        key: Option<&str>,
    ) -> Result<Value> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let key = key
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .unwrap_or(self.store.admin_token()?);
        let expected = task.clone();
        let owned_task = task.clone();
        let this = self.clone();
        let owned_action = action.to_owned();
        let owned_row = row.clone();
        let options = QueryOptions {
            source,
            reason: if action == "reset" {
                "reset_credit_consume"
            } else {
                "reset_credit_check"
            }
            .into(),
            at,
            ..QueryOptions::default()
        };
        let mut result=self.queries.external_read(row.clone(),options,source==Source::Automatic && action=="reset",if source==Source::Automatic && action=="query" {2} else {1},move |data| {let current=&data["scheduler"][id.to_string()]["auto_reset_credit"];current["episode"]==expected["episode"] && current["stage"]==expected["stage"] && current["revision"]==expected["revision"]},move ||async move {
            this.request_owned(&owned_row,&owned_action,&owned_task,at,source,&key).await.unwrap_or_else(|_|json!({"success":false,"uncertain":true,"error_code":"result_uncertain"}))
        }).await;
        if source == Source::Automatic
            && action == "reset"
            && result["error_code"] == "result_uncertain"
        {
            let latest = self.state(id)?;
            if latest["episode"] == task["episode"] && !truthy(&latest["attempt_at"]) {
                result["uncertain"] = false.into();
                result["error_code"] = if latest["stage"] == "pausing" {
                    "pause_uncertain"
                } else {
                    "query_state_unavailable"
                }
                .into();
            }
        }
        self.audit(id,action,json!({"source":source.as_str(),"success":truthy(&result["success"]),"error_code":string(&result["error_code"]),"next_at":result["next_query_at"],"diagnostics":result.get("diagnostics").cloned().unwrap_or(json!({}))}));
        Ok(result)
    }
    async fn request_owned(
        &self,
        row: &Value,
        action: &str,
        task: &Value,
        at: DateTime<Utc>,
        source: Source,
        key: &str,
    ) -> Result<Value> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let live = self.row(id).await?;
        if !live.is_object() {
            return Ok(failed("reset_state_changed"));
        }
        let mut active = task.clone();
        if source == Source::Automatic && truthy(&task["policy_version"]) {
            let reason = self.guard_reason(id, &live, task, self.now())?;
            if !reason.is_empty() {
                return Ok(failed(reason));
            }
            if credit_rules::expired(task, self.now()) || task["stage"] == "finished" {
                return Ok(failed("consume_deadline"));
            }
            if action == "query" {
                if truthy(&task["supplement_attempt_at"]) {
                    return Ok(failed("supplement_limit"));
                }
                active = self.save(
                    id,
                    task,
                    json!({"supplement_attempt_at":datetime(self.now())}),
                )?;
            }
        }
        if source == Source::Automatic {
            if !live["extra"]["auto_reset_credit_enabled"].is_null()
                && live["extra"]["auto_reset_credit_enabled"] != false
            {
                return Ok(failed("conflict"));
            }
            if action == "reset" {
                let depletion = self.depletion(&live, at, false).await?;
                if !truthy(&depletion)
                    || depletion["reset_at"] != task["reset_at"]
                    || depletion["evidence_fingerprint"] != task["evidence_fingerprint"]
                {
                    return Ok(failed("reset_state_changed"));
                }
                let reason = self.positive_credit_reason(&live, &depletion, self.now())?;
                if !reason.is_empty() {
                    return Ok(failed(reason));
                }
                {
                    let _control = self.actions.locks.control(id).await;
                    active=self.save(id,task,json!({"stage":"pausing","signature":credit_ownership::signature(&live),"next_at":null,"control_generation":credit_ownership::generation(self.store.shared_snapshot()?.as_ref(),id)}))?;
                    let paused = actions::set_schedulable(&self.actions, id, false, key, 3).await;
                    let after = self.row(id).await?;
                    let latest = self.state(id)?;
                    if latest["stage"] == "finished" && latest["episode"] == active["episode"] {
                        if after.is_object()
                            && after["schedulable"] == false
                            && credit_ownership::signature(&after) == active["signature"]
                            && json!(credit_ownership::generation(
                                self.store.shared_snapshot()?.as_ref(),
                                id
                            )) == active["control_generation"]
                        {
                            self.save(
                                id,
                                &latest,
                                json!({"owns_pause":true,"owned_version":after["updated_at"]}),
                            )?;
                        }
                        return Ok(failed("consume_deadline"));
                    }
                    if !truthy(&paused["success"])
                        || !after.is_object()
                        || after["schedulable"] != false
                        || credit_ownership::signature(&after) != active["signature"]
                    {
                        self.save(
                            id,
                            &active,
                            json!({"stage":"blocked","error_code":"pause_uncertain"}),
                        )?;
                        return Ok(failed("pause_uncertain"));
                    }
                    active=self.save(id,&active,json!({"stage":"resetting","owns_pause":true,"owned_version":after["updated_at"]}))?;
                }
                let final_row = self.row(id).await?;
                let depletion = if final_row.is_object() {
                    self.depletion(&final_row, self.now(), true).await?
                } else {
                    Value::Null
                };
                if !self.owned(&final_row, &active, true)?
                    || !truthy(&depletion)
                    || depletion["reset_at"] != task["reset_at"]
                    || depletion["evidence_fingerprint"] != task["evidence_fingerprint"]
                {
                    self.save(
                        id,
                        &active,
                        json!({"stage":"blocked","error_code":"ownership_changed"}),
                    )?;
                    return Ok(failed("ownership_changed"));
                }
                let reason = self.positive_credit_reason(&final_row, &depletion, self.now())?;
                if !reason.is_empty() {
                    self.save(id, &active, json!({"stage":"blocked","error_code":reason}))?;
                    return Ok(failed(reason));
                }
            } else if truthy(&task["owns_pause"]) && !self.owned(&live, task, true)? {
                return Ok(failed("ownership_changed"));
            } else if !truthy(&task["owns_pause"])
                && !truthy(&self.depletion(&live, at, false).await?)
            {
                return Ok(failed("reset_state_changed"));
            }
        } else if action == "reset" {
            active = self.save(
                id,
                task,
                json!({"stage":"uncertain","attempt_at":datetime(self.now())}),
            )?;
        }
        if self.state(id)?["revision"] != active["revision"] {
            return Ok(failed("manual_intervention"));
        }
        let requested = self.now();
        if source == Source::Automatic && action == "reset" {
            let _control = self.actions.locks.control(id).await;
            let reason = self.guard_reason(id, &self.row(id).await?, &active, requested)?;
            if !reason.is_empty() {
                return Ok(failed(reason));
            }
            if credit_rules::expired(&active, requested) {
                return Ok(failed("consume_deadline"));
            }
            active = self.save(id, &active, json!({"attempt_at":datetime(requested)}))?;
        }
        let mut result = credit_rules::execute(&self.actions, action, id, key, 90).await;
        let completed = self.now();
        let diagnostics = result
            .get("diagnostics")
            .filter(|v| truthy(v))
            .cloned()
            .unwrap_or_else(|| reset_credit_observation::diagnostics(None, &result["data"]));
        if action == "reset" {
            let consumed = truthy(&result["consumed"]);
            let outcome = if truthy(&result["consumption_outcome"]) {
                string(&result["consumption_outcome"])
            } else if consumed {
                "consumed"
            } else {
                "unknown"
            };
            let receipt_code = if consumed {
                ""
            } else if truthy(&result["error_code"]) {
                string(&result["error_code"])
            } else {
                "result_uncertain"
            };
            let latest = self.state(id)?;
            if latest["episode"] == active["episode"]
                && matches!(string(&latest["stage"]), "manual" | "finished")
            {
                self.save(id,&latest,json!({"consumed":consumed,"reset_completed_at":if consumed {json!(datetime(completed))} else {Value::Null},"error_code":if latest["stage"]=="finished" {string(&latest["error_code"])} else {receipt_code},"consumption_outcome":outcome,"receipt":diagnostics}))?;
                if latest["stage"] == "finished" {
                    self.store.transaction(|state| {
                        let meta = &mut state["scheduler"][id.to_string()];
                        let value = &mut meta["auto_reset_credit"];
                        value["outcome"] = credit_rules::outcome(value).into();
                        let episode = string(&value["episode"]).to_owned();
                        let outcome = value["outcome"].clone();
                        let reset = value["reset_completed_at"].clone();
                        if let Some(notice) = meta
                            .get_mut("credit_result_notifications")
                            .and_then(|v| v.get_mut(&episode))
                        {
                            if notice["status"] == "pending" && !truthy(&notice["attempts"]) {
                                notice["outcome"] = outcome;
                                notice["reset_completed_at"] = reset;
                            }
                        }
                        Ok(())
                    })?;
                }
                return Ok(result);
            }
            let stage = if source != Source::Automatic && outcome != "unknown" {
                "manual"
            } else if consumed {
                "testing"
            } else if outcome == "not_consumed" {
                "confirming"
            } else {
                "uncertain"
            };
            active=self.save(id,&active,json!({"stage":stage,"consumed":consumed,"reset_completed_at":if consumed {json!(datetime(completed))} else {Value::Null},"consumption_outcome":outcome,"error_code":receipt_code,"receipt":diagnostics,"verification_deadline_at":if consumed && source==Source::Automatic {json!(datetime(completed+Duration::seconds(300)))} else {Value::Null}}))?;
        }
        if result["data"].is_object() {
            result["quota_result"] =
                credit_rules::quota_result(&result["data"], &live, completed, Some(requested));
            if action == "query" && !quota_complete(&live, &result["quota_result"]) {
                result["success"] = false.into();
                result["error_code"] = "incomplete_quota".into();
            }
        }
        let after = self.row(id).await?;
        if action == "query" {
            if let Some(observation) =
                credit_rules::credit_observation(&live, &after, &result, at, completed)
            {
                result["reset_credit_observation"] = observation;
            } else if truthy(&result["success"]) {
                let stamp = crate::compat::parse_datetime(
                    &usage::reset_credits(&after["extra"], completed)["observed_at"],
                );
                let legacy = result["data"].get("rate_limit_reset_credits").is_none()
                    && result["data"]["cache_persisted"] != false
                    && stamp.is_some_and(|t| t >= at - Duration::seconds(1) && t <= completed);
                if !legacy {
                    result["success"] = false.into();
                    result["error_code"] = "credit_snapshot_unconfirmed".into();
                }
            }
        }
        if truthy(&active["owns_pause"]) && self.owned(&after, &active, false)? {
            self.save(id, &active, json!({"owned_version":after["updated_at"]}))?;
        }
        Ok(result)
    }
    pub async fn manual(&self, row: &Value, action: &str, key: &str) -> Result<Value> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let at = self.now();
        let _lease = self.disk_lease(id)?;
        let mut task = self.state(id)?;
        if action == "reset" && truthy(&task["attempt_at"]) && task["stage"] != "recovered" {
            return Ok(failed("result_uncertain"));
        }
        if action == "reset" || !truthy(&task) {
            task = json!({"episode":uuid::Uuid::new_v4().simple().to_string(),"stage":if action=="reset" {"manual"} else {"closed"},"source":"manual"});
            self.store.transaction(|state| {
                let meta = &mut state["scheduler"][id.to_string()];
                if !meta.is_object() {
                    *meta = json!({});
                }
                meta["auto_reset_credit"] = task.clone();
                Ok(())
            })?;
        }
        let result = self
            .request(row, action, &task, at, Source::Manual, Some(key))
            .await?;
        let task = self.state(id)?;
        let live = self.row(id).await?;
        if live.is_object()
            && !truthy(&task["owns_pause"])
            && truthy(&task["attempt_at"])
            && self.eligible(&live, self.now(), false)?
            && crate::recovery_rules::confirmed(&live)
            && self.available(&live, &task, self.now())?
        {
            self.save(id,&task,json!({"stage":"recovered","recovered_at":datetime(self.now()),"next_at":null,"error_code":""}))?;
        }
        Ok(result)
    }
}
