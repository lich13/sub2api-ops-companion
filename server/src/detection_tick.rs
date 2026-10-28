use crate::{
    compat::{datetime, integer, parse_datetime, pick, string, truthy},
    degradation_rechecks,
    error::{Error, Result},
    model_detection::{Detection, TARGET},
    model_downgrade::{bank_contains, is_downgrade, valid_report},
    model_tests::ACTIVE,
    security::safe_error,
};
use chrono::Duration;
use serde_json::{json, Value};

impl Detection {
    pub async fn tick(&self) -> Result<()> {
        if self.actions.settings.read_only {
            return Ok(());
        }
        let Ok(_guard) = self.tick_lock.try_lock() else {
            return Ok(());
        };
        self.refresh_call_frequency().await?;
        let alerts = self.capacity.store.shared_snapshot()?;
        let mut acknowledged = std::collections::BTreeSet::new();
        if alerts["detection_events"]
            .as_object()
            .is_some_and(|events| !events.is_empty())
        {
            let tests = self.tests.shared_data()?;
            for job in tests["jobs"]
                .as_object()
                .unwrap()
                .values()
                .filter(|job| job["automatic"] == true)
            {
                if let Some(id) = integer(&job["account_id"]) {
                    for key in job["triggers"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        acknowledged.insert((id, key.to_owned()));
                    }
                }
            }
        }
        for (key, event) in alerts["detection_events"].as_object().into_iter().flatten() {
            if self.store.read_shared()?["consumed"].get(key).is_none() {
                let id = integer(&event["account_id"]).ok_or_else(Error::invalid)?;
                // A persisted job is also the receipt if the process stopped
                // between enqueueing it and acknowledging this clue.
                let existing = acknowledged.contains(&(id, key.clone()));
                if !existing {
                    if let Err(error) = self.trigger(id, std::slice::from_ref(key)).await {
                        if error.status.as_u16() >= 500 {
                            return Err(error);
                        }
                        let reason = safe_error(&error.detail, 200);
                        self.update(id, json!({"status":"paused","reason":reason}))?;
                        self.clue_status(std::slice::from_ref(key), "failed", &reason, None)?;
                    }
                    acknowledged.insert((id, key.clone()));
                }
                self.store.transaction(|data| {
                    data["consumed"][key] = datetime(self.now()).into();
                    while data["consumed"].as_object().unwrap().len() > 10000 {
                        let first = data["consumed"]
                            .as_object()
                            .unwrap()
                            .keys()
                            .next()
                            .unwrap()
                            .clone();
                        data["consumed"]
                            .as_object_mut()
                            .unwrap()
                            .shift_remove(&first);
                    }
                    Ok(())
                })?;
            }
            self.capacity.store.transaction(|data| {
                data["detection_events"]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove(key);
                Ok(())
            })?;
        }
        let ids = self.store.read_shared()?["accounts"]
            .as_object()
            .unwrap()
            .keys()
            .map(|key| {
                key.parse::<i64>()
                    .map_err(|_| Error::new(503, "模型检测账号编号无效"))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut waiting = Vec::new();
        for id in ids {
            let result = self.tick_account(id).await;
            if let Ok(Some((value, mark, recheck))) = &result {
                waiting.push((id, value.clone(), mark.clone(), *recheck));
            }
            if let Err(error) = result {
                if error.status.as_u16() >= 500 {
                    return Err(error);
                }
                self.update(id,json!({"status":"paused","reason":safe_error(&error.detail,200),"next_at":null}))?;
            }
        }
        if !waiting.is_empty() {
            let now = self.now();
            let ptrs = self.waiting_state_ptrs()?;
            let full_sweep = self
                .waiting_sweep_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none_or(|(prev_ptrs, prev_at)| {
                    prev_ptrs != ptrs || now >= prev_at + Duration::seconds(10) || now < prev_at
                });
            if !full_sweep {
                let mut due_waiting = Vec::new();
                for item in waiting {
                    if self.waiting_needs_immediate_eval(item.0, &item.1, now)? {
                        due_waiting.push(item);
                    }
                }
                waiting = due_waiting;
            }
            if !waiting.is_empty() {
                let ids = waiting.iter().map(|(id, ..)| *id).collect::<Vec<_>>();
                let rows = self
                    .actions
                    .accounts(&ids)
                    .await?
                    .into_iter()
                    .filter_map(|row| integer(&row["id"]).map(|id| (id, row)))
                    .collect::<std::collections::BTreeMap<_, _>>();
                for (id, value, mark, recheck) in waiting {
                    // A manual control change during the batch read wins this tick.
                    if self.control(id)? != value {
                        continue;
                    }
                    let result = match rows.get(&id) {
                        Some(row) => self.tick_waiting(id, &value, &mark, recheck, row).await,
                        None => Err(Error::new(404, "账号不存在")),
                    };
                    if let Err(error) = result {
                        if error.status.as_u16() >= 500 {
                            return Err(error);
                        }
                        self.update(id,json!({"status":"paused","reason":safe_error(&error.detail,200),"next_at":null}))?;
                    }
                }
            }
            if full_sweep {
                let post_ptrs = self.waiting_state_ptrs()?;
                *self
                    .waiting_sweep_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some((post_ptrs, now));
            }
        }
        Ok(())
    }
    fn waiting_state_ptrs(&self) -> Result<(u64, u64, u64, u64)> {
        Ok((
            self.store.cache_revision()?,
            self.monitor.config.oauth.cache_revision()?,
            self.capacity.store.0.cache_revision()?,
            self.monitor.store.0.cache_revision()?,
        ))
    }
    fn waiting_needs_immediate_eval(
        &self,
        id: i64,
        value: &Value,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        if truthy(&value["candidate_blocked_due"])
            || (value["candidate_blocked"] == true
                && parse_datetime(&value["candidate_recheck_at"]).is_none_or(|at| now >= at))
        {
            return Ok(true);
        }
        if crate::detection_schedule::smart(value) {
            let interval = crate::detection_schedule::effective_interval(value);
            if value["schedule_idle_paused"] == true && interval.is_some() {
                return Ok(true);
            }
            let due = self.smart_due(id, value)?;
            if due.is_some_and(|d| now >= d) {
                return Ok(true);
            }
            let expected_status = if due.is_some() { "waiting" } else { "paused" };
            Ok(value["schedule_anchor_at"].is_null()
                || value["status"] != expected_status
                || (due.is_some() && value["next_at"].is_null()))
        } else {
            let due = parse_datetime(&value["next_at"]);
            Ok(due.is_none_or(|d| now >= d) || value["status"] != "waiting")
        }
    }
    async fn tick_account(&self, id: i64) -> Result<Option<(Value, Value, bool)>> {
        let mut value = self.control(id)?;
        if matches!(
            string(&value["disposition"]["status"]),
            "pending" | "marked" | "writing" | "checking"
        ) {
            self.finish_disposition(id).await?;
            value = self.control(id)?;
        }
        if value["recheck_recovery"]["status"] == "pending" {
            self.finish_recheck_recovery(id).await?;
            value = self.control(id)?;
        }
        let mut job = Value::Null;
        if let Some(job_id) = value["job_id"].as_str().filter(|v| !v.is_empty()) {
            match self.tests.get(job_id) {
                Ok(found) => job = found,
                Err(error) if error.status.as_u16() == 404 => {
                    self.update(id,json!({"job_id":null,"status":if value["enabled"]==true {"waiting"}else {"disabled"},"reason":""}))?;
                    value = self.control(id)?;
                }
                Err(error) => return Err(error),
            }
        }
        if ACTIVE.contains(&string(&job["status"])) {
            if value["disposition"]["job_id"] == job["id"] {
                return Ok(None);
            }
            if self.guard(&job).await.is_err() {
                self.tests.cancel(string(&job["id"])).await?;
                return Ok(None);
            }
            if value["status"] != job["status"] {
                self.store.transaction(|data| {
                    let current = &mut data["accounts"][id.to_string()];
                    if current["generation"] == job["detection_generation"] {
                        current["status"] = job["status"].clone();
                    }
                    Ok(())
                })?;
            }
            return Ok(None);
        }
        if truthy(&job)
            && (value["handled_job"] != job["id"]
                || job["degradation_recheck"] == true
                    && value["handled_completed_at"] != job["completed_at"])
        {
            let recheck_reason = match self.complete_recheck(&job).await {
                Ok(receipt) => string(&receipt["reason"]).to_owned(),
                Err(error) if error.status.as_u16() < 500 => safe_error(&error.detail, 200),
                Err(error) => return Err(error),
            };
            value = self.control(id)?;
            let report = &job["report"];
            let valid = job["status"] == "completed" && valid_report(report);
            // Historical aggregate summaries alone must never initiate disposition.
            let confirmed = if crate::lm_verdict::is_lm(&job) {
                job["automatic_disposition"].is_object()
                    && crate::lm_verdict::classify(&job)["outcome"] == "degraded"
            } else {
                valid
                    && (job["first_group_valid"] == true
                        && is_downgrade(
                            string(&job["requested_model"]),
                            string(&job["first_group_prediction"]),
                        )
                        || job["automatic_disposition"]["basis"] == "aggregate"
                            && is_downgrade(
                                string(&job["requested_model"]),
                                string(&report["prediction"]),
                            ))
            };
            let triggers = job["triggers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| string(v).to_owned())
                .collect::<Vec<_>>();
            self.clue_status(
                &triggers,
                if confirmed {
                    "confirmed"
                } else if valid {
                    "ignored"
                } else {
                    "failed"
                },
                if confirmed {
                    if job["automatic_disposition"]["basis"] == "sample_family" {
                        "跨家族采样确认降智"
                    } else if job["automatic_disposition"]["basis"] == "aggregate" {
                        "最终汇总确认降智"
                    } else {
                        "首组指纹确认降智"
                    }
                } else if valid {
                    "未确认降智"
                } else {
                    job["error"]
                        .as_str()
                        .filter(|v| !v.is_empty())
                        .unwrap_or("分析不足")
                },
                job["id"].as_str(),
            )?;
            let mut changes = json!({"handled_job":job["id"],"handled_completed_at":job["completed_at"],"last_result":pick(&job,&["id","status","report","completed_at","error","bank_version","engine","planned_groups","classification"])});
            changes["last_result"]["disposition"] = job["automatic_disposition"].clone();
            if value["generation"] == job["detection_generation"] {
                let ended = parse_datetime(&job["completed_at"]).unwrap_or_else(|| self.now());
                if truthy(&job["started_at"]) {
                    changes["schedule_anchor_at"] = datetime(ended).into();
                }
                let next = (ended.max(self.now())
                    + Duration::minutes(integer(&value["interval_minutes"]).unwrap_or(15)))
                .max(parse_datetime(&self.next_allowed_at(id)?).unwrap_or(ended));
                changes["status"] = if value["enabled"] == true {
                    "waiting"
                } else {
                    "disabled"
                }
                .into();
                changes["next_at"] = if value["enabled"] == true {
                    json!(datetime(next))
                } else {
                    Value::Null
                };
                if job["degradation_recheck"] == true {
                    changes["reason"] = if !recheck_reason.is_empty() {
                        recheck_reason
                    } else if confirmed {
                        "仍符合降智特征".into()
                    } else {
                        job["error"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            .unwrap_or("复查分析不足，保留降智标记")
                            .into()
                    }
                    .into();
                }
            }
            self.update(id, changes)?;
            value = self.control(id)?;
        }
        let mark = self.mark(id)?;
        let recheck = value["enabled"] == true && mark["marked"] == true;
        if value["hold"] == true && !(recheck && degradation_rechecks::owns_pause(&value)) {
            let d = &value["disposition"];
            let reason = if d["schedule_verified"] == true {
                if mark["marked"] == true {
                    "已标记降智，停调度已确认"
                } else {
                    "调度保持关闭"
                }
            } else {
                d["reason"].as_str().unwrap_or("自动处置待核对")
            };
            if value["status"] != "paused" || truthy(&value["next_at"]) || value["reason"] != reason
            {
                self.update(
                    id,
                    json!({"status":"paused","next_at":null,"reason":reason}),
                )?;
            }
            return Ok(None);
        }
        if value["enabled"] != true {
            return Ok(None);
        }
        if crate::detection_schedule::smart(&value)
            && crate::detection_schedule::effective_interval(&value).is_none()
        {
            self.update_smart_schedule(id, &value)?;
            return Ok(None);
        }
        Ok(Some((value, mark, recheck)))
    }
    async fn tick_waiting(
        &self,
        id: i64,
        value: &Value,
        mark: &Value,
        recheck: bool,
        row: &Value,
    ) -> Result<()> {
        let reason = self.reason(row, mark, recheck).await?;
        if !reason.is_empty() {
            if value["status"] != "paused" || value["reason"] != reason {
                self.update(
                    id,
                    json!({"status":"paused","reason":reason,"next_at":null}),
                )?;
            }
            return Ok(());
        }
        if value["candidate_blocked"] == true {
            if parse_datetime(&value["candidate_recheck_at"]).is_some_and(|at| self.now() < at) {
                return Ok(());
            }
            let allowed = self
                .actions
                .model_allowed(row, &self.effective_model(value, row)?)
                .await?;
            let platform = self.monitor.config.current()?["model_detection_platforms"]
                [string(&row["platform"])]
            .clone();
            let ready = if platform["engine"] == crate::lm_detector::ENGINE {
                self.tests
                    .lm_bank
                    .load(self.tests.lm_bank.revision()?)
                    .await
                    .is_ok()
            } else {
                bank_contains(&self.tests.bank.snapshot().0, TARGET)
            };
            if !allowed || !ready {
                self.update(
                    id,
                    json!({"candidate_recheck_at":datetime(self.now()+Duration::minutes(1))}),
                )?;
                return Ok(());
            }
            self.update(
                id,
                json!({"candidate_blocked":false,"candidate_recheck_at":null}),
            )?;
        }
        if crate::detection_schedule::smart(value) {
            if self
                .update_smart_schedule(id, value)?
                .is_some_and(|due| self.now() >= due)
            {
                self.trigger(id, &["scheduled".into()]).await?;
            }
        } else if let Some(due) = parse_datetime(&value["next_at"]) {
            if self.now() >= due {
                self.trigger(id, &["scheduled".into()]).await?;
            }
        } else {
            let due =
                self.now() + Duration::minutes(integer(&value["interval_minutes"]).unwrap_or(15));
            let allowed = parse_datetime(&self.next_allowed_at(id)?).unwrap_or(due);
            self.update(
                id,
                json!({"status":"waiting","reason":"","next_at":datetime(due.max(allowed))}),
            )?;
        }
        Ok(())
    }
}
