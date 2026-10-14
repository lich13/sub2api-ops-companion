use crate::{
    actions::{OperationContext, OPERATION},
    compat::{integer, string, truthy},
    credit_ownership,
    error::{Error, Result},
    oauth_queries::{QueryOptions, Source},
    runtime::Runtime,
};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) type UsageFlight = (String, tokio::sync::watch::Receiver<Option<Result<Value>>>);
fn uncertain(message: &str, code: &str) -> Error {
    Error::new(502, json!({"message":message,"code":code}))
}

impl Runtime {
    async fn manual_account_write(&self, id: i64, recover: bool, key: &str) -> String {
        let path = format!(
            "accounts/{id}{}",
            if recover { "/recover-state" } else { "" }
        );
        let body = recover.then(|| json!({}));
        let response = self
            .actions
            .request(
                if recover {
                    reqwest::Method::POST
                } else {
                    reqwest::Method::DELETE
                },
                &path,
                key,
                body.as_ref(),
                Duration::from_secs(5),
                false,
            )
            .await;
        let Ok(response) = response else {
            return "result_uncertain".into();
        };
        if !response.status().is_success() {
            return format!("http_{}", response.status().as_u16());
        }
        let Ok(bytes) = crate::upstream::read_limited(response, 2_000_000).await else {
            return "result_uncertain".into();
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            return "result_uncertain".into();
        };
        if value.is_object() && value["code"] == 0 {
            "ok".into()
        } else {
            "upstream_rejected".into()
        }
    }
    pub(crate) async fn revoke_manual(
        &self,
        row: &Value,
        enabled: bool,
        origin: &str,
    ) -> Result<()> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        self.detection.human_control(id, true)?;
        if row["platform"] == "openai" && row["type"] == "oauth" {
            let live = self.monitor.row(id).await?;
            self.monitor
                .store
                .manual_control(&live, enabled, self.monitor.now(), origin)?;
        }
        Ok(())
    }
    pub async fn set_schedulable(&self, id: i64, payload: &Value, key: &str) -> Result<Value> {
        self.writable()?;
        if id < 1 {
            return Err(Error::new(422, "账号编号无效"));
        }
        let initial = self.actions.account(id, None).await?;
        let oauth = initial["platform"] == "openai" && initial["type"] == "oauth";
        let _config = if oauth {
            None
        } else {
            Some(self.config.lock.lock().await)
        };
        let _fallback = if oauth {
            None
        } else {
            Some(self.config.fallback.lock.lock().await)
        };
        let _control = self.actions.locks.control(id).await;
        let row = self
            .actions
            .account(id, Some(string(&payload["expected_version"])))
            .await?;
        let config = self.config.fallback.load();
        if !config.valid {
            return Err(Error::new(409, "托管配置无法读取，操作已中止"));
        }
        let detached = config.ids.contains(&id);
        if detached {
            if payload["detach_managed"] != true {
                return Err(Error::new(409, "此账号由 Key 回退托管，请确认解除托管"));
            }
            self.config.fallback.write_locked(
                config.openai,
                config.grok,
                &config
                    .ids
                    .iter()
                    .copied()
                    .filter(|v| *v != id)
                    .collect::<Vec<_>>(),
                None,
                config.version + 1,
                "desktop:admin",
            )?;
        }
        let enabled = crate::validation::boolean(&payload["schedulable"])?;
        self.revoke_manual(&row, enabled, "manual").await?;
        let response =
            crate::key_fallback::set_schedulable(&self.actions, id, enabled, key, 3).await;
        let live = self.actions.account(id, None).await;
        let verified = response["success"] == true
            && live.as_ref().is_ok_and(|v| {
                v["schedulable"] == enabled
                    && v["platform"] == row["platform"]
                    && v["type"] == row["type"]
            });
        self.invalidate().await;
        crate::audit::write(
            &self.settings.audit_path,
            "desktop_schedulable",
            json!({"account_id":id,"schedulable":enabled,"verified":verified,"detached":detached,"error_code":response["error_code"]}),
            false,
        );
        if !verified {
            return Err(Error::new(
                502,
                json!({"message":"调度写入未确认，请刷新查看实际状态","detached":detached,"code":response["error_code"].as_str().filter(|v|!v.is_empty()).unwrap_or("readback_mismatch")}),
            ));
        }
        Ok(json!({"verified":true,"detached":detached,"account_id":id,"schedulable":enabled}))
    }
    pub async fn delete_account(&self, id: i64, payload: &Value, key: &str) -> Result<Value> {
        self.writable()?;
        if id < 1 {
            return Err(Error::new(422, "账号编号无效"));
        }
        let row = self
            .actions
            .account(id, Some(string(&payload["expected_version"])))
            .await?;
        let _lease = self.monitor.lease(&row)?;
        let _config = self.config.lock.lock().await;
        let _fallback = self.config.fallback.lock.lock().await;
        let _control = self.actions.locks.control(id).await;
        let row = self
            .actions
            .account(id, Some(string(&payload["expected_version"])))
            .await?;
        let config = self.config.fallback.load();
        if !config.valid {
            return Err(Error::new(409, "托管配置无法读取，删除已中止"));
        }
        let detached = config.ids.contains(&id);
        if detached {
            if payload["detach_managed"] != true {
                return Err(Error::new(
                    409,
                    "此账号由 Key 回退托管，请刷新并确认解除托管后删除",
                ));
            }
            self.config
                .fallback
                .write_locked(
                    config.openai,
                    config.grok,
                    &config
                        .ids
                        .iter()
                        .copied()
                        .filter(|v| *v != id)
                        .collect::<Vec<_>>(),
                    None,
                    config.version + 1,
                    "desktop:admin",
                )
                .map_err(|_| Error::new(503, "解除托管保存失败，未执行删除"))?;
        }
        self.revoke_manual(&row, false, "delete").await?;
        let code = self.manual_account_write(id, false, key).await;
        let verified = self
            .actions
            .queries
            .account(id)
            .await
            .is_err_and(|error| error.status.as_u16() == 404);
        self.invalidate().await;
        crate::audit::write(
            &self.settings.audit_path,
            "desktop_delete_account",
            json!({"account_id":id,"verified":verified,"detached":detached,"code":code}),
            false,
        );
        if !verified {
            return Err(Error::new(
                502,
                json!({"message":if detached {"已解除托管；删除未确认，请刷新核对实际状态"} else {"删除未确认，请刷新核对实际状态"},"detached":detached,"code":if code == "ok" {"readback_mismatch"} else {&code}}),
            ));
        }
        Ok(json!({"account_id":id,"deleted":true,"verified":true,"detached":detached}))
    }
    pub async fn recover_account(&self, id: i64, payload: &Value, key: &str) -> Result<Value> {
        self.writable()?;
        if id < 1 {
            return Err(Error::new(422, "账号编号无效"));
        }
        let row = self
            .actions
            .account(id, Some(string(&payload["expected_version"])))
            .await?;
        let _lease = self.monitor.lease(&row)?;
        let _control = self.actions.locks.control(id).await;
        let row = self
            .actions
            .account(id, Some(string(&payload["expected_version"])))
            .await?;
        if !crate::queries::recoverable(&row, chrono::Utc::now()) {
            return Err(Error::new(409, "账号已无可恢复状态，请刷新"));
        }
        credit_ownership::cancel(&self.monitor.store, id)?;
        let code = self.manual_account_write(id, true, key).await;
        let live = self.actions.account(id, None).await;
        let verified = live.is_ok_and(|v| {
            v["platform"] == row["platform"]
                && v["type"] == row["type"]
                && !crate::queries::recoverable(&v, chrono::Utc::now())
        });
        self.invalidate().await;
        crate::audit::write(
            &self.settings.audit_path,
            "desktop_recover_state",
            json!({"account_id":id,"verified":verified,"code":code}),
            false,
        );
        if !verified {
            return Err(uncertain(
                "恢复状态未确认，请刷新核对实际状态",
                if code == "ok" {
                    "readback_mismatch"
                } else {
                    &code
                },
            ));
        }
        Ok(json!({"account_id":id,"verified":true,"message":"状态已恢复"}))
    }
    pub async fn set_degradation(&self, id: i64, payload: &Value) -> Result<Value> {
        self.writable()?;
        let row = self.actions.account(id, None).await?;
        if row["platform"] != "openai" || !matches!(string(&row["type"]), "oauth" | "apikey") {
            return Err(Error::new(422, "仅支持 OpenAI OAuth 或 Key 账号"));
        }
        let _control = self.actions.locks.control(id).await;
        let expected = string(&payload["expected_mark_version"]);
        if self.detection.mark(id)?["version"] != expected {
            return Err(Error::new(409, "降智标记已变化，请重新读取"));
        }
        self.detection.human_control(id, false)?;
        let marked = crate::validation::boolean(&payload["marked"])?;
        let mark = self.detection.capacity.store.set_mark(
            id,
            marked,
            expected,
            chrono::Utc::now(),
            None,
        )?;
        self.invalidate().await;
        crate::audit::write(
            &self.settings.audit_path,
            "desktop_degradation_mark",
            json!({"account_id":id,"marked":marked}),
            false,
        );
        Ok(json!({"verified":true,"account_id":id,"degradation_mark":mark}))
    }

    pub async fn usage_action(
        self: &Arc<Self>,
        id: i64,
        payload: &Value,
        key: &str,
        requested: Option<Instant>,
    ) -> Result<Value> {
        self.writable()?;
        if id < 1 {
            return Err(Error::new(422, "账号编号无效"));
        }
        if payload["action"] != "query_usage" {
            return self.perform_usage_action(id, payload, key, requested).await;
        }
        let mut receiver = {
            let mut pending = self.usage_flights.lock().unwrap_or_else(|e| e.into_inner());
            let version = string(&payload["expected_version"]);
            if let Some((previous, future)) = pending.get(&id) {
                if previous != version {
                    return Err(Error::new(409, "账号已变化，请刷新后再操作"));
                }
                future.clone()
            } else {
                let runtime = self.clone();
                let payload = payload.clone();
                let key = key.to_owned();
                let context = OPERATION
                    .try_with(|c| OperationContext {
                        action: c.action.clone(),
                        expected: c.expected.clone(),
                        managed: c.managed,
                    })
                    .ok();
                let operation = async move {
                    let run = runtime.perform_usage_action(
                        id,
                        &payload,
                        &key,
                        Some(requested.unwrap_or_else(Instant::now)),
                    );
                    if let Some(context) = context {
                        OPERATION.scope(context, run).await
                    } else {
                        run.await
                    }
                };
                let (send, receive) = tokio::sync::watch::channel(None);
                pending.insert(id, (version.to_owned(), receive.clone()));
                let runtime = Arc::downgrade(self);
                if let Err(error) = self.requests.spawn(async move {
                    let result = operation.await;
                    let _ = send.send(Some(result));
                    if let Some(runtime) = runtime.upgrade() {
                        runtime
                            .usage_flights
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&id);
                    }
                }) {
                    pending.remove(&id);
                    return Err(error);
                }
                receive
            }
        };
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            if receiver.changed().await.is_err() {
                return Err(uncertain("查询已中断，请刷新核对结果", "result_uncertain"));
            }
        }
    }
    async fn perform_usage_action(
        &self,
        id: i64,
        payload: &Value,
        key: &str,
        requested: Option<Instant>,
    ) -> Result<Value> {
        let expected = string(&payload["expected_version"]);
        let row = self.actions.account(id, Some(expected)).await?;
        let action = string(&payload["action"]);
        if action == "query_usage" && row["platform"] == "openai" && row["type"] == "oauth" {
            if let Some(at) = requested {
                if let Some(result) = self.monitor.queries.join_existing(id, at).await {
                    if result["success"] != true {
                        return Err(uncertain(
                            "额度查询失败，未重复请求",
                            string(&result["error_code"]),
                        ));
                    }
                    self.invalidate().await;
                    return Ok(
                        json!({"message":"用量查询完成","account":self.account_snapshot(id).await?}),
                    );
                }
            }
        }
        let _lease = self.monitor.lease(&row)?;
        let _budget = self
            .budget
            .clone()
            .try_acquire_owned()
            .map_err(|_| crate::account_locks::busy())?;
        let row = self.actions.account(id, Some(expected)).await?;
        let now = chrono::Utc::now();
        let usage = crate::usage::project(&row, now, &Value::Null);
        if !usage["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == action)
        {
            return Err(Error::new(422, "此账号不支持该用量操作"));
        }
        if matches!(action, "reset_quota" | "probe_quota") && payload["confirmed"] != true {
            return Err(Error::new(409, "请先确认重置次数消耗或模型探测请求"));
        }
        if action == "reset_quota"
            && integer(&crate::usage::reset_credits(&row["extra"], now)["available"]).unwrap_or(0)
                <= 0
        {
            return Err(Error::new(409, "没有已确认可用的重置次数，请先查询次数"));
        }
        let result = match action {
            "query_usage" if row["platform"] == "openai" => Ok(self
                .monitor
                .queries
                .query(
                    id,
                    key,
                    QueryOptions {
                        source: Source::Manual,
                        reason: "single_refresh".into(),
                        at: now,
                        requested_at: requested,
                        timeout: 30,
                        generation: None,
                    },
                )
                .await),
            "query_reset_credits" | "reset_quota" => {
                let raw = self.monitor.row(id).await?;
                self.monitor
                    .credits
                    .manual(
                        &raw,
                        if action == "reset_quota" {
                            "reset"
                        } else {
                            "query"
                        },
                        key,
                    )
                    .await
            }
            "probe_quota" => self
                .actions
                .json(
                    reqwest::Method::GET,
                    &format!("grok/accounts/{id}/quota"),
                    key,
                    None,
                    90,
                )
                .await
                .map(|data| json!({"success":true,"data":data})),
            _ => Err(Error::new(422, "此账号不支持该用量操作")),
        };
        self.invalidate().await;
        let result = result?;
        if result["success"] != true {
            return Err(uncertain(
                "操作未确认，未自动重放；请先查询确认实际结果",
                result["error_code"].as_str().unwrap_or("result_uncertain"),
            ));
        }
        let data = &result["data"];
        let code = if !data.is_object() {
            "invalid_result"
        } else if action == "probe_quota"
            && (truthy(&data["probe_error"]) || integer(&data["status_code"]).unwrap_or(200) >= 400)
        {
            "probe_failed"
        } else if action == "query_reset_credits" && data["cache_persisted"] == false {
            "snapshot_not_saved"
        } else {
            "ok"
        };
        crate::audit::write(
            &self.settings.audit_path,
            "desktop_usage_action",
            json!({"account_id":id,"action":action,"code":code}),
            false,
        );
        if code != "ok" {
            return Err(uncertain(
                "操作未确认，未自动重放；请先查询确认实际结果",
                code,
            ));
        }
        let current = self.actions.account(id, None).await?;
        if current["platform"] != row["platform"] || current["type"] != row["type"] {
            return Err(Error::new(409, "操作后账号已变化，请刷新确认结果"));
        }
        let warning = action == "reset_quota"
            && (data["cache_refreshed"] == false || truthy(&data["warning_code"]));
        let message = if warning {
            "重置已执行，但快照更新未确认；请查询次数，勿重复重置"
        } else {
            match action {
                "query_usage" => "用量查询完成",
                "query_reset_credits" => "重置次数已查询",
                "reset_quota" => "重置已执行",
                _ => "探测完成",
            }
        };
        Ok(json!({"message":message,"account":self.account_snapshot(id).await?}))
    }
}
