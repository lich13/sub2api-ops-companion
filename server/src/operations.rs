//! Durable intents. A dispatched request is reconciled by reading its result;
//! losing a response never turns it back into a new upstream request.
use crate::{
    account_templates,
    actions::{current_value, target_reached, Actions, OperationContext, EDIT_ACTIONS, OPERATION},
    compat::{account_version, digest, integer, now, operation_versions, pick, string, truthy},
    error::{Error, Result},
    queries::{recoverable, settings_capabilities},
    security::clean,
    store::Store,
    validation,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex as AsyncMutex, Semaphore},
    task::JoinHandle,
};

mod cycle;
mod retry;
pub use cycle::{observe_cycle, repeat_open, CycleObservation};

pub const PENDING: &[&str] = &["queued", "running", "checking", "needs_confirmation"];
const SUPERSEDE: &[&str] = &[
    "priority",
    "concurrency",
    "load_factor",
    "model_config",
    "proxy",
    "groups",
    "schedulable",
    "degradation_mark",
];
pub fn short(job: &Value) -> bool {
    matches!(string(&job["action"]), "schedulable" | "degradation_mark")
}
pub fn connection_test(job: &Value) -> bool {
    job["action"] == "test"
        && (truthy(&job["batch_id"])
            || job["scheduled_at"].is_string()
            || job["repeat"] == "quota_cycle")
}
pub fn native(job: &Value) -> bool {
    truthy(&job["batch_id"])
        && matches!(
            string(&job["action"]),
            "proxy" | "account_template" | "delete" | "groups"
        )
        && matches!(string(&job["status"]), "queued" | "checking")
}
pub fn id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
pub fn scheduled_time(value: &Value) -> Result<chrono::DateTime<chrono::Utc>> {
    use chrono::Timelike;
    let at = value
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .ok_or_else(Error::invalid)?;
    if at.nanosecond() != 0 {
        return Err(Error::new(422, "预约时间须精确到秒"));
    }
    Ok(at.with_timezone(&chrono::Utc))
}
pub fn validate_store(data: &Value) -> Result<()> {
    let error = || Error::new(503, "操作状态无法读取");
    if data["version"] != 1 || !data["jobs"].is_object() || !data["requests"].is_object() {
        return Err(error());
    }
    for job in data["jobs"].as_object().unwrap().values() {
        crate::connection_retry::Policy::saved(job)?;
        if job
            .get("attempt_count")
            .is_some_and(|v| v.as_u64().is_none())
            || job
                .get("next_retry_at")
                .is_some_and(|v| !v.is_null() && crate::compat::parse_datetime(v).is_none())
        {
            return Err(error());
        }
        if !job.is_object()
            || !job["account_id"].is_i64()
            || !job["payload"].is_object()
            || !job["status"].is_string()
            || job
                .get("scheduled_at")
                .is_some_and(|value| job["action"] != "test" || scheduled_time(value).is_err())
        {
            return Err(error());
        }
    }
    for job in data["jobs"].as_object().unwrap().values() {
        if job.get("repeat").is_some()
            && (job["repeat"] != "quota_cycle"
                || job["action"] != "test"
                || validation::hex(&job["repeat_series_id"], 32).is_err()
                || !matches!(
                    string(&job["repeat_state"]),
                    "active" | "continued" | "stopped" | "needs_confirmation"
                ))
        {
            return Err(error());
        }
        if let Some(next) = job.get("next_operation_id") {
            let successor = &data["jobs"][string(next)];
            if validation::hex(next, 32).is_err()
                || successor["account_id"] != job["account_id"]
                || successor["repeat_series_id"] != job["repeat_series_id"]
                || job["repeat_state"] != "continued"
            {
                return Err(error());
            }
        }
    }
    for field in ["batches", "batch_requests"] {
        if data.get(field).is_some_and(|v| !v.is_object()) {
            return Err(error());
        }
    }
    for (bid, batch) in data["batches"].as_object().into_iter().flatten() {
        if batch
            .get("chunks")
            .is_some_and(|chunks| !chunks.is_null() && !chunks.is_array())
        {
            return Err(error());
        }
        if !batch["job_ids"].as_array().is_some_and(|v| {
            v.iter().all(|j| {
                j.as_str().is_some_and(|jid| {
                    data["jobs"].get(jid).is_some() && data["jobs"][jid]["batch_id"] == *bid
                })
            })
        }) {
            return Err(error());
        }
    }
    if data["batch_requests"]
        .as_object()
        .into_iter()
        .flat_map(|v| v.values())
        .any(|id| {
            id.as_str()
                .is_none_or(|id| data["batches"].get(id).is_none())
        })
    {
        return Err(error());
    }
    Ok(())
}
#[derive(Clone)]
pub struct OperationStore(pub Store);
impl OperationStore {
    pub fn new(path: PathBuf, read_only: bool) -> Self {
        Self(Store::new(
            path,
            json!({"version":1,"jobs":{},"requests":{},"batches":{},"batch_requests":{}}),
            validate_store,
            read_only,
        ))
    }
    fn normalize(data: &mut Value) {
        for field in ["batches", "batch_requests"] {
            if data.get(field).is_none() {
                data[field] = json!({});
            }
        }
    }
    pub fn read(&self) -> Result<Value> {
        Ok((*self.read_shared()?).clone())
    }
    pub(crate) fn read_shared(&self) -> Result<Arc<Value>> {
        Ok(self.read_shared_versioned()?.1)
    }
    pub(crate) fn read_shared_versioned(&self) -> Result<(u64, Arc<Value>)> {
        let (rev, mut data) = self.0.read_shared_versioned()?;
        if data.get("batches").is_none() || data.get("batch_requests").is_none() {
            Self::normalize(Arc::make_mut(&mut data));
        }
        Ok((rev, data))
    }
    pub fn transaction<T>(&self, f: impl FnOnce(&mut Value) -> Result<T>) -> Result<T> {
        self.0.transaction(|data| {
            Self::normalize(data);
            let out = f(data)?;
            Self::prune_terminal_jobs(data);
            Ok(out)
        })
    }
    fn prune_terminal_jobs(data: &mut Value) {
        const MAX_TERMINAL_JOBS: usize = 10_000;
        let Some(jobs_len) = data["jobs"].as_object().map(serde_json::Map::len) else {
            return;
        };
        if jobs_len <= MAX_TERMINAL_JOBS {
            return;
        }
        let jobs = data["jobs"].as_object().unwrap();
        let mut open_series = BTreeSet::new();
        for job in jobs.values() {
            if repeat_open(job) {
                let series = string(&job["repeat_series_id"]);
                if !series.is_empty() {
                    open_series.insert(series.to_owned());
                }
            }
        }
        let mut active_batches = BTreeSet::new();
        for (bid, batch) in data["batches"].as_object().into_iter().flatten() {
            let has_pending = batch["job_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|jid| {
                    jobs.get(jid)
                        .is_some_and(|job| PENDING.contains(&string(&job["status"])))
                });
            if has_pending {
                active_batches.insert(bid.clone());
            }
        }
        let mut protected = BTreeSet::new();
        let mut terminal_candidates = Vec::new();
        for (jid, job) in jobs {
            let is_pending = PENDING.contains(&string(&job["status"]));
            let in_open_series =
                repeat_open(job) || open_series.contains(string(&job["repeat_series_id"]));
            let in_active_batch = active_batches.contains(string(&job["batch_id"]));
            if is_pending || in_open_series || in_active_batch {
                protected.insert(jid.clone());
            } else {
                let stamp = job["updated_at"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| job["created_at"].as_str())
                    .unwrap_or("")
                    .to_owned();
                terminal_candidates.push((stamp, jid.clone()));
            }
        }
        if terminal_candidates.len() <= MAX_TERMINAL_JOBS {
            return;
        }
        terminal_candidates.sort();
        let skip = terminal_candidates.len() - MAX_TERMINAL_JOBS;
        let mut retain_ids = protected;
        for (_, jid) in terminal_candidates.into_iter().skip(skip) {
            retain_ids.insert(jid);
        }
        let mut worklist: Vec<String> = retain_ids.iter().cloned().collect();
        while let Some(jid) = worklist.pop() {
            if let Some(next_id) = jobs
                .get(&jid)
                .and_then(|j| j.get("next_operation_id"))
                .and_then(Value::as_str)
            {
                if jobs.contains_key(next_id) && retain_ids.insert(next_id.to_owned()) {
                    worklist.push(next_id.to_owned());
                }
            }
        }
        if retain_ids.len() == jobs_len {
            return;
        }
        data["jobs"]
            .as_object_mut()
            .unwrap()
            .retain(|jid, _| retain_ids.contains(jid));
        if let Some(batches) = data["batches"].as_object_mut() {
            batches.retain(|_, batch| {
                if let Some(job_ids) = batch["job_ids"].as_array_mut() {
                    job_ids.retain(|j| j.as_str().is_some_and(|jid| retain_ids.contains(jid)));
                    !job_ids.is_empty()
                } else {
                    false
                }
            });
        }
        let surviving_batches: BTreeSet<String> = data["batches"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(bid, _)| bid.clone())
            .collect();
        if let Some(batch_requests) = data["batch_requests"].as_object_mut() {
            batch_requests
                .retain(|_, bid| bid.as_str().is_some_and(|b| surviving_batches.contains(b)));
        }
        if let Some(requests) = data["requests"].as_object_mut() {
            requests.retain(|_, jid| jid.as_str().is_some_and(|j| retain_ids.contains(j)));
        }
    }
    pub fn recover_startup(&self) -> Result<()> {
        let data = self.read_shared()?;
        if self.0.is_read_only() {
            return Ok(());
        }
        let needs_recovery = data["jobs"]
            .as_object()
            .into_iter()
            .flatten()
            .any(|(_, job)| job["status"] == "running")
            || data["batches"]
                .as_object()
                .into_iter()
                .flatten()
                .any(|(_, batch)| {
                    batch
                        .get("chunks")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .any(|chunk| chunk["status"] == "dispatched")
                });
        if !needs_recovery {
            return Ok(());
        }
        self.transaction(|data| {
            for job in data["jobs"].as_object_mut().unwrap().values_mut() {
                if job["status"] == "running" {
                    job["status"] = "checking".into();
                    job["reason"] = "服务重启，核对已派发请求".into();
                }
            }
            for batch in data["batches"].as_object_mut().unwrap().values_mut() {
                for chunk in batch
                    .get_mut("chunks")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                {
                    if chunk["status"] == "dispatched" {
                        chunk["status"] = "checking".into();
                    }
                }
            }
            Ok(())
        })
    }
}

