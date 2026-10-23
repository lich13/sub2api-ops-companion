use crate::{
    actions::Actions,
    audit,
    bark::Bark,
    capacity_alerts::{mark_view, CapacityAlerts},
    compat::{datetime, digest, integer, operation_versions, parse_datetime, pick, string, truthy},
    detection_quota,
    error::{Error, Result},
    key_fallback,
    model_downgrade::{bank_contains, is_downgrade, valid_report},
    model_target,
    model_tests::{ModelTests, ACTIVE},
    oauth_monitor::Monitor,
    store::Store,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use std::sync::Arc;

pub use crate::model_downgrade::LEGACY_TARGET as TARGET;
pub fn defaults() -> Value {
    json!({
        "enabled":false, "interval_minutes":15, "model_id":crate::settings::DEFAULT_DETECTION_MODEL, "model_override":null,
        "generation":0, "next_at":null, "status":"disabled", "reason":"",
        "job_id":null, "hold":false,
        "schedule_mode":"smart", "min_interval_minutes":5,
        "schedule_anchor_at":null, "schedule_idle_paused":false,
        "recent_request_count":null, "frequency_sampled_at":null
    })
}
pub fn version(value: &Value) -> String {
    digest(&pick(
        value,
        &[
            "enabled",
            "interval_minutes",
            "schedule_mode",
            "min_interval_minutes",
            "model_id",
            "model_override",
            "generation",
        ],
    ))
}
fn validate(value: &Value) -> Result<()> {
    if value["version"] != 1 || !value["consumed"].is_object() || !value["accounts"].is_object() {
        return Err(Error::new(503, "模型检测状态无效"));
    }
    for control in value["accounts"].as_object().unwrap().values() {
        if control["enabled"].as_bool().is_none()
            || control["hold"].as_bool().is_none()
            || control["generation"].as_i64().is_none()
            || !control["interval_minutes"]
                .as_i64()
                .is_some_and(|n| (1..=525600).contains(&n))
            || control
                .get("schedule_mode")
                .is_some_and(|v| v != "fixed" && v != "smart")
            || control
                .get("min_interval_minutes")
                .is_some_and(|v| !v.as_i64().is_some_and(|n| (5..=30).contains(&n)))
            || !crate::account_templates::MODEL_ID.is_match(string(&control["model_id"]))
            || (!control["model_override"].is_null()
                && !crate::account_templates::MODEL_ID.is_match(string(&control["model_override"])))
        {
            return Err(Error::new(503, "模型检测控制状态无效"));
        }
    }
    Ok(())
}
#[derive(Clone)]
pub struct Detection {
    pub actions: Actions,
    pub tests: ModelTests,
    pub monitor: Monitor,
    pub capacity: CapacityAlerts,
    pub bark: Bark,
    pub store: Store,
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    pub(crate) tick_lock: Arc<tokio::sync::Mutex<()>>,
    pub(crate) notice_lock: Arc<tokio::sync::Mutex<()>>,
    pub(crate) frequency_lock: Arc<tokio::sync::Mutex<()>>,
}
impl Detection {
    pub fn new(
        actions: Actions,
        tests: ModelTests,
        monitor: Monitor,
        capacity: CapacityAlerts,
        bark: Bark,
    ) -> Self {
        let store = Store::new(
            actions
                .settings
                .data_dir()
                .join("model-detection-state.json"),
            json!({"version":1,"accounts":{},"consumed":{}}),
            validate,
            actions.settings.read_only,
        );
        Self {
            actions,
            tests,
            monitor,
            capacity,
            bark,
            store,
            clock: Arc::new(Utc::now),
            tick_lock: Arc::new(tokio::sync::Mutex::new(())),
            notice_lock: Arc::new(tokio::sync::Mutex::new(())),
            frequency_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
    pub fn model_id(&self) -> Result<String> {
        crate::settings::detection_model(
            &self.monitor.config.current()?["model_detection_model_id"],
        )
    }
    pub(crate) fn effective_model(&self, value: &Value) -> Result<String> {
        match value["model_override"].as_str() {
            Some(model) => Ok(model.to_owned()),
            None => self.model_id(),
        }
    }
    fn effective_version(value: &Value, model: &str) -> String {
        let mut effective = value.clone();
        effective["model_id"] = model.into();
        effective
            .as_object_mut()
            .unwrap()
            .entry("schedule_mode")
            .or_insert(json!("smart"));
        effective
            .as_object_mut()
            .unwrap()
            .entry("min_interval_minutes")
            .or_insert(json!(5));
        version(&effective)
    }
    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }
    pub fn control(&self, id: i64) -> Result<Value> {
        let mut value = self.store.read()?["accounts"]
            .get(id.to_string())
            .cloned()
            .unwrap_or_else(defaults);
        value
            .as_object_mut()
            .unwrap()
            .entry("schedule_mode")
            .or_insert(json!("smart"));
        value
            .as_object_mut()
            .unwrap()
            .entry("min_interval_minutes")
            .or_insert(json!(5));
        Ok(value)
    }
    pub fn mark(&self, id: i64) -> Result<Value> {
        Ok(mark_view(
            id,
            &self.capacity.store.snapshot()?["marks"][id.to_string()],
        ))
    }
    pub fn held(&self, id: i64) -> bool {
        self.control(id).map(|v| v["hold"] == true).unwrap_or(true)
    }
    pub fn recovery_held(&self, id: i64) -> bool {
        self.held(id) || self.mark(id).map(|v| v["marked"] == true).unwrap_or(true)
    }
    pub fn credit_control_version(&self, id: i64) -> Result<Value> {
        Ok(digest(&json!([
            self.control(id)?["generation"],
            self.mark(id)?["version"]
        ]))
        .into())
    }
    pub fn update(&self, id: i64, changes: Value) -> Result<Value> {
        self.store.transaction(|data| {
            let value = data["accounts"]
                .as_object_mut()
                .unwrap()
                .entry(id.to_string())
                .or_insert_with(defaults);
            value
                .as_object_mut()
                .unwrap()
                .extend(changes.as_object().cloned().ok_or_else(Error::invalid)?);
            Ok(value.clone())
        })
    }
    pub fn audit(&self, event: &str, value: Value) {
        audit::write(
            &self.actions.settings.audit_path,
            event,
            value,
            self.actions.settings.read_only,
        );
    }
    pub fn view(&self, id: i64) -> Result<Value> {
        let data = self.store.read()?;
        let mut value = data["accounts"]
            .get(id.to_string())
            .cloned()
            .unwrap_or_else(defaults);
        value
            .as_object_mut()
            .unwrap()
            .entry("schedule_mode")
            .or_insert(json!("smart"));
        value
            .as_object_mut()
            .unwrap()
            .entry("min_interval_minutes")
            .or_insert(json!(5));
        let model = self.effective_model(&value)?;
        value["version"] = Self::effective_version(&value, &model).into();
        value["model_id"] = model.into();
        value["model_source"] = if value["model_override"].is_string() {
            "account"
        } else {
            "global"
        }
        .into();
        value["model_override"] = value["model_override"].clone();
        value["global_model_id"] = self.model_id()?.into();
        value["account_id"] = id.into();
        value["last_automatic_started_at"] =
            data["automatic_starts"][id.to_string()]["started_at"].clone();
        value["next_allowed_at"] = self.next_allowed_at(id)?;
        value["recent_request_count"] = value["recent_request_count"].clone();
        value["effective_interval_minutes"] =
            crate::detection_schedule::effective_interval(&value).into();
        value["disposition_notification"] =
            data["disposition_notifications"][string(&value["disposition"]["job_id"])].clone();
        value["recovery_notification"] =
            data["disposition_notifications"][string(&value["recheck_recovery"]["job_id"])].clone();
        Ok(value)
    }
    pub fn human_control(&self, id: i64, release: bool) -> Result<()> {
        self.store.transaction(|data| {
            let value = data["accounts"]
                .as_object_mut()
                .unwrap()
                .entry(id.to_string())
                .or_insert_with(defaults);
            value["generation"] = json!(integer(&value["generation"]).unwrap_or(0) + 1);
            value.as_object_mut().unwrap().extend(
                json!({
                    "pending_request_id": null,
                    "status": "paused",
                    "reason": "人工操作后重新核对",
                    "next_at": null,
                })
                .as_object()
                .unwrap()
                .clone(),
            );
            if release {
                value["hold"] = false.into();
                if truthy(&value["disposition"]) {
                    value["disposition"]["status"] = "overridden".into();
                    value["disposition"]["reason"] = "人工操作已优先处理".into();
                }
            }
            Ok(())
        })
    }
    pub async fn save(&self, id: i64, payload: &Value) -> Result<Value> {
        let request = detection_request(payload)?;
        let row = model_target::account(&self.actions, id).await?;
        if request.get("model_override").is_some() && row["type"] != "apikey" {
            return Err(Error::new(422, "仅 OpenAI Key 账号可单独指定检测模型"));
        }
        let current = self.control(id)?;
        let previous_model = self.effective_model(&current)?;
        let mut candidate = current.clone();
        for field in ["schedule_mode", "min_interval_minutes"] {
            if let Some(value) = request.get(field) {
                candidate[field] = value.clone();
            }
        }
        if let Some(model) = request.get("model_override") {
            candidate["model_override"] = model.clone();
        }
        let model = self.effective_model(&candidate)?;
        let allowed = if request["enabled"] == true {
            self.actions.model_allowed(&row, &model).await
        } else {
            Ok(true)
        };
        let _config = self.monitor.config.lock.lock().await;
        if self.effective_model(&candidate)? != model
            || self.effective_model(&current)? != previous_model
        {
            return Err(Error::new(409, "自动检测模型已变化，请重新读取"));
        }
        if !allowed? {
            return Err(Error::new(422, "所选模型不在当前账号上游模型目录中"));
        }
        let _guard = self.actions.locks.control(id).await;
        self.store.transaction(|data| {
            let value = data["accounts"]
                .as_object_mut()
                .unwrap()
                .entry(id.to_string())
                .or_insert_with(defaults);
            if Self::effective_version(value, &self.effective_model(value)?)
                != request["expected_version"]
            {
                return Err(Error::new(409, "检测设置已变化，请重新读取"));
            }
            value["generation"] = json!(integer(&value["generation"]).unwrap_or(0) + 1);
            let enabled = request["enabled"] == true;
            let smart = crate::detection_schedule::smart(&candidate);
            let next_at = if enabled && !smart {
                json!(datetime(
                    self.now() + Duration::minutes(integer(&request["interval_minutes"]).unwrap())
                ))
            } else {
                Value::Null
            };
            let changes = json!({
                "enabled": request["enabled"],
                "interval_minutes": request["interval_minutes"],
                "schedule_mode": candidate["schedule_mode"],
                "min_interval_minutes": candidate["min_interval_minutes"],
                "schedule_anchor_at": datetime(if value["enabled"] == true || truthy(&value["last_result"]) { parse_datetime(&value["schedule_anchor_at"]).or_else(|| parse_datetime(&value["last_result"]["completed_at"])).unwrap_or_else(|| self.now()) } else { self.now() }),
                "schedule_idle_paused": if crate::detection_schedule::smart(&candidate) { value["schedule_idle_paused"].clone() } else { json!(false) },
                "model_override": candidate["model_override"],
                "pending_request_id": null,
                "candidate_blocked": false,
                "candidate_recheck_at": null,
                "status": if enabled { "waiting" } else { "disabled" },
                "reason": "",
                "next_at": next_at,
            });
            value
                .as_object_mut()
                .unwrap()
                .extend(changes.as_object().unwrap().clone());
            Ok(())
        })?;
        if crate::detection_schedule::smart(&candidate) && request["enabled"] == true {
            self.refresh_call_frequency().await?;
            self.update_smart_schedule(id, &self.control(id)?)?;
        }
        self.view(id)
    }
    pub async fn reason(&self, row: &Value, mark: &Value, recheck: bool) -> Result<String> {
        if !row.is_object()
            || row["platform"] != "openai"
            || !matches!(string(&row["type"]), "oauth" | "apikey")
            || truthy(&row["deleted_at"])
        {
            return Ok("账号已删除或不支持检测".into());
        }
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        if recheck && (self.control(id)?["enabled"] != true || mark["marked"] != true) {
            return Ok("定时复查设置或降智标记已变化".into());
        }
        if mark["marked"] == true && !recheck {
            return Ok("已标记降智".into());
        }
        if row["status"] != "active" {
            return Ok("账号停用或认证异常".into());
        }
        if row["schedulable"] != true {
            return Ok("调度已关闭".into());
        }
        let state = self.monitor.state()?;
        let result = &state["oauth_results"][id.to_string()];
        if row["type"] == "oauth" {
            let auth = &state["scheduler"][id.to_string()]["quota_query"]["auth_fingerprint"];
            if truthy(auth) {
                let identity = if row.get("credentials").is_some() {
                    row.clone()
                } else {
                    self.monitor.row(id).await?
                };
                if auth == &json!(crate::oauth_admission::credential_fingerprint(&identity)) {
                    return Ok("认证异常，等待凭据更新".into());
                }
            }
        }
        Ok(detection_quota::reason(row, self.now(), result).into())
    }
    pub async fn guard(&self, job: &Value) -> Result<Value> {
        let id = integer(&job["account_id"]).ok_or_else(Error::invalid)?;
        let row = self.actions.account(id, None).await?;
        let value = self.control(id)?;
        let mark = self.mark(id)?;
        if value["generation"] != job["detection_generation"]
            || mark["version"] != job["detection_mark_version"]
        {
            return Err(Error::new(409, "检测已被新的人工操作替代"));
        }
        self.check_smart_dispatch(job).await?;
        let context = &job["recovery_context"];
        let recheck = job["degradation_recheck"] == true;
        if recheck && self.recheck_target_changed(&row, &value, job) {
            return Err(Error::new(409, "复查目标或调度状态已变化"));
        }
        let reason = if truthy(context) {
            self.recovery_reason(
                &row,
                context,
                truthy(&context["consume"]) && !truthy(&job["attempts"]),
            )
            .await?
        } else {
            self.reason(&row, &mark, recheck).await?
        };
        if !reason.is_empty() {
            return Err(Error::new(409, reason));
        }
        Ok(row)
    }
    pub fn clue_status(
        &self,
        causes: &[String],
        status: &str,
        reason: &str,
        job: Option<&str>,
    ) -> Result<()> {
        let clues = causes
            .iter()
            .filter(|cause| cause.starts_with("error:") || cause.starts_with("slow:"))
            .collect::<Vec<_>>();
        if clues.is_empty() {
            return Ok(());
        }
        self.capacity.store.transaction(|data| {
            for field in ["detection_clues", "notifications"] {
                if !data[field].is_object() {
                    data[field] = json!({});
                }
            }
            for key in &clues {
                let record = json!({
                    "status": status,
                    "reason": reason,
                    "job_id": job,
                    "updated_at": datetime(self.now()),
                });
                data["detection_clues"][key.as_str()] = record.clone();
                if let Some(key) = key.strip_prefix("error:") {
                    data["notifications"][key] = record;
                }
            }
            while data["detection_clues"].as_object().unwrap().len() > 10000 {
                let key = data["detection_clues"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .next()
                    .unwrap()
                    .clone();
                data["detection_clues"]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove(&key);
            }
            Ok(())
        })?;

        self.audit(
            "model_detection_clue",
            json!({"triggers":clues,"result":status,"reason":reason,"job_id":job}),
        );
        Ok(())
    }
    pub async fn trigger(&self, id: i64, causes: &[String]) -> Result<Value> {
        let value = self.control(id)?;
        if let Some(request) = value["pending_request_id"]
            .as_str()
            .filter(|v| !v.is_empty())
        {
            let jobs = self.tests.data()?;
            if let Some(job_id) = jobs["requests"][request].as_str() {
                let job = self.tests.get(job_id)?;
                if job["account_id"] != id || job["automatic"] != true {
                    return Err(Error::new(503, "自动检测幂等状态无效"));
                }
                self.update(id, json!({"job_id":job["id"],"pending_request_id":null,"status":job["status"],"reason":job["error"]}))?;
                return Ok(job);
            }
        }
        if let Some(job) = value["job_id"]
            .as_str()
            .and_then(|id| self.tests.get(id).ok())
            .filter(|job| ACTIVE.contains(&string(&job["status"])))
        {
            self.clue_status(causes, "ignored", "已有检测排队或运行", None)?;
            return Ok(job);
        }
        let latest = self.tests.latest(id)?;
        if ACTIVE.contains(&string(&latest["status"])) {
            self.clue_status(causes, "ignored", "已有模型测试排队或运行", None)?;
            return Ok(latest);
        }
        if let Some(allowed) =
            parse_datetime(&self.next_allowed_at(id)?).filter(|at| *at > self.now())
        {
            if causes.iter().any(|v| v == "scheduled") {
                self.update(
                    id,
                    json!({
                        "status":"waiting", "next_at":datetime(allowed), "reason":"等待自动检测冷却"
                    }),
                )?;
            }
            self.audit(
                "model_detection_cooldown",
                json!({
                    "account_id":id, "triggers":causes,
                    "result":if causes.iter().any(|v|v=="scheduled") {"deferred"}else {"ignored"}
                }),
            );
            self.clue_status(causes, "ignored", "五分钟冷却中", None)?;
            return Ok(Value::Null);
        }
        if causes.len() == 1 && causes[0] == "scheduled" && crate::detection_schedule::smart(&value)
        {
            self.refresh_call_frequency().await?;
            let value = self.control(id)?;
            let due = self.update_smart_schedule(id, &value)?;
            if due.is_none_or(|at| at > self.now()) {
                return Ok(Value::Null);
            }
        }
        let row = self.actions.account(id, None).await?;
        let mark = self.mark(id)?;
        let recheck = causes.iter().any(|v| v == "scheduled")
            && value["enabled"] == true
            && mark["marked"] == true;
        let reason = self.reason(&row, &mark, recheck).await?;
        if !reason.is_empty() {
            self.update(
                id,
                json!({"status":"paused","reason":reason,"next_at":null}),
            )?;
            self.clue_status(causes, "ignored", &reason, None)?;
            return Ok(Value::Null);
        }
        let request = value["pending_request_id"]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let model = self.effective_model(&value)?;
        let allowed = self.actions.model_allowed(&row, &model).await?;
        let bank = self.tests.bank.snapshot();
        let ready = bank_contains(&bank.0, TARGET);
        if !allowed || !ready {
            let reason = if !allowed {
                "所选模型不在当前账号上游模型目录中"
            } else {
                "当前指纹库缺少降智判定模型"
            };
            self.update(
                id,
                json!({
                    "status":"paused", "reason":reason, "next_at":null, "candidate_blocked":true,
                    "candidate_recheck_at":datetime(self.now()+Duration::minutes(1))
                }),
            )?;
            self.clue_status(causes, "failed", reason, None)?;
            return Ok(Value::Null);
        }
        self.update(
            id,
            json!({
                "pending_request_id": request,
                "candidate_blocked": false,
                "candidate_recheck_at": null,
                "triggers": causes,
                "status": "queued",
                "reason": "等待检测",
                "next_at": null,
            }),
        )?;

        let _config = self.monitor.config.lock.lock().await;
        if self.effective_model(&self.control(id)?)? != model {
            return Err(Error::new(409, "自动检测模型已变化，等待重新核对"));
        }
        let job = self
            .tests
            .start(
                id,
                &json!({
                    "model_id": model,
                    "expected_version": "0".repeat(64),
                    "expected_operation_version": operation_versions(&row, false)["model_test"],
                    "request_id": request,
                    "concurrency": 1,
                }),
                Some(&json!({
                        "detection_generation": value["generation"],
                        "detection_mark_version": mark["version"],
                        "triggers": causes,
                        "degradation_recheck": recheck,
                        "recheck_account_version": if recheck { operation_versions(&row, false)["model_test"].clone()} else { Value::Null},
                        "recheck_schedulable": if recheck { row["schedulable"].clone()} else { Value::Null},
                    })),
            )
            .await?;

        self.update(id,json!({"job_id":job["id"],"pending_request_id":null,"status":job["status"],"reason":job["error"]}))?;
        self.clue_status(causes, "testing", "", job["id"].as_str())?;
        Ok(job)
    }
    pub async fn verdict(&self, job: &Value, report: &Value) -> Result<Value> {
        // The automatic caller supplies the first group's independent report.
        if !valid_report(report)
            || !is_downgrade(
                string(&job["requested_model"]),
                string(&report["prediction"]),
            )
        {
            return Ok(Value::Null);
        }
        self.guard(job).await?;
        if job["degradation_recheck"] == true {
            return Ok(
                json!({"kind":"recheck","status":"still_degraded","mark_cleared":false,"reason":"仍符合降智特征"}),
            );
        }
        let id = integer(&job["account_id"]).ok_or_else(Error::invalid)?;
        {
            let _control = self.actions.locks.control(id).await;
            let value = self.control(id)?;
            let mark = self.mark(id)?;
            if value["generation"] != job["detection_generation"]
                || mark["version"] != job["detection_mark_version"]
            {
                return Err(Error::new(409, "人工操作已变化，旧检测不再处置"));
            }
            let row = self.actions.account(id, None).await?;
            let reason = if truthy(&job["recovery_context"]) {
                self.recovery_reason(&row, &job["recovery_context"], false)
                    .await?
            } else {
                self.reason(&row, &mark, false).await?
            };
            if !reason.is_empty() {
                return Err(Error::new(409, reason));
            }
            let mut disposition = json!({
                "status": "pending",
                "job_id": job["id"],
                "generation": value["generation"],
                "mark_version": mark["version"],
                "marked": false,
                "schedule_verified": false,
                "created_at": datetime(self.now()),
                "model_id": job["requested_model"],
                "prediction": report["prediction"],
                "probability": report["probability"],
                "triggers": job["triggers"],
            });

            if job["recovery_context"]["kind"] == "credit" {
                disposition["credit_episode"] = job["recovery_context"]["fingerprint"].clone();
                let task = self.monitor.credits.state(id)?;
                if truthy(&task["policy_version"]) {
                    self.monitor.credits.save(
                        id,
                        &task,
                        json!({"degradation_pending":job["id"]}),
                    )?;
                }
            }
            self.update(
                id,
                json!({
                    "hold":true, "disposition":disposition, "recheck_recovery":null,
                    "status":"handling", "reason":"正在标记并停止调度"
                }),
            )?;
        }
        self.finish_disposition(id).await
    }
    pub async fn finish_disposition(&self, id: i64) -> Result<Value> {
        let _control = self.actions.locks.control(id).await;
        let value = self.control(id)?;
        let mut d = value["disposition"].clone();
        if !matches!(
            string(&d["status"]),
            "pending" | "marked" | "writing" | "checking"
        ) {
            return Ok(d);
        }
        if d["generation"] != value["generation"] {
            d["status"] = "overridden".into();
            d["reason"] = "人工操作已变化".into();
            self.update(id, json!({"disposition":d}))?;
            return Ok(d);
        }
        let row = self.actions.account(id, None).await?;
        if row["platform"] != "openai" || !matches!(string(&row["type"]), "oauth" | "apikey") {
            d["status"] = "needs_confirmation".into();
            d["reason"] = "账号资格已变化".into();
            self.update(id, json!({"disposition":d}))?;
            return Ok(d);
        }
        if d["marked"] != true {
            let current = self.mark(id)?;
            let state = self.capacity.store.snapshot()?["marks"][id.to_string()].clone();
            if current["version"] != d["mark_version"] && state["detection_job_id"] != d["job_id"] {
                d["status"] = "needs_confirmation".into();
                d["reason"] = "降智标记已变化".into();
                self.update(id, json!({"disposition":d}))?;
                return Ok(d);
            }
            if current["marked"] != true {
                self.capacity.store.set_mark(
                    id,
                    true,
                    string(&current["version"]),
                    self.now(),
                    d["job_id"].as_str(),
                )?;
            }
            d["marked"] = true.into();
            d["status"] = "marked".into();
            self.update(id, json!({"disposition":d}))?;
        }
        if row["type"] == "oauth" && d["recovery_revoked"] != true {
            self.monitor
                .store
                .manual_control(&row, false, self.now(), "model_detection")?;
            d["recovery_revoked"] = true.into();
            self.update(id, json!({"disposition":d}))?;
        }
        if row["schedulable"] == false {
            d["status"] = "completed".into();
            d["schedule_verified"] = true.into();
            d["reason"] = "".into();
        } else if matches!(string(&d["status"]), "writing" | "checking") {
            d["status"] = "needs_confirmation".into();
            d["reason"] = "停调度结果未确认，未重复写入".into();
        } else {
            let key = self.monitor.store.admin_token()?;
            if key.is_empty() {
                d["status"] = "needs_confirmation".into();
                d["reason"] = "管理员授权不可用".into();
            } else {
                d["status"] = "writing".into();
                self.update(id, json!({"disposition":d}))?;
                let _ = key_fallback::set_schedulable(&self.actions, id, false, &key, 3).await;
                let live = self.actions.account(id, None).await?;
                if live["schedulable"] == false {
                    d["status"] = "completed".into();
                    d["schedule_verified"] = true.into();
                    d["reason"] = "".into();
                } else {
                    d["status"] = "checking".into();
                    d["reason"] = "停调度待核对".into();
                }
            }
        }
        self.commit_disposition(id, &d, &row).await?;
        self.audit(
            "model_detection_disposition",
            json!({
                "account_id": id,
                "job_id": d["job_id"],
                "marked": d["marked"],
                "schedule_verified": d["schedule_verified"],
                "status": d["status"],
            }),
        );

        Ok(d)
    }
}
pub fn detection_request(payload: &Value) -> Result<Value> {
    let object = payload.as_object().ok_or_else(Error::invalid)?;
    if object.keys().any(|key| {
        ![
            "expected_version",
            "enabled",
            "interval_minutes",
            "schedule_mode",
            "min_interval_minutes",
            "model_id",
            "model_override",
        ]
        .contains(&key.as_str())
    }) || payload["expected_version"]
        .as_str()
        .is_none_or(|v| v.chars().count() != 64)
        || !payload["enabled"].is_boolean()
    {
        return Err(Error::invalid());
    }
    let mut value = payload.clone();
    if value
        .get("schedule_mode")
        .is_some_and(|v| v != "fixed" && v != "smart")
        || value
            .get("min_interval_minutes")
            .is_some_and(|v| !v.as_i64().is_some_and(|n| (5..=30).contains(&n)))
    {
        return Err(Error::invalid());
    }
    if let Some(model) = value.get_mut("model_override") {
        if !model.is_null() {
            let normalized = model.as_str().map(str::trim).ok_or_else(Error::invalid)?;
            if !crate::account_templates::MODEL_ID.is_match(normalized) {
                return Err(Error::invalid());
            }
            *model = normalized.into();
        }
    }
    if value
        .as_object_mut()
        .unwrap()
        .shift_remove("model_id")
        .is_some()
    {
        eprintln!("model_detection_diagnostic level=debug reason=legacy_model_ignored");
    }
    if value.get("interval_minutes").is_none() {
        value["interval_minutes"] = 15.into();
    }
    if !value["interval_minutes"]
        .as_i64()
        .is_some_and(|v| (1..=525600).contains(&v))
    {
        return Err(Error::invalid());
    }
    Ok(value)
}
