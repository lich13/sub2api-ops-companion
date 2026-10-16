use crate::{
    compat::{datetime, integer, operation_versions, string, truthy},
    error::{Error, Result},
    model_detection::Detection,
    model_downgrade::{is_downgrade, valid_report},
    security::safe_error,
};
use serde_json::{json, Value};

pub fn owns_pause(value: &Value) -> bool {
    value["hold"] == true
        && value["disposition"]["status"] == "completed"
        && value["disposition"]["schedule_verified"] == true
}
pub fn normal_result(job: &Value) -> bool {
    let report = &job["report"];
    let first = string(&job["first_group_prediction"]);
    let prediction = string(&report["prediction"]);
    let requested = string(&job["requested_model"]);
    // Clearing a mark requires both the original first group and the full
    // report to be normal, plus the complete group/terminal checks below.
    let groups = job["groups"].as_array();
    let used = report["used_outputs"].as_i64();
    job["automatic"] == true
        && job["degradation_recheck"] == true
        && job["status"] == "completed"
        && job["first_group_valid"] == true
        && !first.trim().is_empty()
        && !is_downgrade(requested, first)
        && valid_report(report)
        && !is_downgrade(requested, prediction)
        && used.is_some_and(|n| n >= 1)
        && !truthy(&job["error_code"])
        && !truthy(&job["can_retry"])
        && groups.is_some_and(|groups| {
            !groups.is_empty()
                && used == Some(groups.iter().filter(|g| g["status"] == "completed").count() as i64)
                && groups
                    .iter()
                    .all(|g| matches!(string(&g["status"]), "completed" | "skipped"))
        })
}
impl Detection {
    pub fn recheck_target_changed(&self, row: &Value, value: &Value, context: &Value) -> bool {
        !row.is_object()
            || value["enabled"] != true
            || row["schedulable"] != true
            || operation_versions(row, false)["model_test"] != context["recheck_account_version"]
            || row["schedulable"] != context["recheck_schedulable"]
    }
    pub async fn complete_recheck(&self, job: &Value) -> Result<Value> {
        if !normal_result(job) {
            return Ok(Value::Null);
        }
        let id = integer(&job["account_id"]).ok_or_else(Error::invalid)?;
        if self.control(id)?["recheck_recovery"]["job_id"] == job["id"] {
            return self.finish_recheck_recovery(id).await;
        }
        self.guard(job).await?;
        {
            let _control = self.actions.locks.control(id).await;
            let value = self.control(id)?;
            let mark = self.mark(id)?;
            let row = self.actions.account(id, None).await?;
            if value["generation"] != job["detection_generation"]
                || mark["version"] != job["detection_mark_version"]
                || self.recheck_target_changed(&row, &value, job)
            {
                return Err(Error::new(409, "账号或人工操作已变化，旧复查不再解除标记"));
            }
            let reason = self.reason(&row, &mark, true).await?;
            if !reason.is_empty() {
                return Err(Error::new(409, reason));
            }
            let report = &job["report"];
            self.update(
                id,
                json!({
                    "recheck_recovery": {
                        "status": "pending",
                        "job_id": job["id"],
                        "generation": value["generation"],
                        "mark_version": mark["version"],
                        "mark_cleared": false,
                        "recheck_account_version": job["recheck_account_version"],
                        "recheck_schedulable": job["recheck_schedulable"],
                        "created_at": datetime(self.now()),
                        "model_id": job["requested_model"],
                        "prediction": report["prediction"],
                        "first_group_prediction": job["first_group_prediction"],
                        "probability": report["probability"],
                        "valid_groups": report["used_outputs"],
                    },
                    "status": "handling",
                    "reason": "正在取消降智标记",
                }),
            )?;
        }
        self.finish_recheck_recovery(id).await
    }
    pub async fn finish_recheck_recovery(&self, id: i64) -> Result<Value> {
        let _control = self.actions.locks.control(id).await;
        let value = self.control(id)?;
        let mut receipt = value["recheck_recovery"].clone();
        if receipt["status"] != "pending" {
            return Ok(receipt);
        }
        let row = self.actions.account(id, None).await?;
        let state = self.capacity.store.snapshot()?["marks"][id.to_string()].clone();
        let mark = self.mark(id)?;
        let adopted = mark["marked"] == false && state["detection_job_id"] == receipt["job_id"];
        let changed = value["generation"] != receipt["generation"]
            || self.recheck_target_changed(&row, &value, &receipt)
            || mark["version"] != receipt["mark_version"] && !adopted;
        let reason = if changed {
            "账号或人工操作已变化，旧复查不再解除标记".into()
        } else if !adopted {
            self.reason(&row, &mark, true).await?
        } else {
            String::new()
        };
        if !reason.is_empty() {
            receipt["status"] = "overridden".into();
            receipt["reason"] = reason.into();
            receipt["mark_cleared"] = adopted.into();
            self.update(id, json!({"recheck_recovery":receipt}))?;
            return Ok(receipt);
        }
        if !adopted {
            self.capacity.store.set_mark(
                id,
                false,
                string(&mark["version"]),
                self.now(),
                receipt["job_id"].as_str(),
            )?;
        }
        let verified = self.mark(id)?;
        let state = self.capacity.store.snapshot()?["marks"][id.to_string()].clone();
        if verified["marked"] != false || state["detection_job_id"] != receipt["job_id"] {
            return Err(Error::new(503, "降智标记取消结果尚未确认"));
        }
        let now = datetime(self.now());
        receipt.as_object_mut().unwrap().extend(
            json!({
                "status": "completed",
                "mark_cleared": true,
                "mark_version_after": verified["version"],
                "completed_at": now,
                "reason": "检测恢复正常，已取消降智标记",
            })
            .as_object()
            .unwrap()
            .clone(),
        );

        self.store.transaction(|data| {
            let current = &mut data["accounts"][id.to_string()];
            if current["generation"] != receipt["generation"] {
                return Err(Error::new(409, "人工操作已替代复查结果"));
            }
            current["recheck_recovery"] = receipt.clone();
            if !data["disposition_notifications"].is_object() {
                data["disposition_notifications"] = json!({});
            }
            let events = data["disposition_notifications"].as_object_mut().unwrap();
            for event in events.values_mut() {
                if event["account_id"] == id
                    && event["kind"] != "recovered"
                    && matches!(string(&event["status"]), "queued" | "retry")
                {
                    event["status"] = "suppressed".into();
                    event["reason"] = "degradation_recovered".into();
                }
            }

            events
                .entry(string(&receipt["job_id"]).to_owned())
                .or_insert_with(|| {
                    json!({
                        "kind": "recovered",
                        "job_id": receipt["job_id"],
                        "account_id": id,
                        "account_name": safe_error(&row["name"], 120),
                        "account_type": row["type"],
                        "generation": receipt["generation"],
                        "mark_version": verified["version"],
                        "model_id": receipt["model_id"],
                        "prediction": receipt["prediction"],
                        "first_group_prediction": receipt["first_group_prediction"],
                        "probability": receipt["probability"],
                        "valid_groups": receipt["valid_groups"],
                        "schedulable": row["schedulable"],
                        "triggers": ["scheduled"],
                        "created_at": now,
                        "next_at": now,
                        "attempts": 0,
                        "status": "queued",
                    })
                });
            Ok(())
        })?;

        self.audit(
            "model_detection_recovered",
            json!({
                "account_id":id, "job_id":receipt["job_id"], "mark_cleared":true,
                "schedulable":row["schedulable"]
            }),
        );
        Ok(receipt)
    }
}