/// Domain controllers implement the remaining operations without delegating to
/// another service or a legacy runtime. Shared guards survive native requests.
pub trait Backend: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
    fn mark(&self, id: i64) -> Result<Value>;
    fn dispatch<'a>(
        &'a self,
        row: &'a Value,
        job: &'a Value,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Value>>;
    fn model_get(&self, id: &str) -> Result<Value>;
    fn model_jobs(&self) -> Result<Vec<Value>>;
    fn model_cancel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Value>>;
    fn test_config(&self) -> Result<(String, usize)>;
    fn retry_config(&self) -> Result<crate::connection_retry::Policy> {
        Ok(crate::connection_retry::Policy::default())
    }
    fn cycle_observation<'a>(
        &'a self,
        _id: i64,
        _key: &'a str,
        _refresh: bool,
    ) -> BoxFuture<'a, Result<CycleObservation>> {
        Box::pin(async { Err(Error::new(422, "此服务不支持持续预约")) })
    }
    fn cycle_observations<'a>(
        &'a self,
        accounts: &'a [i64],
    ) -> BoxFuture<'a, Result<BTreeMap<i64, Result<CycleObservation>>>> {
        Box::pin(async move {
            let mut observations = BTreeMap::new();
            for id in accounts {
                observations.insert(*id, self.cycle_observation(*id, "", false).await);
            }
            Ok(observations)
        })
    }
    fn test_slots(&self) -> Option<crate::execution::TestSlots> {
        None
    }
    fn account_deleted(&self, _id: i64) {}
}

struct EventBuffer {
    items: Vec<Value>,
    terminal_at: Option<chrono::DateTime<chrono::Utc>>,
    last_push_at: chrono::DateTime<chrono::Utc>,
}

