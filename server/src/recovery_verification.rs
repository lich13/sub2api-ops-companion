use crate::{
    account_locks::Lease,
    compat::{datetime, integer, pick, string, truthy},
    credit_ownership,
    error::Result,
    oauth_admission,
    oauth_monitor::Monitor,
    oauth_state::{block_signature, Update},
    quota, recovery_events, recovery_policy, recovery_rules,
    security::safe_error,
};
use chrono::Duration;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) fn state_block(row: &Value) -> Value {
    let mut block = pick(
        row,
        &[
            "id",
            "platform",
            "type",
            "status",
            "schedulable",
            "concurrency",
            "rate_limited_at",
            "rate_limit_reset_at",
            "overload_until",
            "temp_unschedulable_until",
            "temp_unschedulable_reason",
        ],
    );
    block["extra"] = pick(
        &row["extra"],
        &["model_rate_limits", "antigravity_quota_scopes"],
    );
    block
}
fn receipt_result(receipt: &Value) -> Value {
    if !receipt.is_object() {
        return Value::Null;
    }
    let mut result = receipt["result"].clone();
    for key in ["credential_fingerprint", "block_state", "manual_generation"] {
        if let Some(value) = receipt.get(key) {
            result[key] = value.clone();
        }
    }
    result
}
impl Monitor {
    fn selected(
        &self,
        id: i64,
        generation: i64,
        method_generation: i64,
        method: &str,
    ) -> Result<bool> {
        let state = self.shared_state()?;
        Ok(credit_ownership::generation(&state, id) == generation
            && integer(&state["scheduler"][id.to_string()]["recovery_method_generation"])
                .unwrap_or(0)
                == method_generation
            && recovery_policy::intent_method(
                &self.config.current()?,
                id,
                &state["scheduler"][id.to_string()]["recovery_intent"],
            ) == Some(method))
    }
    fn save_intent(
        &self,
        id: i64,
        previous: &Value,
        intent: &Value,
        generation: i64,
        method_generation: i64,
    ) -> Result<bool> {
        self.store.transaction(|state| {
            let meta = &state["scheduler"][id.to_string()];
            if credit_ownership::generation(state, id) != generation
                || integer(&meta["recovery_method_generation"]).unwrap_or(0) != method_generation
                || meta["recovery_intent"]["fingerprint"] != previous["fingerprint"]
                || matches!(
                    string(&meta["recovery_intent"]["status"]),
                    "cancelled" | "recovered"
                )
            {
                return Ok(false);
            }
            state["scheduler"][id.to_string()]["recovery_intent"] = intent.clone();
            Ok(true)
        })
    }
    pub(crate) fn blocked_intent(
        &self,
        id: i64,
        intent: &Value,
        code: &str,
        generation: i64,
        method_generation: i64,
    ) -> Result<Value> {
        let mut next = intent.clone();
        next["status"] = "blocked".into();
        next["last_error_code"] = code.into();
        next["last_error"] = "恢复任务待核对，未重复派发".into();
        next["next_retry_at"] = "".into();
        next["waiting_reason"] = "".into();
        self.save_intent(id, intent, &next, generation, method_generation)?;
        Ok(json!({"success":false,"skipped":true,"error_code":code}))
    }
    pub async fn reconcile_unknown(&self, id: i64) -> Result<()> {
        let state = self.shared_state()?;
        let meta = &state["scheduler"][id.to_string()];
        let intent = &meta["recovery_intent"];
        if !truthy(intent) || matches!(string(&intent["status"]), "recovered" | "cancelled") {
            return Ok(());
        }
        let generation = credit_ownership::generation(&state, id);
        let method_generation = integer(&meta["recovery_method_generation"]).unwrap_or(0);
        if truthy(&intent["verification_dispatched"]) && !truthy(&intent["verification_receipt"]) {
            self.blocked_intent(
                id,
                intent,
                "verification_uncertain",
                generation,
                method_generation,
            )?;
        } else if truthy(&intent["verification_receipt"]) {
            let row = self.row(id).await?;
            let receipt = receipt_result(&intent["verification_receipt"]);
            if receipt["success"] == true
                && integer(&receipt["recovery_method_generation"]).unwrap_or(0) == method_generation
                && integer(&receipt["manual_generation"]).is_none_or(|value| value == generation)
                && self.selected(
                    id,
                    generation,
                    method_generation,
                    string(&receipt["recovery_method"]),
                )?
                && recovery_rules::confirmed(&row)
                && (intent["kind"] != "early_reset"
                    || crate::oauth_early_reset::eligible(&row, meta, self.now()))
                && receipt["credential_fingerprint"]
                    == oauth_admission::credential_fingerprint(&row)
                && recovery_rules::change_is_safe(&receipt["block_state"], &row)
                && !self.hooks.detection_held(id)?
            {
                self.recovery_complete(id, &row, intent, &receipt, generation, method_generation)?;
            } else if truthy(&intent["recovery_write_dispatched"]) {
                self.blocked_intent(
                    id,
                    intent,
                    "recovery_uncertain",
                    generation,
                    method_generation,
                )?;
            }
        }
        Ok(())
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "Recovery must carry separate manual and method generations with its account lease"
    )]
    pub async fn verify_recovery(
        &self,
        id: i64,
        original: &Value,
        usage: &Value,
        generation: i64,
        method_generation: i64,
        method: &str,
        lease: Lease,
    ) -> Result<Value> {
        let mut lease = Some(lease);
        let state = self.shared_state()?;
        let meta = &state["scheduler"][id.to_string()];
        let mut intent = meta["recovery_intent"].clone();
        if intent["fingerprint"] != original["fingerprint"]
            || !self.selected(id, generation, method_generation, method)?
        {
            return Ok(json!({"success":false,"skipped":true,"error_code":"manual_intervention"}));
        }
        if intent["status"] == "retry_exhausted" {
            return Ok(json!({"success":false,"skipped":true,"error_code":"retry_exhausted"}));
        }
        if method == "connection"
            && integer(&intent["connection_attempt_count"]).unwrap_or(0) > 0
            && crate::compat::parse_datetime(&intent["next_retry_at"])
                .is_some_and(|due| due > self.now())
        {
            return Ok(
                json!({"success":false,"deferred":true,"error_code":"verification_backoff"}),
            );
        }
        let row = match self.row(id).await {
            Ok(row) => row,
            Err(error)
                if error.status.as_u16() >= 500
                    && !truthy(&intent["verification_dispatched"])
                    && !truthy(&intent["verification_receipt"])
                    && !truthy(&intent["recovery_write_dispatched"]) =>
            {
                let attempts = integer(&intent["attempt_count"]).unwrap_or(0) + 1;
                let mut retry = intent.clone();
                retry["status"] = "retry".into();
                retry["attempt_count"] = attempts.into();
                retry["next_retry_at"] = recovery_rules::retry_at(self.now(), attempts).into();
                retry["last_error_code"] = "account_read_failed".into();
                retry["last_error"] = "读取账号状态失败".into();
                self.save_intent(id, &intent, &retry, generation, method_generation)?;
                return Ok(json!({"success":false,"error_code":"account_read_failed"}));
            }
            Err(error) => return Err(error),
        };
        if truthy(&intent["recovery_write_dispatched"]) {
            self.reconcile_unknown(id).await?;
            return Ok(
                json!({"success":self.shared_state()?["scheduler"][id.to_string()]["recovery_intent"]["status"]=="recovered","recovered":self.shared_state()?["scheduler"][id.to_string()]["recovery_intent"]["status"]=="recovered","reused":true}),
            );
        }
        if truthy(&intent["verification_dispatched"]) && !truthy(&intent["verification_receipt"]) {
            return self.blocked_intent(
                id,
                &intent,
                "verification_uncertain",
                generation,
                method_generation,
            );
        }
        if self.hooks.detection_held(id)? {
            return self.blocked_intent(
                id,
                &intent,
                "detection_held",
                generation,
                method_generation,
            );
        }
        let saved = quota::latest_result(&row, usage, self.now());
        let early = intent["kind"] == "early_reset";
        if !early && recovery_rules::obsolete_healthy_intent(&row, &saved, meta, self.now()) {
            self.save_intent(
                id,
                &intent,
                &recovery_rules::cancel_healthy_intent(&intent),
                generation,
                method_generation,
            )?;
            return Ok(json!({"success":false,"skipped":true,"error_code":"already_serving"}));
        }
        let summary = quota::summary_from_result(&row, &saved);
        if !early && recovery_rules::obsolete_rate_intent(&row, &summary, &intent, self.now()) {
            self.save_intent(
                id,
                &intent,
                &recovery_rules::cancel_obsolete_rate_intent(&intent),
                generation,
                method_generation,
            )?;
            return Ok(json!({"success":false,"skipped":true,"error_code":"already_serving"}));
        }
        if !recovery_rules::quota_ready(&row, &saved, meta, self.now()) {
            self.waiting(id, "reset_boundary", None, generation)?;
            return Ok(json!({"success":false,"deferred":true,"error_code":"quota_not_released"}));
        }
        let summary = quota::summary_from_result(&row, &saved);
        let windows = quota::required_windows(&summary["plan_type"])
            .iter()
            .map(|v| (*v).to_owned())
            .collect::<Vec<_>>();
        let prior = receipt_result(&intent["verification_receipt"]);
        if !(recovery_rules::eligible(&row, &windows, self.now())
            || early
                && recovery_rules::confirmed(&row)
                && crate::oauth_early_reset::eligible(&row, meta, self.now())
            || truthy(&prior["success"]) && recovery_rules::confirmed(&row))
        {
            return self.blocked_intent(
                id,
                &intent,
                "recovery_state_changed",
                generation,
                method_generation,
            );
        }
        let result = if truthy(&prior) {
            if prior["credential_fingerprint"] != oauth_admission::credential_fingerprint(&row)
                || !recovery_rules::change_is_safe(&prior["block_state"], &row)
                || integer(&prior["manual_generation"]).is_some_and(|value| value != generation)
                || integer(&prior["recovery_method_generation"]).unwrap_or(0) != method_generation
                || prior["recovery_method"] != method
            {
                return self.blocked_intent(
                    id,
                    &intent,
                    "verification_state_changed",
                    generation,
                    method_generation,
                );
            }
            prior
        } else {
            intent["status"] = "testing".into();
            intent["block_signature"] = block_signature(&row).into();
            intent["tested_at"] = datetime(self.now()).into();
            intent["attempt_count"] = json!(integer(&intent["attempt_count"]).unwrap_or(0) + 1);
            if method == "connection" {
                let policy = match crate::connection_retry::Policy::saved(&intent)? {
                    Some(policy) => policy,
                    None => crate::connection_retry::Policy::config(&self.config.current()?)?,
                };
                intent["retry_policy"] = serde_json::to_value(policy)?;
                intent["connection_attempt_count"] =
                    (integer(&intent["connection_attempt_count"]).unwrap_or(0) + 1).into();
            }
            if method == "model" {
                if let Some(attempts) = integer(&intent["model_attempt_count"])
                    .or_else(|| (intent["attempt_count"] == 1).then_some(0))
                {
                    intent["model_attempt_count"] = (attempts + 1).into();
                }
            }
            intent["verification_dispatched"] = true.into();
            intent["next_retry_at"] = "".into();
            intent["waiting_reason"] = "".into();
            if !self.save_intent(id, original, &intent, generation, method_generation)? {
                return Ok(
                    json!({"success":false,"skipped":true,"error_code":"manual_intervention"}),
                );
            }
            let model = self.model()?;
            if method == "model" {
                lease.take();
            }
            let output = if method == "model" {
                self.hooks
                    .model_recovery(
                        id,
                        string(&intent["fingerprint"]).to_owned(),
                        integer(&intent["attempt_count"]).unwrap_or(1),
                    )
                    .await
            } else {
                self.hooks
                    .connection_test(id, model.clone(), generation, 30, false)
                    .await
            };
            let mut result = output.unwrap_or_else(
                |_| json!({"success":false,"uncertain":true,"error_code":"account_test_error"}),
            );
            if method == "model"
                && !truthy(&result["deferred"])
                && !truthy(&result["uncertain"])
                && !result["job_id"].is_string()
            {
                if let Some(attempts) = integer(&intent["model_attempt_count"]) {
                    intent["model_attempt_count"] = attempts.saturating_sub(1).into();
                }
            }
            if !self.selected(id, generation, method_generation, method)? {
                return Ok(
                    json!({"success":false,"skipped":true,"error_code":"manual_intervention"}),
                );
            }
            if truthy(&result["deferred"]) {
                if method == "model" {
                    if let Some(attempts) = integer(&intent["model_attempt_count"]) {
                        intent["model_attempt_count"] = attempts.saturating_sub(1).into();
                    }
                }
                intent["status"] = "retry".into();
                intent["attempt_count"] = json!(integer(&intent["attempt_count"])
                    .unwrap_or(1)
                    .saturating_sub(1));
                if method == "connection" {
                    let attempts = integer(&intent["connection_attempt_count"])
                        .unwrap_or(1)
                        .saturating_sub(1);
                    intent["connection_attempt_count"] = attempts.into();
                    if attempts == 0 {
                        intent.as_object_mut().unwrap().remove("retry_policy");
                    }
                }
                intent["next_retry_at"] = result
                    .get("next_at")
                    .filter(|v| truthy(v))
                    .cloned()
                    .unwrap_or(json!(datetime(self.now() + Duration::seconds(5))));
                intent["waiting_reason"] = result
                    .get("waiting_reason")
                    .unwrap_or(&result["error_code"])
                    .clone();
                intent
                    .as_object_mut()
                    .unwrap()
                    .shift_remove("verification_dispatched");
                self.save_intent(id, original, &intent, generation, method_generation)?;
                return Ok(result);
            }
            result["error"] = safe_error(&result["error"], 800).into();
            result["completed_at"] = result
                .get("completed_at")
                .filter(|v| truthy(v))
                .cloned()
                .unwrap_or(json!(datetime(self.now())));
            if !truthy(&result["model_id"]) {
                result["model_id"] = model.into();
            }
            result["recovery_method"] = method.into();
            result["recovery_method_generation"] = method_generation.into();
            let safe = pick(
                &result,
                &[
                    "success",
                    "skipped",
                    "uncertain",
                    "error_code",
                    "error",
                    "completed_at",
                    "model_id",
                    "requested_model",
                    "actual_model",
                    "duration_ms",
                    "recovery_method",
                    "recovery_method_generation",
                ],
            );
            let receipt = json!({"result":safe,"manual_generation":generation,"credential_fingerprint":oauth_admission::credential_fingerprint(&row),"block_state":state_block(&row)});
            intent["verification_receipt"] = receipt.clone();
            if !self.save_intent(id, original, &intent, generation, method_generation)? {
                return Ok(
                    json!({"success":false,"skipped":true,"error_code":"manual_intervention"}),
                );
            }
            receipt_result(&receipt)
        };
        if truthy(&result["uncertain"]) {
            return self.blocked_intent(
                id,
                &intent,
                "verification_uncertain",
                generation,
                method_generation,
            );
        }
        if lease.is_none() {
            let current = self.row(id).await?;
            match self.lease(&current) {
                Ok(acquired) => lease = Some(acquired),
                Err(e) if e.status.as_u16() == 409 => {
                    let mut next = intent.clone();
                    next["status"] = "retry".into();
                    next["next_retry_at"] = datetime(self.now() + Duration::seconds(5)).into();
                    next["waiting_reason"] = "account_busy".into();
                    self.save_intent(id, &intent, &next, generation, method_generation)?;
                    return Ok(json!({"deferred":true,"error_code":"account_busy"}));
                }
                Err(e) => return Err(e),
            }
        }
        let _held_lease = lease;
        let after = self.row(id).await?;
        if !truthy(&result["success"]) {
            self.recovery_failed(id, &after, &intent, &result, generation, method_generation)?;
            let mut result = result;
            if let Some(attempts) = intent
                .get("connection_attempt_count")
                .or_else(|| intent.get("model_attempt_count"))
            {
                result["attempt_count"] = attempts.clone();
                let state = self.shared_state()?;
                let next = &state["scheduler"][id.to_string()]["recovery_intent"]["next_retry_at"];
                result["next_retry_at"] = if truthy(next) {
                    next.clone()
                } else {
                    Value::Null
                };
            }
            return Ok(result);
        }
        if !self.selected(id, generation, method_generation, method)? {
            return Ok(json!({"success":false,"skipped":true,"error_code":"manual_intervention"}));
        }
        if self.hooks.detection_held(id)? {
            return self.known_recovery_failure(
                id,
                &after,
                &intent,
                &result,
                "detection_held",
                generation,
                method_generation,
            );
        }
        if early
            && !crate::oauth_early_reset::eligible(
                &after,
                &self.shared_state()?["scheduler"][id.to_string()],
                self.now(),
            )
            || result["credential_fingerprint"] != oauth_admission::credential_fingerprint(&after)
            || !recovery_rules::change_is_safe(&result["block_state"], &after)
        {
            return self.known_recovery_failure(
                id,
                &after,
                &intent,
                &result,
                "recovery_state_changed",
                generation,
                method_generation,
            );
        }
        if recovery_rules::confirmed(&after) {
            return self.recovery_complete(
                id,
                &after,
                &intent,
                &result,
                generation,
                method_generation,
            );
        }
        if !recovery_rules::eligible(&after, &windows, self.now()) {
            return self.known_recovery_failure(
                id,
                &after,
                &intent,
                &result,
                "recovery_state_changed",
                generation,
                method_generation,
            );
        }
        let _control = self.actions.locks.control(id).await;
        let fresh = self.row(id).await?;
        if !self.selected(id, generation, method_generation, method)?
            || self.hooks.detection_held(id)?
            || early
                && !crate::oauth_early_reset::eligible(
                    &fresh,
                    &self.shared_state()?["scheduler"][id.to_string()],
                    self.now(),
                )
            || !recovery_rules::change_is_safe(&result["block_state"], &fresh)
            || result["credential_fingerprint"] != oauth_admission::credential_fingerprint(&fresh)
        {
            return self.blocked_intent(
                id,
                &intent,
                "recovery_state_changed",
                generation,
                method_generation,
            );
        }
        intent["recovery_write_dispatched"] = true.into();
        intent["recovery_write_at"] = datetime(self.now()).into();
        if !self.save_intent(id, original, &intent, generation, method_generation)? {
            return Ok(json!({"success":false,"skipped":true,"error_code":"manual_intervention"}));
        }
        let _response = self.hooks.recover(id, self.store.admin_token()?).await;
        let current = self.row(id).await?;
        if self.selected(id, generation, method_generation, method)?
            && !self.hooks.detection_held(id)?
            && recovery_rules::confirmed(&current)
            && recovery_rules::change_is_safe(&result["block_state"], &current)
            && result["credential_fingerprint"] == oauth_admission::credential_fingerprint(&current)
        {
            self.recovery_complete(
                id,
                &current,
                &intent,
                &result,
                generation,
                method_generation,
            )
        } else {
            self.blocked_intent(
                id,
                &intent,
                "recovery_uncertain",
                generation,
                method_generation,
            )
        }
    }
    fn recovery_event(
        &self,
        id: i64,
        row: &Value,
        intent: &Value,
        test: &Value,
        status: &str,
        code: &str,
    ) -> Value {
        let at = self.now();
        let summary = quota::summary_from_result(
            row,
            &quota::latest_result(
                row,
                &self
                    .shared_state()
                    .unwrap_or_else(|_| std::sync::Arc::new(json!({})))["oauth_results"]
                    [id.to_string()],
                at,
            ),
        );
        let fingerprint = string(&intent["fingerprint"]);
        let suffix = if status == "recovered" {
            "success".to_owned()
        } else {
            format!("failure:{}", if code.is_empty() { "unknown" } else { code })
        };
        let key = format!("recovery:{id}:{fingerprint}:{suffix}");
        let mut event = json!({"account_id":id,"account_name":row["name"].as_str().filter(|v|!v.is_empty()).unwrap_or("-"),"plan_type":quota::account_plan(row),"window_labels":intent["window_keys"].as_array().into_iter().flatten().map(|v|string(v).trim_start_matches("codex_")).collect::<Vec<_>>(),"status":status,"stage":if status=="auth_failed" {"account_test"} else {"recovery"},"checked_at":datetime(at),"model_id":test["model_id"],"duration_ms":test["duration_ms"],"test_success":truthy(&test["success"]),"error_code":code,"error":safe_error(&test["error"],800),"dedupe_key":key,"reset_at":intent["due_at"],"reset_source":intent["source"],"fingerprint":fingerprint,"remaining_summary":summary["remaining_summary"]});
        if let Some(attempts) = intent
            .get("connection_attempt_count")
            .or_else(|| intent.get("model_attempt_count"))
        {
            event["attempt_count"] = attempts.clone();
        }
        if test["job_id"].is_string() {
            event["model_test_job_id"] = test["job_id"].clone();
        }
        for key in [
            "early_reset_detected",
            "old_7d_used_percent",
            "new_7d_used_percent",
            "old_reset_at",
            "detected_at",
        ] {
            if let Some(value) = intent.get(key) {
                event[key] = value.clone();
            }
        }
        event
    }
    fn recovery_failed(
        &self,
        id: i64,
        row: &Value,
        intent: &Value,
        test: &Value,
        generation: i64,
        method_generation: i64,
    ) -> Result<()> {
        let code = string(&test["error_code"]);
        let auth = recovery_events::auth_code(code);
        let mut next = intent.clone();
        let connection = test["recovery_method"] == "connection";
        let connection_next = if connection && crate::connection_retry::retryable(test) {
            crate::connection_retry::Policy::saved(intent)?
                .unwrap_or_default()
                .next(
                    integer(&intent["connection_attempt_count"]).unwrap_or(1),
                    crate::compat::parse_datetime(&test["completed_at"])
                        .unwrap_or_else(|| self.now()),
                )
        } else {
            None
        };
        let exhausted =
            connection && crate::connection_retry::retryable(test) && connection_next.is_none();
        next["status"] = if !auth.is_empty() {
            "auth_failed"
        } else if exhausted {
            "retry_exhausted"
        } else if connection && connection_next.is_none() || truthy(&test["skipped"]) {
            "blocked"
        } else {
            "retry"
        }
        .into();
        next["next_retry_at"] = if connection {
            connection_next.unwrap_or_default()
        } else if !auth.is_empty() || truthy(&test["skipped"]) {
            String::new()
        } else {
            recovery_rules::retry_at(self.now(), integer(&intent["attempt_count"]).unwrap_or(1))
        }
        .into();
        next["last_error_code"] = if exhausted { "retry_exhausted" } else { code }.into();
        next["last_error"] = safe_error(&test["error"], 800).into();
        next["test_completed_at"] = test["completed_at"].clone();
        next["waiting_reason"] = "".into();
        next["waiting_until"] = "".into();
        next.as_object_mut()
            .unwrap()
            .shift_remove("verification_dispatched");
        next.as_object_mut()
            .unwrap()
            .shift_remove("verification_receipt");
        let mut update = Update::default();
        let mut meta = json!({"recovery_intent":next});
        if !auth.is_empty() {
            let state = self.shared_state()?;
            let mut query = crate::oauth_state::query_metadata(
                &state["scheduler"][id.to_string()],
                &state["oauth_results"][id.to_string()],
            )?;
            query["auth_fingerprint"] = oauth_admission::credential_fingerprint(row).into();
            meta["quota_query"] = query;
            meta["last_error_code"] = auth.into();
            meta["last_error_at"] = datetime(self.now()).into();
        }
        update.scheduler.insert(id, meta);
        let event = self.recovery_event(
            id,
            row,
            intent,
            test,
            if !auth.is_empty() {
                "auth_failed"
            } else if truthy(&test["success"]) {
                "recovery_failed"
            } else {
                "test_failed"
            },
            if auth.is_empty() { code } else { auth },
        );
        let key = string(&event["dedupe_key"]).to_owned();
        if !recovery_events::known(&key, id, self.shared_state()?.as_ref()) {
            update.pending.insert(key, event);
        }
        update.generations = Some(BTreeMap::from([(id, generation)]));
        update.methods = Some(BTreeMap::from([(id, method_generation)]));
        self.store.commit(update)
    }
    #[allow(clippy::too_many_arguments)]
    fn known_recovery_failure(
        &self,
        id: i64,
        row: &Value,
        intent: &Value,
        test: &Value,
        code: &str,
        generation: i64,
        method_generation: i64,
    ) -> Result<Value> {
        let mut result = test.clone();
        result["error_code"] = code.into();
        result["error"] = if code == "detection_held" {
            "账号已由模型检测接管"
        } else {
            "账号阻断签名已变化，已停止自动恢复"
        }
        .into();
        // The verification has a definitive receipt and no recovery write was
        // dispatched. Keep the established retry/notification semantics while
        // leaving unknown verification and write outcomes on the no-replay path.
        self.recovery_failed(id, row, intent, &result, generation, method_generation)?;
        result["success"] = false.into();
        Ok(result)
    }
    fn recovery_complete(
        &self,
        id: i64,
        row: &Value,
        intent: &Value,
        test: &Value,
        generation: i64,
        method_generation: i64,
    ) -> Result<Value> {
        let at = self.now();
        let mut next = intent.clone();
        for key in [
            "verification_receipt",
            "verification_dispatched",
            "recovery_write_dispatched",
            "recovery_write_at",
        ] {
            next.as_object_mut().unwrap().shift_remove(key);
        }
        next["test_completed_at"] = test["completed_at"].clone();
        next["deferred_until"] = "".into();
        next["status"] = "recovered".into();
        next["recovered_at"] = datetime(at).into();
        for key in [
            "next_retry_at",
            "waiting_reason",
            "last_error",
            "last_error_code",
        ] {
            next[key] = "".into();
        }
        let mut update = Update::default();
        update.scheduler.insert(id, json!({"recovery_intent":next}));
        let key = format!("recovery:{id}:{}:success", string(&intent["fingerprint"]));
        let event = self.recovery_event(id, row, intent, test, "recovered", "");
        if !recovery_events::known(&key, id, self.shared_state()?.as_ref()) {
            update.pending.insert(key.clone(), event);
        }
        let mut history = json!({"account_id":id,"account_name":row["name"],"model_test_job_id":test["job_id"],"model_id":test["model_id"],"legacy":false,"test_completed_at":test["completed_at"],"recovered_at":datetime(at)});
        if let Some(attempts) = intent
            .get("connection_attempt_count")
            .or_else(|| intent.get("model_attempt_count"))
        {
            history["attempt_count"] = attempts.clone();
        }
        update.history.insert(key, history);
        update.generations = Some(BTreeMap::from([(id, generation)]));
        update.methods = Some(BTreeMap::from([(id, method_generation)]));
        self.store.commit(update)?;
        let mut result = test.clone();
        result["recovered"] = true.into();
        if let Some(attempts) = intent
            .get("connection_attempt_count")
            .or_else(|| intent.get("model_attempt_count"))
        {
            result["attempt_count"] = attempts.clone();
            result["next_retry_at"] = Value::Null;
        }
        Ok(result)
    }
}
