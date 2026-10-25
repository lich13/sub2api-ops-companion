use crate::{
    actions::Actions,
    compat::{integer, operation_versions, parse_datetime, string, truthy},
    error::{Error, Result},
    fingerprint_bank::FingerprintBank,
    model_target,
    model_test_stream::Target,
    modeltrace,
    store::Store,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

pub const ACTIVE: &[&str] = &["queued", "running", "retrying"];
pub fn error_text(code: &str) -> &'static str {
    match code {
        "auth_or_quota" => "认证失败或额度不足",
        "rate_limited" => "账号受到限流",
        "invalid_model_or_request" => "模型不可用或请求不兼容",
        "upstream_error" => "上游请求失败",
        "output_limit" => "输出超过保护上限",
        "inconsistent_stream" => "响应内容不一致",
        "incomplete_stream" => "响应未完整结束",
        "invalid_stream" => "响应格式无效",
        "incomplete_response" => "上游明确结束但响应不完整",
        "protocol_mismatch" => "上游响应不是受支持的模型协议",
        "network_error" => "连接中断",
        "account_changed" => "账号配置已变化",
        _ => "测试无法完成",
    }
}
pub trait ModelHooks: Send + Sync {
    fn guard(&self, job: Value) -> BoxFuture<'_, Result<Value>>;
    fn next_allowed_at(&self, id: i64) -> Result<Value>;
    fn register_dispatch(&self, job: &Value) -> Result<()>;
    fn prepare_recovery(&self, job: Value) -> BoxFuture<'_, Result<Value>>;
    fn verdict(&self, job: Value, report: Value) -> BoxFuture<'_, Result<Value>>;
}
pub(crate) struct Context {
    pub samples: Mutex<BTreeMap<usize, String>>,
    pub challenges: Vec<(usize, String)>,
    pub snapshot: Arc<(Value, Value)>,
    pub lm: Option<Arc<crate::lm_detector::Snapshot>>,
    pub row: Value,
    pub owner: Value,
    pub target: Target,
}
impl Context {
    pub fn analyze(&self, samples: &BTreeMap<usize, String>) -> Result<Value> {
        match &self.lm {
            Some(lm) => lm.analyze(samples, &self.challenges),
            None => modeltrace::analyze(
                &samples.values().cloned().collect::<Vec<_>>(),
                &self.snapshot,
            ),
        }
    }
}
#[derive(Clone)]
pub struct ModelTests {
    pub actions: Actions,
    pub store: Store,
    pub bank: FingerprintBank,
    pub lm_bank: crate::lm_detector::Bank,
    pub hooks: Arc<dyn ModelHooks>,
    pub budget: Arc<Semaphore>,
    pub(crate) clock: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
    pub(crate) tasks: Arc<Mutex<BTreeMap<String, tokio::task::JoinHandle<()>>>>,
    pub(crate) contexts: Arc<Mutex<BTreeMap<String, Arc<Context>>>>,
    start: Arc<AsyncMutex<()>>,
    jobs: Arc<Semaphore>,
    pub(crate) closing: Arc<AtomicBool>,
    #[cfg(any(test, feature = "test-fixtures"))]
    test_endpoint: Arc<Mutex<Option<String>>>,
}
fn validate(value: &Value) -> Result<()> {
    let error = || Error::new(503, "模型测试状态损坏");
    if !value.is_object()
        || value["version"] != 1
        || value["accounts"].as_object().is_none_or(|rows| {
            rows.values().any(|v| {
                !v.is_object() || string(&v["id"]).is_empty() || v["account_id"].as_i64().is_none()
            })
        })
        || value.get("requests").is_some_and(|v| !v.is_object())
        || value.get("jobs").is_some_and(|v| {
            v.as_object().is_none_or(|rows| {
                rows.values().any(|v| {
                    !v.is_object()
                        || string(&v["id"]).is_empty()
                        || !crate::detection_logs::valid_summary(&v["result_summary"])
                })
            })
        })
    {
        return Err(error());
    }
    Ok(())
}
fn normalized(mut value: Value) -> Value {
    if value.get("requests").is_none() {
        value["requests"] = json!({});
    }
    if value.get("jobs").is_none() {
        value["jobs"] = value["accounts"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, v)| (string(&v["id"]).to_owned(), v.clone()))
            .collect::<serde_json::Map<_, _>>()
            .into();
    }
    value
}
fn interruption(job: &Value, error: &str, at: Value) -> Value {
    let mut changes = json!({"status":"interrupted","error":error,"completed_at":at,"can_retry":false,"uncertain":true});
    if job["phase"] == "tokenizer" {
        let mut tokenizer = job["tokenizer"].as_object().cloned().unwrap_or_default();
        tokenizer.insert("status".into(), "interrupted".into());
        tokenizer.insert(
            "error".into(),
            "服务中断，数字结果已保留；请重新测试以再次探测".into(),
        );
        changes["tokenizer"] = tokenizer.into();
        changes["phase"] = "finished".into();
    }
    changes
}
impl ModelTests {
    pub fn new(
        actions: Actions,
        bank: FingerprintBank,
        hooks: Arc<dyn ModelHooks>,
        budget: Arc<Semaphore>,
    ) -> Self {
        let store = Store::new(
            actions.settings.data_dir().join("model-test-results.json"),
            json!({"version":1,"accounts":{},"requests":{},"jobs":{}}),
            validate,
            actions.settings.read_only,
        );
        Self {
            lm_bank: crate::lm_detector::Bank::new(
                actions.settings.clone(),
                actions.supervisor.clone(),
            ),
            actions,
            store,
            bank,
            hooks,
            budget,
            clock: Arc::new(chrono::Utc::now),
            tasks: Arc::new(Mutex::new(BTreeMap::new())),
            contexts: Arc::new(Mutex::new(BTreeMap::new())),
            start: Arc::new(AsyncMutex::new(())),
            jobs: Arc::new(Semaphore::new(2)),
            closing: Arc::new(AtomicBool::new(false)),
            #[cfg(any(test, feature = "test-fixtures"))]
            test_endpoint: Arc::new(Mutex::new(None)),
        }
    }
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn set_test_endpoint(&self, endpoint: &str) -> Result<()> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| Error::invalid())?;
        if url.scheme() != "http"
            || !matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::invalid());
        }
        *self
            .test_endpoint
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(endpoint.into());
        Ok(())
    }
    pub fn data(&self) -> Result<Value> {
        Ok(normalized(self.store.read()?))
    }
    pub fn transaction<T>(&self, change: impl FnOnce(&mut Value) -> Result<T>) -> Result<T> {
        self.store.transaction(|data| {
            *data = normalized(data.clone());
            change(data)
        })
    }
    pub fn public(job: &Value) -> Value {
        let mut job = job.clone();
        if let Some(object) = job.as_object_mut() {
            object.shift_remove("target_signature");
            object.shift_remove("owner_signature");
        }
        if ACTIVE.contains(&string(&job["status"])) && job["status"] != "queued" {
            if let Some(start) = parse_datetime(&job["started_at"]) {
                job["duration_ms"] = chrono::Utc::now()
                    .signed_duration_since(start)
                    .num_milliseconds()
                    .max(0)
                    .into();
            }
        }
        job
    }
    pub fn get(&self, id: &str) -> Result<Value> {
        if id.len() != 32
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::new(404, "测试结果不存在"));
        }
        let state = self.data()?;
        state["jobs"]
            .get(id)
            .map(Self::public)
            .ok_or_else(|| Error::new(404, "测试结果不存在"))
    }
    pub fn latest(&self, id: i64) -> Result<Value> {
        Ok(self.data()?["accounts"]
            .get(id.to_string())
            .map(Self::public)
            .unwrap_or(Value::Null))
    }
    pub fn update(&self, id: &str, changes: Value) -> Result<Value> {
        self.transaction(|data| {
            let job = data["jobs"]
                .get_mut(id)
                .ok_or_else(|| Error::new(409, "任务已被替换"))?;
            job.as_object_mut()
                .ok_or_else(Error::internal)?
                .extend(changes.as_object().cloned().ok_or_else(Error::invalid)?);
            crate::detection_logs::preserve_summary(job);
            let job = job.clone();
            let aid = integer(&job["account_id"])
                .ok_or_else(Error::internal)?
                .to_string();
            if data["accounts"][&aid]["id"] == id {
                data["accounts"][aid] = job.clone();
            }
            Ok(job)
        })
    }
    pub fn startup(&self) -> Result<()> {
        self.data()?;
        if self.actions.settings.read_only {
            return Ok(());
        }
        self.transaction(|data| {
            let mut changed = vec![];
            for job in data["jobs"].as_object_mut().unwrap().values_mut() {
                if matches!(string(&job["status"]), "running" | "retrying") {
                    let changes =
                        interruption(job, "服务重启，测试已中断", crate::compat::now().into());
                    job.as_object_mut()
                        .unwrap()
                        .extend(changes.as_object().unwrap().clone());
                    crate::detection_logs::preserve_summary(job);
                    changed.push(job.clone());
                }
            }
            for job in changed {
                let aid = integer(&job["account_id"])
                    .ok_or_else(Error::internal)?
                    .to_string();
                if data["accounts"][&aid]["id"] == job["id"] {
                    data["accounts"][aid] = job;
                }
            }
            Ok(())
        })
    }
    pub fn resume(&self) -> Result<()> {
        if self.actions.settings.read_only
            || self.closing.load(Ordering::Acquire)
            || self.actions.supervisor.is_stopping()
        {
            return Ok(());
        }
        for job in self.data()?["jobs"]
            .as_object()
            .unwrap()
            .values()
            .filter(|job| job["status"] == "queued")
        {
            self.launch(job.clone(), None)?;
        }
        Ok(())
    }
    pub async fn start(
        &self,
        id: i64,
        payload: &Value,
        automatic: Option<&Value>,
    ) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        if self.closing.load(Ordering::Acquire) || self.actions.supervisor.is_stopping() {
            return Err(Error::new(503, "服务正在停止"));
        }
        let _guard = self.start.lock().await;
        let payload = model_request(payload)?;
        if automatic.is_some_and(|auto| {
            payload["tokenizer_probe"] == true
                || payload["engine"] == crate::lm_detector::ENGINE
                    && (auto["triggers"] != json!(["scheduled"])
                        || auto["recovery_context"].is_object())
        }) {
            return Err(Error::new(
                422,
                "新检测源仅支持手动和定时检测，分词器仅支持手动检测",
            ));
        }
        let request = string(&payload["request_id"]);
        let model = string(&payload["model_id"]);
        let data = self.data()?;
        if let Some(previous) = data["requests"].get(request) {
            let old = self.get(string(previous))?;
            let same_recovery = automatic.is_some_and(|auto| {
                old["automatic"] == true
                    && old["recovery_context"].is_object()
                    && old["detection_generation"] == auto["detection_generation"]
                    && [
                        "account_id",
                        "kind",
                        "fingerprint",
                        "control_generation",
                        "method_generation",
                        "attempt",
                    ]
                    .iter()
                    .all(|key| old["recovery_context"][*key] == auto["recovery_context"][*key])
            });
            if old["account_id"] != id
                || old["requested_model"] != model && !same_recovery
                || old.get("concurrency").unwrap_or(&json!(1)) != &payload["concurrency"]
                || old.get("engine").unwrap_or(&json!("modeltrace")) != &payload["engine"]
                || old.get("tokenizer_probe").unwrap_or(&json!(false))
                    != &payload["tokenizer_probe"]
                || payload["engine"] == crate::lm_detector::ENGINE
                    && old["planned_groups"] != payload["sample_count"]
            {
                return Err(Error::new(
                    409,
                    json!({"code":"idempotency_conflict","message":"请求 ID 已用于其他测试"}),
                ));
            }
            return Ok(old);
        }
        let row = model_target::account(&self.actions, id).await?;
        let live = self.actions.account(id, None).await?;
        let conflict = if truthy(&payload["expected_operation_version"]) {
            operation_versions(&live)["model_test"] != payload["expected_operation_version"]
        } else {
            crate::compat::account_version(&live) != payload["expected_version"]
        };
        let (target, owner) = model_target::target(&self.actions, &row, model).await?;
        let job_id = uuid::Uuid::new_v4().simple().to_string();
        let auto = automatic.cloned().unwrap_or(json!({}));
        let groups = if payload["engine"] == crate::lm_detector::ENGINE {
            integer(&payload["sample_count"]).unwrap_or(1)
        } else {
            integer(&auto["planned_groups"]).unwrap_or(3).clamp(1, 3)
        } as usize;
        let lm = if payload["engine"] == crate::lm_detector::ENGINE {
            Some(self.lm_bank.load(self.lm_bank.revision()?).await?)
        } else {
            None
        };
        let mut job = json!({"id":job_id,"request_id":request,"account_id":id,"account_name":row["name"],"requested_model":model,"forwarded_model":target.model,"returned_models":[],"status":if conflict {"needs_confirmation"}else {"queued"},"completed_groups":0,"valid_groups":0,"attempts":0,"queued_at":crate::compat::now(),"started_at":null,"completed_at":null,"duration_ms":0,"error":if conflict {"测试目标已变化，请重新确认"}else {""},"report":null,"bank_version":null,"concurrency":payload["concurrency"],"trigger_source":if automatic.is_some() {"automatic"}else {"manual"},"automatic":automatic.is_some(),"triggers":auto["triggers"].as_array().into_iter().flatten().filter_map(|v|v.as_str()).collect::<std::collections::BTreeSet<_>>(),"degradation_recheck":auto["degradation_recheck"]==true,"planned_groups":groups,"first_group_prediction":null,"first_group_valid":false,"automatic_disposition":null,"target_signature":model_target::signature(&row),"owner_signature":model_target::signature(&owner),"groups":(0..groups).map(|i|json!({"index":i+1,"status":"queued","attempts":0,"ttft_ms":null,"duration_ms":null,"error":""})).collect::<Vec<_>>()});
        job["engine"] = payload["engine"].clone();
        job["sample_count"] = groups.into();
        job["tokenizer_probe"] = payload["tokenizer_probe"].clone();
        if let Some(lm) = lm {
            job["resource_revision"] = lm.version["revision"].clone();
            job["bank_version"] = lm.version.clone();
        }
        for key in [
            "detection_generation",
            "detection_mark_version",
            "recheck_account_version",
            "recheck_schedulable",
            "recovery_context",
        ] {
            job[key] = auto[key].clone();
        }
        crate::detection_logs::preserve_summary(&mut job);
        self.transaction(|data| {
            for old in data["jobs"]
                .as_object_mut()
                .unwrap()
                .values_mut()
                .filter(|old| old["account_id"] == id && !ACTIVE.contains(&string(&old["status"])))
            {
                crate::detection_logs::preserve_summary(old);
                old["report"] = Value::Null;
                old["groups"] = json!([]);
                old["can_retry"] = false.into();
                self.contexts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(string(&old["id"]));
            }
            data["jobs"][&job_id] = job.clone();
            data["accounts"][id.to_string()] = job.clone();
            data["requests"][request] = job_id.clone().into();
            Ok(())
        })?;
        if !conflict {
            self.launch(job.clone(), None)?;
        }
        Ok(Self::public(&job))
    }
    pub(crate) fn launch(&self, job: Value, context: Option<Arc<Context>>) -> Result<()> {
        let id = string(&job["id"]).to_owned();
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if self.closing.load(Ordering::Acquire)
            || self.actions.supervisor.is_stopping()
            || tasks.contains_key(&id)
        {
            return Ok(());
        }
        let this = self.clone();
        let task_id = id.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        let cleanup = self.clone();
        let cleanup_id = id.clone();
        let registration = crate::supervision::on_drop(move || {
            cleanup
                .tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&cleanup_id);
        });
        let handle = self.actions.supervisor.spawn("model_test", async move {
            let _registration = registration;
            let _ = receive.await;
            let result = this.actions.supervisor.guard("model_test", this.wait_and_run(&job, context)).await;
            if result.is_none() {
                if this.get(&task_id).is_ok_and(|job| ACTIVE.contains(&string(&job["status"])) && job["status"] != "queued") {
                    if let Ok(job) = this.get(&task_id) { let _=this.update(&task_id,interruption(&job,"测试已中断，结果未确认",crate::compat::datetime((this.clock)()).into())); }
                }
                this.contexts.lock().unwrap_or_else(|e| e.into_inner()).remove(&task_id);
            }
            if let Some(Err(error)) = result {
                let current = this.get(&task_id).unwrap_or(Value::Null);
                if ACTIVE.contains(&string(&current["status"])) {
                    if error.detail["code"] == "smart_schedule_deferred" && !truthy(&current["started_at"]) {
                        let _ = this.finish_cancel(&task_id);
                        return;
                    }
                    let message = if matches!(error.status.as_u16(), 409 | 422) {
                        crate::security::safe_error(&error.detail, 200)
                    } else {
                        "测试无法完成".into()
                    };
                    let _=this.update(&task_id,json!({"status":if matches!(error.status.as_u16(),409|422) {"needs_confirmation"}else {"failed"},"error":message,"completed_at":crate::compat::now(),"can_retry":false}));
                }
            }
        });
        tasks.insert(id, handle);
        let _ = send.send(());
        Ok(())
    }
    async fn wait_and_run(&self, job: &Value, context: Option<Arc<Context>>) -> Result<()> {
        let id = string(&job["id"]);
        let aid = integer(&job["account_id"]).ok_or_else(Error::internal)?;
        let (_lease, _job_slot, _budget) = loop {
            if self.actions.supervisor.is_stopping() {
                return Ok(());
            }
            let current = self.get(id)?;
            if current["status"] != "queued" {
                return Ok(());
            }
            if job["automatic"] == true {
                let until = self.hooks.next_allowed_at(aid)?;
                if parse_datetime(&until).is_some_and(|at| at > (self.clock)()) {
                    if current["waiting_until"] != until {
                        self.update(id, json!({"waiting_until":until}))?;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            }
            let mut manual_waiting = false;
            if job["automatic"] == true {
                let ids = self.data()?["jobs"]
                    .as_object()
                    .unwrap()
                    .values()
                    .filter(|row| row["automatic"] != true && row["status"] == "queued")
                    .filter_map(|row| integer(&row["account_id"]))
                    .collect::<Vec<_>>();
                for id in ids {
                    if let Ok(row) = model_target::account(&self.actions, id).await {
                        if self.actions.locks.try_account(&row).is_ok() {
                            manual_waiting = true;
                            break;
                        }
                    }
                }
            }
            if !manual_waiting {
                let row = model_target::account(&self.actions, aid).await?;
                if let Ok(slot) = self.jobs.clone().try_acquire_owned() {
                    if let Ok(budget) = self.budget.clone().try_acquire_owned() {
                        if let Ok(lease) = self.actions.locks.try_account(&row) {
                            break (lease, slot, budget);
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        if job["automatic"] == true {
            self.hooks.guard(self.get(id)?).await?;
        }
        let row = model_target::account(&self.actions, aid).await?;
        if job["automatic"] == true
            && !self
                .actions
                .model_allowed(&row, string(&job["requested_model"]))
                .await?
        {
            return Err(Error::new(409, "所选模型已不在当前账号上游模型目录中"));
        }
        let (target, owner) =
            model_target::target(&self.actions, &row, string(&job["requested_model"])).await?;
        if model_target::signature(&row) != job["target_signature"]
            || model_target::signature(&owner) != job["owner_signature"]
        {
            return Err(Error::new(409, "测试目标已变化，请重新选择"));
        }
        let context = if let Some(context) = context {
            context
        } else {
            let lm = if crate::lm_verdict::is_lm(job) {
                Some(
                    self.lm_bank
                        .load(string(&job["resource_revision"]).into())
                        .await?,
                )
            } else {
                None
            };
            let snapshot = self.bank.snapshot();
            if job["automatic"] == true
                && lm.is_none()
                && !crate::model_downgrade::bank_contains(
                    &snapshot.0,
                    crate::model_downgrade::LEGACY_TARGET,
                )
            {
                return Err(Error::new(409, "当前指纹库缺少降智判定模型"));
            }
            if job["automatic"] == true
                && lm.is_none()
                && !crate::model_downgrade::bank_contains(
                    &snapshot.0,
                    string(&job["requested_model"]),
                )
            {
                eprintln!("model_detection_diagnostic level=debug reason=requested_model_absent");
            }
            #[cfg(any(test, feature = "test-fixtures"))]
            let target = {
                let mut target = target;
                if let Some(endpoint) = self
                    .test_endpoint
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref()
                {
                    target.url.clone_from(endpoint);
                }
                target
            };
            Arc::new(Context {
                samples: Mutex::new(BTreeMap::new()),
                challenges: if lm.is_some() {
                    lmfpd::generate_challenges(integer(&job["planned_groups"]).unwrap_or(1) as usize)
                        .map_err(|_| Error::new(503,"无法生成检测挑战"))?.into_iter().map(|c|(c.expected_count as usize,c.prompt)).collect()
                } else {
                    modeltrace::challenges()
                },
                lm,
                snapshot,
                row,
                owner,
                target,
            })
        };
        self.contexts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.into(), context.clone());
        if truthy(&job["recovery_context"]) {
            let result = self.hooks.prepare_recovery(job.clone()).await?;
            if truthy(&result) {
                self.update(id,json!({"status":"failed","preflight_result":result,"completed_at":crate::compat::now(),"error":result.get("error").unwrap_or(&json!("恢复条件未确认"))}))?;
                return Ok(());
            }
        }
        if self.actions.supervisor.is_stopping() {
            return Ok(());
        }
        if job["automatic"] == true {
            self.hooks.guard(self.get(id)?).await?;
        }
        let job=self.update(id,json!({"status":"running","started_at":crate::compat::now(),"error":"","completed_at":null,"waiting_until":null,"bank_version":context.lm.as_ref().map(|lm|&lm.version).unwrap_or(&context.snapshot.1),"can_retry":false}))?;
        self.run(job, context).await
    }
    pub async fn retry_failed(&self, id: &str, request: &str) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        crate::validation::request_id(&json!(request))?;
        let _guard = self.start.lock().await;
        let state = self.data()?;
        if let Some(previous) = state["requests"].get(request) {
            if previous != id {
                return Err(Error::new(
                    409,
                    json!({"code":"idempotency_conflict","message":"请求 ID 已使用"}),
                ));
            }
            return self.get(id);
        }
        let job = self.get(id)?;
        if ACTIVE.contains(&string(&job["status"])) {
            return Ok(job);
        }
        let context = self
            .contexts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned();
        if job["can_retry"] != true || job["uncertain"] == true || context.is_none() {
            return Err(Error::new(
                409,
                json!({"code":"retry_unavailable","message":"原测试上下文已失效，请重新测试"}),
            ));
        }
        let context = context.unwrap();
        let stored=self.transaction(|data| {let job=&mut data["jobs"][id];for group in job["groups"].as_array_mut().unwrap() {let index=integer(&group["index"]).unwrap_or(1) as usize-1;if !context.samples.lock().unwrap_or_else(|e|e.into_inner()).contains_key(&index) {group.as_object_mut().unwrap().extend(json!({"status":"queued","error":"","attempts":0,"ttft_ms":null}).as_object().unwrap().clone());}}job["previous_attempts"]=json!(integer(&job["previous_attempts"]).unwrap_or(0)+integer(&job["attempts"]).unwrap_or(0));job.as_object_mut().unwrap().extend(json!({"status":"queued","can_retry":false,"queued_at":crate::compat::now(),"error":""}).as_object().unwrap().clone());let stored=job.clone();data["accounts"][integer(&stored["account_id"]).unwrap().to_string()]=stored.clone();data["requests"][request]=id.into();Ok(stored)})?;
        self.launch(stored.clone(), Some(context))?;
        Ok(Self::public(&stored))
    }
    pub async fn cancel(&self, id: &str) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        let current = self.get(id)?;
        let handle = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
            self.finish_cancel(id)?;
        } else if matches!(string(&current["status"]), "queued" | "needs_confirmation") {
            self.finish_cancel(id)?;
        }
        self.get(id)
    }
    fn finish_cancel(&self, id: &str) -> Result<()> {
        // The worker has drained before this read, so its final progress write
        // cannot restore an active group after the cancellation is persisted.
        let current = self.get(id)?;
        if current["phase"] == "tokenizer" {
            let mut tokenizer = current["tokenizer"].clone();
            if !tokenizer.is_object() {
                tokenizer = json!({});
            }
            tokenizer["status"] = "cancelled".into();
            self.update(id,json!({"status":"completed","phase":"finished","tokenizer":tokenizer,"completed_at":crate::compat::now(),"can_retry":false}))?;
            return Ok(());
        }
        let mut groups = current["groups"].clone();
        for group in groups.as_array_mut().into_iter().flatten() {
            if matches!(
                string(&group["status"]),
                "queued" | "running" | "receiving" | "retrying" | "analyzing"
            ) {
                group["status"] = "cancelled".into();
                group["retryable"] = false.into();
            }
        }
        self.update(id,json!({"status":"cancelled","groups":groups,"error":"测试已停止","completed_at":crate::compat::now(),"can_retry":false}))?;
        Ok(())
    }
    pub async fn close(&self) {
        self.closing.store(true, Ordering::Release);
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        for (id, handle) in tasks {
            handle.abort();
            let _ = handle.await;
            if self.get(&id).is_ok_and(|job| {
                ACTIVE.contains(&string(&job["status"])) && job["status"] != "queued"
            }) {
                if let Ok(job) = self.get(&id) {
                    let _ = self.update(
                        &id,
                        interruption(&job, "服务重启，测试已中断", crate::compat::now().into()),
                    );
                }
            }
        }
        self.contexts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}
pub fn model_request(payload: &Value) -> Result<Value> {
    let object = payload.as_object().ok_or_else(Error::invalid)?;
    if object.keys().any(|key| {
        ![
            "model_id",
            "expected_version",
            "expected_operation_version",
            "request_id",
            "concurrency",
            "engine",
            "sample_count",
            "tokenizer_probe",
        ]
        .contains(&key.as_str())
    }) || !crate::account_templates::MODEL_ID.is_match(string(&payload["model_id"]))
    {
        return Err(Error::invalid());
    }
    let version = string(&payload["expected_version"]);
    if version.chars().count() != 64 {
        return Err(Error::invalid());
    }
    if payload
        .get("expected_operation_version")
        .is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|v| v.chars().count() != 64))
    {
        return Err(Error::invalid());
    }
    crate::validation::request_id(&payload["request_id"])?;
    let concurrency = payload
        .get("concurrency")
        .map(|v| v.as_i64())
        .unwrap_or(Some(1))
        .filter(|n| (1..=3).contains(n))
        .ok_or_else(Error::invalid)?;
    let mut payload = payload.clone();
    payload["concurrency"] = concurrency.into();
    let engine = payload
        .get("engine")
        .and_then(Value::as_str)
        .unwrap_or("modeltrace");
    if !matches!(engine, "modeltrace" | "lm_detector")
        || payload.get("engine").is_some_and(|v| !v.is_string())
    {
        return Err(Error::invalid());
    }
    let count = payload
        .get("sample_count")
        .map(Value::as_i64)
        .unwrap_or(Some(if engine == "lm_detector" { 1 } else { 3 }))
        .filter(|n| matches!(n, 1 | 3))
        .ok_or_else(Error::invalid)?;
    let tokenizer = payload
        .get("tokenizer_probe")
        .map(Value::as_bool)
        .unwrap_or(Some(false))
        .ok_or_else(Error::invalid)?;
    if engine == "modeltrace" && (count != 3 || tokenizer) {
        return Err(Error::new(422, "此选项仅适用于 LM Detector"));
    }
    payload["engine"] = engine.to_owned().into();
    payload["sample_count"] = count.into();
    payload["tokenizer_probe"] = tokenizer.into();
    Ok(payload)
}

#[cfg(test)]
#[path = "model_recovery_tests.rs"]
mod recovery_tests;
