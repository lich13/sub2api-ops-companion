//! Opt-in clearing of native temporary scheduling blocks. Never opens scheduling.
use crate::{
    compat::{integer, string},
    db::Param,
    error::{Error, Result},
    model_detection::Detection,
    operations::Operations,
    store::Store,
    tasks::Tasks,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use state::{Control, Intent, Notice, CONCURRENCY, LIMIT};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

pub(crate) mod state {
    use crate::{
        compat::{digest, parse_datetime, string, truthy},
        error::{Error, Result},
    };
    use chrono::{DateTime, Duration, Utc};
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};

    pub(super) const LIMIT: usize = 5;
    pub(super) const CONCURRENCY: usize = 8;

    #[derive(Clone, Default, Deserialize, Serialize)]
    pub struct Control {
        pub enabled: bool,
        pub generation: u64,
        #[serde(default)]
        pub last_restore_at: Option<DateTime<Utc>>,
        #[serde(default)]
        pub restores: Vec<DateTime<Utc>>,
        #[serde(default)]
        pub paused_reason: String,
        #[serde(default)]
        pub failures: u32,
        #[serde(default)]
        pub retry_at: Option<DateTime<Utc>>,
        #[serde(default)]
        pub notifications: Vec<Notice>,
    }

    #[derive(Clone, Deserialize, Serialize)]
    pub(super) struct Intent {
        pub id: String,
        pub generation: u64,
        pub fingerprint: String,
        pub before_until: DateTime<Utc>,
        pub reason: String,
        pub dispatched_at: DateTime<Utc>,
    }

    #[derive(Clone, Deserialize, Serialize)]
    pub struct Notice {
        pub id: String,
        pub generation: u64,
        pub status: String,
        pub attempts: u32,
        pub next_at: Option<DateTime<Utc>>,
    }

    pub(super) fn invalid() -> Error {
        Error::new(503, "账号保活状态无效")
    }

    pub(super) fn validate(value: &Value) -> Result<()> {
        if value["version"] != 1 {
            return Err(invalid());
        }
        let accounts = value["accounts"].as_object().ok_or_else(invalid)?;
        for (id, value) in accounts {
            if !id
                .parse::<i64>()
                .is_ok_and(|number| number > 0 && number.to_string() == *id)
            {
                return Err(invalid());
            }
            let control: Control = Control::deserialize(value).map_err(|_| invalid())?;
            if control.generation > 9_007_199_254_740_991
                || !matches!(control.paused_reason.as_str(), "" | "恢复过于频繁")
                || control.restores.len() > LIMIT
                || control.notifications.iter().any(|n| {
                    n.attempts > 5
                        || n.id.is_empty()
                        || !matches!(
                            n.status.as_str(),
                            "pending" | "delivered" | "failed" | "suppressed"
                        )
                })
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    impl Control {
        pub(super) fn version(&self) -> String {
            digest(&json!({"enabled":self.enabled,"generation":self.generation}))
        }
        pub(super) fn prune(&mut self, at: DateTime<Utc>) {
            self.restores
                .retain(|v| at.signed_duration_since(*v) < Duration::hours(1));
            let mut completed = 0;
            self.notifications.reverse();
            self.notifications.retain(|n| {
                if n.status == "pending" {
                    true
                } else {
                    completed += 1;
                    completed <= 32
                }
            });
            self.notifications.reverse();
        }
        pub(crate) fn used(&self, at: DateTime<Utc>) -> usize {
            self.restores
                .iter()
                .filter(|t| at.signed_duration_since(**t) < Duration::hours(1))
                .count()
        }
        pub(super) fn next_allowed(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
            let mut next = self
                .last_restore_at
                .into_iter()
                .filter_map(|t| t.checked_add_signed(Duration::seconds(60)))
                .max();
            if self.used(at) >= LIMIT {
                let oldest = self
                    .restores
                    .iter()
                    .copied()
                    .filter(|t| at.signed_duration_since(*t) < Duration::hours(1))
                    .min();
                next = next
                    .into_iter()
                    .chain(oldest.and_then(|t| t.checked_add_signed(Duration::hours(1))))
                    .max();
            }
            next.into_iter()
                .chain(self.retry_at)
                .max()
                .filter(|t| *t > at)
        }
        pub(super) fn waiting(&self, at: DateTime<Utc>) -> bool {
            self.used(at) >= LIMIT
                || self.retry_at.is_some_and(|t| t > at)
                || self
                    .last_restore_at
                    .into_iter()
                    .any(|t| at.signed_duration_since(t) < Duration::seconds(60))
        }
    }

    pub(super) fn fingerprint(row: &Value) -> String {
        digest(&json!([
            row["temp_unschedulable_until"],
            row["temp_unschedulable_reason"]
        ]))
    }

    /// Only recognized native error-policy records are eligible. A threshold source
    /// is excluded before inspecting its optional details, even if those are broken.
    pub(crate) fn eligible_reason(row: &Value) -> std::result::Result<&'static str, &'static str> {
        let raw = string(&row["temp_unschedulable_reason"]);
        if raw.len() > 16_384 {
            return Err("暂停原因无法识别");
        }
        let value: Value = serde_json::from_str(raw).map_err(|_| "暂停原因无法识别")?;
        if value["source"] == "account_scheduling_threshold" {
            return Err("额度阈值暂停不参与保活");
        }
        if !value.is_object() || truthy(&value["source"]) {
            return Err("暂停原因无法识别");
        }
        let code = value["status_code"].as_i64().ok_or("暂停原因无法识别")?;
        let rule = value["rule_index"].as_i64().ok_or("暂停原因无法识别")?;
        let keyword = value["matched_keyword"]
            .as_str()
            .ok_or("暂停原因无法识别")?;
        let message = value["error_message"].as_str().ok_or("暂停原因无法识别")?;
        let until = value["until_unix"].as_i64().ok_or("暂停原因无法识别")?;
        let triggered = value["triggered_at_unix"]
            .as_i64()
            .ok_or("暂停原因无法识别")?;
        if triggered <= 0
            || until < triggered
            || parse_datetime(&row["temp_unschedulable_until"])
                .is_none_or(|t| t.timestamp() != until)
            || !(rule >= 0 && (400..=599).contains(&code)
                || rule == -1 && code == 0 && keyword == "stream_timeout")
        {
            return Err("暂停原因无法识别");
        }
        let text = format!("{keyword} {message}").to_lowercase();
        if matches!(code, 401..=403)
            || [
                "banned",
                "deactivated",
                "suspended",
                "organization has been disabled",
                "account disabled",
                "invalid_api_key",
                "unauthorized",
                "authentication",
                "credential",
                "token_revoked",
                "token_invalidated",
                "token expired",
                "invalid_grant",
                "needs_reauth",
                "identity verification",
                "usage_limit_reached",
                "usage limit has been reached",
                "insufficient_quota",
                "quota_exceeded",
                "quota exhausted",
                "quota exceeded",
                "credit balance",
                "credits exhausted",
                "weekly limit reached",
                "monthly limit reached",
                "额度耗尽",
                "额度已用尽",
                "余额不足",
                "封禁",
                "凭证失效",
            ]
            .iter()
            .any(|term| text.contains(term))
        {
            return Err("认证、封禁或额度异常不参与保活");
        }
        Ok(if code == 0 {
            "流响应超时"
        } else {
            "临时错误策略"
        })
    }

    pub(crate) fn row_reason(row: &Value, at: DateTime<Utc>) -> &'static str {
        if !row.is_object() || truthy(&row["deleted_at"]) {
            return "账号已删除";
        }
        if row["status"] != "active" {
            return "账号停用或认证异常";
        }
        if row["schedulable"] != true {
            return "调度已关闭";
        }
        if !row["rate_limited_at"].is_null() || !row["overload_until"].is_null() {
            return "账号限流或过载不参与保活";
        }
        if parse_datetime(&row["temp_unschedulable_until"]).is_none_or(|t| t <= at) {
            return "等待临时不可调度";
        }
        eligible_reason(row).err().unwrap_or("")
    }
}

const SELECT: &str = "SELECT id,status,schedulable,temp_unschedulable_until,temp_unschedulable_reason,rate_limited_at,overload_until,error_message,updated_at,nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id,jsonb_build_object('parent_account_id',extra->'parent_account_id') AS extra FROM accounts WHERE deleted_at IS NULL";

#[derive(Clone)]
pub struct Keepalive {
    pub store: Store,
    detection: Detection,
    operations: Arc<Operations>,
    budget: Arc<Semaphore>,
    tasks: Tasks,
    running: Arc<Mutex<BTreeSet<i64>>>,
    cursor: Arc<Mutex<i64>>,
    tick_lock: Arc<tokio::sync::Mutex<()>>,
    notice_lock: Arc<tokio::sync::Mutex<()>>,
}

struct Running {
    id: i64,
    accounts: Arc<Mutex<BTreeSet<i64>>>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.accounts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

struct Gates {
    marks: Arc<Value>,
    detection: Arc<Value>,
    oauth: Arc<Value>,
    models: BTreeSet<i64>,
    operations: BTreeSet<i64>,
}

enum Outcome {
    Confirmed,
    Failed,
    Superseded,
}

impl Keepalive {
    pub fn new(detection: Detection, operations: Arc<Operations>, budget: Arc<Semaphore>) -> Self {
        let actions = &detection.actions;
        Self {
            store: Store::new(
                &actions.settings.keepalive_path,
                json!({"version":1,"accounts":{}}),
                state::validate,
            ),
            tasks: Tasks::supervised(actions.supervisor.clone()),
            detection,
            operations,
            budget,
            running: Default::default(),
            cursor: Default::default(),
            tick_lock: Default::default(),
            notice_lock: Default::default(),
        }
    }
    fn now(&self) -> DateTime<Utc> {
        self.detection.now()
    }
    pub fn control(&self, id: i64) -> Result<Control> {
        match self.store.read_shared()?["accounts"].get(id.to_string()) {
            Some(value) => Control::deserialize(value).map_err(|_| state::invalid()),
            None => Ok(Control::default()),
        }
    }
    fn change<T>(&self, id: i64, change: impl FnOnce(&mut Control) -> Result<T>) -> Result<T> {
        self.store.transaction(|data| {
            let mut control = match data["accounts"].get(id.to_string()) {
                Some(value) => Control::deserialize(value).map_err(|_| state::invalid())?,
                None => Control::default(),
            };
            let result = change(&mut control)?;
            data["accounts"][id.to_string()] =
                serde_json::to_value(control).map_err(|_| state::invalid())?;
            Ok(result)
        })
    }
    async fn row(&self, id: i64) -> Result<Option<Value>> {
        self.detection
            .actions
            .queries
            .db
            .one(&format!("{SELECT} AND id=$1"), &[id.into()])
            .await
    }
    fn inflight(&self, id: i64) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&id)
    }
    fn audit(&self, event: &str, id: i64, intent: &Intent, result: &str) {
        self.detection.actions.audit(
            event,
            json!({"account_id":id,"before_until":intent.before_until,"reason":intent.reason,
                "result":result,"generation":intent.generation,"request_id":intent.id}),
        );
    }
    fn gates(&self) -> Result<Gates> {
        let active = |data: &Value, statuses: &[&str]| {
            data["jobs"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, job)| statuses.contains(&string(&job["status"])))
                .flat_map(|(_, job)| {
                    [
                        integer(&job["account_id"]),
                        integer(&job["parent_account_id"]),
                    ]
                })
                .flatten()
                .filter(|id| *id > 0)
                .collect()
        };
        Ok(Gates {
            marks: self.detection.capacity.store.shared_snapshot()?,
            detection: self.detection.store.read_shared()?,
            oauth: self.detection.monitor.shared_state()?,
            models: active(
                self.detection.tests.shared_data()?.as_ref(),
                crate::model_tests::ACTIVE,
            ),
            operations: active(
                self.operations.store.read_shared()?.as_ref(),
                crate::operations::PENDING,
            ),
        })
    }
    fn blocked_in(&self, id: i64, row: &Value, gates: &Gates) -> &'static str {
        let reason = state::row_reason(row, self.now());
        if !reason.is_empty() {
            return reason;
        }
        if gates.marks["marks"][id.to_string()]["marked"] == true {
            return "已判定降智，保活暂停";
        }
        if gates.detection["accounts"][id.to_string()]["hold"] == true {
            return "检测处置未释放，保活暂停";
        }
        let parent = integer(&row["parent_account_id"])
            .or_else(|| integer(&row["extra"]["parent_account_id"]));
        for target in [Some(id), parent].into_iter().flatten() {
            if gates.models.contains(&target) {
                return "模型测试或复查正在排队或执行";
            }
            if gates.operations.contains(&target) {
                return "等待人工操作完成或核对";
            }
            if self.detection.monitor.processing(target) {
                return "等待恢复任务完成";
            }
            let meta = &gates.oauth["scheduler"][target.to_string()];
            let recovery = &meta["recovery_intent"];
            if recovery.is_object()
                && !matches!(
                    string(&recovery["status"]),
                    "recovered" | "blocked" | "auth_failed" | "cancelled"
                )
            {
                return "等待恢复任务完成";
            }
            let credit = &meta["auto_reset_credit"];
            if credit.is_object()
                && !crate::credit::rules::TERMINAL.contains(&string(&credit["stage"]))
            {
                return "等待用卡任务完成或核对";
            }
        }
        ""
    }
    fn blocked(&self, id: i64, row: &Value) -> Result<&'static str> {
        let reason = state::row_reason(row, self.now());
        if !reason.is_empty() {
            return Ok(reason);
        }
        Ok(self.blocked_in(id, row, &self.gates()?))
    }
    pub async fn view(&self, id: i64) -> Result<Value> {
        let row = self
            .row(id)
            .await?
            .ok_or_else(|| Error::new(404, "账号不存在"))?;
        let mut control = self.control(id)?;
        control.prune(self.now());
        let (status, reason) = if self.inflight(id) {
            ("running", "正在恢复临时不可调度")
        } else if !control.enabled {
            ("disabled", control.paused_reason.as_str())
        } else if control.waiting(self.now()) {
            ("waiting", "等待恢复间隔或可用额度")
        } else {
            let reason = self.blocked(id, &row)?;
            (
                if reason.is_empty() || reason == "等待临时不可调度" {
                    "waiting"
                } else {
                    "paused"
                },
                reason,
            )
        };
        Ok(
            json!({"account_id":id,"enabled":control.enabled,"generation":control.generation,
            "version":control.version(),"status":status,"reason":reason,"paused_reason":control.paused_reason,
            "last_restore_at":control.last_restore_at,"restore_count":control.restores.len(),
            "budget_used":control.used(self.now()),"next_allowed_at":control.next_allowed(self.now())}),
        )
    }

    pub(crate) async fn save_validated(&self, id: i64, payload: &Value) -> Result<Value> {
        if self.tasks.is_closed() {
            return Err(Error::new(503, "服务正在停止"));
        }
        let _guard = self.detection.actions.locks.control(id).await;
        self.row(id)
            .await?
            .ok_or_else(|| Error::new(404, "账号不存在"))?;
        self.change(id, |control| {
            if control.version() != string(&payload["expected_version"]) {
                return Err(Error::new(409, "保活设置已变化，请重新读取"));
            }
            control.generation = control
                .generation
                .checked_add(1)
                .ok_or_else(state::invalid)?;
            control.enabled = payload["enabled"] == true;
            control.paused_reason.clear();
            control.prune(self.now());
            Ok(())
        })?;
        self.view(id).await
    }
    pub async fn tick(&self) -> Result<()> {
        if self.tasks.is_closed() {
            return Ok(());
        }
        let Ok(_tick) = self.tick_lock.try_lock() else {
            return Ok(());
        };
        let data = self.store.read_shared()?;
        let mut controls = BTreeMap::new();
        for (id, value) in data["accounts"].as_object().into_iter().flatten() {
            if value["enabled"] != true {
                continue;
            }
            let control: Control = Control::deserialize(value).map_err(|_| state::invalid())?;
            if !control.waiting(self.now()) {
                controls.insert(id.parse::<i64>().map_err(|_| state::invalid())?, control);
            }
        }
        if controls.is_empty() {
            return Ok(());
        }
        let rows = self
            .detection
            .actions
            .queries
            .db
            .all(
                &format!("{SELECT} AND id=ANY($1) ORDER BY id"),
                &[Param::Ints(controls.keys().copied().collect())],
            )
            .await?;
        let gates = self.gates()?;
        let cursor = *self.cursor.lock().map_err(|_| Error::internal())?;
        let mut rows = rows;
        rows.sort_by_key(|r| {
            let id = integer(&r["id"]).unwrap_or(0);
            (id <= cursor, id)
        });
        let mut dispatched = 0;
        for row in rows {
            let id = integer(&row["id"]).ok_or_else(state::invalid)?;
            if self.inflight(id) {
                continue;
            }
            let Some(control) = controls.get(&id) else {
                continue;
            };
            if !control.enabled
                || control.waiting(self.now())
                || !self.blocked_in(id, &row, &gates).is_empty()
            {
                continue;
            }
            let Ok(permit) = self.budget.clone().try_acquire_owned() else {
                break;
            };
            let mut running = self.running.lock().map_err(|_| Error::internal())?;
            if running.len() >= CONCURRENCY || dispatched >= CONCURRENCY {
                break;
            }
            running.insert(id);
            let guard = Running {
                id,
                accounts: self.running.clone(),
            };
            drop(running);
            *self.cursor.lock().map_err(|_| Error::internal())? = id;
            let owner = self.clone();
            let generation = control.generation;
            self.tasks.spawn(async move {
                let (_guard, _permit) = (guard, permit);
                if owner.run(id, generation, &row).await.is_err() {
                    owner.detection.actions.audit(
                        "keepalive_task_error",
                        json!({"account_id":id,"error_code":"cycle_failed"}),
                    );
                }
            })?;
            dispatched += 1;
        }
        Ok(())
    }
    async fn run(&self, id: i64, generation: u64, original: &Value) -> Result<()> {
        let actions = &self.detection.actions;
        let Ok(_account) = actions.locks.try_account(original) else {
            return Ok(());
        };
        let Ok(_control) = actions.locks.try_control(id) else {
            return Ok(());
        };
        let Some(row) = self.row(id).await? else {
            return Ok(());
        };
        if state::fingerprint(&row) != state::fingerprint(original)
            || row["parent_account_id"] != original["parent_account_id"]
            || row["extra"] != original["extra"]
            || !self.blocked(id, &row)?.is_empty()
            || self.tasks.is_closed()
        {
            return Ok(());
        }
        let key = actions.queries.oauth.admin_token()?;
        if key.trim().is_empty() || actions.upstream.active_url().is_empty() {
            return Ok(());
        }
        let at = self.now();
        let intent = Intent {
            id: uuid::Uuid::new_v4().to_string(),
            generation,
            fingerprint: state::fingerprint(&row),
            before_until: crate::compat::parse_datetime(&row["temp_unschedulable_until"])
                .ok_or_else(state::invalid)?,
            reason: state::eligible_reason(&row)
                .map_err(|_| state::invalid())?
                .into(),
            dispatched_at: at,
        };
        let control = self.control(id)?;
        if !control.enabled || control.generation != generation || control.waiting(at) {
            return Ok(());
        }
        if self.tasks.is_closed() {
            self.finish(id, &intent, Outcome::Failed)?;
            return Ok(());
        }
        self.audit("keepalive_dispatch", id, &intent, "dispatched");
        let _response = crate::oauth_native::admin(
            actions,
            id,
            "temp-unschedulable",
            &key,
            reqwest::Method::DELETE,
            10,
        )
        .await;
        let row = match self.row(id).await {
            Ok(row) => row,
            Err(error) => {
                self.finish(id, &intent, Outcome::Failed)?;
                return Err(error);
            }
        };
        let outcome = match row.as_ref() {
            Some(row) if row["temp_unschedulable_until"].is_null() => Outcome::Confirmed,
            Some(row) if state::fingerprint(row) == intent.fingerprint => Outcome::Failed,
            _ => Outcome::Superseded,
        };
        self.finish(id, &intent, outcome)
    }
    fn finish(&self, id: i64, intent: &Intent, outcome: Outcome) -> Result<()> {
        let at = self.now();
        let result = match outcome {
            Outcome::Confirmed => "confirmed",
            Outcome::Failed => "failed",
            Outcome::Superseded => "incident_changed",
        };
        let changed = self.change(id, |control| {
            if control.generation != intent.generation {
                return Ok(false);
            }
            control.prune(at);
            match outcome {
                Outcome::Confirmed => {
                    control.last_restore_at = Some(at);
                    control.restores.push(at);
                    control.failures = 0;
                    control.retry_at = None;
                    if control.restores.len() >= LIMIT && control.generation == intent.generation {
                        control.enabled = false;
                        control.paused_reason = "恢复过于频繁".into();
                        control.generation = control
                            .generation
                            .checked_add(1)
                            .ok_or_else(state::invalid)?;
                        control.notifications.push(Notice {
                            id: intent.id.clone(),
                            generation: control.generation,
                            status: "pending".into(),
                            attempts: 0,
                            next_at: Some(at),
                        });
                    }
                }
                Outcome::Failed => {
                    let delay = crate::operations::retry_delay(control.failures.into());
                    control.failures = control.failures.saturating_add(1);
                    control.retry_at = Some(at + Duration::seconds(delay));
                }
                _ => {}
            }
            Ok(true)
        })?;
        if changed {
            self.audit("keepalive_result", id, intent, result);
        }
        Ok(())
    }
    pub async fn deliver(&self) -> Result<()> {
        if self.tasks.is_closed() {
            return Ok(());
        }
        let Ok(_guard) = self.notice_lock.try_lock() else {
            return Ok(());
        };
        let config = self.detection.bark.runtime();
        if !config.valid {
            return Ok(());
        }
        let data = self.store.read_shared()?;
        for (id, value) in data["accounts"].as_object().into_iter().flatten() {
            if !value["notifications"]
                .as_array()
                .is_some_and(|list| list.iter().any(|n| n["status"] == "pending"))
            {
                continue;
            }
            let control: Control = Control::deserialize(value).map_err(|_| state::invalid())?;
            let id: i64 = id.parse().map_err(|_| state::invalid())?;
            for notice in control.notifications {
                if notice.status != "pending"
                    || (config.enabled && notice.next_at.is_some_and(|t| t > self.now()))
                {
                    continue;
                }
                let reserved = self.change(id, |control| {
                    let stale = control.generation != notice.generation || control.enabled;
                    let Some(current) = control
                        .notifications
                        .iter_mut()
                        .find(|n| n.id == notice.id && n.status == "pending")
                    else {
                        return Ok(false);
                    };
                    if stale || !config.enabled {
                        current.status = "suppressed".into();
                        current.next_at = None;
                        return Ok(false);
                    }
                    if current.attempts >= 5 {
                        current.status = "failed".into();
                        current.next_at = None;
                        return Ok(false);
                    }
                    current.attempts += 1;
                    current.next_at = Some(
                        self.now()
                            + Duration::seconds(crate::operations::retry_delay(
                                current.attempts.saturating_sub(1).into(),
                            )),
                    );
                    Ok(true)
                })?;
                if !reserved {
                    continue;
                }
                let result = self.detection.bark.push_with_config("账号保活已暂停",
                    &format!("账号 #{id}\n一小时内已恢复 5 次，保活已暂停。请检查账号后手动重新开启。"),
                    10, &json!({"group":"Sub2Ops"}), &config).await;
                if !result.success && notice.attempts + 1 < 5 {
                    continue;
                }
                self.change(id, |control| {
                    if let Some(current) = control
                        .notifications
                        .iter_mut()
                        .find(|n| n.id == notice.id && n.status == "pending")
                    {
                        current.status = if result.success {
                            "delivered"
                        } else if current.attempts >= 5 {
                            "failed"
                        } else {
                            "pending"
                        }
                        .into();
                        current.next_at = if current.status == "pending" {
                            Some(
                                self.now()
                                    + Duration::seconds(crate::operations::retry_delay(
                                        current.attempts.saturating_sub(1).into(),
                                    )),
                            )
                        } else {
                            None
                        };
                    }
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
    pub async fn close(&self) {
        self.tasks.close(55).await;
    }
}

pub fn request(value: &Value) -> Result<()> {
    crate::validation::fields(
        value,
        &["enabled", "expected_version"],
        &["enabled", "expected_version"],
    )?;
    crate::validation::boolean(&value["enabled"])?;
    crate::validation::text(&value["expected_version"], 64, 64)?;
    Ok(())
}