pub struct Operations {
    pub actions: Actions,
    pub store: OperationStore,
    pub backend: Arc<dyn Backend>,
    pub submit_lock: AsyncMutex<()>,
    pub keys: Mutex<BTreeMap<String, String>>,
    events: Mutex<BTreeMap<String, EventBuffer>>,
    tasks: Mutex<BTreeMap<String, JoinHandle<()>>>,
    pub budget: Arc<Semaphore>,
    closing: AtomicBool,
    pub(super) cycle_poll_cache: Mutex<Option<(u64, u64, chrono::DateTime<chrono::Utc>)>>,
    scheduled_results_cache: Mutex<(u64, u64)>,
    pub(super) active_scan_cache: Mutex<(u64, bool, bool)>,
}
impl Operations {
    pub fn new(
        actions: Actions,
        backend: Arc<dyn Backend>,
        budget: Arc<Semaphore>,
    ) -> Result<Arc<Self>> {
        let queue = Self::new_deferred(actions, backend, budget)?;
        queue.store.recover_startup()?;
        Ok(queue)
    }
    pub fn new_deferred(
        actions: Actions,
        backend: Arc<dyn Backend>,
        budget: Arc<Semaphore>,
    ) -> Result<Arc<Self>> {
        let store = OperationStore::new(
            actions
                .settings
                .usage_path
                .with_file_name("account-operations.json"),
            actions.settings.read_only,
        );
        store.read_shared()?;
        Ok(Arc::new(Self {
            actions,
            store,
            backend,
            submit_lock: AsyncMutex::new(()),
            keys: Mutex::new(BTreeMap::new()),
            events: Mutex::new(BTreeMap::new()),
            tasks: Mutex::new(BTreeMap::new()),
            budget,
            closing: AtomicBool::new(false),
            cycle_poll_cache: Mutex::new(None),
            scheduled_results_cache: Mutex::new((0, 0)),
            active_scan_cache: Mutex::new((0, false, false)),
        }))
    }
    pub fn public(&self, job: &Value, detail: bool, after: usize) -> Value {
        let mut out: Value = job
            .as_object()
            .unwrap()
            .iter()
            .filter(|(field, _)| {
                !matches!(
                    field.as_str(),
                    "client_id"
                        | "request_id"
                        | "request_hash"
                        | "expected_version"
                        | "payload"
                        | "template_target"
                        | "managed_child_ids"
                        | "detached"
                        | "retry_policy"
                        | "notification_suppressed"
                )
            })
            .map(|(field, value)| (field.clone(), value.clone()))
            .collect::<serde_json::Map<_, _>>()
            .into();
        let requested = match &job["payload"] {
            Value::Object(map) => map
                .iter()
                .filter(|(field, _)| {
                    !matches!(
                        field.as_str(),
                        "prompt"
                            | "image_data_url"
                            | "audio_data_url"
                            | "expected_version"
                            | "detach_managed"
                    )
                })
                .map(|(field, value)| (field.clone(), value.clone()))
                .collect::<serde_json::Map<_, _>>()
                .into(),
            value => value.clone(),
        };
        out["requested"] = requested;
        if let Some(result) = out["result"].as_object_mut() {
            result.shift_remove("detached");
        }
        if detail && job["action"] == "test" {
            let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
            let values = events
                .get(string(&job["id"]))
                .map(|buf| buf.items.as_slice())
                .unwrap_or(&[]);
            out["events"] = json!(values.iter().skip(after).take(100).collect::<Vec<_>>());
            out["next_event"] = json!(values.len().min(after.saturating_add(100)));
        }
        out
    }
    pub fn model_view(job: &Value) -> Value {
        json!({"id":job["id"],"account_id":job["account_id"],"account_name":job["account_name"],"action":"model_test","status":if job["status"]=="retrying" {json!("running")} else {job["status"].clone()},"reason":job.get("error").cloned().unwrap_or(json!("")),"requested":{"model_id":job["requested_model"],"concurrency":job["concurrency"]}})
    }
    pub fn get(&self, jid: &str, after: usize) -> Result<Value> {
        if validation::hex(&json!(jid), 32).is_err() {
            return Err(Error::new(404, "操作不存在"));
        }
        let data = self.store.read_shared()?;
        if let Some(job) = data["jobs"].get(jid) {
            Ok(self.public(job, true, after))
        } else {
            Ok(Self::model_view(&self.backend.model_get(jid)?))
        }
    }
    pub fn listing(&self, bid: Option<&str>) -> Result<Value> {
        let data = self.store.read_shared()?;
        if let Some(bid) = bid {
            if validation::hex(&json!(bid), 32).is_err() || data["batches"].get(bid).is_none() {
                return Err(Error::new(404, "批量操作不存在"));
            }
            let items: Vec<_> = data["batches"][bid]["job_ids"]
                .as_array()
                .ok_or_else(|| Error::new(503, "操作状态无法读取"))?
                .iter()
                .map(|j| self.public(&data["jobs"][string(j)], false, 0))
                .collect();
            return Ok(
                json!({"batch_id":bid,"pending":items.iter().filter(|v|PENDING.contains(&string(&v["status"]))).count(),"items":items}),
            );
        }
        let mut jobs: Vec<_> = data["jobs"].as_object().unwrap().values().collect();
        jobs.sort_by(|a, b| string(&b["created_at"]).cmp(string(&a["created_at"])));
        let mut items: Vec<_> = jobs
            .iter()
            .filter(|j| cycle::pending(j))
            .chain(
                jobs.iter()
                    .filter(|j| !cycle::pending(j) && j["repeat_state"] != "continued")
                    .take(30),
            )
            .map(|j| self.public(j, false, 0))
            .collect();
        items.extend(
            self.backend
                .model_jobs()?
                .iter()
                .filter(|j| {
                    matches!(
                        string(&j["status"]),
                        "queued" | "running" | "retrying" | "needs_confirmation"
                    )
                })
                .map(Self::model_view),
        );
        Ok(json!({"pending":items.iter().filter(|v|cycle::pending(v)).count(),"items":items}))
    }
    pub fn update(&self, jid: &str, changes: Value) -> Result<Value> {
        self.update_many(&[(jid, changes)])?
            .pop()
            .ok_or_else(Error::internal)
    }
    pub(crate) fn update_many(&self, changes: &[(&str, Value)]) -> Result<Vec<Value>> {
        let updated = self.store.transaction(|data| {
            let at = now();
            let mut updated = Vec::with_capacity(changes.len());
            for (jid, changes) in changes {
                let job = data["jobs"]
                    .get_mut(*jid)
                    .ok_or_else(|| Error::new(404, "操作不存在"))?;
                if !(connection_test(job) && job["status"] == "cancelled") {
                    job.as_object_mut()
                        .unwrap()
                        .extend(changes.as_object().cloned().ok_or_else(Error::invalid)?);
                    job["updated_at"] = at.clone().into();
                    if job["repeat_state"] == "stopped" && job["status"] == "queued" {
                        job["status"] = "cancelled".into();
                        job["reason"] = job["repeat_reason"].clone();
                    }
                    if job["repeat_state"] == "active"
                        && matches!(string(&job["status"]), "failed" | "needs_confirmation")
                    {
                        job["repeat_state"] = "needs_confirmation".into();
                        job["repeat_reason"] = job["reason"].clone();
                    }
                    if !PENDING.contains(&string(&job["status"])) {
                        for name in ["prompt", "image_data_url", "audio_data_url"] {
                            job["payload"].as_object_mut().unwrap().shift_remove(name);
                        }
                    }
                }
                updated.push(job.clone());
            }
            Ok(updated)
        })?;
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        for job in &updated {
            if !Self::needs_key(job) {
                keys.remove(string(&job["id"]));
            }
        }
        Ok(updated)
    }
    pub fn fingerprint(&self, row: &Value, job: &Value) -> Result<String> {
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        if job["action"] == "account_template" {
            return Ok(string(&account_templates::account_version(row)).to_owned());
        }
        if job["action"] == "degradation_mark" {
            return Ok(string(&self.backend.mark(id)?["version"]).to_owned());
        }
        Ok(string(&operation_versions(row)[string(&job["action"])]).to_owned())
    }
    pub fn current(&self, row: &Value, job: &Value) -> Result<Value> {
        let action = string(&job["action"]);
        if EDIT_ACTIONS.contains(&action) {
            return current_value(row, action);
        }
        if matches!(action, "groups" | "schedulable") {
            let field = if action == "groups" {
                "group_ids"
            } else {
                action
            };
            return Ok(json!({field:row[field]}));
        }
        Ok(pick(row, &["id", "name", "platform", "type", "status"]))
    }
    pub fn achieved(&self, row: Option<&Value>, job: &Value) -> Result<bool> {
        let action = string(&job["action"]);
        let p = &job["payload"];
        if action == "delete" {
            return Ok(row.is_none());
        }
        let Some(row) = row else {
            return Ok(false);
        };
        if EDIT_ACTIONS.contains(&action) {
            return Ok(target_reached(row, action, p));
        }
        let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
        Ok(match action {
            "schedulable" => row[action] == p[action],
            "groups" => {
                let current = ids(&row["group_ids"]);
                let scope = ids(&p["scope_group_ids"]);
                current
                    .intersection(&scope)
                    .copied()
                    .collect::<BTreeSet<_>>()
                    == ids(&p["group_ids"])
            }
            "recover" => !recoverable(row, chrono::Utc::now()),
            "account_template" => {
                let target = if job["status"] == "checking" && job["template_target"].is_object() {
                    job["template_target"].clone()
                } else {
                    self.actions.templates.desired(p)?
                };
                account_templates::eligible(row)
                    && !account_templates::passthrough(row)
                    && row["model_mapping"]
                        .as_object()
                        .cloned()
                        .unwrap_or_default()
                        == target.as_object().cloned().unwrap_or_default()
            }
            "degradation_mark" => truthy(&self.backend.mark(id)?["marked"]) == truthy(&p["marked"]),
            _ => false,
        })
    }
    pub fn audit(&self, job: &Value, result: &str) {
        crate::audit::write(
            &self.actions.settings.audit_path,
            "desktop_operation",
            json!({"operation_id":job["id"],"account_id":job["account_id"],"action":job["action"],"result":result}),
            self.actions.settings.read_only,
        );
    }
    pub async fn read_account(&self, id: i64) -> Result<Option<Value>> {
        match self.actions.account(id, None).await {
            Ok(row) => Ok(Some(row)),
            Err(e) if e.status.as_u16() == 404 => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub fn resolve_test_model(&self, row: &Value, payload: &mut Value) -> Result<()> {
        if row["platform"] == "openai" && string(&payload["model_id"]).trim().is_empty() {
            let model = self.backend.test_config()?.0;
            if !account_templates::MODEL_ID.is_match(&model) {
                return Err(Error::new(422, "测活模型配置无效"));
            }
            payload["model_id"] = model.into();
        }
        Ok(())
    }
    pub async fn submit(self: &Arc<Self>, aid: i64, request: &Value, key: &str) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        if aid < 1 {
            return Err(Error::new(422, "账号编号无效"));
        }
        validation::fields(
            request,
            &[
                "action",
                "request_id",
                "client_id",
                "expected_version",
                "payload",
                "scheduled_at",
                "repeat",
            ],
            &["action", "request_id", "client_id", "expected_version"],
        )?;
        let rid = validation::request_id(&request["request_id"])?;
        let client = validation::request_id(&request["client_id"])?;
        let expected = validation::text(&request["expected_version"], 64, 64)?;
        let action = string(&request["action"]);
        let mut payload =
            validation::operation_payload(action, request.get("payload").unwrap_or(&json!({})))?;
        let scheduled = request
            .get("scheduled_at")
            .map(scheduled_time)
            .transpose()?;
        let repeat = request.get("repeat");
        if repeat.is_some_and(|value| value != "quota_cycle")
            || repeat.is_some() && scheduled.is_some()
        {
            return Err(Error::new(422, "连续预约由服务端计算周期时间"));
        }
        if (scheduled.is_some() || repeat.is_some())
            && (action != "test"
                || !matches!(string(&payload["mode"]), "default" | "text")
                || repeat.is_some() && payload["mode"] != "default"
                || truthy(&payload["prompt"])
                || truthy(&payload["image_data_url"])
                || truthy(&payload["audio_data_url"]))
        {
            return Err(Error::new(422, "预约仅支持普通测试连接"));
        }
        let signature = if let Some(repeat) = repeat {
            digest(&json!([action, payload, expected, repeat]))
        } else if let Some(at) = scheduled {
            digest(&json!([
                action,
                payload,
                expected,
                crate::compat::datetime(at)
            ]))
        } else {
            digest(&json!([action, payload, expected]))
        };
        let _submit = self.submit_lock.lock().await;
        let data = self.store.read_shared()?;
        if data["batch_requests"].get(rid).is_some() {
            return Err(idempotency("请求 ID 已用于批量操作"));
        }
        if let Some(jid) = data["requests"].get(rid).and_then(Value::as_str) {
            let old = &data["jobs"][jid];
            if old["account_id"] != aid
                || old["action"] != action
                || old["client_id"] != client
                || old["request_hash"] != signature
            {
                return Err(idempotency("请求 ID 已用于其他操作"));
            }
            self.keys
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(jid.into(), key.into());
            return Ok(self.public(old, false, 0));
        }
        if scheduled.is_some_and(|at| at <= self.backend.now()) {
            return Err(Error::new(422, "预约时间须晚于当前时间"));
        }
        let _priority = if action == "priority" {
            Some(self.actions.priorities.enter().await?)
        } else {
            None
        };
        let row = self.actions.account(aid, None).await?;
        if repeat.is_some() {
            if row["platform"] != "openai" || row["type"] != "oauth" {
                return Err(Error::new(422, "持续预约仅支持 OpenAI OAuth 账号"));
            }
            if data["jobs"]
                .as_object()
                .unwrap()
                .values()
                .any(|job| job["account_id"] == aid && repeat_open(job))
            {
                return Err(Error::new(409, "此账号已有连续预约，请先停止现有预约"));
            }
        }
        if action == "test" {
            self.resolve_test_model(&row, &mut payload)?;
        }
        let jid = id();
        let mut job = json!({"id":jid,"account_id":aid,"account_name":row["name"],"action":action,"payload":payload,"expected_version":expected,"client_id":client,"request_id":rid,"request_hash":signature,"parent_account_id":row["parent_account_id"],"created_at":now(),"updated_at":now(),"status":"queued","reason":"等待执行","result":null});
        if let Some(at) = scheduled {
            job["scheduled_at"] = crate::compat::datetime(at).into();
            job["reason"] = "等待预约时间".into();
        }
        if action == "account_template" {
            match self.actions.templates.desired(&payload) {
                Ok(target) => job["template_target"] = target,
                Err(e) if e.status.as_u16() == 409 => {
                    job["status"] = "needs_confirmation".into();
                    job["reason"] = e.detail;
                }
                Err(e) => return Err(e),
            }
        }
        if self.fingerprint(&row, &job)? != expected {
            job["status"] = "needs_confirmation".into();
            job["reason"] = "相关字段已变化，请核对后重新提交".into();
            job["current"] = self.current(&row, &job)?;
        }
        if repeat.is_some() {
            self.initialize_cycle(&mut job, key).await?;
        }
        self.store.transaction(|data| {
            for old in data["jobs"].as_object_mut().unwrap().values_mut() {
                if old["account_id"] == aid
                    && old["client_id"] == client
                    && old["action"] == action
                    && matches!(string(&old["status"]), "queued" | "needs_confirmation")
                    && SUPERSEDE.contains(&action)
                {
                    old["status"] = "superseded".into();
                    old["reason"] = "已由本客户端的新请求替代".into();
                    old["updated_at"] = now().into();
                }
            }
            data["jobs"][&jid] = job.clone();
            data["requests"][rid] = jid.clone().into();
            Ok(())
        })?;
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(jid, key.into());
        self.audit(&job, "accepted");
        if short(&job) && job["status"] == "queued" {
            self.launch(job.clone(), None, None);
        }
        Ok(self.public(&job, false, 0))
    }
    pub async fn submit_batch(&self, request: &Value, key: &str, template: bool) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        crate::request_schema::batch(request, template)?;
        let mut normalized = request.clone();
        if !template
            && matches!(
                string(&normalized["action"]),
                "delete" | "groups" | "recover"
            )
            && normalized.get("payload").is_none()
        {
            normalized["payload"] = json!({});
        }
        let request = &normalized;
        let action = if template {
            "account_template"
        } else {
            string(&request["action"])
        };
        if !matches!(
            action,
            "test" | "proxy" | "delete" | "groups" | "recover" | "account_template"
        ) {
            return Err(Error::invalid());
        }
        let fields = if template {
            vec![
                "template_id",
                "template_version",
                "accounts",
                "request_id",
                "client_id",
            ]
        } else {
            vec!["action", "payload", "accounts", "request_id", "client_id"]
        };
        validation::fields(request, &fields, &fields)?;
        let rid = if template {
            validation::hex(&request["request_id"], 32)?
        } else {
            validation::request_id(&request["request_id"])?
        };
        let client = if template {
            validation::hex(&request["client_id"], 32)?
        } else {
            validation::request_id(&request["client_id"])?
        };
        let accounts = request["accounts"]
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 1000)
            .ok_or_else(Error::invalid)?;
        let mut accepted = Vec::new();
        let mut seen = BTreeSet::new();
        let mut canonical = request.clone();
        for item in accounts {
            validation::fields(
                item,
                if matches!(action, "delete" | "groups") {
                    &["account_id", "expected_version", "payload"]
                } else {
                    &["account_id", "expected_version"]
                },
                &["account_id", "expected_version"],
            )?;
            let aid = if template {
                validation::coercible_integer(&item["account_id"], 1, i64::MAX)?
            } else {
                validation::integer(&item["account_id"], 1, i64::MAX)?
            };
            if !seen.insert(aid) {
                return Err(Error::new(422, "批量账号不能重复"));
            }
            if template {
                validation::text(&item["expected_version"], 64, 64)?;
            } else {
                validation::hex(&item["expected_version"], 64)?;
            }
            let mut value = item.clone();
            value["account_id"] = aid.into();
            if matches!(action, "delete" | "groups") && value.get("payload").is_none() {
                value["payload"] = json!({});
            }
            accepted.push(value);
        }
        let mut sorted = accepted.clone();
        sorted.sort_by_key(|v| integer(&v["account_id"]));
        canonical["accounts"] = sorted.into();
        canonical
            .as_object_mut()
            .unwrap()
            .shift_remove("request_id");
        let signature = digest(&canonical);
        let payload = &request["payload"];
        if action == "test" {
            validation::fields(payload, &["confirmed"], &["confirmed"])?;
            if !validation::boolean(&payload["confirmed"])? {
                return Err(Error::new(422, "请先确认连接测试"));
            }
        }
        if action == "proxy" {
            validation::operation_payload("proxy", payload)?;
        }
        if action == "delete" && *payload != json!({"confirmed":true})
            || matches!(action, "groups" | "recover") && *payload != json!({})
        {
            return Err(Error::new(422, "请确认删除，或核对批量操作参数"));
        }
        let item_payloads = if matches!(action, "delete" | "groups") {
            accepted
                .iter()
                .map(|v| validation::operation_payload(action, &v["payload"]))
                .collect::<Result<Vec<_>>>()?
        } else {
            vec![]
        };
        let _submit = self.submit_lock.lock().await;
        let data = self.store.read_shared()?;
        if data["requests"].get(rid).is_some() {
            return Err(idempotency("请求 ID 已用于其他操作"));
        }
        if let Some(bid) = data["batch_requests"].get(rid).and_then(Value::as_str) {
            if data["batches"][bid]["request_hash"] != signature {
                return Err(idempotency("请求 ID 已用于其他批量操作"));
            }
            let ids = data["batches"][bid]["job_ids"]
                .as_array()
                .ok_or_else(|| Error::new(503, "操作状态无法读取"))?;
            let ids = ids
                .iter()
                .map(|jid| {
                    jid.as_str()
                        .filter(|jid| {
                            data["jobs"]
                                .get(*jid)
                                .is_some_and(|job| job["batch_id"] == bid)
                        })
                        .ok_or_else(|| Error::new(503, "操作状态无法读取"))
                })
                .collect::<Result<Vec<_>>>()?;
            let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
            for jid in ids {
                keys.insert(jid.into(), key.into());
            }
            drop(keys);
            return self.listing(Some(bid));
        }
        let model = if action == "test" {
            let model = self.backend.test_config()?.0;
            if !account_templates::MODEL_ID.is_match(&model) {
                return Err(Error::new(422, "测活模型配置无效"));
            }
            model
        } else {
            String::new()
        };
        let (target, template_version) = if template {
            let target = self.actions.templates.desired(request)?;
            let view = self.actions.templates.view(None)?;
            let selected = &view["templates"][string(&request["template_id"])];
            if selected.is_null() || account_templates::combine(selected)? != target {
                return Err(Error::new(409, "模板内容已变化，请重新预览"));
            }
            (
                target,
                view["template_versions"][string(&request["template_id"])].clone(),
            )
        } else {
            (Value::Null, Value::Null)
        };
        let choices = if action == "proxy" && !payload["proxy_id"].is_null() {
            self.actions.proxies(key).await?
        } else {
            vec![]
        };
        let proxy_available = payload["proxy_id"].is_null()
            || choices
                .iter()
                .any(|p| p["id"] == payload["proxy_id"] && p["status"] == "active");
        let rows: BTreeMap<_, _> = self
            .actions
            .accounts(&seen.iter().copied().collect::<Vec<_>>())
            .await?
            .into_iter()
            .filter_map(|r| integer(&r["id"]).map(|id| (id, r)))
            .collect();
        let descendants = if action == "delete" {
            self.children(&seen.iter().copied().collect::<Vec<_>>())
                .await?
        } else {
            BTreeMap::new()
        };
        let bid = id();
        let created = now();
        let mut jobs = vec![];
        for (index, item) in accepted.iter().enumerate() {
            let aid = integer(&item["account_id"]).unwrap();
            let row = rows.get(&aid).unwrap_or(&Value::Null);
            let grok = row["platform"] == "grok";
            let p = match action {
                "test" => {
                    json!({"confirmed":true,"mode":if grok {"text"} else {"default"},"model_id":if grok {""} else {&model}})
                }
                "account_template" => {
                    json!({"template_id":request["template_id"],"template_version":template_version})
                }
                "delete" | "groups" => item_payloads[index].clone(),
                _ => payload.clone(),
            };
            let mut job = json!({"id":id(),"batch_id":bid,"account_id":aid,"account_name":clean(&row.get("name").cloned().filter(truthy).unwrap_or(json!(format!("#{aid}"))),200),"action":action,"payload":p,"expected_version":item["expected_version"],"client_id":client,"request_id":id(),"request_hash":if matches!(action,"test"|"account_template") {digest(&json!([p,item["expected_version"]]))} else {digest(&json!([action,p,item["expected_version"]]))},"parent_account_id":if action=="proxy" && !truthy(&row["parent_account_id"]) {row["extra"]["parent_account_id"].clone()} else {row["parent_account_id"].clone()},"created_at":created,"updated_at":created,"status":"queued","reason":"等待执行","result":null});
            if template {
                job["template_target"] = target.clone();
            }
            if action == "delete" {
                let children = descendants.get(&aid).cloned().unwrap_or_default();
                job["child_account_ids"] = json!(children);
            }
            let mut change = None;
            if action == "test" {
                change = if row.is_null() || truthy(&row["deleted_at"]) {
                    Some(("failed", "账号不存在或已删除"))
                } else if !matches!(string(&row["platform"]), "openai" | "grok")
                    || !matches!(string(&row["type"]), "oauth" | "apikey")
                {
                    Some(("failed", "此账号不支持连接测试"))
                } else if operation_versions(row)["test"] != item["expected_version"] {
                    Some((
                        "needs_confirmation",
                        "账号凭据、代理或模型配置已变化，请刷新后重新选择",
                    ))
                } else {
                    None
                };
            } else if template {
                if !account_templates::eligible(row) {
                    change = Some(("failed", "仅可应用到未删除的独立 OpenAI OAuth 或 Key 账号"));
                } else if account_templates::passthrough(row) {
                    change = Some((
                        "needs_confirmation",
                        "透传模式会绕过模型限制，请先处理透传设置",
                    ));
                } else if native_reached(Some(row), &job) {
                    job["status"] = "completed".into();
                    job["reason"] = "".into();
                    job["result"] = json!({"verified":true,"unchanged":true});
                } else if self.fingerprint(row, &job)? != string(&item["expected_version"]) {
                    change = Some(("needs_confirmation", "账号模型配置或凭据已变化，请重新预览"));
                }
            } else if action == "proxy" {
                if row.is_null() || truthy(&row["deleted_at"]) {
                    change = Some(("failed", "账号已删除或不存在"));
                } else if settings_capabilities(row)["proxy"]["editable"] != true {
                    job["status"] = "failed".into();
                    job["reason"] = settings_capabilities(row)["proxy"]["reason"].clone();
                } else if target_reached(row, "proxy", &p) {
                    job["status"] = "completed".into();
                    job["reason"] = "".into();
                    job["result"] =
                        json!({"verified":true,"unchanged":true,"proxy_id":p["proxy_id"]});
                } else if !proxy_available {
                    change = Some(("needs_confirmation", "所选代理已不可用，请重新选择"));
                } else if self.fingerprint(row, &job)? != string(&item["expected_version"]) {
                    change = Some(("needs_confirmation", "代理或继承关系已变化，请重新预览"));
                    job["current"] = json!({"proxy_id":row["proxy_id"]});
                }
            } else if action == "recover" {
                if row.is_null() || truthy(&row["deleted_at"]) {
                    change = Some(("failed", "账号已删除或不存在"));
                } else if self.fingerprint(row, &job)? != string(&item["expected_version"]) {
                    change = Some(("needs_confirmation", "账号恢复状态已变化，请刷新后重新选择"));
                } else if !recoverable(row, chrono::Utc::now()) {
                    job["status"] = "completed".into();
                    job["reason"] = "无需恢复".into();
                    job["result"] = json!({"verified":true,"account_id":aid,"unchanged":true});
                }
            } else if native_reached(rows.get(&aid), &job) {
                job["status"] = "completed".into();
                job["reason"] = "".into();
                job["result"] = native_confirmed(rows.get(&aid), &job, true);
            } else if row.is_null()
                || self.fingerprint(row, &job)? != string(&item["expected_version"])
            {
                change = Some(("needs_confirmation", "相关字段或账号资格已变化，请重新预览"));
            }
            if let Some((status, reason)) = change {
                job["status"] = status.into();
                job["reason"] = reason.into();
            }
            jobs.push(job);
        }
        self.store.transaction(|data| {
            let mut batch=json!({"id":bid,"request_hash":signature,"client_id":client,"request_id":rid,"created_at":created,"job_ids":jobs.iter().map(|j|&j["id"]).collect::<Vec<_>>()});if action=="test" {batch["action"]="test".into();}data["batches"][&bid]=batch;data["batch_requests"][rid]=bid.clone().into();
            for job in &jobs {
                if matches!(action,"proxy"|"groups") && job["status"]=="queued" {for old in data["jobs"].as_object_mut().unwrap().values_mut() {if old["account_id"]==job["account_id"] && old["client_id"]==client && old["action"]==action && matches!(string(&old["status"]),"queued"|"needs_confirmation") {old["status"]="superseded".into();old["reason"]="已由本客户端的新请求替代".into();old["updated_at"]=created.clone().into();}}}
                data["jobs"][string(&job["id"])]=job.clone();data["requests"][string(&job["request_id"])]=job["id"].clone();
            }Ok(())
        })?;
        for job in &jobs {
            self.keys
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(string(&job["id"]).into(), key.into());
            self.audit(job, "accepted");
        }
        self.listing(Some(&bid))
    }
    pub async fn cancel_batch(&self, bid: &str) -> Result<Value> {
        if validation::hex(&json!(bid), 32).is_err() {
            return Err(Error::new(404, "批量测试不存在"));
        }
        let _submit = self.submit_lock.lock().await;
        self.store.transaction(|data| {
            if data["batches"][bid]["action"] != "test" {
                return Err(Error::new(404, "批量测试不存在"));
            }
            let ids = data["batches"][bid]["job_ids"]
                .as_array()
                .ok_or_else(|| Error::new(503, "操作状态无法读取"))?
                .clone();
            for jid in ids {
                let job = &mut data["jobs"][string(&jid)];
                if matches!(string(&job["status"]), "queued" | "needs_confirmation")
                    && !truthy(&job["started_at"])
                {
                    job["status"] = "cancelled".into();
                    job["reason"] = "已停止，未派发测试".into();
                    job["updated_at"] = now().into();
                }
            }
            Ok(())
        })?;
        self.listing(Some(bid))
    }
    pub async fn cancel(&self, jid: &str) -> Result<Value> {
        let data = self.store.read_shared()?;
        let Some(job) = data["jobs"].get(jid) else {
            return Ok(Self::model_view(&self.backend.model_cancel(jid).await?));
        };
        if job["repeat"] == "quota_cycle" {
            return self.stop_repeat(jid, "已停止连续预约；已派发的测试保留结果", true);
        }
        if connection_test(job) && truthy(&job["started_at"]) && job["status"] != "queued" {
            return self.get(jid, 0);
        }
        if matches!(string(&job["status"]), "queued" | "needs_confirmation") {
            self.update(jid, json!({"status":"cancelled","reason":"已取消"}))?;
        } else if job["status"] == "running" && job["action"] == "test" {
            if let Some(task) = self
                .tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(jid)
            {
                task.abort();
            }
            self.update(
                jid,
                json!({"status":"cancelled","reason":"请求已停止；已派发写入仍需核对"}),
            )?;
        } else if matches!(string(&job["status"]), "running" | "checking") {
            self.update(jid, json!({"reason":"请求已派发，等待核对结果"}))?;
        }
        self.get(jid, 0)
    }
    pub async fn tick(self: &Arc<Self>) -> Result<()> {
        if self.actions.settings.read_only
            || self.closing.load(Ordering::Acquire)
            || self.actions.supervisor.is_stopping()
        {
            return Ok(());
        }
        let (mut ops_rev, mut data) = self.store.read_shared_versioned()?;
        if self.prepare_cycles(ops_rev, &data).await? || self.store.0.cache_revision()? != ops_rev {
            (ops_rev, data) = self.store.read_shared_versioned()?;
        }
        let jobs = data["jobs"].as_object().unwrap();
        {
            let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
            if !keys.is_empty() {
                keys.retain(|id, _| jobs.get(id).is_none_or(Self::needs_key));
            }
        }
        self.evict_events(Some(jobs));
        self.record_scheduled_results(ops_rev, jobs.values())?;
        let has_active = {
            let cache = self
                .active_scan_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cache.0 == ops_rev {
                cache.1
            } else {
                drop(cache);
                let mut found_active = false;
                let mut found_cycle = false;
                for job in jobs.values() {
                    if matches!(string(&job["status"]), "queued" | "checking") {
                        found_active = true;
                    }
                    if job["repeat_state"] == "active" {
                        found_cycle = true;
                    }
                    if found_active && found_cycle {
                        break;
                    }
                }
                *self
                    .active_scan_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = (ops_rev, found_active, found_cycle);
                found_active
            }
        };
        if !has_active {
            return Ok(());
        }
        let mut running: BTreeSet<_> = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for bid in jobs
            .values()
            .filter(|j| native(j))
            .map(|j| string(&j["batch_id"]))
            .collect::<BTreeSet<_>>()
        {
            if running.contains(bid) {
                continue;
            }
            if running.len() >= 8 {
                break;
            }
            let Ok(permit) = self.budget.clone().try_acquire_owned() else {
                break;
            };
            self.launch_native(bid.to_owned(), permit);
            running.insert(bid.to_owned());
        }
        let mut occupied: BTreeSet<_> = running
            .iter()
            .filter_map(|jid| jobs.get(jid))
            .filter(|j| !short(j))
            .flat_map(|j| {
                [
                    integer(&j["account_id"]).unwrap_or(0),
                    integer(&j["parent_account_id"]).unwrap_or(0),
                ]
            })
            .filter(|id| *id > 0)
            .collect();
        let mut active_tests = running
            .iter()
            .filter_map(|jid| jobs.get(jid))
            .filter(|j| connection_test(j))
            .count();
        let mut test_limit = None;
        let mut waiting = BTreeMap::new();
        for job in jobs.values() {
            let jid = string(&job["id"]);
            let aid = integer(&job["account_id"]).unwrap();
            if !matches!(string(&job["status"]), "queued" | "checking") || running.contains(jid) {
                continue;
            }
            if native(job) {
                if !running.contains(string(&job["batch_id"])) {
                    waiting.insert(jid, "等待操作执行容量");
                }
                continue;
            }
            if job["status"] == "queued"
                && crate::compat::parse_datetime(&job["next_retry_at"])
                    .is_some_and(|at| at > self.backend.now())
            {
                waiting.insert(jid, "等待连接测试重试");
                continue;
            }
            if job["status"] == "queued"
                && crate::compat::parse_datetime(&job["scheduled_at"])
                    .is_some_and(|at| at > self.backend.now())
            {
                waiting.insert(jid, "等待预约时间");
                continue;
            }
            if connection_test(job) && job["status"] == "queued" {
                let limit = match test_limit {
                    Some(limit) => limit,
                    None => {
                        let limit = self.backend.test_config()?.1.clamp(1, 8);
                        test_limit = Some(limit);
                        limit
                    }
                };
                if active_tests >= limit {
                    waiting.insert(jid, "等待测试执行容量");
                    continue;
                }
            }
            if !short(job) && running.len() >= 8 {
                waiting.insert(jid, "等待操作执行容量");
                continue;
            }
            let account_ids = [aid, integer(&job["parent_account_id"]).unwrap_or(0)];
            if !short(job) && account_ids.iter().any(|id| occupied.contains(id)) {
                waiting.insert(jid, "等待账号空闲");
                continue;
            }
            if !short(job)
                && job["status"] == "queued"
                && self.actions.locks.try_ids(account_ids).is_err()
            {
                waiting.insert(jid, "等待账号空闲");
                continue;
            }
            let permit = if short(job) {
                None
            } else {
                match self.budget.clone().try_acquire_owned() {
                    Ok(p) => Some(p),
                    Err(_) => {
                        waiting.insert(jid, "等待操作执行容量");
                        continue;
                    }
                }
            };
            let test_permit = if job["action"] == "test" && job["status"] == "queued" {
                match self.backend.test_slots() {
                    Some(slots) => match slots.acquire() {
                        Some(permit) => Some(permit),
                        None => {
                            waiting.insert(jid, "等待测试执行容量");
                            continue;
                        }
                    },
                    None => None,
                }
            } else {
                None
            };
            self.launch(job.clone(), permit, test_permit);
            running.insert(jid.into());
            if !short(job) {
                occupied.extend(account_ids.into_iter().filter(|id| *id > 0));
            }
            if connection_test(job) {
                active_tests += 1;
            }
        }
        waiting.retain(|id, reason| jobs[*id]["reason"] != *reason);
        if !waiting.is_empty() {
            self.store.transaction(|data| {
                for (id, reason) in &waiting {
                    if data["jobs"][*id]["status"] == "queued" {
                        data["jobs"][*id]["reason"] = (*reason).into();
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    }
    fn record_scheduled_results<'a>(
        &self,
        ops_rev: u64,
        jobs: impl Iterator<Item = &'a Value>,
    ) -> Result<()> {
        // Connection-test receipts need no legacy recovery normalization.
        let (oauth_rev, state) = self.actions.queries.oauth.0.read_shared_versioned()?;
        if *self
            .scheduled_results_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            == (ops_rev, oauth_rev)
        {
            return Ok(());
        }
        let mut jobs = jobs
            .filter(|job| {
                (job["scheduled_at"].is_string() || job["repeat"] == "quota_cycle")
                    && !matches!(string(&job["status"]), "queued" | "running" | "checking")
            })
            .peekable();
        if jobs.peek().is_none() {
            *self
                .scheduled_results_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = (ops_rev, oauth_rev);
            return Ok(());
        }
        let mut history = BTreeMap::new();
        let mut pending = BTreeMap::new();
        for job in jobs {
            let key = format!("connection-test:{}", string(&job["id"]));
            if state["recovery_history"]
                .get(&key)
                .is_some_and(Value::is_object)
            {
                continue;
            }
            let result = &job["result"];
            let status = if job["status"] == "completed" && result["success"] != true {
                "failed"
            } else {
                string(&job["status"])
            };
            let entry = json!({"account_id":job["account_id"],"account_name":job["account_name"],"model_id":result["actual_model"].as_str().unwrap_or(string(&job["payload"]["model_id"])),"test_completed_at":result["completed_at"].as_str().unwrap_or(string(&job["updated_at"])),"recovered_at":null,"legacy":false,"kind":"connection_test","result_status":status,"attempt_count":job.get("attempt_count").cloned().unwrap_or_else(|| if status == "cancelled" && !truthy(&job["started_at"]) { json!(0) } else { Value::Null })});
            if matches!(status, "completed" | "failed" | "needs_confirmation")
                && job["notification_suppressed"] != true
            {
                let mut event = entry.clone();
                event["dedupe_key"] = key.clone().into();
                event["duration_ms"] = result["duration_ms"].clone();
                event["error"] = crate::security::safe_error(
                    if truthy(&result["error"]) {
                        &result["error"]
                    } else {
                        &job["reason"]
                    },
                    400,
                )
                .into();
                pending.insert(key.clone(), event);
            }
            history.insert(key, entry);
        }
        let next_oauth_rev = if !history.is_empty() {
            self.actions
                .queries
                .oauth
                .commit(crate::oauth_state::Update {
                    history,
                    pending,
                    ..Default::default()
                })?;
            self.actions.queries.oauth.0.cache_revision()?
        } else {
            oauth_rev
        };
        *self
            .scheduled_results_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (ops_rev, next_oauth_rev);
        Ok(())
    }
    fn launch(
        self: &Arc<Self>,
        job: Value,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
        test_permit: Option<crate::execution::TestPermit>,
    ) {
        if self.actions.supervisor.is_stopping() {
            return;
        }
        let jid = string(&job["id"]).to_owned();
        let queue = self.clone();
        let task_id = jid.clone();
        let (send, ready) = tokio::sync::oneshot::channel();
        let cleanup = self.clone();
        let cleanup_id = jid.clone();
        let registration = crate::supervision::on_drop(move || {
            cleanup
                .tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&cleanup_id);
        });
        let task = self.actions.supervisor.spawn("operation", async move {
            let _registration = registration;
            let _ = ready.await;
            let _permit = permit;
            let _test_permit = test_permit;
            let result = queue
                .actions
                .supervisor
                .guard("operation", queue.run(&task_id))
                .await;
            if let Some(Err(error)) = result {
                let _ = queue.run_error(&task_id, error);
            }
        });
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(jid, task);
        let _ = send.send(());
    }
    fn launch_native(self: &Arc<Self>, bid: String, permit: tokio::sync::OwnedSemaphorePermit) {
        if self.actions.supervisor.is_stopping() {
            return;
        }
        let queue = self.clone();
        let task_id = bid.clone();
        let (send, ready) = tokio::sync::oneshot::channel();
        let cleanup = self.clone();
        let cleanup_id = bid.clone();
        let registration = crate::supervision::on_drop(move || {
            cleanup
                .tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&cleanup_id);
        });
        let task = self.actions.supervisor.spawn("native_batch", async move {
            let _registration = registration;
            let _ = ready.await;
            let _permit = permit;
            let result = queue.actions.supervisor.guard("native_batch", queue.run_native(&task_id)).await;
            if let Some(Err(error)) = result {
                if let Ok(data) = queue.store.read_shared() {
                    for jid in data["batches"][&task_id]["job_ids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        let status = string(&data["jobs"][string(jid)]["status"]);
                        if matches!(status, "queued" | "running") {
                            let _=queue.update(string(jid),json!({"status":if status=="running" {"checking"} else {"needs_confirmation"},"reason":crate::security::safe_error(&error.detail,600)}));
                        }
                    }
                }
            }
            let _ = queue.refresh_chunks(&task_id);
        });
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(bid, task);
        let _ = send.send(());
    }
    async fn run(&self, jid: &str) -> Result<()> {
        if self.actions.supervisor.is_stopping() {
            return Ok(());
        }
        let job = self.store.read_shared()?["jobs"][jid].clone();
        if !matches!(string(&job["status"]), "queued" | "checking") {
            return Ok(());
        }
        let aid = integer(&job["account_id"]).unwrap();
        let mut row = self.read_account(aid).await?;
        if self.achieved(row.as_ref(), &job)? {
            let mut result = if let Some(row) = &row {
                self.current(row, &job)?
            } else {
                json!({"deleted":true})
            };
            result["verified"] = true.into();
            if job["action"] == "recover" && job["status"] == "queued" {
                result["unchanged"] = true.into();
            }
            if let Some(row) = &row {
                result["version"] = account_version(row).into();
                if job["action"] == "degradation_mark" {
                    result["degradation_mark"] = self.backend.mark(aid)?;
                }
                if EDIT_ACTIONS.contains(&string(&job["action"])) {
                    let versions = operation_versions(row);
                    result["field_version"] = versions[string(&job["action"])].clone();
                    result["settings_versions"] = pick(&versions, EDIT_ACTIONS);
                }
            }
            self.update(
                jid,
                json!({"status":"completed","reason":"","result":result}),
            )?;
            return Ok(());
        }
        if job["status"] == "checking" {
            self.update(jid,json!({"status":"needs_confirmation","reason":"已派发请求的结果未确认，未重复执行","current":self.current(row.as_ref().unwrap_or(&json!({})),&job)?}))?;
            return Ok(());
        }
        let scheduled = job["scheduled_at"].is_string();
        if row.is_none()
            || !scheduled
                && self.fingerprint(row.as_ref().unwrap(), &job)?
                    != string(&job["expected_version"])
        {
            self.update(jid,json!({"status":"needs_confirmation","reason":"相关字段、凭据或资格已变化","current":self.current(row.as_ref().unwrap_or(&json!({})),&job)?}))?;
            return Ok(());
        }
        let key = self.key(jid)?;
        if key.is_empty() {
            self.update(jid,json!({"status":"needs_confirmation","reason":"管理员授权不可用，请重新连接后提交"}))?;
            return Ok(());
        }
        self.actions.upstream.authenticate(&key, true).await?;
        if !self.cycle_allowed(&job, true).await? {
            return Ok(());
        }
        if job["repeat"] == "quota_cycle" {
            row = self.read_account(aid).await?;
            if !row
                .as_ref()
                .is_some_and(|row| row["platform"] == "openai" && row["type"] == "oauth")
            {
                self.pause_repeat(jid, "账号状态无法确认，请检查账号后重新预约")?;
                return Ok(());
            }
        }
        if self.actions.supervisor.is_stopping() {
            return Ok(());
        }
        if self.store.read_shared()?["jobs"][jid]["status"] != "queued" {
            return Ok(());
        }
        let mut started = json!({"status":"running","reason":"","started_at":now()});
        if job["action"] == "test" {
            let mut payload = job["payload"].clone();
            self.resolve_test_model(row.as_ref().unwrap(), &mut payload)?;
            started["payload"] = payload;
        }
        if scheduled {
            // A future connection test follows this account's current credentials,
            // including routine OAuth refreshes since the appointment was created.
            started["expected_version"] = self.fingerprint(row.as_ref().unwrap(), &job)?.into();
            let policy = if integer(&job["attempt_count"]).unwrap_or(0) == 0 {
                self.backend.retry_config()?
            } else {
                match crate::connection_retry::Policy::saved(&job)? {
                    Some(policy) => policy,
                    None => self.backend.retry_config()?,
                }
            };
            started["retry_policy"] = serde_json::to_value(policy)?;
            started["attempt_count"] = (integer(&job["attempt_count"]).unwrap_or(0) + 1).into();
            started["next_retry_at"] = Value::Null;
        }
        let job = self.update(jid, started)?;
        if job["status"] != "running" {
            return Ok(());
        }
        let row = row.unwrap();
        let context = OperationContext {
            action: string(&job["action"]).into(),
            expected: string(&job["expected_version"]).into(),
        };
        let result = OPERATION
            .scope(context, self.dispatch(&row, &job, &key))
            .await?;
        let uncertain = connection_test(&job) && truthy(&result["uncertain"]);
        if scheduled && self.retry_scheduled(&job, &result)? {
            return Ok(());
        }
        self.update(jid,json!({"status":if uncertain {"needs_confirmation"} else {"completed"},"reason":if uncertain {result.get("error").cloned().unwrap_or(json!(""))} else {json!("")},"result":result}))?;
        self.audit(&job, "completed");
        Ok(())
    }
    pub fn key(&self, jid: &str) -> Result<String> {
        if let Some(key) = self.keys.lock().unwrap_or_else(|e| e.into_inner()).get(jid) {
            return Ok(key.clone());
        }
        self.actions.queries.oauth.admin_token()
    }
    fn run_error(&self, jid: &str, error: Error) -> Result<()> {
        let job = self.store.read_shared()?["jobs"][jid].clone();
        let message = if error.detail.is_object() {
            error
                .detail
                .get("message")
                .cloned()
                .unwrap_or(json!("操作失败"))
        } else {
            error.detail.clone()
        };
        let mut changes = if error.detail["code"] == "account_busy" {
            json!({"status":"queued","reason":"等待账号空闲"})
        } else if error.status.as_u16() == 409 {
            json!({"status":"needs_confirmation","reason":message})
        } else if matches!(error.status.as_u16(), 502 | 504) {
            json!({"status":"checking","reason":message})
        } else if error.status.is_server_error() {
            json!({"status":"needs_confirmation","reason":"操作状态无法确认，未重复执行"})
        } else {
            json!({"status":"failed","reason":message})
        };
        if connection_test(&job)
            && (error.detail["code"] == "account_busy" || error.status.as_u16() == 409)
        {
            changes["started_at"] = Value::Null;
            if job["status"] == "running" && job["retry_policy"].is_object() {
                let attempts = integer(&job["attempt_count"])
                    .unwrap_or(1)
                    .saturating_sub(1);
                changes["attempt_count"] = attempts.into();
            }
        }
        self.update(jid, changes)?;
        Ok(())
    }
    async fn dispatch(&self, row: &Value, job: &Value, key: &str) -> Result<Value> {
        let aid = integer(&row["id"]).unwrap();
        let action = string(&job["action"]);
        let mut payload = job["payload"].clone();
        payload["expected_version"] = account_version(row).into();
        if action == "account_template" {
            payload["expected_version"] = job["expected_version"].clone();
            return self.actions.apply_template(aid, &payload, key).await;
        }
        if EDIT_ACTIONS.contains(&action) {
            return self
                .actions
                .apply_setting(
                    aid,
                    action,
                    &job["payload"],
                    string(&job["expected_version"]),
                    key,
                )
                .await;
        }
        if action == "groups" {
            return self.actions.set_groups(aid, &payload, key).await;
        }
        if action != "test" {
            return self.backend.dispatch(row, job, key).await;
        }
        let summary = connection_test(job);
        let lease = self.actions.prepare_test(aid, &payload).await?;
        let start = Instant::now();
        let mut stream = self
            .actions
            .test_stream(aid, payload.clone(), key.into(), lease, summary);
        let mut result = if summary {
            json!({"requested_model":payload["model_id"],"actual_model":null,"success":false})
        } else {
            json!({"success":false})
        };
        let mut terminal = false;
        let event_id = string(&job["id"]).to_owned();
        if !summary {
            let at = self.backend.now();
            self.evict_events(None);
            self.events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    event_id.clone(),
                    EventBuffer {
                        items: vec![],
                        terminal_at: None,
                        last_push_at: at,
                    },
                );
        }
        while let Some(event) = stream.recv().await {
            if !summary {
                let at = self.backend.now();
                let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
                let entry = events
                    .entry(event_id.clone())
                    .or_insert_with(|| EventBuffer {
                        items: vec![],
                        terminal_at: None,
                        last_push_at: at,
                    });
                entry.items.push(event.clone());
                entry.last_push_at = at;
            }
            match string(&event["type"]) {
                "test_start" if summary && event["model"].is_string() => {
                    result["actual_model"] = clean(&event["model"], 200).into()
                }
                "test_complete" => {
                    terminal = true;
                    result["success"] = (event["success"] == true).into();
                    if summary {
                        if result["success"] != true {
                            result["error"] = "上游未确认测试通过".into();
                        }
                        break;
                    }
                    result["duration_ms"] = event["duration_ms"].clone();
                }
                "error" => {
                    terminal = true;
                    result["success"] = false.into();
                    result["error"] =
                        clean(&event["error"], if summary { 240 } else { 16000 }).into();
                    if summary && event["error_code"].is_string() {
                        result["error_code"] = event["error_code"].clone();
                    }
                    if event["uncertain"] == true {
                        result["uncertain"] = true.into();
                    }
                    if summary {
                        break;
                    }
                }
                _ => {}
            }
        }
        if !summary {
            let at = self.backend.now();
            if let Some(buf) = self
                .events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&event_id)
            {
                buf.terminal_at = Some(at);
            }
            self.evict_events(None);
        }
        if summary {
            if !terminal {
                result["uncertain"] = true.into();
                result["error"] = "连接结束，未收到测试完成结果；未重放请求".into();
            }
            result["duration_ms"] =
                json!((start.elapsed().as_secs_f64() * 1000.0).round_ties_even() as u64);
            result["completed_at"] = now().into();
        }
        Ok(result)
    }
    fn evict_events(&self, jobs: Option<&serde_json::Map<String, Value>>) {
        const MAX_TERMINAL_EVENT_BUFFERS: usize = 64;
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        if events.is_empty() {
            return;
        }
        let now = self.backend.now();
        if let Some(jobs) = jobs {
            for (jid, buf) in events.iter_mut() {
                if buf.terminal_at.is_none()
                    && jobs
                        .get(jid)
                        .is_none_or(|job| !PENDING.contains(&string(&job["status"])))
                {
                    buf.terminal_at = Some(now);
                }
            }
        }
        events.retain(|_, buf| {
            buf.terminal_at
                .is_none_or(|done_at| now < done_at + chrono::Duration::seconds(60))
        });
        let terminal_count = events
            .values()
            .filter(|buf| buf.terminal_at.is_some())
            .count();
        if terminal_count > MAX_TERMINAL_EVENT_BUFFERS {
            let mut ranked: Vec<_> = events
                .iter()
                .filter_map(|(jid, buf)| {
                    buf.terminal_at
                        .map(|done_at| (done_at, buf.last_push_at, jid.clone()))
                })
                .collect();
            ranked.sort();
            for (_, _, jid) in ranked
                .into_iter()
                .take(terminal_count - MAX_TERMINAL_EVENT_BUFFERS)
            {
                events.remove(&jid);
            }
        }
    }
    pub async fn close(&self) {
        self.closing.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(55);
        while !self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        for task in tasks.values() {
            task.abort();
        }
        for (_, task) in tasks {
            let _ = task.await;
        }
        // A forced shutdown leaves running records intact for startup readback.
        self.keys.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}
