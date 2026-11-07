use crate::{
    compat::{datetime, digest, integer, operation_versions, parse_datetime, string, truthy},
    credit_rules,
    error::{Error, Result},
    model_detection::{Detection, TARGET},
    model_downgrade::{bank_contains, is_downgrade, valid_report},
    model_tests::ACTIVE,
    oauth_queries::Source,
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
        evidence: Option<&mut Value>,
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
        let state = self.monitor.shared_state()?;
        let meta = &state["scheduler"][id.to_string()];
        let early =
            context["kind"] == "recovery" && meta["recovery_intent"]["kind"] == "early_reset";
        if (if early {
            recovery_policy::intent_method(&config, id, &meta["recovery_intent"])
        } else {
            recovery_policy::method(&config, id)
        }) != Some("model")
            || !early && config["oauth_recovery_monitor_enabled"] == false
        {
            return Ok("恢复验证设置已变化".into());
        }
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
                    || matches!(string(&task["stage"]), "manual" | "closed" | "blocked")
                {
                    return Ok("账号不再符合自动用卡条件".into());
                }
                // Locked preparation checks credit first to retain its specific
                // error code. Other callers still need this eligibility check.
                if evidence.is_none() {
                    let credit_reason = controller.positive_credit_reason(row, now);
                    if !credit_reason.is_empty() {
                        return Ok(credit_reason.into());
                    }
                }
                if !controller.eligible(row, now, false)? {
                    return Ok("账号不再符合自动用卡条件".into());
                }
                // Only locked preparation needs the expensive evidence; queued jobs
                // must obtain it again when they actually execute.
                if let Some(evidence) = evidence {
                    *evidence = controller.depletion(row, now, false).await?;
                    if !truthy(evidence) {
                        return Ok("账号不再符合自动用卡条件".into());
                    }
                }
            } else if !controller.owned(row, &task)?
                || !controller.eligible(row, now, true)?
                || !controller.available(row, &task, now)?
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
            if !(recovery_rules::eligible(row, &windows, now)
                || early
                    && recovery_rules::confirmed(row)
                    && crate::oauth_early_reset::eligible(row, meta, now))
            {
                return Ok("账号状态不允许自动恢复".into());
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
        let state = self.monitor.shared_state()?;
        let meta = &state["scheduler"][id.to_string()];
        let context = json!({
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
        // A config change must not create another job for an already registered
        // recovery attempt, including requests persisted by older releases.
        let jobs = self.tests.shared_data()?;
        let registered = jobs["requests"].as_object().ok_or_else(Error::internal)?;
        let request_key =
            digest(&json!(["recovery", identity, value["generation"]]))[..32].to_owned();
        let table = jobs["jobs"].as_object().ok_or_else(Error::internal)?;
        let same = |job: &Value| {
            job["automatic"] == true
                && job["account_id"] == id
                && job["detection_generation"] == value["generation"]
                && [
                    "account_id",
                    "kind",
                    "fingerprint",
                    "control_generation",
                    "method_generation",
                    "attempt",
                ]
                .iter()
                .all(|key| job["recovery_context"][*key] == identity[*key])
        };
        let direct = registered
            .get_key_value(&request_key)
            .and_then(|(key, id)| {
                table
                    .get(id.as_str()?)
                    .filter(|job| same(job))
                    .map(|job| (key, job))
            });
        let previous = direct.or_else(|| {
            registered
                .iter()
                .rev()
                .filter_map(|(key, id)| {
                    table
                        .get(id.as_str()?)
                        .filter(|job| same(job))
                        .map(|job| (key, job))
                })
                .max_by_key(|(_, job)| (parse_datetime(&job["queued_at"]), string(&job["id"])))
        });
        let request = previous.map(|(key, _)| key.clone()).unwrap_or(request_key);
        if let Some(previous) = previous.and_then(|(_, job)| job["id"].as_str()) {
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
            let policy_verdict = crate::verdict_policy::classify(&job);
            let valid = if crate::verdict_policy::relaxed(&job) {
                matches!(string(&policy_verdict["outcome"]), "normal" | "degraded")
            } else {
                job["status"] == "completed" && valid_report(report)
            };
            let degraded = if crate::verdict_policy::relaxed(&job) {
                policy_verdict["outcome"] == "degraded"
            } else {
                is_downgrade(
                    string(&job["requested_model"]),
                    string(&report["prediction"]),
                )
            };
            if valid && !degraded {
                if let Err(error) = self.guard(&job).await {
                    if error.status.is_server_error() {
                        return Ok(self.recovery_wait(
                            "账号状态暂不可读取，等待复核",
                            None,
                            job["id"].as_str(),
                            "model_verification_pending",
                        ));
                    }
                    self.log_receipt(string(&job["id"]), "verification", &json!({"status":"failed","reason":crate::security::safe_error(&error.detail,200),"completed_at":datetime(self.now())}));
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
            self.log_receipt(string(&job["id"]), "verification", &json!({"status":if valid && !degraded {"passed"} else {"failed"},"reason":default_error,"completed_at":job["completed_at"]}));
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
        if kind == "recovery"
            && meta["recovery_intent"]["kind"] == "early_reset"
            && self.monitor.config.current()?["oauth_early_reset_enabled"] != true
        {
            return Ok(json!({"success":false,"error_code":"early_reset_disabled"}));
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
        let row = crate::model_target::account(&self.actions, id).await?;
        let reason = self.recovery_reason(&row, &context, consume, None).await?;
        if !reason.is_empty() {
            return Ok(json!({"success":false,"error":reason,"error_code":"recovery_ineligible"}));
        }
        let model = self.model_id()?;
        if !self.actions.model_allowed(&row, &model).await? {
            return Ok(
                json!({"success":false,"error":"所选模型不在当前账号上游模型目录中","error_code":"model_unavailable"}),
            );
        }
        let bank = self.tests.bank.snapshot();
        if if crate::verdict_policy::relaxed(&value) {
            !crate::verdict_policy::bank_contains(&bank.0, &value)
        } else {
            !bank_contains(&bank.0, TARGET)
        } {
            return Ok(
                json!({"success":false,"error":"指纹库缺少降智判定模型","error_code":"bank_unavailable"}),
            );
        }
        let mark = self.mark(id)?;
        let _config = self.monitor.config.lock.lock().await;
        if self.model_id()? != model {
            return Err(Error::new(409, "自动检测模型已变化，等待重新核对"));
        }
        let job = self
            .tests
            .start(
                id,
                &json!({
                    "model_id": model,
                    "expected_operation_version": operation_versions(&row)["model_test"],
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
    pub async fn prepare_recovery(
        &self,
        job: &Value,
        row: Value,
    ) -> Result<(Value, Option<Value>)> {
        let context = &job["recovery_context"];
        if !truthy(context) || context["consume"] != true {
            return Ok((Value::Null, None));
        }
        let id = integer(&job["account_id"]).ok_or_else(Error::invalid)?;
        let controller = &self.monitor.credits;
        let task = controller.state(id)?;
        if truthy(&task["attempt_at"]) {
            return Ok((
                if task["consumed"] == true && task["stage"] == "testing" {
                    Value::Null
                } else {
                    json!({"success":false,"error_code":"reset_uncertain","error":"重置结果待确认"})
                },
                Some(row),
            ));
        }
        let reason = controller.positive_credit_reason(&row, self.now());
        if !reason.is_empty() {
            return Ok((json!({"success":false,"error_code":reason}), Some(row)));
        }
        let mut evidence = Value::Null;
        let reason = self
            .recovery_reason(&row, context, true, Some(&mut evidence))
            .await?;
        if !reason.is_empty() {
            return Ok((
                json!({"success":false,"error_code":"recovery_ineligible","error":reason}),
                Some(row),
            ));
        }
        let task = controller.save(id, &task, json!({"model_test_job_id":job["id"]}))?;
        let (result, latest_row) = controller
            .request(
                &row,
                "reset",
                &task,
                self.now(),
                Source::Automatic,
                None,
                Some(&evidence),
            )
            .await?;
        if result["consumed"] == true
            && result["uncertain"] != true
            && controller.state(id)?["stage"] == "testing"
        {
            Ok((Value::Null, latest_row))
        } else {
            Ok((
                json!({
                    "success": false,
                    "deferred": truthy(&result["next_query_at"]),
                    "error_code": result["error_code"].as_str().filter(|v|!v.is_empty()).unwrap_or("reset_uncertain"),
                    "error": "重置尚未确认成功",
                    "next_at": result["next_query_at"],
                }),
                latest_row,
            ))
        }
    }
}
