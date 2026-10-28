//! Durable, platform-scoped priority ordering. Dispatched writes are reconciled,
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
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::{OwnedRwLockReadGuard, RwLock};

tokio::task_local! { static ORDER: String; }
const ACTIVE: &[&str] = &["queued", "running", "checking"];
const ACCOUNT_SQL: &str = "SELECT id,name,platform,type,priority,status,schedulable,extra,nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id FROM accounts WHERE deleted_at IS NULL ORDER BY id";

async fn account_rows(db: &crate::db::Database, platform: Option<&str>) -> Result<Vec<Value>> {
    match platform {
        Some(platform) => {
            static BY_PLATFORM_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
                ACCOUNT_SQL.replace("ORDER BY id", "AND platform=$1 ORDER BY id")
            });
            db.all(&BY_PLATFORM_SQL, &[platform.into()]).await
        }
        None => db.all(ACCOUNT_SQL, &[]).await,
    }
}
fn platform_valid(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 64)
}
pub fn upgrade_required() -> Error {
    Error::new(
        422,
        json!({"code":"client_upgrade_required","message":"请升级客户端以按平台排序"}),
    )
}

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
    validate_store_schema(data, false)
}
fn validate_v2(data: &Value) -> Result<()> {
    validate_store_schema(data, true)
}
fn validate_store_schema(data: &Value, v2: bool) -> Result<()> {
    if data["version"] != if v2 { 2 } else { 1 }
        || !data["jobs"].is_object()
        || !data["requests"].is_object()
    {
        return Err(invalid_state());
    }
    if v2
        && data.get("parallelism").is_some_and(|values| {
            !values.as_object().is_some_and(|values| {
                values.iter().all(|(platform, value)| {
                    platform_valid(&Value::String(platform.clone()))
                        && value.as_u64().is_some_and(|value| (1..=8).contains(&value))
                })
            })
        })
    {
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
        if v2
            && (!platform_valid(&job["platform"])
                || job["base"] != 1
                || job.get("parallelism").is_some_and(|value| {
                    !value.as_u64().is_some_and(|value| (1..=8).contains(&value))
                })
                || !job["waiting_reason"].is_string()
                || !job["waiting_account_ids"].as_array().is_some_and(|ids| {
                    let accounts: HashSet<_> = job["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|item| item["account_id"].as_i64())
                        .collect();
                    let mut seen = HashSet::new();
                    ids.iter().all(|id| {
                        id.as_i64()
                            .is_some_and(|n| n > 0 && seen.insert(n) && accounts.contains(&n))
                    })
                }))
        {
            return Err(invalid_state());
        }
        let mut ids = BTreeSet::new();
        let mut target_counts = HashMap::<i64, usize>::new();
        for (index, item) in job["items"].as_array().unwrap().iter().enumerate() {
            let target = item["target_priority"].as_i64();
            let target_valid = if v2 && job.get("parallelism").is_some() {
                target.is_some_and(|value| (0..=i32::MAX as i64).contains(&value))
            } else {
                item["target_priority"].as_u64() == Some(index as u64 + u64::from(v2))
            };
            if let Some(target) = target {
                *target_counts.entry(target).or_default() += 1;
            }
            if !item["account_id"]
                .as_i64()
                .is_some_and(|id| id > 0 && ids.insert(id))
                || !target_valid
                || v2 && item["platform"] != job["platform"]
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
        if v2 && job.get("parallelism").is_some() {
            let capacity = job["parallelism"].as_u64().unwrap_or(1) as usize;
            if target_counts.values().any(|count| *count > capacity) {
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
    pub legacy: Store,
    admission: Arc<RwLock<()>>,
}
impl PriorityGate {
    pub fn new(settings: &Settings) -> Self {
        Self {
            store: Store::new(
                settings
                    .usage_path
                    .with_file_name("account-priority-orders-v2.json"),
                json!({"version":2,"active_id":null,"last_id":null,"jobs":{},"requests":{},"parallelism":{}}),
                validate_v2,
                settings.read_only,
            ),
            legacy: Store::new(
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
        let legacy = self.legacy.read_shared()?;
        if active(&legacy).is_some() {
            return Err(Error::new(409, "旧版排序正在收束，请稍后重新核对"));
        }
        let data = self.store.read_shared()?;
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
        let data = self.store.read_shared()?;
        let rows = account_rows(db, Some(string(&data["jobs"][&owner]["platform"]))).await?;
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
            job["reason"] = "正在保存".into();
            job["waiting_reason"] = "".into();
            job["waiting_account_ids"] = json!([]);
            job["updated_at"] = now().into();
            Ok(())
        })
    }
    pub fn after_write(&self, account_id: i64, row: &Value) -> Result<()> {
        let Ok(owner) = ORDER.try_with(Clone::clone) else {
            return Ok(());
        };
        record_verified(&self.store, &owner, account_id, row)
    }
}

pub struct PriorityOrders {
    pub store: Store,
    pub legacy: Store,
    actions: Actions,
    operations: Arc<Operations>,
    running: Arc<AtomicBool>,
}
fn version(row: &Value) -> Value {
    operation_versions(row)["priority"].clone()
}
fn identity(row: &Value) -> String {
    let mut stable = row.clone();
    stable["priority"] = 0.into();
    string(&version(&stable)).to_owned()
}
fn list_version(rows: &[Value], parallelism: u64) -> String {
    let entries = json!(rows
        .iter()
        .map(|row| json!([
            row["id"],
            version(row),
            settings_capabilities(row)["priority"]
        ]))
        .collect::<Vec<_>>());
    if parallelism == 1 {
        digest(&entries)
    } else {
        digest(&json!([parallelism, entries]))
    }
}
fn parallelism(data: &Value, platform: &str) -> u64 {
    data["parallelism"][platform]
        .as_u64()
        .filter(|value| (1..=8).contains(value))
        .unwrap_or(1)
}
fn public_item(row: &Value) -> Value {
    let capability = settings_capabilities(row)["priority"].clone();
    json!({"id":row["id"],"name":clean(&row["name"],200),"platform":clean(&row["platform"],64),"type":clean(&row["type"],60),
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
fn public_job_v2(job: &Value, details: bool) -> Value {
    let mut result = public_job(job, details);
    if result.is_null() {
        return result;
    }
    result["platform"] = job["platform"].clone();
    result["base"] = if job["base"] == 1 { json!(1) } else { json!(0) };
    if let Some(parallelism) = job.get("parallelism") {
        result["parallelism"] = parallelism.clone();
    }
    result["waiting_reason"] = job.get("waiting_reason").cloned().unwrap_or(json!(""));
    result["waiting_account_ids"] = job.get("waiting_account_ids").cloned().unwrap_or(json!([]));
    if details && job["base"] == 1 {
        for item in result["items"].as_array_mut().unwrap() {
            item["platform"] = job["platform"].clone();
        }
    }
    result
}
fn record_verified(store: &Store, id: &str, account_id: i64, row: &Value) -> Result<()> {
    store.transaction(|data| {
        if active(data) != Some(id) {
            return Err(Error::new(409, "排序任务已结束"));
        }
        let job = &mut data["jobs"][id];
        let item = job["items"]
            .as_array_mut()
            .ok_or_else(invalid_state)?
            .iter_mut()
            .find(|item| item["account_id"] == account_id)
            .ok_or_else(invalid_state)?;
        if item["status"] != "dispatched"
            || row["priority"] != item["target_priority"]
            || identity(row) != item["identity"]
        {
            return Err(Error::new(409, "已派发账号身份或优先级尚未确认"));
        }
        item["status"] = "verified".into();
        item["verified_version"] = version(row);
        item["observed_priority"] = row["priority"].clone();
        job["updated_at"] = now().into();
        Ok(())
    })
}

pub fn validate_request(request: &Value) -> Result<()> {
    validation::fields(
        request,
        &[
            "platform",
            "ordered_account_ids",
            "target_priorities",
            "parallelism",
            "expected_version",
            "request_id",
            "client_id",
        ],
        &[
            "platform",
            "ordered_account_ids",
            "expected_version",
            "request_id",
            "client_id",
        ],
    )?;
    if !platform_valid(&request["platform"]) {
        return Err(Error::invalid());
    }
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
    if let Some(parallelism) = request.get("parallelism") {
        validation::integer(parallelism, 1, 8)?;
    }
    if let Some(targets) = request.get("target_priorities") {
        let targets = targets
            .as_array()
            .filter(|targets| targets.len() == ids.len())
            .ok_or_else(Error::invalid)?;
        for target in targets {
            validation::integer(target, 0, i32::MAX as i64)?;
        }
    }
    Ok(())
}

pub fn validate_parallelism_request(request: &Value) -> Result<()> {
    validation::fields(
        request,
        &["platform", "parallelism", "expected_version"],
        &["platform", "parallelism", "expected_version"],
    )?;
    if !platform_valid(&request["platform"]) {
        return Err(Error::invalid());
    }
    validation::integer(&request["parallelism"], 1, 8)?;
    validation::hex(&request["expected_version"], 64).map(|_| ())
}
impl PriorityOrders {
    pub fn new(actions: Actions, operations: Arc<Operations>) -> Arc<Self> {
        Arc::new(Self {
            store: actions.priorities.store.clone(),
            legacy: actions.priorities.legacy.clone(),
            actions,
            operations,
            running: Arc::new(AtomicBool::new(false)),
        })
    }
    async fn rows(&self, platform: Option<&str>) -> Result<Vec<Value>> {
        account_rows(&self.actions.queries.db, platform).await
    }
    pub async fn view(&self, v2: bool) -> Result<Value> {
        let rows = self.rows(None).await?;
        let legacy = self.legacy.read_shared()?;
        let data = self.store.read_shared()?;
        let items: Vec<_> = rows
            .iter()
            .map(|row| {
                let mut item = public_item(row);
                if !v2 || self.actions.settings.read_only {
                    item["editable"] = false.into();
                    item["reason"] = if !v2 {
                        "请升级客户端以按平台排序"
                    } else {
                        "Companion 处于只读模式"
                    }
                    .into();
                }
                item
            })
            .collect();
        if !v2 {
            return Ok(json!({"items":items,"version":list_version(&rows, 1),
                "active_task":active(&legacy).map(|id|public_job(&legacy["jobs"][id],false)),
                "last_task":legacy["last_id"].as_str().map(|id|public_job(&legacy["jobs"][id],false))}));
        }
        let mut groups: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
        for item in &items {
            groups
                .entry(string(&item["platform"]))
                .or_default()
                .push(item.clone());
        }
        // Keep a deleted platform's last task reachable for reconciliation.
        for job in data["jobs"].as_object().unwrap().values() {
            groups.entry(string(&job["platform"])).or_default();
        }
        let mut raw: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
        for row in &rows {
            raw.entry(string(&row["platform"]))
                .or_default()
                .push(row.clone());
        }
        let platforms: Vec<_> = groups.into_iter().map(|(platform, items)| {
            let width = parallelism(&data, platform);
            let last = data["jobs"].as_object().unwrap().values().filter(|job| job["platform"] == platform)
                .max_by_key(|job| (string(&job["created_at"]), string(&job["id"])));
            json!({"platform":platform,"items":items,"parallelism":width,"version":list_version(raw.get(platform).map(Vec::as_slice).unwrap_or_default(), width),"last_task":last.map(|job|public_job_v2(job,false))})
        }).collect();
        Ok(json!({"platforms":platforms,
            "active_task":active(&data).map(|id|public_job_v2(&data["jobs"][id],false)),
            "legacy_last_task":legacy["last_id"].as_str().map(|id|public_job_v2(&legacy["jobs"][id],false))}))
    }
    pub fn get(&self, id: &str, v2: bool, details: bool) -> Result<Value> {
        if !id_valid(id) {
            return Err(Error::new(404, "优先级排序任务不存在"));
        }
        if v2 {
            let data = self.store.read_shared()?;
            if let Some(job) = data["jobs"].get(id) {
                return Ok(public_job_v2(job, details));
            }
        }
        let data = self.legacy.read_shared()?;
        let job = data["jobs"]
            .get(id)
            .ok_or_else(|| Error::new(404, "优先级排序任务不存在"))?;
        Ok(if v2 {
            public_job_v2(job, details)
        } else {
            public_job(job, details)
        })
    }
    pub async fn submit(&self, request: &Value) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        validate_request(request)?;
        let _submit = self.operations.submit_lock.lock().await;
        let _gate = self.actions.priorities.admission.write().await;
        let data = self.store.read_shared()?;
        let rid = string(&request["request_id"]);
        let mut request_identity = vec![
            request["platform"].clone(),
            request["client_id"].clone(),
            request["ordered_account_ids"].clone(),
            request["expected_version"].clone(),
        ];
        if request.get("parallelism").is_some() || request.get("target_priorities").is_some() {
            request_identity.extend([
                request["parallelism"].clone(),
                request["target_priorities"].clone(),
            ]);
        }
        let signature = digest(&json!(request_identity));
        if let Some(id) = data["requests"][rid].as_str() {
            let job = &data["jobs"][id];
            if job["request_hash"] != signature || job["client_id"] != request["client_id"] {
                return Err(Error::new(409, "请求 ID 已用于其他排序"));
            }
            return Ok(public_job_v2(job, true));
        }
        let legacy = self.legacy.read_shared()?;
        if active(&data).is_some() || active(&legacy).is_some() {
            return Err(busy());
        }
        if legacy["requests"].get(rid).is_some() {
            return Err(Error::new(409, "请求 ID 已用于旧版排序"));
        }
        if self.operations.store.read_shared()?["jobs"]
            .as_object()
            .ok_or_else(invalid_state)?
            .values()
            .any(|job| job["action"] == "priority" && PENDING.contains(&string(&job["status"])))
        {
            return Err(Error::new(409, "请先完成或取消操作待办中的优先级请求"));
        }
        let platform = string(&request["platform"]);
        let rows = self.rows(Some(platform)).await?;
        let width = parallelism(&data, platform);
        if request
            .get("parallelism")
            .is_some_and(|value| value.as_u64() != Some(width))
        {
            return Err(Error::new(409, "平台并列值已变化，请重新核对"));
        }
        let by_id: HashMap<i64, &Value> = rows
            .iter()
            .map(|row| (integer(&row["id"]).unwrap(), row))
            .collect();
        let ids = request["ordered_account_ids"].as_array().unwrap();
        if rows.is_empty()
            || !(1..=8).contains(&width)
            || list_version(&rows, width) != request["expected_version"]
            || ids.len() != rows.len()
            || ids
                .iter()
                .any(|id| !by_id.contains_key(&integer(id).unwrap()))
        {
            return Err(Error::new(409, "账号集合或优先级已变化，请重新核对"));
        }
        let targets = request["target_priorities"].as_array();
        let mut target_counts = HashMap::<i64, usize>::new();
        let mut items = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            let row = by_id[&integer(id).unwrap()];
            let original = integer(&row["priority"]).unwrap_or(0);
            let target = targets
                .and_then(|targets| targets.get(index))
                .and_then(Value::as_i64)
                .unwrap_or(index as i64 / width as i64 + 1);
            *target_counts.entry(target).or_default() += 1;
            let changed = original != target;
            if changed && settings_capabilities(row)["priority"]["editable"] != true {
                return Err(Error::new(409, "排序包含不可修改的账号，请重新核对"));
            }
            items.push(json!({"account_id":id,"account_name":clean(&row["name"],200),"original_priority":original,"target_priority":target,"platform":platform,
                "expected_version":version(row),"identity":identity(row),"capability":settings_capabilities(row)["priority"],"status":if changed {"pending"} else {"unchanged"},
                "observed_priority":if changed {Value::Null} else {json!(original)}}));
        }
        if target_counts.values().any(|count| *count > width as usize) {
            return Err(Error::new(422, "并列槽位数量超过平台设置"));
        }
        let id = operations::id();
        let created = now();
        let job = json!({"id":id,"platform":platform,"base":1,"parallelism":width,"waiting_reason":"","waiting_account_ids":[],"status":"queued","reason":"等待执行","cancel_requested":false,"items":items,
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
        Ok(public_job_v2(&job, true))
    }
    pub async fn set_parallelism(&self, request: &Value) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        validate_parallelism_request(request)?;
        let _gate = self.actions.priorities.admission.write().await;
        let data = self.store.read_shared()?;
        let legacy = self.legacy.read_shared()?;
        if active(&data).is_some() || active(&legacy).is_some() {
            return Err(busy());
        }
        let platform = string(&request["platform"]);
        let rows = self.rows(Some(platform)).await?;
        let current = parallelism(&data, platform);
        if rows.is_empty() || list_version(&rows, current) != request["expected_version"] {
            return Err(Error::new(409, "账号集合或优先级已变化，请重新核对"));
        }
        let width = request["parallelism"].as_u64().unwrap();
        self.store.transaction(|data| {
            if active(data).is_some() {
                return Err(busy());
            }
            if !data["parallelism"].is_object() {
                data["parallelism"] = json!({});
            }
            data["parallelism"][platform] = width.into();
            Ok(())
        })?;
        self.view(true).await
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
            job["waiting_reason"] = "".into();
            job["waiting_account_ids"] = json!([]);
            data["active_id"] = Value::Null;
            Ok(())
        })
    }
    pub async fn cancel(&self, id: &str, v2: bool) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        self.get(id, v2, true)?;
        if v2 {
            self.store.transaction(|data| {
                if active(data) == Some(id) {
                    data["jobs"][id]["cancel_requested"] = true.into();
                    data["jobs"][id]["reason"] = "正在停止，已派发项仍需核对".into();
                    data["jobs"][id]["updated_at"] = now().into();
                }
                Ok(())
            })?;
        }
        self.get(id, v2, true)
    }
    pub async fn recover_startup(&self) -> Result<()> {
        self.legacy.read_shared()?;
        let data = self.store.read_shared()?;
        if self.actions.settings.read_only {
            return Ok(());
        }
        self.retire_legacy().await?;
        if active(&data).is_some() {
            self.store.transaction(|data| {
                if let Some(id) = active(data).map(str::to_owned) {
                    data["jobs"][&id]["status"] = "checking".into();
                    data["jobs"][&id]["reason"] = "服务重启，正在核对排序进度".into();
                }
                Ok(())
            })?;
        }
        Ok(())
    }
    async fn retire_legacy(&self) -> Result<bool> {
        let data = self.legacy.read_shared()?;
        let Some(id) = active(&data) else {
            return Ok(true);
        };
        let _gate = self.actions.priorities.admission.write().await;
        let mut observed = None;
        let mut _lease = None;
        if let Some(item) = data["jobs"][id]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["status"] == "dispatched")
        {
            let rows = self.rows(None).await?;
            if let Some(row) = rows.iter().find(|row| row["id"] == item["account_id"]) {
                _lease = match self.actions.locks.try_account(row) {
                    Ok(lease) => Some(lease),
                    Err(error) if error.detail["code"] == "account_busy" => return Ok(false),
                    Err(error) => return Err(error),
                };
                let fresh = self.rows(None).await?;
                if let Some(row) = fresh
                    .into_iter()
                    .find(|row| row["id"] == item["account_id"])
                {
                    credit_ownership::reconcile(
                        &self.actions.queries.oauth,
                        &credit_ownership::account(
                            &self.actions.queries.db,
                            integer(&item["account_id"]).unwrap(),
                        )
                        .await?,
                    )?;
                    observed = Some(row);
                }
            }
        }
        self.legacy.transaction(|data| {
            if active(data) != Some(id) {
                return Ok(());
            }
            let job = &mut data["jobs"][id];
            if let Some(row) = &observed {
                if let Some(item) = job["items"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|item| item["account_id"] == row["id"])
                {
                    item["observed_priority"] = row["priority"].clone();
                    if identity(row) == item["identity"]
                        && row["priority"] == item["target_priority"]
                    {
                        item["status"] = "verified".into();
                        item["verified_version"] = version(row);
                    }
                }
            }
            job["status"] = "needs_confirmation".into();
            job["reason"] = "升级后停止，请在新版按平台重新核对".into();
            job["updated_at"] = now().into();
            job["completed_at"] = now().into();
            data["active_id"] = Value::Null;
            Ok(())
        })?;
        Ok(true)
    }
    fn waiting(&self, id: &str, ids: &[i64]) -> Result<()> {
        let value = json!(ids);
        if self.store.read_shared()?["jobs"][id]["waiting_account_ids"] == value {
            return Ok(());
        }
        self.store.transaction(|data| {
            if active(data) == Some(id) {
                data["jobs"][id]["waiting_reason"] = if ids.is_empty() {
                    ""
                } else {
                    "等待账号空闲"
                }
                .into();
                data["jobs"][id]["waiting_account_ids"] = value.clone();
                data["jobs"][id]["updated_at"] = now().into();
            }
            Ok(())
        })
    }
    fn matches(job: &Value, rows: &[Value]) -> bool {
        let items = job["items"].as_array().unwrap();
        let by_id: HashMap<i64, &Value> = rows
            .iter()
            .map(|row| (integer(&row["id"]).unwrap(), row))
            .collect();
        items.len() == rows.len()
            && by_id.len() == rows.len()
            && items.iter().all(|item| {
                by_id
                    .get(&integer(&item["account_id"]).unwrap())
                    .is_some_and(|row| {
                        identity(row) == item["identity"]
                            && settings_capabilities(row)["priority"] == item["capability"]
                            && (item["original_priority"] == item["target_priority"]
                                || settings_capabilities(row)["priority"]["editable"] == true)
                            && (item["status"] == "dispatched"
                                || version(row)
                                    == if item["status"] == "verified" {
                                        &item["verified_version"]
                                    } else {
                                        &item["expected_version"]
                                    }
                                    .clone())
                    })
            })
    }
    async fn run(&self, id: &str) -> Result<()> {
        let mut key = None;
        let mut busy_ids = BTreeSet::new();
        loop {
            if self.actions.supervisor.is_stopping() {
                return Ok(());
            }
            let data = self.store.read_shared()?;
            if active(&data) != Some(id) {
                return Ok(());
            }
            let job = &data["jobs"][id];
            let items = job["items"].as_array().unwrap();
            if let Some(item) = items.iter().find(|item| item["status"] == "dispatched") {
                let rows = self.rows(Some(string(&job["platform"]))).await?;
                let Some(row) = rows.iter().find(|row| row["id"] == item["account_id"]) else {
                    return self.finish(id, "needs_confirmation", "已派发账号无法读取，请重新核对");
                };
                let _gate = self.actions.priorities.admission.read().await;
                let _lease = match self.actions.locks.try_account(row) {
                    Ok(v) => v,
                    Err(error) if error.detail["code"] == "account_busy" => {
                        self.waiting(id, &[integer(&item["account_id"]).unwrap()])?;
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                };
                let aid = integer(&row["id"]).unwrap();
                let fresh = self.rows(Some(string(&job["platform"]))).await?;
                let Some(row) = fresh.iter().find(|row| row["id"] == aid) else {
                    return self.finish(id, "needs_confirmation", "已派发账号无法读取，请重新核对");
                };
                if row["priority"] != item["target_priority"] || identity(row) != item["identity"] {
                    self.store.transaction(|data| {
                        let item = data["jobs"][id]["items"]
                            .as_array_mut()
                            .unwrap()
                            .iter_mut()
                            .find(|item| item["account_id"] == aid)
                            .unwrap();
                        item["observed_priority"] = row["priority"].clone();
                        Ok(())
                    })?;
                    credit_ownership::reconcile(
                        &self.actions.queries.oauth,
                        &credit_ownership::account(&self.actions.queries.db, aid).await?,
                    )?;
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
                record_verified(&self.store, id, aid, row)?;
                continue;
            }
            if job["cancel_requested"] == true {
                return self.finish(id, "cancelled", "剩余修改已停止，已完成的优先级保持不变");
            }
            let Some(item) = items.iter().find(|item| {
                item["status"] == "pending"
                    && !busy_ids.contains(&integer(&item["account_id"]).unwrap())
            }) else {
                if items.iter().any(|item| item["status"] == "pending") {
                    self.waiting(id, &busy_ids.iter().copied().collect::<Vec<_>>())?;
                    return Ok(());
                }
                let rows = self.rows(Some(string(&job["platform"]))).await?;
                if !Self::matches(job, &rows) {
                    return self.finish(
                        id,
                        "needs_confirmation",
                        "账号集合、优先级或编辑资格已变化，剩余修改未执行",
                    );
                }
                if items
                    .iter()
                    .all(|item| item["observed_priority"] == item["target_priority"])
                {
                    return self.finish(id, "completed", "该平台全部优先级已回读确认");
                }
                return self.finish(id, "needs_confirmation", "最终优先级核验不一致，请重新核对");
            };
            let aid = integer(&item["account_id"]).unwrap();
            if key.is_none() {
                let token = self.actions.queries.oauth.admin_token()?;
                if token.is_empty() {
                    return self.finish(
                        id,
                        "needs_confirmation",
                        "管理员授权不可用，请重新连接后核对",
                    );
                }
                self.actions.upstream.authenticate(&token, false).await?;
                key = Some(token);
            }
            let result = ORDER
                .scope(
                    id.to_owned(),
                    self.actions.apply_setting(
                        aid,
                        "priority",
                        &json!({"priority":item["target_priority"]}),
                        string(&item["expected_version"]),
                        key.as_deref().unwrap(),
                    ),
                )
                .await;
            match result {
                Ok(_) => {}
                Err(error) if error.detail["code"] == "account_busy" => {
                    busy_ids.insert(aid);
                }
                Err(error) => {
                    let current = self.store.read_shared()?;
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
                        if matches!(error.status.as_u16(), 404 | 409) {
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
        if !self.retire_legacy().await? {
            return Ok(());
        }
        let Some(id) = active(self.store.read_shared()?.as_ref()).map(str::to_owned) else {
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