fn idempotency(message: &str) -> Error {
    Error::new(
        409,
        json!({"code":"idempotency_conflict","message":message}),
    )
}
pub fn ids(value: &Value) -> BTreeSet<i64> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(integer)
        .collect()
}
pub fn native_reached(row: Option<&Value>, job: &Value) -> bool {
    if job["action"] == "delete" {
        return row.is_none();
    }
    let Some(row) = row else {
        return false;
    };
    match string(&job["action"]) {
        "account_template" => {
            account_templates::eligible(row)
                && !account_templates::passthrough(row)
                && row["model_mapping"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
                    == job["template_target"]
                        .as_object()
                        .cloned()
                        .unwrap_or_default()
        }
        "proxy" => {
            (if truthy(&row["proxy_id"]) {
                &row["proxy_id"]
            } else {
                &Value::Null
            }) == &job["payload"]["proxy_id"]
        }
        _ => {
            if job.get("native_target").is_some() {
                ids(&row["group_ids"]) == ids(&job["native_target"]["group_ids"])
            } else {
                ids(&row["group_ids"])
                    .intersection(&ids(&job["payload"]["scope_group_ids"]))
                    .copied()
                    .collect::<BTreeSet<_>>()
                    == ids(&job["payload"]["group_ids"])
            }
        }
    }
}
pub fn native_confirmed(row: Option<&Value>, job: &Value, unchanged: bool) -> Value {
    let mut result = json!({"verified":true,"account_id":job["account_id"]});
    if unchanged {
        result["unchanged"] = true.into();
    }
    let row = row.unwrap_or(&Value::Null);
    match string(&job["action"]) {
        "delete" => {
            result["deleted"] = true.into();
        }
        "groups" => {
            result["group_ids"] = json!(row["group_ids"].as_array().cloned().unwrap_or_default())
        }
        "proxy" => {
            result["proxy_id"] = if truthy(&row["proxy_id"]) {
                row["proxy_id"].clone()
            } else {
                Value::Null
            }
        }
        _ => result["applied"] = true.into(),
    }
    result
}
