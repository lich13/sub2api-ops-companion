use crate::{
    compat::{datetime, integer, parse_datetime, string, truthy},
    credit::CreditController,
    credit_ownership, credit_rules,
    error::{Error, Result},
    oauth_queries::Source,
    recovery_policy, recovery_rules,
};
use chrono::{DateTime, Duration, Utc};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};
impl CreditController {
    pub async fn sweep(
        &self,
        rows: &[impl std::borrow::Borrow<Value>],
        at: DateTime<Utc>,
    ) -> Result<()> {
        if self.actions.settings.read_only {
            return Ok(());
        }
        let mut live = rows
            .iter()
            .map(|row| row.borrow())
            .filter_map(|row| integer(&row["id"]).map(|id| (id, Cow::Borrowed(row))))
            .collect::<BTreeMap<_, _>>();
        let state = self.store.shared_snapshot()?;
        let mut checked = Vec::new();
        let result = async {
            for (key, meta) in state["scheduler"].as_object().into_iter().flatten() {
                let id = key.parse::<i64>().map_err(|_| Error::internal())?;
                let mut task = meta["auto_reset_credit"]
                    .as_object()
                    .cloned()
                    .map(Value::Object)
                    .unwrap_or(json!({}));
                if truthy(&task)
                    && !truthy(&task["attempt_at"])
                    && !truthy(&task["credit_admitted_at"])
                {
                    let row = if let Some(row) = live.get(&id) {
                        row.clone()
                    } else {
                        Cow::Owned(self.row(id).await?)
                    };
                    task = self.migrate_admission(id, &row, &task, at)?;
                }
                if !truthy(&task)
                    || credit_rules::manual(&task)
                    || credit_rules::LEGACY_ENDED.contains(&string(&task["stage"]))
                        && !truthy(&task["policy_version"])
                    || matches!(string(&task["stage"]), "finished" | "recovered" | "closed")
                {
                    continue;
                }
                if let std::collections::btree_map::Entry::Vacant(e) = live.entry(id) {
                    e.insert(Cow::Owned(self.row(id).await?));
                }
                let row = &live[&id];
                task = self.policy(id, &task, at).await?;
                let current = credit_rules::policy_now(&task, at);
                let control = &meta["manual_control"];
                let expected = integer(&task["policy_control_generation"]).unwrap_or(0);
                let generation = integer(&control["generation"]).unwrap_or(0);
                let finishing = truthy(&task["degradation_pending"])
                    && (generation == expected
                        || control["origin"] == "model_detection" && generation == expected + 1);
                let mut reason = if finishing {
                    ""
                } else {
                    self.guard_reason(id, row, &task, current)?
                };
                if reason.is_empty() && credit_rules::expired(&task, current) {
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
                        .finish(id, &task, reason, current, row, true, &Value::Null)
                        .await
                    {
                        if error.status.as_u16() != 409 {
                            return Err(error);
                        }
                    }
                } else if task["policy_checked_at"] != datetime(current) {
                    checked.push((key.clone(), task, current));
                }
            }
            Ok(())
        }
        .await;
        if !checked.is_empty() {
            self.store.transaction(|state| {
                for (key, expected, at) in &checked {
                    let meta = &mut state["scheduler"][key];
                    if integer(&meta["manual_control"]["generation"]).unwrap_or(0)
                        != integer(&expected["policy_control_generation"]).unwrap_or(0)
                        || integer(&meta["recovery_method_generation"]).unwrap_or(0)
                            != integer(&expected["policy_method_generation"]).unwrap_or(0)
                    {
                        continue;
                    }
                    let value = &mut meta["auto_reset_credit"];
                    if value["episode"] == expected["episode"]
                        && value["revision"] == expected["revision"]
                        && parse_datetime(&value["policy_checked_at"])
                            .is_none_or(|previous| previous < *at)
                    {
                        value["policy_checked_at"] = datetime(*at).into();
                    }
                }
                Ok(())
            })?;
        }
        result
    }
    pub async fn run(&self, rows: &[Value], at: DateTime<Utc>) -> Result<()> {
        if self.actions.settings.read_only {
            return Ok(());
        }
        let config = self.hooks.config()?;
        let enabled = truthy(&config["oauth_auto_reset_credit_enabled"]);
        if !enabled
            && !self.store.shared_snapshot()?["scheduler"]
                .as_object()
                .into_iter()
                .flatten()
                .any(|(_, meta)| truthy(&meta["auto_reset_credit"]))
        {
            return Ok(());
        }
        self.sweep(rows, at).await?;
        if rows.is_empty() {
            return Ok(());
        }
        let snapshot = self.store.shared_snapshot()?;
        let mut active_set = BTreeSet::new();
        for (key, meta) in snapshot["scheduler"].as_object().into_iter().flatten() {
            let Ok(id) = key.parse::<i64>() else {
                continue;
            };
            let task = &meta["auto_reset_credit"];
            let task_needs_step = if !truthy(task) || (!enabled && !truthy(&task["policy_version"]))
            {
                false
            } else {
                let stage = string(&task["stage"]);
                let owns_unissued = truthy(&task["owns_pause"]) && !truthy(&task["attempt_at"]);
                !((matches!(stage, "recovered" | "closed") && !truthy(&task["owns_pause"]))
                    || (stage == "finished"
                        && truthy(&task["recovered_evidence_at"])
                        && !owns_unissued)
                    || (credit_rules::LEGACY_ENDED.contains(&stage)
                        && !truthy(&task["policy_version"])
                        && stage != "manual"))
            };
            let admission = &meta["credit_admission"];
            let admission_unrecovered =
                enabled && truthy(admission) && !truthy(&admission["recovered_at"]);
            if task_needs_step || admission_unrecovered {
                active_set.insert(id);
            }
        }
        if !enabled && active_set.is_empty() {
            return Ok(());
        }
        let candidate_ids: Vec<i64> = rows
            .iter()
            .filter_map(|row| integer(&row["id"]))
            .filter(|id| enabled || active_set.contains(id))
            .collect();
        if candidate_ids.is_empty() {
            return Ok(());
        }
        let active_ids: Vec<i64> = active_set.into_iter().collect();
        let matched: BTreeSet<i64> = self
            .actions
            .queries
            .db
            .all(
                "SELECT id FROM accounts WHERE deleted_at IS NULL AND id = ANY($1) AND (id = ANY($2) OR rate_limit_reset_at > $3)",
                &[
                    crate::db::Param::Ints(candidate_ids),
                    crate::db::Param::Ints(active_ids),
                    at.into(),
                ],
            )
            .await?
            .into_iter()
            .filter_map(|row| integer(&row["id"]))
            .collect();
        if matched.is_empty() {
            return Ok(());
        }
        let results = stream::iter(
            rows.iter()
                .filter(|row| integer(&row["id"]).is_some_and(|id| matched.contains(&id)))
                .map(|row| self.run_account(row, at)),
        )
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;
        for result in results {
            result?;
        }
        Ok(())
    }
    pub async fn run_account(&self, item: &Value, at: DateTime<Utc>) -> Result<()> {
        if self.actions.settings.read_only {
            return Ok(());
        }
        let id = integer(&item["id"]).ok_or_else(Error::invalid)?;
        let enabled = self.hooks.config()?["oauth_auto_reset_credit_enabled"] == true;
        if !enabled && !truthy(&self.state(id)?["policy_version"]) {
            return Ok(());
        }
        let _disk = match self.disk_lease(id) {
            Ok(lease) => lease,
            Err(e) if e.status.as_u16() == 409 => return Ok(()),
            Err(e) => return Err(e),
        };
        let live = self.row(id).await?;
        let row = if live.is_object() { &live } else { item };
        if !self.candidate_ready(row, at).await? {
            return Ok(());
        }
        let Ok(_budget) = self.budget.clone().try_acquire_owned() else {
            return Ok(());
        };
        let _lease = match self.actions.locks.try_account(row) {
            Ok(v) => v,
            Err(e) if e.status.as_u16() == 409 => return Ok(()),
            Err(e) => return Err(e),
        };
        if let Err(error) = self.bounded_step(id, at, enabled).await {
            self.audit(id,"paused",json!({"error_code":if error.status.as_u16()==500 || error.status.as_u16()==503 {"query_state_unavailable"} else {"state_check_failed"}}));
            if error.status.as_u16() != 409 {
                return Err(error);
            }
        }
        Ok(())
    }
    pub async fn bounded_step(&self, id: i64, at: DateTime<Utc>, enabled: bool) -> Result<()> {
        let mut row = self.row(id).await?;
        let mut task = self.state(id)?;
        if row.is_object() {
            task = credit_ownership::reconcile(&self.store, &row)?;
        }
        task = self.migrate_admission(id, &row, &task, at)?;
        self.observe_recovery(&row, at)?;
        if truthy(&task["credit_gate_skipped"]) && !truthy(&task["owns_pause"]) && enabled {
            let evidence = self.depletion(&row, at, false).await?;
            if truthy(&evidence) {
                task = self.begin_episode(id, &task, &evidence).await?;
                if !truthy(&task) {
                    return Ok(());
                }
            }
        }
        if credit_rules::manual(&task) && !matches!(string(&task["stage"]), "recovered" | "closed")
            || task["stage"] == "manual" && !truthy(&task["policy_version"])
        {
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
            let evidence = if row.is_object() && enabled && !truthy(&task["attempt_at"]) {
                self.depletion(&row, at, false).await?
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
            task = self.begin_episode(id, &task, &evidence).await?;
            if !truthy(&task) {
                return Ok(());
            }
        }
        if task["stage"] == "finished" {
            if truthy(&task["owns_pause"]) && !truthy(&task["attempt_at"]) {
                self.undo_unissued_pause(id, &row, &task, at).await?;
                task = self.state(id)?;
                row = self.row(id).await?;
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
                self.depletion(&row, at, false).await?
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
            task = self.begin_episode(id, &task, &evidence).await?;
            if !truthy(&task) {
                return Ok(());
            }
        }
        if !truthy(&task) || matches!(string(&task["stage"]), "recovered" | "closed") {
            let evidence = if row.is_object() && enabled {
                self.depletion(&row, at, false).await?
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
            task = self.begin_episode(id, &task, &evidence).await?;
            if !truthy(&task) {
                return Ok(());
            }
        }
        if credit_rules::LEGACY_ENDED.contains(&string(&task["stage"]))
            && !truthy(&task["policy_version"])
        {
            return Ok(());
        }
        task = self.policy(id, &task, at).await?;
        let at = credit_rules::policy_now(&task, at);
        let reason = self.guard_reason(id, &row, &task, at)?;
        if !reason.is_empty() {
            self.finish(id, &task, reason, at, &row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        if credit_rules::expired(&task, at) {
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
        if task["stage"] == "releasing" {
            return self.reconcile_release(id, &task, at).await;
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
                if self.available(&row, &task, at)? {
                    self.release(&row, &task, at).await?;
                } else {
                    self.finish(id, &task, "incomplete_quota", at, &row, true, &Value::Null)
                        .await?;
                }
                return Ok(());
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
        // An interrupted pause has no durable ownership receipt. A now-disabled
        // account is not evidence that the original block cleared, and must not
        // authorize consumption or an attempt to restore scheduling.
        if task["stage"] == "pausing" {
            self.finish(id, &task, "pause_uncertain", at, &row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        let evidence = self
            .depletion(&row, at, truthy(&task["owns_pause"]))
            .await?;
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
                .unwrap()
                .iter()
                .filter(|(key, _)| key.as_str() != "evidence_at")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<serde_json::Map<_, _>>();
            task = self.save(id, &task, changes.into())?;
        }
        if parse_datetime(&task["next_at"]).is_some_and(|due| due > at) {
            return Ok(());
        }
        let reason = self.positive_credit_reason(&row, &evidence, at)?;
        if !reason.is_empty() {
            self.finish(id, &task, reason, at, &row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        let at = credit_rules::policy_now(&task, self.now());
        if credit_rules::expired(&task, at) {
            self.finish(id, &task, "consume_deadline", at, &row, true, &Value::Null)
                .await?;
            return Ok(());
        }
        if recovery_policy::method(&self.hooks.config()?, id) == Some("model") {
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
        let Some(_slot) = self.slots.acquire() else {
            self.bounded_wait(
                id,
                &task,
                &json!({"error_code":"test_slots","next_at":datetime(at+Duration::seconds(2))}),
                at,
            )
            .await?;
            return Ok(());
        };
        let result = self
            .request(&row, "reset", &task, at, Source::Automatic, None)
            .await?;
        task = self.state(id)?;
        if task["stage"] == "finished" {
            return Ok(());
        }
        if truthy(&task["consumed"]) && !truthy(&result["uncertain"]) {
            self.test(&self.row(id).await?, &task, self.now(), true)
                .await?;
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
}
