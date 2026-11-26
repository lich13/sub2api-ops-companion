use super::{generation, rules, signature, CreditController};
use crate::{
    actions,
    compat::{datetime, integer, parse_datetime, string, truthy},
    error::{Error, Result},
    oauth_admission::{quota_complete, AUTH_ERRORS},
    oauth_queries::{QueryOptions, Source},
    recovery_rules,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use std::{borrow::Cow, collections::BTreeMap};

fn failed(code: &str) -> Value {
    json!({"success":false,"error_code":code})
}

impl CreditController {
    pub async fn sweep(
        &self,
        rows: &[impl std::borrow::Borrow<Value>],
        at: DateTime<Utc>,
    ) -> Result<()> {
        let mut live = rows
            .iter()
            .map(|row| row.borrow())
            .filter_map(|row| integer(&row["id"]).map(|id| (id, Cow::Borrowed(row))))
            .collect::<BTreeMap<_, _>>();
        let state = self.store.shared_snapshot()?;
        for (key, meta) in state["scheduler"].as_object().into_iter().flatten() {
            let id = key.parse::<i64>().map_err(|_| Error::internal())?;
            let task = &meta["auto_reset_credit"];
            if !truthy(task)
                || rules::manual(task)
                || matches!(string(&task["stage"]), "finished" | "recovered" | "closed")
            {
                continue;
            }
            if let std::collections::btree_map::Entry::Vacant(e) = live.entry(id) {
                e.insert(Cow::Owned(self.row(id).await?));
            }
            let row = &live[&id];
            let current = at;
            let control = &meta["manual_control"];
            let expected = integer(&task["policy_control_generation"]).unwrap_or(0);
            let gen_val = integer(&control["generation"]).unwrap_or(0);
            let finishing = truthy(&task["degradation_pending"])
                && (gen_val == expected
                    || control["origin"] == "model_detection" && gen_val == expected + 1);
            let mut reason = if finishing {
                ""
            } else {
                self.guard_reason(id, row, task, current)?
            };
            if reason.is_empty() && rules::expired(task, current) {
                reason = if truthy(&task["consumed"]) {
                    "verification_deadline"
                } else {
                    "consume_deadline"
                };
            }
            if matches!(string(&task["stage"]), "manual" | "blocked")
                && !finishing
                && reason.is_empty()
            {
                reason = if truthy(&task["error_code"]) {
                    string(&task["error_code"])
                } else {
                    "manual_intervention"
                };
            }
            if !reason.is_empty() {
                if let Err(error) = self
                    .finish(id, task, reason, current, row, true, &Value::Null)
                    .await
                {
                    if error.status.as_u16() != 409 {
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn run_account(&self, item: &Value, at: DateTime<Utc>) -> Result<()> {
        let id = integer(&item["id"]).ok_or_else(Error::invalid)?;
        let enabled = self.hooks.config()?["oauth_auto_reset_credit_enabled"] == true;
        if !enabled && !truthy(&self.state(id)?) {
            return Ok(());
        }
        let Ok(_budget) = self.budget.clone().try_acquire_owned() else {
            return Ok(());
        };
        let _lease = match self.actions.locks.try_account(item) {
            Ok(v) => v,
            Err(e) if e.status.as_u16() == 409 => return Ok(()),
            Err(e) => return Err(e),
        };
        if let Err(error) = self.bounded_step(id, at, enabled).await {
            self.audit(id, "paused", json!({"error_code": if error.status.as_u16() == 500 || error.status.as_u16() == 503 { "query_state_unavailable" } else { "state_check_failed" }}));
            if error.status.as_u16() != 409 {
                return Err(error);
            }
        }
        Ok(())
    }

    pub async fn bounded_step(&self, id: i64, at: DateTime<Utc>, enabled: bool) -> Result<()> {
        let mut row = self.row(id).await?;
        let mut task = self.state(id)?;
        let mut admitted = false;
        self.observe_recovery(&row, at)?;
        let evidence = if row.is_object()
            && enabled
            && (if task["stage"] == "finished" {
                truthy(&task["recovered_evidence_at"])
            } else {
                !truthy(&task["attempt_at"])
                    || rules::manual(&task)
                    || matches!(string(&task["stage"]), "recovered" | "closed")
            }) {
            self.depletion(&row, at, truthy(&task["owns_pause"]))
                .await?
        } else {
            Value::Null
        };
        if rules::manual(&task) && !matches!(string(&task["stage"]), "recovered" | "closed") {
            if row.is_object()
                && self.eligible(&row, at, false)?
                && recovery_rules::confirmed(&row)
                && self.available(&row, &task, at)?
            {
                self.save(
                    id,
                    &task,
                    json!({"stage":"recovered","recovered_at":datetime(at),"next_at":null}),
                )?;
                return Ok(());
            }
            let control = self.store.shared_snapshot()?["scheduler"][id.to_string()]
                ["manual_control"]
                .clone();
            let changed = parse_datetime(&control["at"]);
            let evidence = if row.is_object() && enabled {
                evidence.clone()
            } else {
                Value::Null
            };
            if !truthy(&evidence)
                || control["enabled"] != true
                || changed.is_none()
                || parse_datetime(&evidence["evidence_at"])
                    .zip(changed)
                    .is_none_or(|(observed, changed)| observed <= changed)
                || evidence["evidence_fingerprint"] == task["evidence_fingerprint"]
            {
                return Ok(());
            }
            task = self.begin_episode(&row, &task, &evidence, at)?;
            if !truthy(&task) {
                return Ok(());
            }
            admitted = true;
        }
        if task["stage"] == "finished" {
            if truthy(&task["owns_pause"]) && !truthy(&task["attempt_at"]) {
                row = self.undo_unissued_pause(id, &row, &task, at).await?;
                task = self.state(id)?;
            }
            if row.is_object()
                && self.eligible(&row, at, false)?
                && recovery_rules::confirmed(&row)
                && self.available(&row, &task, at)?
                && !truthy(&task["recovered_evidence_at"])
            {
                self.save(id, &task, json!({"recovered_evidence_at":datetime(at)}))?;
                return Ok(());
            }
            let recovered = parse_datetime(&task["recovered_evidence_at"]);
            let evidence = if row.is_object() && recovered.is_some() && enabled {
                evidence.clone()
            } else {
                Value::Null
            };
            if !truthy(&evidence)
                || parse_datetime(&evidence["evidence_at"])
                    .zip(recovered)
                    .is_none_or(|(observed, recovered)| observed <= recovered)
            {
                return Ok(());
            }
            task = self.begin_episode(&row, &task, &evidence, at)?;
            if !truthy(&task) {
                return Ok(());
            }
            admitted = true;
        }
        if !truthy(&task) || matches!(string(&task["stage"]), "recovered" | "closed") {
            let evidence = if row.is_object() && enabled {
                evidence.clone()
            } else {
                Value::Null
            };
            if !truthy(&evidence) {
                return Ok(());
            }
            let control = self.store.shared_snapshot()?["scheduler"][id.to_string()]
                ["manual_control"]
                .clone();
            let observed = parse_datetime(&evidence["evidence_at"]);
            if [&task["recovered_at"], &control["at"]]
                .into_iter()
                .filter_map(parse_datetime)
                .any(|boundary| observed.is_none_or(|t| t <= boundary))
            {
                return Ok(());
            }
            task = self.begin_episode(&row, &task, &evidence, at)?;
            if !truthy(&task) {
                return Ok(());
            }
            admitted = true;
        }
        self.advance_credit(id, row, task, evidence, at, admitted)
            .await
    }
    async fn advance_credit(
        &self,
        id: i64,
        row: Value,
        mut task: Value,
        evidence: Value,
        at: DateTime<Utc>,
        admitted: bool,
    ) -> Result<()> {
        let reason = self.guard_reason(id, &row, &task, at)?;
        if !reason.is_empty() {
            self.finish(id, &task, reason, at, &row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        if rules::expired(&task, at) {
            self.finish(
                id,
                &task,
                if truthy(&task["consumed"]) {
                    "verification_deadline"
                } else {
                    "consume_deadline"
                },
                at,
                &row,
                true,
                &Value::Null,
            )
            .await?;
            return Ok(());
        }
        if truthy(&row["extra"]["auto_reset_credit_enabled"]) {
            self.finish(id, &task, "conflict", at, &row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        if truthy(&task["attempt_at"]) {
            if !truthy(&task["consumed"]) {
                self.finish(
                    id,
                    &task,
                    if truthy(&task["error_code"]) {
                        string(&task["error_code"])
                    } else {
                        "result_uncertain"
                    },
                    at,
                    &row,
                    true,
                    &Value::Null,
                )
                .await?;
                return Ok(());
            }
            if parse_datetime(&task["next_at"]).is_some_and(|due| due > at) {
                return Ok(());
            }
            if truthy(&task["test_success"]) {
                return self.release(&row, &task, at).await;
            }
            if truthy(&task["verification_dispatched"]) && !truthy(&task["model_test_job_id"]) {
                self.finish(
                    id,
                    &task,
                    "verification_interrupted",
                    at,
                    &row,
                    true,
                    &Value::Null,
                )
                .await?;
                return Ok(());
            }
            if integer(&task["test_attempts"]).unwrap_or(0) >= 2 {
                self.finish(
                    id,
                    &task,
                    "verification_limit",
                    at,
                    &row,
                    true,
                    &Value::Null,
                )
                .await?;
                return Ok(());
            }
            return self.test(&row, &task, at, false).await;
        }
        if task["stage"] == "pausing" {
            self.finish(id, &task, "pause_uncertain", at, &row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        if !truthy(&evidence) {
            self.finish(id, &task, "block_cleared", at, &row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        if truthy(&task["owns_pause"]) {
            self.finish(id, &task, "pause_uncertain", at, &row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        if ["reset_at", "evidence_fingerprint", "evidence_source"]
            .iter()
            .any(|key| task[key] != evidence[key])
        {
            let changes = evidence
                .as_object()
                .ok_or_else(Error::invalid)?
                .iter()
                .filter(|(key, _)| key.as_str() != "evidence_at")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<serde_json::Map<_, _>>();
            task = self.save(id, &task, changes.into())?;
        }
        if parse_datetime(&task["next_at"]).is_some_and(|due| due > at) {
            return Ok(());
        }
        if !admitted {
            let reason = self.positive_credit_reason(&row, at);
            if !reason.is_empty() {
                self.finish(id, &task, reason, at, &row, true, &Value::Null)
                    .await?;
                return Ok(());
            }
        }
        let at = self.now();
        if rules::expired(&task, at) {
            self.finish(id, &task, "consume_deadline", at, &row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        self.consume_credit(id, row, task, evidence, at).await
    }

    async fn consume_credit(
        &self,
        id: i64,
        row: Value,
        mut task: Value,
        evidence: Value,
        at: DateTime<Utc>,
    ) -> Result<()> {
        if recovery_rules::method(&self.hooks.config()?, id) == Some("model") {
            let result = self
                .hooks
                .model_verify(id, string(&task["episode"]).into(), 1, true)
                .await?;
            task = self.state(id)?;
            if task["stage"] == "finished" {
                return Ok(());
            }
            if truthy(&result["job_id"]) {
                self.store.transaction(|state| {
                    let current = &mut state["scheduler"][id.to_string()]["auto_reset_credit"];
                    if current["episode"] == task["episode"] {
                        current["model_test_job_id"] = result["job_id"].clone();
                    }
                    Ok(())
                })?;
                task = self.state(id)?;
            }
            if truthy(&result["deferred"]) {
                if !matches!(string(&task["stage"]), "pausing" | "resetting") {
                    self.bounded_wait(id, &task, &result, self.now()).await?;
                }
                return Ok(());
            }
            if truthy(&task["consumed"]) {
                self.test(&row, &task, self.now(), false).await?;
            } else {
                self.finish(
                    id,
                    &task,
                    if truthy(&result["error_code"]) {
                        string(&result["error_code"])
                    } else {
                        "result_uncertain"
                    },
                    self.now(),
                    &row,
                    true,
                    &Value::Null,
                )
                .await?;
            }
            return Ok(());
        }
        let Some(_slot) = self.slots.acquire()? else {
            self.bounded_wait(
                id,
                &task,
                &json!({"error_code":"test_slots","next_at":datetime(at+Duration::seconds(2))}),
                at,
            )
            .await?;
            return Ok(());
        };
        let (result, latest_row) = self
            .request(
                &row,
                "reset",
                &task,
                at,
                Source::Automatic,
                None,
                Some(&evidence),
            )
            .await?;
        task = self.state(id)?;
        if task["stage"] == "finished" {
            return Ok(());
        }
        if truthy(&task["consumed"]) && !truthy(&result["uncertain"]) {
            if let Some(live) = latest_row {
                self.test(&live, &task, self.now(), true).await?;
            }
        } else if truthy(&task["attempt_at"]) {
            self.finish(
                id,
                &task,
                if truthy(&result["error_code"]) {
                    string(&result["error_code"])
                } else {
                    "result_uncertain"
                },
                self.now(),
                &row,
                true,
                &Value::Null,
            )
            .await?;
        } else {
            self.bounded_wait(id, &task, &result, self.now()).await?;
        }
        Ok(())
    }
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
        let (mut result, latest_row) = self
            .queries
            .external_read(
                row.clone(),
                options,
                source == Source::Automatic,
                move |data| {
                    let current = &data["scheduler"][id.to_string()]["auto_reset_credit"];
                    current["episode"] == expected["episode"]
                        && current["stage"] == expected["stage"]
                        && current["revision"] == expected["revision"]
                },
                move || async move {
                    this.request_owned(
                        &owned_row,
                        &owned_action,
                        &owned_task,
                        at,
                        source,
                        &key,
                        evidence.as_ref(),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        (
                            json!({"success":false,"uncertain":true,"error_code":"result_uncertain"}),
                            None,
                        )
                    })
                },
            )
            .await;
        if source == Source::Automatic && result["error_code"] == "result_uncertain" {
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
        self.audit(id, action, json!({"source":source.as_str(),"success":truthy(&result["success"]),"error_code":string(&result["error_code"]),"next_at":result["next_query_at"],"diagnostics":result.get("diagnostics").cloned().unwrap_or(json!({}))}));
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
        let (control, refreshed) = if source == Source::Automatic {
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
            if rules::expired(task, self.now()) || task["stage"] == "finished" {
                return Ok((failed("consume_deadline"), None));
            }
            if !live["extra"]["auto_reset_credit_enabled"].is_null()
                && live["extra"]["auto_reset_credit_enabled"] != false
            {
                return Ok((failed("conflict"), None));
            }
            if evidence.is_none() {
                let reason = self.positive_credit_reason(live, self.now());
                if !reason.is_empty() {
                    return Ok((failed(reason), None));
                }
            }
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
            active = self.save(id, task, json!({"stage":"pausing","signature":signature(live),"next_at":null,"control_generation":generation(self.store.shared_snapshot()?.as_ref(),id)}))?;
            let paused = actions::set_schedulable(&self.actions, id, false, key, 3).await;
            let after = self.row(id).await?;
            let latest = self.state(id)?;
            if latest["stage"] == "finished" && latest["episode"] == active["episode"] {
                if after.is_object()
                    && after["schedulable"] == false
                    && signature(&after) == active["signature"]
                    && json!(generation(self.store.shared_snapshot()?.as_ref(), id))
                        == active["control_generation"]
                {
                    self.save(id, &latest, json!({"owns_pause":true}))?;
                }
                return Ok((failed("consume_deadline"), None));
            }
            if !truthy(&paused["success"])
                || !after.is_object()
                || after["schedulable"] != false
                || signature(&after) != active["signature"]
            {
                self.save(
                    id,
                    &active,
                    json!({"stage":"blocked","error_code":"pause_uncertain"}),
                )?;
                return Ok((failed("pause_uncertain"), None));
            }
            active["owns_pause"] = true.into();
            let requested = self.now();
            let reason = self.guard_reason(id, &after, &active, requested)?;
            let reason = if !reason.is_empty() {
                reason
            } else if rules::expired(&active, requested) {
                "consume_deadline"
            } else {
                self.positive_credit_reason(&after, requested)
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
        let mut result = rules::execute(&self.actions, action, id, key, 90).await;
        let completed = self.now();
        let diagnostics = result
            .get("diagnostics")
            .filter(|v| truthy(v))
            .cloned()
            .unwrap_or_else(|| rules::diagnostics(None, &result["data"]));
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
                self.save(id, &latest, json!({"consumed":consumed,"reset_completed_at":if consumed {json!(datetime(completed))} else {Value::Null},"error_code":if latest["stage"]=="finished" {string(&latest["error_code"])} else {receipt_code},"consumption_outcome":outcome,"receipt":diagnostics}))?;
                if latest["stage"] == "finished" {
                    self.store.transaction(|state| {
                        let meta = &mut state["scheduler"][id.to_string()];
                        let value = &mut meta["auto_reset_credit"];
                        value["outcome"] = rules::outcome(value).into();
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
            let receipt = json!({"stage":stage,"consumed":consumed,"reset_completed_at":if consumed {json!(datetime(completed))} else {Value::Null},"consumption_outcome":outcome,"error_code":receipt_code,"receipt":diagnostics,"verification_deadline_at":if consumed && source==Source::Automatic {json!(datetime(completed+Duration::seconds(300)))} else {Value::Null}});
            self.save(id, &active, receipt)?;
        }
        if result["data"].is_object() {
            result["quota_result"] =
                rules::quota_result(&result["data"], live, completed, Some(requested));
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
        if action == "query" && truthy(&result["success"]) {
            let data = &result["data"];
            let reported =
                crate::oauth_state::normalized_credits(&data["rate_limit_reset_credits"]);
            let fetched = if data["fetched_at"].is_null() {
                Some(completed)
            } else {
                parse_datetime(&data["fetched_at"])
            };
            if data["cache_persisted"] == false
                || reported.is_none()
                || reported
                    != crate::oauth_state::normalized_credits(
                        &after["extra"]["codex_reset_credit_snapshot"],
                    )
                || !fetched.is_some_and(|t| t >= at - Duration::seconds(1) && t <= completed)
            {
                result["success"] = false.into();
                result["error_code"] = "credit_snapshot_unconfirmed".into();
            }
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
                    .acquire()?
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
            let gen_val = generation(self.store.shared_snapshot()?.as_ref(), id);
            let model = string(&self.hooks.config()?["oauth_recovery_test_model_id"]).to_owned();
            let test = self
                .hooks
                .connection_test(id, model.clone(), gen_val, 30, true)
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
            let unchanged = generation(self.store.shared_snapshot()?.as_ref(), id) == gen_val;
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
                self.save(id, &task, json!({"stage":"recovered","recovered_at":datetime(self.now()),"next_at":null,"error_code":""}))?;
            }
        }
        Ok(result)
    }

    fn manual_recovered(&self, row: &Value, task: &Value) -> Result<bool> {
        Ok(row.is_object()
            && !truthy(&task["owns_pause"])
            && truthy(&task["attempt_at"])
            && self.eligible(row, self.now(), false)?
            && recovery_rules::confirmed(row)
            && self.available(row, task, self.now())?)
    }

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
        if task["stage"] == "finished" || rules::expired(task, at) {
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
        if !self.owned(row, task)? || !self.eligible(row, at, true)? {
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
        let mut task = self.save(id, task, json!({"stage":"testing","verification_dispatched":true,"test_attempts":attempts,"next_at":recovery_rules::retry_at(at,attempts)}))?;
        let state = self.store.shared_snapshot()?;
        let gen_val = generation(&state, id);
        let method = recovery_rules::method(&config, id);
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
                    .connection_test(id, model.clone(), gen_val, remaining.min(30), reserved)
                    .await?
            }
            _ => return Ok(()),
        };
        let state = self.store.shared_snapshot()?;
        if generation(&state, id) != gen_val
            || method != recovery_rules::method(&self.hooks.config()?, id)
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
            task = self.save(id, &task, json!({"stage":"retry","verification_dispatched":false,"test_attempts":attempts-1,"next_at":next,"model_test_job_id":test["job_id"],"error_code":code}))?;
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
        if generation(self.store.shared_snapshot()?.as_ref(), id) != gen_val {
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
        if !self.owned(&live, &task)? || !self.eligible(&candidate, completed, true)? {
            self.save(
                id,
                &task,
                json!({"stage":"blocked","owns_pause":false,"error_code":"ownership_changed"}),
            )?;
            return Ok(());
        }
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
        task = self.save(id, &task, json!({"stage":if truthy(&test["success"]) {json!("confirming")} else {task["stage"].clone()},"test_completed_at":datetime(completed),"test_success":truthy(&test["success"]),"verification_dispatched":false,"model_id":model,"verification_method":method}))?;
        if rules::expired(&task, completed) {
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
            if attempts >= 2 || !rules::verification_retryable(&test, method.unwrap_or("")) {
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
            self.save(id, &task, json!({"stage":"retry","error_code":if AUTH_ERRORS.contains(&code) {"auth_paused"} else {"test_failed"},"next_at":if AUTH_ERRORS.contains(&code) {Value::Null} else {json!(datetime(retry))}}))?;
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
        } else if rules::expired(&task, at) {
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
                may_enable = true;
            }
        }
        if !may_enable {
            self.save(id, &task, json!({"stage":"confirming","clear_requested_at":task["clear_requested_at"],
                "error_code":"recovery_failed","next_at":datetime(self.now()+Duration::seconds(2))}))?;
            return Ok(());
        }
        let _ = actions::set_schedulable(&self.actions, id, true, &key, 3).await;
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
                && !rules::expired(&task, at)
            {
                return self.finish_release(row, &task);
            }
        }
        self.save(
            id,
            &task,
            json!({"stage":"confirming","clear_requested_at":task["clear_requested_at"],
            "error_code":"recovery_failed","next_at":datetime(self.now()+Duration::seconds(30))}),
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
                || json!(generation(state, id)) != task["control_generation"]
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
            if !self.owned(&actual, task)? || !self.eligible(&actual, at, true)? {
                return Ok(actual);
            }
            let _ =
                actions::set_schedulable(&self.actions, id, true, &self.store.admin_token()?, 3)
                    .await;
            actual = self.row(id).await?;
        }
        if actual["schedulable"] == true
            && signature(&actual) == task["signature"]
            && json!(generation(self.store.shared_snapshot()?.as_ref(), id))
                == task["control_generation"]
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
