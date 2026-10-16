use crate::{
    compat::{datetime, digest, integer, operation_versions, parse_datetime, string, truthy},
    credit_rules,
    error::{Error, Result},
    model_detection::{Detection, TARGET},
    model_downgrade::{bank_contains, is_downgrade, valid_report},
    model_tests::ACTIVE,
    oauth_queries::Source,
    oauth_state::block_signature,
    quota, recovery_policy, recovery_rules,
};
use chrono::Duration;
use serde_json::{json, Value};

impl Detection {
    pub fn recovery_wait(
        &self,
        reason: &str,
        until: Option<Value>,
        job: Option<&str>,
        waiting: &str,
    ) -> Value {
        json!({
            "success": false,
            "deferred": true,
            "error_code": "model_verification_pending",
            "error": reason,
            "next_at": until.filter(truthy).unwrap_or(json!(datetime(self.now()+Duration::seconds(5)))),
            "job_id": job,
            "waiting_reason": waiting,
        })
    }
    pub async fn recovery_reason(
        &self,
        row: &Value,
        context: &Value,
        before_reset: bool,
    ) -> Result<String> {
        let id = integer(&context["account_id"]).ok_or_else(Error::invalid)?;
        let now = self.now();
        if !row.is_object()
            || row["platform"] != "openai"
            || row["type"] != "oauth"
            || truthy(&row["deleted_at"])
        {
            return Ok("账号不符合恢复验证条件".into());
        }
        let config = self.monitor.config.current()?;
        if recovery_policy::method(&config, id) != Some("model")
            || config["oauth_recovery_monitor_enabled"] == false
        {
            return Ok("恢复验证设置已变化".into());
        }
        let state = self.monitor.state()?;
        let meta = &state["scheduler"][id.to_string()];
        if json!(self.monitor.generation(id)?) != context["control_generation"]
            || json!(integer(&meta["recovery_method_generation"]).unwrap_or(0))
                != context["method_generation"]
        {
            return Ok("人工操作或恢复方式已变化".into());
        }
        if self.held(id) || self.mark(id)?["marked"] == true {
            return Ok("账号已标记降智".into());
        }
        if context["kind"] == "credit" {
            let controller = &self.monitor.credits;
            let task = controller.state(id)?;
            let row = controller.row(id).await?;
            if task["stage"] == "finished"
                || truthy(&task["policy_version"]) && credit_rules::expired(&task, now)
            {
                return Ok("用卡执行期限已结束".into());
            }
            if task["episode"] != context["fingerprint"] {
                return Ok("用卡任务已变化".into());
            }
            if before_reset && !truthy(&task["attempt_at"]) {
                if config["oauth_auto_reset_credit_enabled"] != true
                    || !truthy(&controller.depletion(&row, now, false).await?)
                    || matches!(string(&task["stage"]), "manual" | "closed" | "blocked")
                {
                    return Ok("账号不再符合自动用卡条件".into());
                }
            } else if !controller.owned(&row, &task, false)?
                || !controller.processing_eligible(&row, &task, now)?
                || !controller.available(&row, &task, now)?
            {
                return Ok("用卡后的额度或暂停归属未确认".into());
            }
        } else {
            let intent = &meta["recovery_intent"];
            let result = quota::latest_result(row, &state["oauth_results"][id.to_string()], now);
            if intent["fingerprint"] != context["fingerprint"] {
                return Ok("恢复事件已变化".into());
            }
            let windows = intent["window_keys"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| string(v).to_owned())
                .collect::<Vec<_>>();
            if !recovery_rules::eligible(row, &windows) {
                return Ok("账号状态不允许自动恢复".into());
            }
            if block_signature(row) != context["block_signature"] {
                return Ok("账号阻断状态已变化".into());
            }
            if !recovery_rules::control_allows(row, &result, meta, now)
                || !recovery_rules::quota_ready(row, &result, meta, now)
            {
                return Ok("恢复所需的新鲜额度证据不足".into());
            }
        }
        Ok(String::new())
    }
    pub async fn recovery_job(
        &self,
        id: i64,
        kind: &str,
        fingerprint: &str,
        attempt: i64,
        consume: bool,
    ) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        let value = self.control(id)?;
        let state = self.monitor.state()?;
        let meta = &state["scheduler"][id.to_string()];
        let mut context = json!({
            "account_id": id,
            "kind": kind,
            "fingerprint": fingerprint,
            "control_generation": self.monitor.generation(id)?,
            "method_generation": integer(&meta["recovery_method_generation"]).unwrap_or(0),
            "attempt": attempt,
            "consume": consume,
        });

