use crate::{
    bark,
    compat::{datetime, integer, parse_datetime, string, truthy},
    error::{Error, Result},
    model_detection::{defaults, Detection},
    security::safe_error,
};
use chrono::Duration;
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};

impl Detection {
    pub fn next_allowed_at(&self, id: i64) -> Result<Value> {
        let state = self.store.read()?;
        let record = &state["automatic_starts"][id.to_string()];
        if !truthy(record) {
            return Ok(Value::Null);
        }
        let at = parse_datetime(&record["started_at"])
            .filter(|_| record["job_id"].is_string())
            .ok_or_else(|| Error::new(503, "自动检测派发状态无效"))?;
        Ok(datetime(at + Duration::seconds(300)).into())
    }
    pub fn register_dispatch(&self, job: &Value) -> Result<()> {
        if job["automatic"] != true {
            return Ok(());
        }
        let id = integer(&job["account_id"])
            .ok_or_else(Error::invalid)?
            .to_string();
        self.store.transaction(|state| {
            if !state["automatic_starts"].is_object() {
                state["automatic_starts"] = json!({});
            }
            let previous = &state["automatic_starts"][&id];
            if truthy(previous) {
                let at = parse_datetime(&previous["started_at"])
                    .ok_or_else(|| Error::new(503, "自动检测派发时间无效"))?;
                if previous["job_id"] == job["id"] {
                    return Ok(());
                }
                if self.now() < at + Duration::seconds(300) {
                    return Err(Error::new(409, "自动检测仍在五分钟冷却内"));
                }
            }
            state["automatic_starts"][id] =
                json!({"job_id":job["id"],"started_at":datetime(self.now())});
            Ok(())
        })
    }
    pub fn migrate_dispatches(&self) -> Result<()> {
        if self.actions.settings.read_only {
            self.store.read()?;
            return Ok(());
        }
        let jobs = self.tests.data()?;
        self.store.transaction(|state| {
            if !state["automatic_starts"].is_object() {
                state["automatic_starts"] = json!({});
            }
            for value in state["accounts"].as_object_mut().unwrap().values_mut() {
                if value.get("schedule_mode").is_none() {
                    value["schedule_mode"] = "smart".into();
                    value["min_interval_minutes"] = 5.into();
                    let anchor = parse_datetime(&value["last_result"]["completed_at"])
                        .or_else(|| {
                            parse_datetime(&value["next_at"]).map(|at| {
                                at - Duration::minutes(
                                    integer(&value["interval_minutes"]).unwrap_or(15),
                                )
                            })
                        })
                        .unwrap_or_else(|| self.now());
                    value["schedule_anchor_at"] = datetime(anchor).into();
                }
            }
            for job in jobs["jobs"].as_object().unwrap().values() {
                if job["automatic"] != true {
                    continue;
                }
                let Some(at) = parse_datetime(&job["started_at"]) else {
                    continue;
                };
                let id = integer(&job["account_id"])
                    .ok_or_else(Error::invalid)?
                    .to_string();
                if parse_datetime(&state["automatic_starts"][&id]["started_at"])
                    .is_none_or(|previous| previous < at)
                {
                    state["automatic_starts"][id] =
                        json!({"job_id":job["id"],"started_at":datetime(at)});
                }
            }
            Ok(())
        })
    }
    pub async fn commit_disposition(
        &self,
        id: i64,
        disposition: &Value,
        row: &Value,
    ) -> Result<()> {
        let confirmed = disposition["marked"] == true
            && disposition["schedule_verified"] == true
            && disposition["status"] == "completed";
        let mut merged = false;
        if confirmed {
            let task = self.monitor.credits.state(id)?;
            if truthy(&disposition["credit_episode"])
                && disposition["credit_episode"] == task["episode"]
                && task["model_test_job_id"] == disposition["job_id"]
            {
                merged = self
                    .monitor
                    .credits
                    .merge_disposition(id, string(&disposition["job_id"]), disposition, row)
                    .await?;
            }
        }
        self.store.transaction(|data| {
            let value = data["accounts"]
                .as_object_mut()
                .unwrap()
                .entry(id.to_string())
                .or_insert_with(defaults);
            if value["generation"] != disposition["generation"] {
                return Err(Error::new(409, "人工操作已替代自动处置"));
            }
            value["disposition"] = disposition.clone();
            value["status"] = "paused".into();
            value["reason"] = if disposition["schedule_verified"] == true {
                json!("已标记降智")
            } else {
                disposition
                    .get("reason")
                    .cloned()
                    .unwrap_or(json!("停调度待核对"))
            };
            if !merged && confirmed {
                if !data["disposition_notifications"].is_object() {
                    data["disposition_notifications"] = json!({});
                }
                let at = datetime(self.now());
                data["disposition_notifications"]
                    .as_object_mut()
                    .unwrap()
                    .entry(string(&disposition["job_id"]).to_owned())
                    .or_insert_with(|| {
                        json!({
                            "job_id": disposition["job_id"],
                            "account_id": id,
                            "account_name": safe_error(&row["name"], 120),
                            "account_type": row["type"],
                            "generation": disposition["generation"],
                            "model_id": disposition["model_id"],
                            "prediction": disposition["prediction"],
                            "basis": disposition.get("basis").cloned().unwrap_or(json!("first_group")),
                            "probability": disposition["probability"],
                            "engine":disposition["engine"],"sample_count":disposition["sample_count"],"group_indexes":disposition["group_indexes"],"group_predictions":disposition["group_predictions"],
                            "triggers": disposition.get("triggers").unwrap_or(&json!([])),
                            "created_at": at,
                            "next_at": at,
                            "attempts": 0,
                            "status": "queued"
                        })
                    });
            }
            Ok(())
        })?;
        self.log_receipt(string(&disposition["job_id"]), "disposition", disposition);
        Ok(())
    }
    fn notification_current(&self, event: &Value, current: &Value) -> Result<bool> {
        if !matches!(string(&event["status"]), "queued" | "retry")
            || current["generation"] != event["generation"]
        {
            return Ok(false);
        }
        let recovered = event["kind"] == "recovered";
        let receipt = &current[if recovered {
            "recheck_recovery"
        } else {
            "disposition"
        }];
        if receipt["job_id"] != event["job_id"] || receipt["status"] != "completed" {
            return Ok(false);
        }
        if recovered {
            let mark = self.mark(integer(&event["account_id"]).ok_or_else(Error::invalid)?)?;
            return Ok(receipt["mark_cleared"] == true
                && mark["marked"] == false
                && mark["version"] == event["mark_version"]);
        }
        Ok(true)
    }
    async fn deliver_one(&self, key: &str) -> Result<()> {
        let event = self.store.read()?["disposition_notifications"][key].clone();
        if !matches!(string(&event["status"]), "queued" | "retry") {
            return Ok(());
        }
        let due =
            parse_datetime(&event["next_at"]).ok_or_else(|| Error::new(503, "处置通知时间无效"))?;
        if due > self.now() {
            return Ok(());
        }
        let config = self.bark.runtime();
        if !config.valid {
            return Ok(());
        }
        let id = integer(&event["account_id"]).ok_or_else(Error::invalid)?;
        let reserved = self.store.transaction(|data| {
            let current = data["accounts"]
                .get(id.to_string())
                .cloned()
                .unwrap_or(json!({}));
            let event = &mut data["disposition_notifications"][key];
            if !config.enabled || !self.notification_current(event, &current)? {
                event["status"] = "suppressed".into();
                event["reason"] = if !config.enabled {
                    "disabled"
                } else {
                    "manual_intervention"
                }
                .into();
                return Ok(Value::Null);
            }
            let attempts = integer(&event["attempts"]).unwrap_or(0) + 1;
            event["attempts"] = attempts.into();
            event["next_at"] = datetime(
                self.now()
                    + Duration::seconds(
                        [5, 30, 120, 600][attempts.saturating_sub(1).clamp(0, 3) as usize],
                    ),
            )
            .into();
            Ok(event.clone())
        })?;
        if reserved.is_null() {
            return Ok(());
        }
        let latest = self.store.read()?["disposition_notifications"][key].clone();
        if !self.notification_current(&latest, &self.control(id)?)? {
            self.store.transaction(|data| {
                data["disposition_notifications"][key]["status"] = "suppressed".into();
                data["disposition_notifications"][key]["reason"] = "manual_intervention".into();
                Ok(())
            })?;
            return Ok(());
        }
        let (title, body) = notification_message(&reserved);
        let result = self
            .bark
            .push_with_config(
                title,
                &body,
                3,
                &json!({"level":"critical","sound":"alarm","group":"Sub2Ops 降智处置"}),
                &config,
            )
            .await;
        self.store.transaction(|data| {
            let saved = &mut data["disposition_notifications"][key];
            if !result.success && saved["status"] == "suppressed" {
                return Ok(());
            }
            if saved["attempts"] != reserved["attempts"] {
                return Ok(());
            }
            saved["status"] = if result.success { "delivered" } else { "retry" }.into();
            saved["delivered_at"] = if result.success {
                json!(datetime(self.now()))
            } else {
                Value::Null
            };
            saved["error_code"] = if result.success {
                Value::Null
            } else {
                json!(result.error_code)
            };
            Ok(())
        })?;
        self.audit("model_detection_notification",json!({"account_id":id,"job_id":key,"kind":event.get("kind").unwrap_or(&json!("degraded")),"status":if result.success {"delivered"}else {"retry"}}));
        Ok(())
    }
    pub async fn deliver_notifications(&self) -> Result<()> {
        if self.actions.settings.read_only {
            return Ok(());
        }
        let Ok(_guard) = self.notice_lock.try_lock() else {
            return Ok(());
        };
        let keys = self.store.read()?["disposition_notifications"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, value)| matches!(string(&value["status"]), "queued" | "retry"))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let results = stream::iter(keys.into_iter().map(|key| {
            let controller = self.clone();
            async move { controller.deliver_one(&key).await }
        }))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
        for result in results {
            result?;
        }
        Ok(())
    }
}
pub fn notification_message(event: &Value) -> (&'static str, String) {
    let causes = event["triggers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(string)
        .collect::<Vec<_>>();
    let labels = [
        ("error:", "上游错误"),
        ("slow:", "慢首字"),
        ("scheduled", "定时检测"),
        ("recovery", "额度恢复"),
        ("credit", "用卡恢复"),
    ]
    .into_iter()
    .filter(|(prefix, _)| causes.iter().any(|cause| cause.starts_with(prefix)))
    .map(|(_, label)| label)
    .collect::<Vec<_>>()
    .join("、");
    let probability = event["probability"]
        .as_f64()
        .map(|p| format!("{:.2}%", p * 100.0))
        .unwrap_or_else(|| "未知".into());
    let recovered = event["kind"] == "recovered";
    let mut body = format!(
        "账号：{} #{}（{}）\n触发：{}\n请求模型：{}\n{}指纹推测：{}\n",
        string(&event["account_name"]),
        integer(&event["account_id"]).unwrap_or(0),
        if event["account_type"] == "apikey" {
            "Key"
        } else {
            "OAuth"
        },
        if labels.is_empty() {
            "自动检测"
        } else {
            &labels
        },
        string(&event["model_id"]),
        if event["basis"] == "aggregate" || event["basis"] == "sample_family" {
            "最终汇总"
        } else {
            "首组"
        },
        event[if event["basis"] == "aggregate" || event["basis"] == "sample_family" {
            "prediction"
        } else {
            "first_group_prediction"
        }]
        .as_str()
        .filter(|v| !v.is_empty())
        .unwrap_or(string(&event["prediction"]))
    );
    if event["engine"] == crate::lm_detector::ENGINE {
        body.push_str(&format!(
            "检测源：LM Detector · {} 组\n",
            integer(&event["sample_count"]).unwrap_or(1)
        ));
        if event["basis"] == "sample_family" {
            for group in event["group_predictions"].as_array().into_iter().flatten() {
                body.push_str(&format!(
                    "第 {} 组跨家族证据：{}\n",
                    integer(&group["index"]).unwrap_or(0),
                    string(&group["prediction"])
                ));
            }
        }
    }
    if recovered {
        body.push_str(&format!(
            "本轮指纹推测：{}\n有效组数：{}\n调度保持{}\n",
            string(&event["prediction"]),
            integer(&event["valid_groups"]).unwrap_or(0),
            if event["schedulable"] == true {
                "开启"
            } else {
                "关闭"
            }
        ));
    }
    body.push_str(&format!(
        "匹配度：{probability}\n时间：{}",
        bark::beijing_time(&event["created_at"])
    ));
    (
        if recovered {
            "✅ Codex 检测恢复正常，已取消降智标记"
        } else {
            "⚠️ Codex 已判定降智，已停止调度"
        },
        body,
    )
}
