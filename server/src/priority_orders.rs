//! Durable, connection-wide priority ordering. Dispatched writes are reconciled,
//! never replayed, and legacy priority operations share the same admission gate.
use crate::{
    actions::Actions,
    compat::{digest, integer, now, operation_versions, parse_datetime, string},
    credit_ownership,
    error::{Error, Result},
    operations::{self, Operations, PENDING},
    queries::settings_capabilities,
    security::clean,
    settings::Settings,
    store::Store,
    validation,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::{OwnedRwLockReadGuard, RwLock, Semaphore};

tokio::task_local! { static ORDER: String; }
const ACTIVE: &[&str] = &["queued", "running", "checking"];
const ACCOUNT_SQL: &str = "SELECT id,name,platform,type,priority,status,schedulable,extra,nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id FROM accounts WHERE deleted_at IS NULL ORDER BY id";

fn invalid_state() -> Error {
    Error::new(503, "优先级排序状态无法读取")
}
fn busy() -> Error {
    Error::new(
        409,
        json!({"code":"priority_order_busy","message":"优先级排序正在执行，请等待结果或停止剩余修改"}),
    )
}
fn id_valid(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}
fn active(data: &Value) -> Option<&str> {
    data["active_id"].as_str()
}
fn task_terminal(status: &str) -> bool {
    !ACTIVE.contains(&status)
}
fn validate_store(data: &Value) -> Result<()> {
    if data["version"] != 1 || !data["jobs"].is_object() || !data["requests"].is_object() {
        return Err(invalid_state());
    }
    for field in ["active_id", "last_id"] {
        if data.get(field).is_none()
            || !data[field].is_null()
                && !data[field]
                    .as_str()
                    .is_some_and(|id| id_valid(id) && data["jobs"].get(id).is_some())
        {
            return Err(invalid_state());
        }
    }
    for (id, job) in data["jobs"].as_object().unwrap() {
        if !id_valid(id)
            || job["id"] != *id
            || !job["status"].as_str().is_some_and(|s| {
                [
                    "queued",
                    "running",
                    "checking",
                    "completed",
                    "failed",
                    "cancelled",
                    "needs_confirmation",
                ]
                .contains(&s)
            })
            || !job["cancel_requested"].is_boolean()
            || validation::hex(&job["request_hash"], 64).is_err()
            || validation::request_id(&job["client_id"]).is_err()
            || validation::request_id(&job["request_id"]).is_err()
            || !job["reason"].is_string()
            || parse_datetime(&job["created_at"]).is_none()
            || parse_datetime(&job["updated_at"]).is_none()
            || if task_terminal(string(&job["status"])) {
                parse_datetime(&job["completed_at"]).is_none()
            } else {
                !job["completed_at"].is_null()
            }
            || data["requests"][string(&job["request_id"])] != *id
            || !job["items"].is_array()
        {
            return Err(invalid_state());
        }
        let mut ids = BTreeSet::new();
        for (index, item) in job["items"].as_array().unwrap().iter().enumerate() {
            if !item["account_id"]
                .as_i64()
                .is_some_and(|id| id > 0 && ids.insert(id))
                || item["target_priority"].as_u64() != Some(index as u64)
                || !item["original_priority"].is_i64()
                || validation::hex(&item["expected_version"], 64).is_err()
                || validation::hex(&item["identity"], 64).is_err()
                || !item["account_name"].is_string()
                || !item["capability"]["editable"].is_boolean()
                || !item["capability"]["reason"].is_string()
                || !item["observed_priority"].is_null() && !item["observed_priority"].is_i64()
                || !item["status"].as_str().is_some_and(|s| {
                    ["pending", "unchanged", "dispatched", "verified"].contains(&s)
                })
                || item["status"] == "verified"
                    && (validation::hex(&item["verified_version"], 64).is_err()
                        || item["observed_priority"] != item["target_priority"])
                || item["status"] == "unchanged"
                    && (item["original_priority"] != item["target_priority"]
                        || item["observed_priority"] != item["target_priority"])
                || item["status"] == "dispatched" && parse_datetime(&item["started_at"]).is_none()
                || job["status"] == "completed"
                    && !matches!(string(&item["status"]), "verified" | "unchanged")
            {
                return Err(invalid_state());
            }
        }
        if job["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["status"] == "dispatched")
            .count()
            > 1
        {
            return Err(invalid_state());
        }
        if ACTIVE.contains(&string(&job["status"])) && active(data) != Some(id) {
            return Err(invalid_state());
        }
    }
    if let Some(id) = active(data) {
        if task_terminal(string(&data["jobs"][id]["status"])) {
            return Err(invalid_state());
        }
    }
    for (request, id) in data["requests"].as_object().unwrap() {
        if !id
            .as_str()
            .is_some_and(|id| data["jobs"][id]["request_id"] == *request)
        {
            return Err(invalid_state());
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct PriorityGate {
    pub store: Store,
    admission: Arc<RwLock<()>>,
}
impl PriorityGate {
    pub fn new(settings: &Settings) -> Self {
        Self {
            store: Store::new(
                settings
                    .usage_path
                    .with_file_name("account-priority-orders.json"),
                json!({"version":1,"active_id":null,"last_id":null,"jobs":{},"requests":{}}),
                validate_store,
                settings.read_only,
            ),
            admission: Arc::new(RwLock::new(())),
        }
    }
    pub async fn enter(&self) -> Result<OwnedRwLockReadGuard<()>> {
        let guard = self.admission.clone().read_owned().await;
        let data = self.store.read()?;
        if let Some(id) = active(&data) {
            if !ORDER.try_with(|owner| owner == id).unwrap_or(false) {
                return Err(busy());
            }
            if data["jobs"][id]["cancel_requested"] == true {
                return Err(Error::new(409, "已停止剩余优先级修改"));
            }
        } else if ORDER.try_with(|_| ()).is_ok() {
            return Err(Error::new(409, "排序任务已结束"));
        }
        Ok(guard)
    }
    /// Called with the priority gate and account lease held, immediately before
    /// the only upstream write. A busy account has not reached this boundary.
    pub async fn before_write(
        &self,
        db: &crate::db::Database,
        account_id: i64,
        expected: &str,
        target: &Value,
    ) -> Result<()> {
        let Ok(owner) = ORDER.try_with(Clone::clone) else {
            return Ok(());
        };
        let rows = db.all(ACCOUNT_SQL, &[]).await?;
        let data = self.store.read()?;
        if active(&data) != Some(owner.as_str())
            || !PriorityOrders::matches(&data["jobs"][&owner], &rows)
        {
            return Err(Error::new(
                409,
                "账号集合、优先级或编辑资格已变化，剩余修改未执行",
            ));
        }
        self.store.transaction(|data| {
            if active(data) != Some(owner.as_str())
                || data["jobs"][&owner]["cancel_requested"] == true
            {
                return Err(Error::new(409, "已停止剩余优先级修改"));
            }
            let job = &mut data["jobs"][&owner];
            let item = job["items"]
                .as_array_mut()
                .ok_or_else(invalid_state)?
                .iter_mut()
                .find(|item| item["account_id"] == account_id)
                .ok_or_else(invalid_state)?;
            if item["status"] != "pending"
                || item["expected_version"] != expected
                || item["target_priority"] != *target
            {
                return Err(Error::new(409, "排序请求已变化，未重复派发"));
            }
            item["status"] = "dispatched".into();
            item["started_at"] = now().into();
            job["status"] = "running".into();
            job["updated_at"] = now().into();
            Ok(())
        })
    }
}

pub struct PriorityOrders {
    pub store: Store,
    actions: Actions,
    operations: Arc<Operations>,
    budget: Arc<Semaphore>,
    running: Arc<AtomicBool>,
}
fn version(row: &Value) -> Value {
    operation_versions(row, false)["priority"].clone()
}
fn identity(row: &Value) -> String {
    let mut stable = row.clone();
    stable["priority"] = 0.into();
    string(&version(&stable)).to_owned()
}
fn list_version(rows: &[Value]) -> String {
    digest(&json!(rows
        .iter()
        .map(|row| json!([
            row["id"],
            version(row),
            settings_capabilities(row)["priority"]
        ]))
        .collect::<Vec<_>>()))
}
fn public_item(row: &Value) -> Value {
    let capability = settings_capabilities(row)["priority"].clone();
    json!({"id":row["id"],"name":clean(&row["name"],200),"platform":clean(&row["platform"],60),"type":clean(&row["type"],60),
        "priority":integer(&row["priority"]).unwrap_or(0),"version":version(row),"editable":capability["editable"],"reason":capability["reason"],
        "status":clean(&row["status"],60),"schedulable":row["schedulable"]==true})
}
fn public_job(job: &Value, details: bool) -> Value {
    let Some(items) = job["items"].as_array() else {
        return Value::Null;
    };
    let mut result = json!({"id":job["id"],"status":job["status"],"cancel_requested":job["cancel_requested"],"reason":job["reason"],
        "total":items.len(),"change_count":items.iter().filter(|i|i["original_priority"]!=i["target_priority"]).count(),
        "verified_count":items.iter().filter(|i|matches!(string(&i["status"]),"verified"|"unchanged")).count(),
        "created_at":job["created_at"],"updated_at":job["updated_at"],"completed_at":job["completed_at"]});
    if details {
        result["items"] = json!(items.iter().map(|item| json!({"account_id":item["account_id"],"account_name":item["account_name"],
        "original_priority":item["original_priority"],"target_priority":item["target_priority"],"observed_priority":item["observed_priority"],"status":item["status"]})).collect::<Vec<_>>());
    }
    result
}
pub fn validate_request(request: &Value) -> Result<()> {
    validation::fields(
        request,
        &[
            "ordered_account_ids",
            "expected_version",
            "request_id",
            "client_id",
        ],
        &[
            "ordered_account_ids",
            "expected_version",
            "request_id",
            "client_id",
        ],
    )?;
    validation::hex(&request["expected_version"], 64)?;
    validation::request_id(&request["request_id"])?;
    validation::request_id(&request["client_id"])?;
    let ids = request["ordered_account_ids"]
        .as_array()
        .filter(|ids| ids.len() <= i32::MAX as usize)
        .ok_or_else(Error::invalid)?;
    let mut seen = BTreeSet::new();
    for id in ids {
        let id = validation::integer(id, 1, 9_007_199_254_740_991)?;
        if !seen.insert(id) {
            return Err(Error::new(422, "账号编号不能重复"));
        }
    }
    Ok(())
}
impl PriorityOrders {
    pub fn new(actions: Actions, operations: Arc<Operations>, budget: Arc<Semaphore>) -> Arc<Self> {
        Arc::new(Self {
            store: actions.priorities.store.clone(),
            actions,
            operations,
            budget,
            running: Arc::new(AtomicBool::new(false)),
        })
    }
    async fn rows(&self) -> Result<Vec<Value>> {
        self.actions.queries.db.all(ACCOUNT_SQL, &[]).await
    }
    pub async fn view(&self) -> Result<Value> {
        let rows = self.rows().await?;
        let data = self.store.read()?;
        let items: Vec<_> = rows
            .iter()
            .map(|row| {
                let mut item = public_item(row);
                if self.actions.settings.read_only {
                    item["editable"] = false.into();
                    item["reason"] = "Companion 处于只读模式".into();
                }
                item
            })
            .collect();
        Ok(json!({"items":items,"version":list_version(&rows),
            "active_task":active(&data).map(|id|public_job(&data["jobs"][id],false)),
            "last_task":data["last_id"].as_str().map(|id|public_job(&data["jobs"][id],false))}))
    }
    pub fn get(&self, id: &str) -> Result<Value> {
        if !id_valid(id) {
            return Err(Error::new(404, "优先级排序任务不存在"));
        }
        let data = self.store.read()?;
        let job = data["jobs"]
            .get(id)
            .ok_or_else(|| Error::new(404, "优先级排序任务不存在"))?;
        Ok(public_job(job, true))
    }
    pub async fn submit(&self, request: &Value) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        validate_request(request)?;
        let _submit = self.operations.submit_lock.lock().await;
        let _gate = self.actions.priorities.admission.write().await;
        let data = self.store.read()?;
        let rid = string(&request["request_id"]);
        let signature = digest(&json!([
            request["ordered_account_ids"],
            request["expected_version"]
        ]));
        if let Some(id) = data["requests"][rid].as_str() {
            let job = &data["jobs"][id];
            if job["request_hash"] != signature || job["client_id"] != request["client_id"] {
                return Err(Error::new(409, "请求 ID 已用于其他排序"));
            }
            return Ok(public_job(job, true));
        }
        if active(&data).is_some() {
            return Err(busy());
        }
        if self.operations.store.read()?["jobs"]
            .as_object()
            .ok_or_else(invalid_state)?
            .values()
            .any(|job| job["action"] == "priority" && PENDING.contains(&string(&job["status"])))
        {
            return Err(Error::new(409, "请先完成或取消操作待办中的优先级请求"));
        }
        let rows = self.rows().await?;
        let ids = request["ordered_account_ids"].as_array().unwrap();
        if list_version(&rows) != request["expected_version"]
            || ids.len() != rows.len()
            || ids
                .iter()
                .any(|id| !rows.iter().any(|row| row["id"] == *id))
        {
            return Err(Error::new(409, "账号集合或优先级已变化，请重新核对"));
        }
        let mut items = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            let row = rows.iter().find(|row| row["id"] == *id).unwrap();
            let original = integer(&row["priority"]).unwrap_or(0);
            let changed = original != index as i64;
            if changed && settings_capabilities(row)["priority"]["editable"] != true {
                return Err(Error::new(409, "排序包含不可修改的账号，请重新核对"));
            }
            items.push(json!({"account_id":id,"account_name":clean(&row["name"],200),"original_priority":original,"target_priority":index,
                "expected_version":version(row),"identity":identity(row),"capability":settings_capabilities(row)["priority"],"status":if changed {"pending"} else {"unchanged"},
                "observed_priority":if changed {Value::Null} else {json!(original)}}));
        }
        let id = operations::id();
        let created = now();
        let job = json!({"id":id,"status":"queued","reason":"等待执行","cancel_requested":false,"items":items,
            "request_id":rid,"client_id":request["client_id"],"request_hash":signature,"created_at":created,"updated_at":created,"completed_at":null});
        self.store.transaction(|data| {
            if active(data).is_some() {
                return Err(busy());
            }
            data["jobs"][&id] = job.clone();
            data["requests"][rid] = id.clone().into();
            data["active_id"] = id.clone().into();
            data["last_id"] = id.clone().into();
            Ok(())
        })?;
        Ok(public_job(&job, true))
    }
    fn finish(&self, id: &str, status: &str, reason: &str) -> Result<()> {
        self.store.transaction(|data| {
            if active(data) != Some(id) {
                return Ok(());
            }
            let job = &mut data["jobs"][id];
            job["status"] = status.into();
            job["reason"] = reason.into();
            job["updated_at"] = now().into();
            job["completed_at"] = now().into();
            data["active_id"] = Value::Null;
            Ok(())
        })
    }
    pub async fn cancel(&self, id: &str) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        self.get(id)?;
        self.store.transaction(|data| {
            if active(data) == Some(id) {
                data["jobs"][id]["cancel_requested"] = true.into();
                data["jobs"][id]["reason"] = "正在停止，已派发项仍需核对".into();
                data["jobs"][id]["updated_at"] = now().into();
            }
            Ok(())
        })?;
        self.get(id)
    }
    pub fn recover_startup(&self) -> Result<()> {
        if self.actions.settings.read_only {
            self.store.read()?;
            return Ok(());
        }
        if active(&self.store.read()?).is_none() {
            return Ok(());
        }
        self.store.transaction(|data| {
            if let Some(id) = active(data).map(str::to_owned) {
                let job = &mut data["jobs"][&id];
                job["status"] = "checking".into();
                job["reason"] = "服务重启，正在核对排序进度".into();
            }
            Ok(())
        })
    }
    fn verified(&self, id: &str, account_id: i64, row: &Value) -> Result<()> {
        self.store.transaction(|data| {
            let job = &mut data["jobs"][id];
            let item = job["items"]
                .as_array_mut()
                .ok_or_else(invalid_state)?
                .iter_mut()
                .find(|item| item["account_id"] == account_id)
                .ok_or_else(invalid_state)?;
            item["status"] = "verified".into();
            item["verified_version"] = version(row);
            item["observed_priority"] = row["priority"].clone();
            job["updated_at"] = now().into();
            Ok(())
        })
    }
    fn matches(job: &Value, rows: &[Value]) -> bool {
        let items = job["items"].as_array().unwrap();
        items.len() == rows.len()
            && items.iter().all(|item| {
                rows.iter()
                    .find(|row| row["id"] == item["account_id"])
                    .is_some_and(|row| {
                        identity(row) == item["identity"]
                            && settings_capabilities(row)["priority"] == item["capability"]
                            && (item["original_priority"] == item["target_priority"]
                                || settings_capabilities(row)["priority"]["editable"] == true)
                            && if item["status"] == "dispatched" {
                                true
                            } else {
                                version(row)
                                    == if item["status"] == "verified" {
                                        &item["verified_version"]
                                    } else {
                                        &item["expected_version"]
                                    }
                                    .clone()
                            }
                    })
            })
    }
    async fn run(&self, id: &str) -> Result<()> {
        loop {
            if self.actions.supervisor.is_stopping() {
                return Ok(());
            }
            let data = self.store.read()?;
            if active(&data) != Some(id) {
                return Ok(());
            }
            let job = &data["jobs"][id];
            let rows = self.rows().await?;
            let items = job["items"].as_array().unwrap();
            if let Some(item) = items.iter().find(|item| item["status"] == "dispatched") {
                let Some(row) = rows.iter().find(|row| row["id"] == item["account_id"]) else {
                    return self.finish(id, "needs_confirmation", "已派发账号无法读取，请重新核对");
                };
                let _gate = self.actions.priorities.admission.read().await;
                let _lease = match self.actions.locks.try_account(row) {
                    Ok(v) => v,
                    Err(_) => return Ok(()),
                };
                let aid = integer(&row["id"]).unwrap();
                let fresh = self.rows().await?;
                let Some(row) = fresh.iter().find(|row| row["id"] == aid) else {
                    return self.finish(id, "needs_confirmation", "已派发账号无法读取，请重新核对");
                };
                self.store.transaction(|data| {
                    let item = data["jobs"][id]["items"]
                        .as_array_mut()
                        .ok_or_else(invalid_state)?
                        .iter_mut()
                        .find(|item| item["account_id"] == aid)
                        .ok_or_else(invalid_state)?;
                    item["observed_priority"] = row["priority"].clone();
                    Ok(())
                })?;
                if row["priority"] != item["target_priority"] || identity(row) != item["identity"] {
                    return self.finish(
                        id,
                        "needs_confirmation",
                        "已派发写入未确认，未重复执行；请重新核对",
                    );
                }
                credit_ownership::reconcile(
                    &self.actions.queries.oauth,
                    &credit_ownership::account(&self.actions.queries.db, aid).await?,
                )?;
                self.verified(id, aid, row)?;
                continue;
            }
            if !Self::matches(job, &rows) {
                return self.finish(
                    id,
                    "needs_confirmation",
                    "账号集合、优先级或编辑资格已变化，剩余修改未执行",
                );
            }
            if job["cancel_requested"] == true {
                return self.finish(id, "cancelled", "剩余修改已停止，已完成的优先级保持不变");
            }
            let Some(item) = items.iter().find(|item| item["status"] == "pending") else {
                if items.iter().all(|item| {
                    rows.iter().any(|row| {
                        row["id"] == item["account_id"]
                            && row["priority"] == item["target_priority"]
                    })
                }) {
                    return self.finish(id, "completed", "全部优先级已回读确认");
                }
                return self.finish(id, "needs_confirmation", "最终优先级核验不一致，请重新核对");
            };
            let aid = integer(&item["account_id"]).unwrap();
            let key = self.actions.queries.oauth.admin_token()?;
            if key.is_empty() {
                return self.finish(
                    id,
                    "needs_confirmation",
                    "管理员授权不可用，请重新连接后核对",
                );
            }
            self.actions.upstream.authenticate(&key, true).await?;
            let result = ORDER
                .scope(
                    id.to_owned(),
                    self.actions.apply_setting(
                        aid,
                        "priority",
                        &json!({"priority":item["target_priority"]}),
                        string(&item["expected_version"]),
                        &key,
                    ),
                )
                .await;
            match result {
                Ok(_) => { /* Always reread through the dispatched reconciliation path. */ }
                Err(error) if error.detail["code"] == "account_busy" => return Ok(()),
                Err(error) => {
                    let current = self.store.read()?;
                    if current["jobs"][id]["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item["status"] == "dispatched")
                    {
                        continue;
                    }
                    if current["jobs"][id]["cancel_requested"] == true {
                        return self.finish(
                            id,
                            "cancelled",
                            "剩余修改已停止，已完成的优先级保持不变",
                        );
                    }
                    return self.finish(
                        id,
                        if error.status.as_u16() == 409 {
                            "needs_confirmation"
                        } else {
                            "failed"
                        },
                        &crate::security::safe_error(&error.detail, 300),
                    );
                }
            }
        }
    }
    pub async fn tick(self: &Arc<Self>) -> Result<()> {
        if self.actions.settings.read_only
            || self.actions.supervisor.is_stopping()
            || self.running.load(Ordering::Acquire)
        {
            return Ok(());
        }
        let Some(id) = active(&self.store.read()?).map(str::to_owned) else {
            return Ok(());
        };
        let Ok(permit) = self.budget.clone().try_acquire_owned() else {
            return Ok(());
        };
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let this = self.clone();
        let running = self.running.clone();
        let cleanup = crate::supervision::on_drop(move || running.store(false, Ordering::Release));
        self.actions.supervisor.spawn("priority_order", async move {
            let _cleanup = cleanup;
            let _permit = permit;
            if let Err(error) = this.run(&id).await {
                let _ = this.finish(
                    &id,
                    "needs_confirmation",
                    &crate::security::safe_error(&error.detail, 300),
                );
            }
        });
        Ok(())
    }
}
