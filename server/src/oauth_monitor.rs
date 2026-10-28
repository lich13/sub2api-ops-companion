use crate::{
    actions::Actions,
    audit,
    compat::{datetime, integer, parse_datetime, string, truthy},
    config_service::ConfigService,
    credit::{CreditController, Hooks},
    credit_ownership,
    daily_test::Schedule,
    error::{Error, Result},
    execution::TestSlots,
    oauth_admission,
    oauth_queries::{Coordinator, QueryOptions, Source},
    oauth_state::{OAuthState, Update},
    quota, recovery_events, recovery_policy, recovery_rules,
};
use chrono::{DateTime, Duration, Utc};
use futures_util::{future::BoxFuture, stream, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::sync::{watch, Mutex, Semaphore};

type Inventory = (Option<Instant>, Arc<Vec<Value>>);

pub trait RecoveryHooks: Hooks {
    fn model_notification(&self, _job_id: &str, _suppressed: bool) -> Result<()> {
        Ok(())
    }
    fn model_recovery(
        &self,
        id: i64,
        fingerprint: String,
        attempt: i64,
    ) -> BoxFuture<'_, Result<Value>>;
}
#[derive(Clone)]
pub struct Monitor {
    pub actions: Actions,
    pub store: OAuthState,
    pub config: ConfigService,
    pub queries: Coordinator,
    pub hooks: Arc<dyn RecoveryHooks>,
    pub credits: CreditController,
    pub slots: TestSlots,
    pub budget: Arc<Semaphore>,
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    pub schedule: Schedule,
    pub(crate) cycle: Arc<Mutex<()>>,
    inventory: Arc<Mutex<Inventory>>,
    workers: Arc<Mutex<BTreeMap<i64, tokio::task::JoinHandle<Result<Value>>>>>,
    daily: Arc<Mutex<Option<tokio::task::JoinHandle<Result<()>>>>>,
    force: Arc<Mutex<Option<watch::Receiver<Option<Value>>>>>,
    force_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    closed: Arc<AtomicBool>,
}
impl Monitor {
    pub fn new(
        actions: Actions,
        config: ConfigService,
        queries: Coordinator,
        hooks: Arc<dyn RecoveryHooks>,
        credits: CreditController,
        slots: TestSlots,
        budget: Arc<Semaphore>,
    ) -> Self {
        let store = actions.queries.oauth.clone();
        Self {
            schedule: Schedule::new(store.clone()),
            store,
            actions,
            config,
            queries,
            hooks,
            credits,
            slots,
            budget,
            clock: Arc::new(Utc::now),
            cycle: Arc::new(Mutex::new(())),
            inventory: Arc::new(Mutex::new((None, Arc::new(vec![])))),
            workers: Arc::new(Mutex::new(BTreeMap::new())),
            daily: Arc::new(Mutex::new(None)),
            force: Arc::new(Mutex::new(None)),
            force_task: Arc::new(Mutex::new(None)),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.actions.supervisor.is_stopping()
    }
    pub(crate) fn processing(&self, id: i64) -> bool {
        self.workers
            .try_lock()
            .map(|workers| workers.contains_key(&id))
            .unwrap_or(true)
    }
    pub fn state(&self) -> Result<Value> {
        self.store.snapshot()
    }
    pub fn shared_state(&self) -> Result<Arc<Value>> {
        self.store.shared_snapshot()
    }
    pub fn generation(&self, id: i64) -> Result<i64> {
        Ok(credit_ownership::generation(
            self.shared_state()?.as_ref(),
            id,
        ))
    }
    pub fn model(&self) -> Result<String> {
        Ok(string(&self.config.current()?["oauth_recovery_test_model_id"]).to_owned())
    }
    pub fn audit(&self, event: &str, value: Value) {
        audit::write(
            &self.actions.settings.audit_path,
            event,
            value,
            self.actions.settings.read_only,
        );
    }
    pub async fn row(&self, id: i64) -> Result<Value> {
        credit_ownership::account(&self.actions.queries.db, id).await
    }
    pub fn lease(&self, row: &Value) -> Result<crate::account_locks::Lease> {
        let parent = integer(&row["parent_account_id"])
            .or_else(|| integer(&row["extra"]["parent_account_id"]));
        self.actions
            .locks
            .try_ids([integer(&row["id"]).unwrap_or(0), parent.unwrap_or(0)])
    }
    pub async fn inventory(&self, refresh: bool) -> Result<Arc<Vec<Value>>> {
        let mut cache = self.inventory.lock().await;
        if refresh || cache.0.is_none_or(|at| at.elapsed().as_secs() >= 60) {
            let rows=self.actions.queries.db.all(&format!("{} AND lower(coalesce(platform,''))='openai' AND lower(coalesce(type,''))='oauth' ORDER BY id",credit_ownership::ACCOUNT_SELECT),&[]).await?;
            if !self.actions.settings.read_only {
                self.config.reconcile(&rows).await?;
                self.initialize_queries(&rows)?;
            }
            self.queries.retain_accounts(&rows);
            *cache = (Some(Instant::now()), Arc::new(rows));
        }
        Ok(cache.1.clone())
    }
    fn initialize_queries(&self, rows: &[Value]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let shared = self.store.shared_snapshot()?;
        if rows.iter().all(|row| {
            let id = integer(&row["id"]).unwrap_or(0).to_string();
            shared["scheduler"][&id]["quota_query"].is_object()
        }) {
            return Ok(());
        }
        self.store.transaction(|state| {
            for row in rows {
                let id = integer(&row["id"]).unwrap_or(0).to_string();
                let saved = state["oauth_results"][&id].clone();
                let meta = &mut state["scheduler"][&id];
                if !meta.is_object() {
                    *meta = json!({});
                }
                if meta["quota_query"].is_object() {
                    continue;
                }
                let mut query = crate::oauth_state::query_metadata(meta, &saved)?;
                let last = [
                    &meta["last_attempt_at"],
                    &meta["last_success_at"],
                    &saved["queried_at"],
                ]
                .into_iter()
                .filter_map(parse_datetime)
                .max();
                if let Some(last) = last {
                    query["last_query_at"] = datetime(last).into();
                    query["automatic_attempts"] = json!([datetime(last)]);
                }
                if !recovery_events::auth_code(string(&meta["last_error_code"])).is_empty() {
                    query["auth_fingerprint"] = oauth_admission::credential_fingerprint(row).into();
                }
                meta["quota_query"] = query;
            }
            Ok(())
        })
    }
    pub async fn live_rows<'a>(
        &self,
        rows: &'a [Value],
    ) -> Result<Vec<std::borrow::Cow<'a, Value>>> {
        if rows.is_empty() {
            return Ok(vec![]);
        }
        let state = self.shared_state()?;
        let ids = rows
            .iter()
            .filter_map(|v| integer(&v["id"]))
            .filter(|id| truthy(&state["scheduler"][id.to_string()]["auto_reset_credit"]))
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(rows.iter().map(std::borrow::Cow::Borrowed).collect());
        }
        // Credit ownership can change between inventory refreshes. Refresh
        // those accounts while keeping ordinary idle ticks on the cache.
        let projection=self.actions.queries.db.all("SELECT id,updated_at,status,schedulable,rate_limited_at,rate_limit_reset_at,temp_unschedulable_until,temp_unschedulable_reason,overload_until,expires_at,auto_pause_on_expired,extra AS recovery_extra FROM accounts WHERE deleted_at IS NULL AND platform='openai' AND type='oauth' AND id=ANY($1) ORDER BY id",&[crate::db::Param::Ints(ids)]).await?;
        let mut by_id = projection
            .into_iter()
            .filter_map(|row| integer(&row["id"]).map(|id| (id, row)))
            .collect::<BTreeMap<_, _>>();
        let mut live = vec![];
        for original in rows {
            let id = integer(&original["id"]).unwrap_or(0);
            if !truthy(&state["scheduler"][id.to_string()]["auto_reset_credit"]) {
                live.push(std::borrow::Cow::Borrowed(original));
                continue;
            }
            if let Some(mut current) = by_id.remove(&id) {
                let mut row = original.clone();
                let extra = current
                    .as_object_mut()
                    .unwrap()
                    .shift_remove("recovery_extra")
                    .unwrap_or(json!({}));
                row.as_object_mut()
                    .unwrap()
                    .extend(current.as_object().unwrap().clone());
                row["extra"] = extra;
                live.push(std::borrow::Cow::Owned(row));
            }
        }
        Ok(live)
    }
    pub async fn startup(&self) -> Result<()> {
        self.shared_state()?;
        if self.actions.settings.read_only {
            return Ok(());
        }
        let rows = self.inventory(true).await?;
        self.schedule.startup(&self.config.current()?, self.now())?;
        for row in rows.iter() {
            self.reconcile_unknown(integer(&row["id"]).unwrap_or(0))
                .await?;
        }
        Ok(())
    }
    pub fn waiting(
        &self,
        id: i64,
        reason: &str,
        until: Option<DateTime<Utc>>,
        generation: i64,
    ) -> Result<()> {
        self.store.transaction(|state| {
            if credit_ownership::generation(state, id) != generation {
                return Ok(());
            }
            let intent = &mut state["scheduler"][id.to_string()]["recovery_intent"];
            if intent.is_object()
                && !matches!(
                    string(&intent["status"]),
                    "recovered" | "blocked" | "auth_failed" | "cancelled" | "retry_exhausted"
                )
            {
                intent["waiting_reason"] = reason.into();
                intent["waiting_until"] = until.map(datetime).unwrap_or_default().into();
            }
            Ok(())
        })
    }
    pub async fn tick(&self) -> Result<()> {
        if self.actions.settings.read_only || self.is_closed() {
            return Ok(());
        }
        let config = self.config.current()?;
        let now = self.now();
        // Record the due batch even while a manual monitor cycle owns the
        // execution lock. It may be dispatched after the due minute ends.
        self.schedule.tick(&config, now)?;
        let Ok(_cycle) = self.cycle.try_lock() else {
            return Ok(());
        };
        let completed = {
            let mut workers = self.workers.lock().await;
            let ids = workers
                .iter()
                .filter(|(_, handle)| handle.is_finished())
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| workers.remove(&id).map(|handle| (id, handle)))
                .collect::<Vec<_>>()
        };
        for (id, handle) in completed {
            if !matches!(handle.await, Ok(Ok(_))) {
                self.reconcile_unknown(id).await?;
            }
        }
        {
            let mut daily = self.daily.lock().await;
            if daily.as_ref().is_some_and(|handle| handle.is_finished()) {
                let handle = daily.take().unwrap();
                if !matches!(handle.await, Ok(Ok(()))) {
                    self.audit(
                        "oauth_daily_test_error",
                        json!({"error_code":"daily_cycle_failed"}),
                    );
                }
            }
        }
        let inventory = self.inventory(false).await?;
        let all_live = self.live_rows(&inventory).await?;
        self.credits.sweep(&all_live, now).await?;
        let methods = recovery_policy::MethodIndex::from_config(&config);
        let rows = all_live
            .into_iter()
            .filter(|row| methods.method(integer(&row["id"]).unwrap_or(0)).is_some())
            .collect::<Vec<_>>();
        let enabled = config["oauth_recovery_monitor_enabled"] != false;
        let state = self.shared_state()?;
        self.queries
            .observe_gates(&rows, &state["oauth_results"], &state["scheduler"], now)?;
        if enabled {
            recovery_events::discover(&self.store, &rows, &config, now)?;
        }
        let state = self.shared_state()?;
        let mut candidates = recovery_rules::candidates(
            &rows,
            &state["oauth_results"],
            &state["scheduler"],
            now,
            enabled,
        )?;
        candidates.retain(|item| {
            let credit = &state["scheduler"][integer(&item["account_id"]).unwrap_or(0).to_string()]
                ["auto_reset_credit"];
            !((truthy(&credit["owns_pause"])
                || truthy(&credit["attempt_at"]) && credit["stage"] != "finished")
                && !matches!(string(&credit["stage"]), "recovered" | "closed"))
        });
        let limit = (integer(&config["oauth_usage_refresh_concurrency"]).unwrap_or(4)
            + integer(&config["oauth_recovery_test_concurrency"]).unwrap_or(2))
        .clamp(1, 24) as usize;
        let mut workers = self.workers.lock().await;
        let mut cycle_results = BTreeMap::new();
        if self.is_closed() {
            return Ok(());
        }
        let mut waiting = Update::default();
        let mut generations = BTreeMap::new();
        let mut methods = BTreeMap::new();
        for row in &rows {
            let id = integer(&row["id"]).unwrap_or(0);
            if workers.contains_key(&id) {
                continue;
            }
            let meta = &state["scheduler"][id.to_string()];
            let intent = &meta["recovery_intent"];
            if !intent.is_object()
                || matches!(
                    string(&intent["status"]),
                    "cancelled" | "recovered" | "blocked" | "auth_failed" | "retry_exhausted"
                )
            {
                continue;
            }
            let saved = quota::latest_result(row, &state["oauth_results"][id.to_string()], now);
            let (mut reason, mut until) =
                oauth_admission::automatic_gate(row, &saved, meta, now, false, false, 1)?;
            if matches!(reason, "query_cooldown" | "query_budget" | "query_backoff")
                && recovery_rules::quota_ready(row, &saved, meta, now)
            {
                reason = "";
                until = None;
            }
            let retry_reason = match string(&intent["waiting_reason"]) {
                value @ ("model_cooldown"
                | "model_busy"
                | "test_slots"
                | "model_verification_pending") => value,
                _ => "verification_backoff",
            };
            let boundary = recovery_rules::reset_not_before(row, &saved, meta, now);
            let retry = parse_datetime(&intent["next_retry_at"]);
            if let Some((at, bound)) = [
                (until, reason),
                (boundary, "waiting_reset"),
                (retry, retry_reason),
            ]
            .into_iter()
            .filter_map(|(at, why)| at.filter(|at| *at > now).map(|at| (at, why)))
            .max()
            {
                reason = bound;
                until = Some(at);
            } else if matches!(
                string(&intent["waiting_reason"]),
                "account_busy" | "query_slots" | "test_slots" | "incomplete_quota"
            ) {
                reason = string(&intent["waiting_reason"]);
                until = None;
            } else if workers.len() >= limit
                && candidates
                    .iter()
                    .any(|item| integer(&item["account_id"]) == Some(id))
            {
                reason = "worker_slots";
                until = Some(now + Duration::seconds(2));
            } else if reason.is_empty() {
                until = None;
            }
            let mut next = intent.clone();
            next["waiting_reason"] = reason.into();
            next["waiting_until"] = json!(until.map(datetime));
            if &next != intent {
                waiting
                    .scheduler
                    .insert(id, json!({"recovery_intent":next}));
                generations.insert(
                    id,
                    integer(&meta["manual_control"]["generation"]).unwrap_or(0),
                );
                methods.insert(
                    id,
                    integer(&meta["recovery_method_generation"]).unwrap_or(0),
                );
            }
        }
        if !waiting.scheduler.is_empty() {
            waiting.generations = Some(generations);
            waiting.methods = Some(methods);
            self.store.commit(waiting)?;
        }
        for item in candidates {
            if self.is_closed() {
                break;
            }
            let id = integer(&item["account_id"]).unwrap_or(0);
            if workers.contains_key(&id) {
                continue;
            }
            if workers.len() >= limit {
                self.waiting(
                    id,
                    "worker_slots",
                    Some(now + Duration::seconds(2)),
                    self.generation(id)?,
                )?;
                continue;
            }
            let this = self.clone();
            let (send, receive) = watch::channel(None);
            cycle_results.insert(id, receive);
            workers.insert(
                id,
                self.actions
                    .supervisor
                    .spawn_result("oauth_worker", async move {
                        let result = this.run_item(item, Source::Automatic, true).await;
                        let receipt = result.as_ref().cloned().unwrap_or_else(|_| {
                        json!({"success":false,"skipped":true,"error_code":"recovery_uncertain"})
                    });
                        let _ = send.send(Some(receipt));
                        result
                    }),
            );
        }
        drop(workers);
        let mut credit_rows = rows;
        credit_rows.sort_by_key(|row| {
            let id = integer(&row["id"]).unwrap_or(0);
            (
                string(&state["scheduler"][id.to_string()]["auto_reset_credit"]["next_at"])
                    .to_owned(),
                id,
            )
        });
        for row in credit_rows {
            let id = integer(&row["id"]).unwrap_or(0);
            {
                let workers = self.workers.lock().await;
                if self.is_closed() {
                    break;
                }
                if workers.contains_key(&id) || workers.len() >= limit {
                    continue;
                }
            }
            if self.credits.candidate_ready(&row, now).await? {
                let mut workers = self.workers.lock().await;
                if self.is_closed() {
                    break;
                }
                if workers.contains_key(&id) || workers.len() >= limit {
                    continue;
                }
                let this = self.clone();
                let row = row.into_owned();
                workers.insert(
                    id,
                    self.actions
                        .supervisor
                        .spawn_result("credit_worker", async move {
                            this.credits.run_account(&row, this.now()).await?;
                            Ok(json!({}))
                        }),
                );
            }
        }
        {
            let mut daily = self.daily.lock().await;
            if !self.is_closed() && daily.is_none() {
                if let Some(batch) = self.schedule.claim(&config, now)? {
                    let this = self.clone();
                    *daily = Some(self.actions.supervisor.spawn_result(
                        "daily_worker",
                        async move {
                            this.run_daily_with_recovery(batch, inventory, cycle_results)
                                .await
                        },
                    ));
                }
            }
        }
        Ok(())
    }
    pub async fn run_item(
        &self,
        mut item: Value,
        source: Source,
        recovery_enabled: bool,
    ) -> Result<Value> {
        if self.is_closed() {
            return Ok(json!({"success":false,"skipped":true,"error_code":"service_stopping"}));
        }
        let started = Instant::now();
        let id = integer(&item["account_id"]).ok_or_else(Error::invalid)?;
        let row = self.row(id).await?;
        item["row"] = row.clone();
        let generation = self.generation(id)?;
        let Ok(_permit) = self.budget.clone().try_acquire_owned() else {
            self.waiting(
                id,
                "worker_slots",
                Some(self.now() + Duration::seconds(2)),
                generation,
            )?;
            return Ok(json!({"success":false,"skipped":true,"error_code":"worker_slots"}));
        };
        let lease = match self.lease(&row) {
            Ok(lease) => lease,
            Err(e) if e.status.as_u16() == 409 => {
                self.waiting(
                    id,
                    "account_busy",
                    Some(self.now() + Duration::seconds(2)),
                    generation,
                )?;
                return Ok(json!({"success":false,"skipped":true,"error_code":"account_busy"}));
            }
            Err(e) => return Err(e),
        };
        let key = self.store.admin_token()?;
        if key.is_empty() {
            return Ok(
                json!({"success":false,"error_code":"admin_token_missing","error":"尚未设置管理员凭据"}),
            );
        }
        let config = self.config.current()?;
        let method_generation = integer(
            &self.shared_state()?["scheduler"][id.to_string()]["recovery_method_generation"],
        )
        .unwrap_or(0);
        let result = if truthy(&item["reuse_quota"]) {
            item["reuse_quota"].clone()
        } else {
            self.queries
                .query(
                    id,
                    &key,
                    QueryOptions {
                        source,
                        reason: string(&item["reason"]).into(),
                        at: self.now(),
                        requested_at: Some(started),
                        timeout: 10,
                        generation: Some(generation),
                    },
                )
                .await
        };
        if generation != self.generation(id)? {
            return Ok(json!({"success":false,"skipped":true,"error_code":"manual_intervention"}));
        }
        let current = self.shared_state()?;
        let (mut update, intent, depleted) = recovery_events::quota_update(
            &item,
            &result,
            &current,
            self.now(),
            recovery_enabled,
            truthy(&item["reuse_quota"]),
        );
        update.generations = Some(BTreeMap::from([(id, generation)]));
        update.methods = Some(BTreeMap::from([(id, method_generation)]));
        self.store.commit(update)?;
        let tested = if let Some(intent) = intent {
            if let Some(method) = recovery_policy::method(&config, id) {
                self.verify_recovery(
                    id,
                    &intent,
                    &result,
                    generation,
                    method_generation,
                    method,
                    lease,
                )
                .await?
            } else {
                drop(lease);
                Value::Null
            }
        } else {
            drop(lease);
            Value::Null
        };
        Ok(
            json!({"account_id":id,"success":truthy(&result["success"]),"usage":result,"test":tested,"depleted":depleted,"recovered":tested["recovered"]==true,"reused":truthy(&item["reuse_quota"]),"duration_ms":started.elapsed().as_millis()}),
        )
    }
    pub async fn force_refresh(&self) -> Result<Value> {
        self.force_refresh_with_timeout(std::time::Duration::from_secs(120))
            .await
    }
    pub async fn force_refresh_with_timeout(&self, timeout: std::time::Duration) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        if self.is_closed() {
            return Err(Error::new(503, "服务正在停止"));
        }
        let (mut receive, coalesced) = {
            let mut force = self.force.lock().await;
            if self.is_closed() {
                return Err(Error::new(503, "服务正在停止"));
            }
            if let Some(receive) = force.as_ref() {
                (receive.clone(), true)
            } else {
                let (send, receive) = watch::channel(None);
                *force = Some(receive.clone());
                let this = self.clone();
                let task = self.actions.supervisor.spawn("quota_force", async move {
                    let result=this.force_cycle(timeout).await.unwrap_or_else(|_|json!({"success":false,"timed_out":false,"error_code":"force_refresh_failed","error":"主动查询未完成"}));
                    let _ = send.send(Some(result));
                    *this.force.lock().await = None;
                });
                *self.force_task.lock().await = Some(task);
                (receive, false)
            }
        };
        let result = tokio::time::timeout(timeout.max(std::time::Duration::from_millis(100)), async {
            loop {
                if let Some(result) = receive.borrow().clone() {
                    break result;
                }
                if receive.changed().await.is_err() {
                    break json!({"success":false,"error_code":"force_refresh_failed"});
                }
            }
        }).await.unwrap_or_else(|_|json!({"success":false,"timed_out":true,"error_code":"force_refresh_timeout","error":"查询仍在执行，请稍后查看结果"}));
        let mut result = result;
        result["coalesced"] = coalesced.into();
        Ok(result)
    }
    async fn force_cycle(&self, timeout: std::time::Duration) -> Result<Value> {
        let Ok(_guard) = tokio::time::timeout(
            timeout.max(std::time::Duration::from_millis(100)),
            self.cycle.lock(),
        )
        .await
        else {
            return Ok(json!({"success":false,"timed_out":true,
                "error_code":"force_refresh_timeout","error":"OAuth 全量刷新等待监控锁超时"}));
        };
        if self.is_closed() {
            return Err(Error::new(503, "服务正在停止"));
        }
        let start = Instant::now();
        let at = self.now();
        let rows = self.inventory(true).await?;
        let total = rows.len();
        if total > 0
            && (self.store.admin_token()?.trim().is_empty()
                || self.actions.upstream.active_url().is_empty())
        {
            return Ok(
                json!({"success":false,"refresh_at":datetime(at),"total_count":total,
                "queried_count":0,"skipped_count":0,"reused_count":0,"success_count":0,"failure_count":total,
                "depleted_count":0,"recovered_count":0,"coalesced":false,
                "error_code":"missing_sub2api_admin_credentials","error":"缺少 Sub2API 地址或 Admin API Key"}),
            );
        }
        let state = self.shared_state()?;
        let config = self.config.current()?;
        let results = stream::iter(rows.iter().cloned().map(|row| {
            let this = self.clone();
            let state = &state;
            let config = &config;
            async move {
                let id = integer(&row["id"]).unwrap_or(0);
                let saved = quota::latest_result(&row, &state["oauth_results"][id.to_string()], at);
                let item = json!({"account_id":id,"row":row,"reason":"manual","previous_summary":quota::summary_from_result(&row,&saved)});
                this.run_item(item, Source::Manual, config["oauth_recovery_monitor_enabled"] != false)
                    .await.unwrap_or_else(|_|json!({"success":false,"error_code":"account_read_failed"}))
            }
        })).buffer_unordered(integer(&config["oauth_usage_refresh_concurrency"]).unwrap_or(4).clamp(1,16) as usize)
            .collect::<Vec<_>>().await;
        let count = |key: &str| results.iter().filter(|v| truthy(&v[key])).count();
        let skipped = results
            .iter()
            .filter(|v| truthy(&v["skipped"]) || truthy(&v["usage"]["skipped"]))
            .count();
        let reused = results
            .iter()
            .filter(|v| truthy(&v["reused"]) || truthy(&v["usage"]["coalesced"]))
            .count();
        let report = json!({"success":count("success")==results.len(),"refresh_at":datetime(at),"total_count":total,"queried_count":results.len().saturating_sub(skipped+reused),"skipped_count":skipped,"reused_count":reused,"success_count":count("success"),"failure_count":results.len().saturating_sub(count("success")+skipped),"depleted_count":count("depleted"),"recovered_count":count("recovered"),"coalesced":false,"duration_ms":start.elapsed().as_millis()});
        self.audit("oauth_refresh", report.clone());
        Ok(report)
    }
    pub async fn deliver(&self) -> Result<()> {
        if self.actions.settings.read_only || self.is_closed() {
            return Ok(());
        }
        let config = self.config.bark.runtime();
        if !config.valid {
            return Ok(());
        }
        let events = self.shared_state()?["pending_events"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in events {
            let state = self.shared_state()?;
            let event = &state["pending_events"][&key];
            if !event.is_object() {
                continue;
            }
            if !config.enabled {
                self.store
                    .mark_delivered(std::slice::from_ref(event), true)?;
                if let Some(job) = event["model_test_job_id"].as_str() {
                    if self.hooks.model_notification(job, true).is_err() {
                        eprintln!("detection_log_diagnostic reason=notification_write_failed");
                    }
                }
            } else {
                let delivered = self
                    .config
                    .bark
                    .notify_oauth(std::slice::from_ref(event), Some(&config))
                    .await;
                self.store.mark_delivered(&delivered, false)?;
                for event in &delivered {
                    if let Some(job) = event["model_test_job_id"].as_str() {
                        if self.hooks.model_notification(job, false).is_err() {
                            eprintln!("detection_log_diagnostic reason=notification_write_failed");
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub async fn drain(&self, seconds: u64) {
        self.closed.store(true, Ordering::Release);
        let drain_workers = async {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(seconds), async {
                loop {
                    let idle = self
                        .workers
                        .lock()
                        .await
                        .values()
                        .all(|task| task.is_finished())
                        && self
                            .daily
                            .lock()
                            .await
                            .as_ref()
                            .is_none_or(|task| task.is_finished())
                        && self
                            .force_task
                            .lock()
                            .await
                            .as_ref()
                            .is_none_or(|task| task.is_finished());
                    if idle {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await;
            let workers = std::mem::take(&mut *self.workers.lock().await);
            let daily = self.daily.lock().await.take();
            let force = {
                let _guard = self.force.lock().await;
                self.force_task.lock().await.take()
            };
            for task in workers.values() {
                task.abort();
            }
            if let Some(task) = &daily {
                task.abort();
            }
            if let Some(task) = &force {
                task.abort();
            }
            for (_, task) in workers {
                let _ = task.await;
            }
            if let Some(task) = daily {
                let _ = task.await;
            }
            if let Some(task) = force {
                let _ = task.await;
            }
            *self.force.lock().await = None;
        };
        tokio::join!(drain_workers, self.queries.drain(seconds));
    }
}
