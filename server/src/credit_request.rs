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
    #[expect(
        clippy::too_many_arguments,
        reason = "Pass the current account and depletion evidence through the internal credit request"
    )]
    pub async fn request(
        &self,
        row: &Value,
        action: &str,
        task: &Value,
        at: DateTime<Utc>,
        source: Source,
        key: Option<&str>,
        evidence: Option<&Value>,
    ) -> Result<(Value, Option<Value>)> {
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
        let evidence = evidence.cloned();
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
        let (mut result, latest_row)=self.queries.external_read(row.clone(),options,source==Source::Automatic && action=="reset",if source==Source::Automatic && action=="query" {2} else {1},move |data| {let current=&data["scheduler"][id.to_string()]["auto_reset_credit"];current["episode"]==expected["episode"] && current["stage"]==expected["stage"] && current["revision"]==expected["revision"]},move ||async move {
            this.request_owned(&owned_row,&owned_action,&owned_task,at,source,&key,evidence.as_ref()).await.unwrap_or_else(|_|(json!({"success":false,"uncertain":true,"error_code":"result_uncertain"}),None))
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
        Ok((result, latest_row))
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "Keep private row and prepared evidence in the internal request"
    )]
    async fn request_owned(
        &self,
        row: &Value,
        action: &str,
        task: &Value,
        at: DateTime<Utc>,
        source: Source,
        key: &str,
        evidence: Option<&Value>,
    ) -> Result<(Value, Option<Value>)> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let (control, refreshed) = if source == Source::Automatic && action == "reset" {
            match self.actions.locks.try_control(id) {
                Ok(control) => (Some(control), None),
                Err(_) => {
                    let control = self.actions.locks.control(id).await;
                    (Some(control), Some(self.row(id).await?))
                }
            }
        } else {
            (None, None)
        };
        let live = refreshed.as_ref().unwrap_or(row);
        let evidence = if refreshed.is_none() { evidence } else { None };
        if !live.is_object() {
            return Ok((failed("reset_state_changed"), None));
        }
        let mut active = task.clone();
        if source == Source::Automatic {
            let reason = self.guard_reason(id, live, task, self.now())?;
            if !reason.is_empty() {
                return Ok((failed(reason), None));
            }
            if credit_rules::expired(task, self.now()) || task["stage"] == "finished" {
                return Ok((failed("consume_deadline"), None));
            }
            if action == "query" {
                if truthy(&task["supplement_attempt_at"]) {
                    return Ok((failed("supplement_limit"), None));
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
                return Ok((failed("conflict"), None));
            }
            if action == "reset" {
                let depletion = match evidence {
                    Some(value) => value.clone(),
                    None => self.depletion(live, at, false).await?,
                };
                if !truthy(&depletion)
                    || depletion["reset_at"] != task["reset_at"]
                    || depletion["evidence_fingerprint"] != task["evidence_fingerprint"]
                {
                    return Ok((failed("reset_state_changed"), None));
                }
                if evidence.is_none() {
                    let reason = self.positive_credit_reason(live, &depletion, self.now())?;
                    if !reason.is_empty() {
                        return Ok((failed(reason), None));
                    }
                }
                {
                    active=self.save(id,task,json!({"stage":"pausing","signature":credit_ownership::signature(live),"next_at":null,"control_generation":credit_ownership::generation(self.store.shared_snapshot()?.as_ref(),id)}))?;
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
                        return Ok((failed("consume_deadline"), None));
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
                        return Ok((failed("pause_uncertain"), None));
                    }
                    active["owns_pause"] = true.into();
                    active["owned_version"] = after["updated_at"].clone();
                    let requested = self.now();
                    let reason = self.guard_reason(id, &after, &active, requested)?;
                    let reason = if !reason.is_empty() {
                        reason
                    } else if credit_rules::expired(&active, requested) {
                        "consume_deadline"
                    } else {
                        self.positive_credit_reason(&after, &depletion, requested)?
                    };
                    if !reason.is_empty() {
                        self.save(id, &active, json!({"stage":"blocked","error_code":reason}))?;
                        return Ok((failed(reason), None));
                    }
                    active = self.save(
                        id,
                        &active,
                        json!({"stage":"resetting","attempt_at":datetime(requested)}),
                    )?;
                }
            } else if truthy(&task["owns_pause"]) && !self.owned(live, task, false)? {
                return Ok((failed("ownership_changed"), None));
            } else if !truthy(&task["owns_pause"])
                && !truthy(&self.depletion(live, at, false).await?)
            {
                return Ok((failed("reset_state_changed"), None));
            }
        } else if action == "reset" {
            active = self.save(
                id,
                task,
                json!({"stage":"uncertain","attempt_at":datetime(self.now())}),
            )?;
        }
        if self.state(id)?["revision"] != active["revision"] {
            return Ok((failed("manual_intervention"), None));
        }
        let requested = self.now();
        drop(control);
        let mut result = credit_rules::execute(&self.actions, action, id, key, 90).await;
        let completed = self.now();
        let diagnostics = result
            .get("diagnostics")
            .filter(|v| truthy(v))
            .cloned()
            .unwrap_or_else(|| reset_credit_observation::diagnostics(None, &result["data"]));
        let after = self.row(id).await;
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
                return Ok((result, after.ok()));
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
            let mut receipt = json!({"stage":stage,"consumed":consumed,"reset_completed_at":if consumed {json!(datetime(completed))} else {Value::Null},"consumption_outcome":outcome,"error_code":receipt_code,"receipt":diagnostics,"verification_deadline_at":if consumed && source==Source::Automatic {json!(datetime(completed+Duration::seconds(300)))} else {Value::Null}});
            if let Ok(row) = &after {
                if truthy(&active["owns_pause"]) && self.owned(row, &active, false)? {
                    receipt["owned_version"] = row["updated_at"].clone();
                }
            }
            active = self.save(id, &active, receipt)?;
        }
        if result["data"].is_object() {
            result["quota_result"] =
                credit_rules::quota_result(&result["data"], live, completed, Some(requested));
            if action == "query" && !quota_complete(live, &result["quota_result"]) {
                result["success"] = false.into();
                result["error_code"] = "incomplete_quota".into();
            }
        }
        let after = match after {
            Ok(row) => row,
            Err(_) if action == "reset" => return Ok((result, None)),
            Err(error) => return Err(error),
        };
        if action == "query" {
            if let Some(observation) =
                credit_rules::credit_observation(&after, &result, at, completed)
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
        if action != "reset"
            && truthy(&active["owns_pause"])
            && self.owned(&after, &active, false)?
        {
            self.save(id, &active, json!({"owned_version":after["updated_at"]}))?;
        }
        Ok((result, Some(after)))
    }
    pub async fn manual(&self, row: &Value, action: &str, key: &str) -> Result<Value> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        let at = self.now();
        let mut task = self.state(id)?;
        if action == "reset" && truthy(&task["attempt_at"]) && task["stage"] != "recovered" {
            return Ok(failed("result_uncertain"));
        }
        let _test_slot = if action == "reset" {
            Some(
                self.slots
                    .acquire()
                    .ok_or_else(crate::account_locks::busy)?,
            )
        } else {
            None
        };
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
        let (result, latest_row) = self
            .request(row, action, &task, at, Source::Manual, Some(key), None)
            .await?;
        let mut task = self.state(id)?;
        if action == "reset" && task["consumed"] == true {
            let generation =
                credit_ownership::generation(self.store.shared_snapshot()?.as_ref(), id);
            let model = string(&self.hooks.config()?["oauth_recovery_test_model_id"]).to_owned();
            let test = self
                .hooks
                .connection_test(id, model.clone(), generation, 30, true)
                .await
                .unwrap_or_else(|_| failed("test_failed"));
            let completed = self.now();
            let _control = self.actions.locks.control(id).await;
            let live = self.row(id).await?;
            let recovered = self.manual_recovered(&live, &task)?;
            let current = self.state(id)?;
            if current["episode"] != task["episode"] || current["revision"] != task["revision"] {
                return Ok(result);
            }
            let unchanged =
                credit_ownership::generation(self.store.shared_snapshot()?.as_ref(), id)
                    == generation;
            let verified = test["success"] == true && unchanged;
            let code = if !unchanged {
                "manual_intervention"
            } else if !verified {
                test["error_code"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("test_failed")
            } else if !recovered {
                "recovery_failed"
            } else {
                ""
            };
            let revision = task["revision"].clone();
            task.as_object_mut().unwrap().extend(
                json!({
                    "revision":integer(&revision).unwrap_or(0)+1,
                    "stage":if recovered {"recovered"} else {"manual"},
                    "recovered_at":if recovered {json!(datetime(completed))} else {Value::Null},
                    "next_at":null,"test_completed_at":datetime(completed),"test_success":verified,
                    "test_attempts":1,"verification_method":"connection","model_id":model,
                    "outcome":if verified && recovered {"restored"} else {"unrecovered"},
                    "error_code":code,"result_reason":code,"completed_at":datetime(completed)
                })
                .as_object()
                .unwrap()
                .clone(),
            );
            let mut notice = self.notice(id, &task, completed, &live);
            notice["test_success"] = verified.into();
            self.store.transaction(|state| {
                let meta = &mut state["scheduler"][id.to_string()];
                let current = &meta["auto_reset_credit"];
                if current["episode"] != task["episode"] || current["revision"] != revision {
                    return Err(Error::new(409, "用卡任务已变化"));
                }
                meta["auto_reset_credit"] = task.clone();
                if !meta["credit_result_notifications"].is_object() {
                    meta["credit_result_notifications"] = json!({});
                }
                meta["credit_result_notifications"][string(&task["episode"])] = notice;
                Ok(())
            })?;
            self.audit(
                id,
                "test",
                json!({"source":"manual","success":verified,"error_code":code}),
            );
        } else if let Some(live) = latest_row.as_ref() {
            if self.manual_recovered(live, &task)? {
                self.save(id,&task,json!({"stage":"recovered","recovered_at":datetime(self.now()),"next_at":null,"error_code":""}))?;
            }
        }
        Ok(result)
    }
    fn manual_recovered(&self, row: &Value, task: &Value) -> Result<bool> {
        Ok(row.is_object()
            && !truthy(&task["owns_pause"])
            && truthy(&task["attempt_at"])
            && self.eligible(row, self.now(), false)?
            && crate::recovery_rules::confirmed(row)
            && self.available(row, task, self.now())?)
    }
}