        let mut identity = context.clone();
        identity.as_object_mut().unwrap().shift_remove("consume");
        let request = digest(&json!([
            "recovery",
            identity,
            value["generation"],
            value["model_id"]
        ]))[..32]
            .to_owned();
        let jobs = self.tests.data()?;
        if let Some(previous) = jobs["requests"].get(&request).and_then(|v| v.as_str()) {
            let job = self.tests.get(previous)?;
            if ACTIVE.contains(&string(&job["status"])) {
                return Ok(self.recovery_wait(
                    if job["status"] == "queued" {
                        "模型验证排队中"
                    } else {
                        "模型验证中"
                    },
                    job.get("waiting_until").cloned(),
                    job["id"].as_str(),
                    "model_verification_pending",
                ));
            }
            if truthy(&job["preflight_result"]) {
                let result = &job["preflight_result"];
                let task = if kind == "credit" {
                    self.monitor.credits.state(id)?
                } else {
                    json!({})
                };
                if result["deferred"] == true
                    && parse_datetime(&result["next_at"]).is_some_and(|due| self.now() >= due)
                    && !truthy(&task["attempt_at"])
                    && integer(&job["attempts"]).unwrap_or(0) == 0
                {
                    self.tests.transaction(|data| {
                        if data["requests"][&request] == previous {
                            data["requests"]
                                .as_object_mut()
                                .unwrap()
                                .shift_remove(&request);
                        }
                        Ok(())
                    })?;
                    return Ok(self.recovery_wait(
                        "重新核对用卡条件",
                        None,
                        None,
                        "model_verification_pending",
                    ));
                }
                return Ok(result.clone());
            }
            let report = &job["report"];
            let valid = job["status"] == "completed" && valid_report(report);
            let degraded = is_downgrade(
                string(&job["requested_model"]),
                string(&report["prediction"]),
            );
            if valid && !degraded {
                if let Err(error) = self.guard(&job).await {
                    return Ok(json!({
                        "success":false, "error_code":"recovery_state_changed",
                        "error":crate::security::safe_error(&error.detail,200)
                    }));
                }
            }
            let retryable = job["status"] == "failed" && job["can_retry"] == true;
            let mut code = job["error_code"]
                .as_str()
                .filter(|v| !v.is_empty())
                .unwrap_or(if retryable {
                    "model_request_failed"
                } else {
                    "model_analysis_insufficient"
                })
                .to_owned();
            if code == "auth_or_quota" {
                let status = job["groups"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find_map(|g| {
                        integer(&g["diagnostics"]["http_status"])
                            .filter(|v| matches!(*v, 401 | 402))
                    })
                    .unwrap_or(401);
                code = format!("http_{status}");
            }
            let result_code = if degraded {
                "detection_held"
            } else if valid {
                ""
            } else {
                &code
            };
            let default_error = if degraded {
                "账号已判定降智"
            } else if valid {
                ""
            } else {
                "模型验证未获得有效样本"
            };
            return Ok(json!({
                "success": valid && !degraded,
                "error_code": result_code,
                "error": job["error"].as_str().filter(|v| !v.is_empty()).unwrap_or(default_error),
                "model_id": job["requested_model"],
                "completed_at": job["completed_at"],
                "job_id": job["id"],
                "retryable": retryable,
                "uncertain": job["uncertain"] == true || job["status"] == "interrupted",
            }));
        }
        let allowed = self.next_allowed_at(id)?;
        if parse_datetime(&allowed).is_some_and(|due| self.now() < due) {
            return Ok(self.recovery_wait(
                "等待自动检测冷却",
                Some(allowed),
                None,
                "model_cooldown",
            ));
        }
        if ACTIVE.contains(&string(&self.tests.latest(id)?["status"])) {
            return Ok(self.recovery_wait("等待当前模型测试结束", None, None, "model_busy"));
        }
        let row = self.actions.account(id, None).await?;
        context["block_signature"] = block_signature(&row).into();
        let reason = self.recovery_reason(&row, &context, consume).await?;
        if !reason.is_empty() {
            return Ok(json!({"success":false,"error":reason,"error_code":"recovery_ineligible"}));
        }
        if !self
            .actions
            .model_allowed(&row, string(&value["model_id"]), true)
            .await?
        {
            return Ok(
                json!({"success":false,"error":"所选模型不在当前分组白名单中","error_code":"model_unavailable"}),
            );
        }
        let bank = self.tests.bank.snapshot();
        if !bank_contains(&bank.0, TARGET) {
            return Ok(
                json!({"success":false,"error":"指纹库缺少降智判定模型","error_code":"bank_unavailable"}),
            );
        }
        let mark = self.mark(id)?;
        let job = self
            .tests
            .start(
                id,
                &json!({
                    "model_id": value["model_id"],
                    "expected_version": "0".repeat(64),
                    "expected_operation_version": operation_versions(&row, false)["model_test"],
                    "request_id": request,
                    "concurrency": 1,
                }),
                Some(&json!({
                    "detection_generation": value["generation"],
                    "detection_mark_version": mark["version"],
                    "triggers": [format!("{kind}:{fingerprint}")],
                    "recovery_context": context,
                    "planned_groups": 1,
                })),
            )
            .await?;

        self.update(
            id,
            json!({"job_id":job["id"],"triggers":job["triggers"],"status":job["status"]}),
        )?;
        Ok(self.recovery_wait(
            "模型验证排队中",
            None,
            job["id"].as_str(),
            "model_verification_pending",
        ))
    }
    pub async fn prepare_recovery(&self, job: &Value) -> Result<Value> {
        let context = &job["recovery_context"];
        if !truthy(context) || context["consume"] != true {
            return Ok(Value::Null);
        }
        let id = integer(&job["account_id"]).ok_or_else(Error::invalid)?;
        let controller = &self.monitor.credits;
        let row = controller.row(id).await?;
        let task = controller.state(id)?;
        if truthy(&task["attempt_at"]) {
            return Ok(if task["consumed"] == true && task["stage"] == "testing" {
                Value::Null
            } else {
                json!({"success":false,"error_code":"reset_uncertain","error":"重置结果待确认"})
            });
        }
        let reason = self.recovery_reason(&row, context, true).await?;
        if !reason.is_empty() {
            return Ok(json!({"success":false,"error_code":"recovery_ineligible","error":reason}));
        }
        let task = controller.save(id, &task, json!({"model_test_job_id":job["id"]}))?;
        let result = controller
            .request(&row, "reset", &task, self.now(), Source::Automatic, None)
            .await?;
        if result["consumed"] == true
            && result["uncertain"] != true
            && controller.state(id)?["stage"] == "testing"
        {
            Ok(Value::Null)
        } else {
            Ok(json!({
                "success": false,
                "deferred": truthy(&result["next_query_at"]),
                "error_code": result["error_code"].as_str().filter(|v|!v.is_empty()).unwrap_or("reset_uncertain"),
                "error": "重置尚未确认成功",
                "next_at": result["next_query_at"],
            }))
        }
    }
}
