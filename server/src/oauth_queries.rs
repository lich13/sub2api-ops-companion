use crate::{
    actions::Actions,
    audit,
    compat::{datetime, integer, parse_datetime, string, truthy},
    credit_ownership,
    error::{Error, Result},
    oauth_admission::{
        self, automatic_gate, credential_fingerprint, quota_complete, AUTH_ERRORS,
        AUTO_WINDOW_SECONDS, BACKOFF,
    },
    oauth_state::{query_metadata, validate_observation},
    quota, reset_credit_observation,
    upstream::read_limited,
};
use chrono::{DateTime, Duration as TimeDelta, Utc};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Automatic,
    Manual,
    EarlyReset,
}
impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
            Self::EarlyReset => "early_reset",
        }
    }
}
#[derive(Clone)]
pub struct QueryOptions {
    pub source: Source,
    pub reason: String,
    pub at: DateTime<Utc>,
    pub requested_at: Option<Instant>,
    pub timeout: u64,
    pub generation: Option<i64>,
}
impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            source: Source::Automatic,
            reason: String::new(),
            at: Utc::now(),
            requested_at: None,
            timeout: 10,
            generation: None,
        }
    }
}
type Pending = watch::Receiver<Option<Value>>;
#[derive(Default)]
struct Flights {
    pending: BTreeMap<i64, Pending>,
    completed: BTreeMap<i64, (Instant, String, Value)>,
    deferred: BTreeMap<i64, (String, Option<String>)>,
    tasks: BTreeMap<i64, tokio::task::JoinHandle<()>>,
    closed: bool,
}
impl Flights {
    fn forget(&mut self, id: i64) {
        self.completed.remove(&id);
        self.deferred.remove(&id);
    }
    fn retain_accounts(&mut self, ids: &BTreeSet<i64>) {
        self.completed.retain(|id, _| ids.contains(id));
        self.deferred.retain(|id, _| ids.contains(id));
    }
}
#[derive(Clone)]
pub struct Coordinator {
    pub actions: Actions,
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    flights: Arc<Mutex<Flights>>,
    capacity: Arc<dyn Fn() -> usize + Send + Sync>,
}
fn skipped(code: &str) -> Value {
    json!({"success":false,"skipped":true,"error_code":code})
}
fn coalesced(mut value: Value) -> Value {
    value["coalesced"] = true.into();
    value
}
async fn wait(mut pending: Pending, seconds: u64) -> Value {
    let waiting = async {
        loop {
            if let Some(value) = pending.borrow().clone() {
                return value;
            }
            if pending.changed().await.is_err() {
                return json!({"success":false,"uncertain":true,"error_code":"result_uncertain"});
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(seconds), waiting)
        .await
        .unwrap_or_else(|_| json!({"success":false,"uncertain":true,"error_code":"timeout"}))
}
impl Coordinator {
    pub(crate) fn forget(&self, id: i64) {
        self.flights
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget(id);
    }
    pub(crate) fn retain_accounts(&self, rows: &[Value]) {
        let ids: BTreeSet<_> = rows.iter().filter_map(|row| integer(&row["id"])).collect();
        self.flights
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain_accounts(&ids);
    }
    pub fn new(actions: Actions, capacity: Arc<dyn Fn() -> usize + Send + Sync>) -> Self {
        Self {
            actions,
            clock: Arc::new(Utc::now),
            flights: Arc::new(Mutex::new(Flights::default())),
            capacity,
        }
    }
    fn spawn_owned(&self, id: i64, future: impl Future<Output = ()> + Send + 'static) {
        let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
        if flights.closed || self.actions.supervisor.is_stopping() {
            flights.pending.remove(&id);
            return;
        }
        let (send, receive) = tokio::sync::oneshot::channel();
        let task = self.actions.supervisor.spawn("quota_query", async move {
            if receive.await.is_ok() {
                future.await;
            }
        });
        flights.tasks.insert(id, task);
        let _ = send.send(());
    }
    fn slot_wait(at: DateTime<Utc>) -> Value {
        oauth_admission::gate_result("query_slots", Some(at + TimeDelta::seconds(2)))
    }
    pub async fn join_existing(&self, id: i64, requested_at: Instant) -> Option<Value> {
        let (pending, previous) = {
            let flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
            (
                flights.pending.get(&id).cloned(),
                flights
                    .completed
                    .get(&id)
                    .filter(|(at, _, _)| *at >= requested_at)
                    .map(|(_, _, v)| v.clone()),
            )
        };
        let result = if let Some(pending) = pending {
            wait(pending, 100).await
        } else {
            previous?
        };
        if truthy(&result["skipped"]) {
            None
        } else {
            Some(coalesced(result))
        }
    }
    fn complete(
        &self,
        id: i64,
        row: &Value,
        admitted: bool,
        result: Value,
        tx: watch::Sender<Option<Value>>,
    ) {
        let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
        if admitted {
            flights.completed.insert(
                id,
                (Instant::now(), credential_fingerprint(row), result.clone()),
            );
        }
        let _ = tx.send(Some(result));
        flights.pending.remove(&id);
        flights.tasks.remove(&id);
    }
    pub fn observe_gates(
        &self,
        rows: &[impl std::borrow::Borrow<Value>],
        results: &Value,
        scheduler: &Value,
        at: DateTime<Utc>,
    ) -> Result<()> {
        for row in rows {
            let row = row.borrow();
            let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
            let saved = quota::latest_result(row, &results[id.to_string()], at);
            let (blocked, deadline) =
                automatic_gate(row, &saved, &scheduler[id.to_string()], at, true, false, 1)?;
            let value = (blocked.to_owned(), deadline.map(datetime));
            let changed = {
                let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
                let changed = flights.deferred.get(&id) != Some(&value);
                flights.deferred.insert(id, value.clone());
                changed
            };
            if !blocked.is_empty() && changed {
                audit::write(
                    &self.actions.settings.audit_path,
                    "oauth_usage_deferred",
                    json!({"account_id":id,"source":"automatic","reason":blocked,"next_query_at":value.1}),
                    self.actions.settings.read_only,
                );
            }
        }
        Ok(())
    }
    pub async fn query(&self, id: i64, key: &str, options: QueryOptions) -> Value {
        if self.actions.settings.read_only {
            return skipped("read_only");
        }
        let row = match credit_ownership::account(&self.actions.queries.db, id).await {
            Ok(row) => row,
            Err(_) => return skipped("account_read_failed"),
        };
        if row["platform"] != "openai" || row["type"] != "oauth" {
            return skipped("account_changed");
        }
        let fingerprint = credential_fingerprint(&row);
        let (pending, owner) = {
            let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
            if flights.closed || self.actions.supervisor.is_stopping() {
                return skipped("service_stopping");
            }
            if let Some(pending) = flights.pending.get(&id) {
                (pending.clone(), None)
            } else if let Some((_, _, previous)) =
                flights.completed.get(&id).filter(|(at, fp, _)| {
                    options.requested_at.is_some_and(|request| *at >= request) && fp == &fingerprint
                })
            {
                return coalesced(previous.clone());
            } else {
                if flights.pending.len() >= (self.capacity)().clamp(1, 16) {
                    return Self::slot_wait(options.at);
                }
                let (tx, rx) = watch::channel(None);
                flights.pending.insert(id, rx.clone());
                (rx, Some(tx))
            }
        };
        let is_owner = owner.is_some();
        let seconds = options.timeout.saturating_add(5).max(40);
        if let Some(tx) = owner {
            let this = self.clone();
            let key = key.to_owned();
            self.spawn_owned(id, async move {
                let (admitted, result) = this.query_owned(id, &row, &key, &options).await;
                this.audit(id, &options, admitted, &result);
                this.complete(id, &row, admitted, result, tx);
            });
        }
        let result = wait(pending, seconds).await;
        if is_owner {
            result
        } else {
            coalesced(result)
        }
    }
    async fn query_owned(
        &self,
        id: i64,
        row: &Value,
        key: &str,
        options: &QueryOptions,
    ) -> (bool, Value) {
        let at = options.at;
        let identity = id.to_string();
        let admission = self.actions.queries.oauth.transaction(|state| {
            let saved = state["oauth_results"][&identity].clone();
            let metadata = &mut state["scheduler"][&identity];
            if !metadata.is_object() {
                *metadata = json!({});
            }
            if options.generation.is_some_and(|generation| {
                integer(&metadata["manual_control"]["generation"]).unwrap_or(0) != generation
            }) {
                return Ok(skipped("manual_intervention"));
            }
            let evidence = quota::latest_result(row, &saved, at);
            let mut query = query_metadata(metadata, &saved)?;
            if options.source == Source::EarlyReset {
                if !crate::oauth_early_reset::eligible(row, metadata, at) {
                    return Ok(skipped("account_ineligible"));
                }
                query["early_reset_last_query_at"] = datetime(at).into();
                metadata["quota_query"] = query;
                return Ok(json!({"admitted":true}));
            }
            if options.source == Source::Automatic {
                let (blocked, deadline) =
                    automatic_gate(row, &evidence, metadata, at, true, false, 1)?;
                if !blocked.is_empty() {
                    return Ok(oauth_admission::gate_result(blocked, deadline));
                }
                let mut attempts = query["automatic_attempts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| {
                        parse_datetime(v)
                            .is_some_and(|t| t > at - TimeDelta::seconds(AUTO_WINDOW_SECONDS))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                attempts.push(datetime(at).into());
                query["automatic_attempts"] = attempts.into();
            }
            query["last_query_at"] = datetime(at).into();
            query["last_source"] = options.source.as_str().into();
            query["last_reason"] = options.reason.clone().into();
            metadata["quota_query"] = query;
            metadata["last_attempt_at"] = datetime(at).into();
            Ok(json!({"admitted":true}))
        });
        let admission = match admission {
            Ok(v) => v,
            Err(_) => {
                return (
                    false,
                    json!({"success":false,"skipped":true,"error_code":"query_state_unavailable","error":"额度查询状态无法安全保存"}),
                )
            }
        };
        if admission["admitted"] != true {
            return (false, admission);
        }
        let mut result = self.execute(row, key, options.timeout, at).await;
        if options.source == Source::EarlyReset {
            result["query_source"] = "early_reset".into();
        }
        if truthy(&result["success"]) && !quota_complete(row, &result) {
            result["success"] = false.into();
            result["error_code"] = "incomplete_quota".into();
            result["error"] = "额度响应缺少必要窗口".into();
        }
        let observation = if truthy(&result["success"])
            && result["data"].get("rate_limit_reset_credits").is_some()
        {
            match credit_ownership::account(&self.actions.queries.db, id).await {
                Ok(after) => {
                    reset_credit_observation::from_usage(row, &after, &result, at, (self.clock)())
                }
                Err(_) => None,
            }
        } else {
            None
        };
        let finish = self.actions.queries.oauth.transaction(|state| {
            let meta = &mut state["scheduler"][&identity];
            if !meta.is_object() {
                *meta = json!({});
            }
            let mut query = meta["quota_query"]
                .as_object()
                .cloned()
                .map(Value::Object)
                .unwrap_or(json!({}));
            let code = string(&result["error_code"]);
            if options.source == Source::EarlyReset {
                if AUTH_ERRORS.contains(&code) {
                    query["auth_fingerprint"] = credential_fingerprint(row).into();
                } else if truthy(&result["success"]) {
                    query
                        .as_object_mut()
                        .unwrap()
                        .shift_remove("auth_fingerprint");
                }
                meta["quota_query"] = query;
                if truthy(&result["success"]) {
                    state["oauth_results"][&identity] = result.clone();
                }
                return Ok(());
            }
            if truthy(&result["success"]) {
                if let Some(observation) = &observation {
                    meta["reset_credit_observation"] = observation.clone();
                }
                meta["last_success_at"] = datetime(at).into();
                meta["last_error_code"] = "".into();
                query
                    .as_object_mut()
                    .unwrap()
                    .shift_remove("auth_fingerprint");
                if meta["recovery_intent"]["status"] == "auth_failed" {
                    meta["recovery_intent"]["status"] = "retry".into();
                    meta["recovery_intent"]["next_retry_at"] =
                        datetime(at + TimeDelta::seconds(oauth_admission::MIN_QUERY_SECONDS))
                            .into();
                }
            } else {
                meta["last_error_at"] = datetime(at).into();
                meta["last_error_code"] = code.into();
            }
            if AUTH_ERRORS.contains(&code) {
                query["auth_fingerprint"] = credential_fingerprint(row).into();
            }
            let summary = quota::summary_from_result(row, &result);
            let windows = quota::windows_by_key(&summary["ui_windows"]);
            let full = quota::required_windows(&summary["plan_type"])
                .iter()
                .any(|key| {
                    quota::percent(&windows.get(*key).unwrap_or(&Value::Null)["used_percent"])
                        .is_some_and(|v| v >= 100.0)
                });
            let until = oauth_admission::reset_not_before(row, &result);
            let mut unresolved =
                !truthy(&result["success"]) || full && until.is_none_or(|t| t <= at);
            let threshold: Value = serde_json::from_str(string(&row["temp_unschedulable_reason"]))
                .unwrap_or(Value::Null);
            let window = format!(
                "codex_{}",
                string(&threshold["window"]).trim_start_matches("codex_")
            );
            if let (Some(used), Some(limit)) = (
                quota::percent(&windows.get(&window).unwrap_or(&Value::Null)["used_percent"]),
                quota::percent(&threshold["threshold_percent"]),
            ) {
                unresolved |= used >= limit && until.is_none_or(|t| t <= at);
            }
            if unresolved {
                let failures = integer(&query["failure_count"])
                    .unwrap_or(0)
                    .saturating_add(1);
                query["failure_count"] = failures.into();
                query["retry_at"] =
                    datetime(at + TimeDelta::seconds(BACKOFF[(failures - 1).clamp(0, 3) as usize]))
                        .into();
            } else if !full {
                query["failure_count"] = 0.into();
                query["retry_at"] = "".into();
            }
            meta["quota_query"] = query;
            if truthy(&result["success"]) {
                state["oauth_results"][&identity] = result.clone();
            }
            Ok(())
        });
        if finish.is_err() {
            result = json!({"success":false,"skipped":false,"error_code":"query_state_unavailable","error":"额度查询状态无法安全保存"});
        }
        (true, result)
    }
    pub async fn execute(&self, row: &Value, key: &str, seconds: u64, at: DateTime<Utc>) -> Value {
        let id = integer(&row["id"]).unwrap_or(0);
        let failure = |code: &str, message: &str| json!({"account_id":id,"template_type":"oauth","success":false,"data":{},"error":message,"queried_at":datetime(at),"unit":"%","oauth_quota":quota::quota_windows(row,None),"source":"sub2api_admin_usage","error_code":code});
        if self.actions.settings.read_only {
            return failure("read_only", "服务处于只读验收模式");
        }
        if key.trim().is_empty() || self.actions.upstream.active_url().is_empty() {
            return failure(
                "missing_sub2api_admin_credentials",
                "缺少 Sub2API 地址或 Admin API Key",
            );
        }
        let response = match self
            .actions
            .request(
                reqwest::Method::GET,
                &format!("accounts/{id}/usage?source=active&force=true"),
                key,
                None,
                Duration::from_secs(seconds.clamp(2, 60)),
                false,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return failure(
                    if e.status.as_u16() == 504 {
                        "timeout"
                    } else {
                        "network_error"
                    },
                    if e.status.as_u16() == 504 {
                        "请求超时"
                    } else {
                        "额度请求失败"
                    },
                )
            }
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            return failure(&format!("http_{status}"), &format!("HTTP {status}"));
        }
        let body = match read_limited(response, 8 * 1024 * 1024)
            .await
            .and_then(|v| serde_json::from_slice::<Value>(&v).map_err(|_| Error::internal()))
        {
            Ok(v) => v,
            Err(_) => return failure("usage_query_error", "响应不是有效 JSON"),
        };
        if !body.is_object() {
            return failure("usage_query_error", "Sub2API usage 响应必须是对象");
        }
        let data = if body.get("data").is_none() {
            body
        } else {
            if body["success"] == false
                || !body["code"].is_null() && body["code"] != 0 && body["code"] != "0"
            {
                return failure("usage_query_error", "Sub2API usage 查询失败");
            }
            if !body["data"].is_object() {
                return failure("usage_query_error", "Sub2API usage 响应缺少 data 对象");
            }
            body["data"].clone()
        };
        let summary = quota::from_usage_data(&data, row, at);
        json!({"account_id":id,"template_type":"oauth","success":true,"data":data,"error":"","queried_at":datetime(at),"unit":"%","plan_name":summary["plan_type"],"oauth_quota":summary,"source":"sub2api_admin_usage"})
    }
    fn audit(&self, id: i64, options: &QueryOptions, admitted: bool, result: &Value) {
        audit::write(
            &self.actions.settings.audit_path,
            "oauth_usage_query",
            json!({"account_id":id,"source":options.source.as_str(),"reason":options.reason,"requested":admitted,"success":truthy(&result["success"]),"error_code":string(&result["error_code"]),"next_query_at":result["next_query_at"]}),
            self.actions.settings.read_only,
        );
    }
    pub async fn external_read<F, Fut, V>(
        &self,
        row: Value,
        options: QueryOptions,
        bypass_interval: bool,
        required_budget: usize,
        validate: V,
        operation: F,
    ) -> Value
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Value> + Send + 'static,
        V: Fn(&Value) -> bool + Send + 'static,
    {
        if self.actions.settings.read_only {
            return skipped("read_only");
        }
        if bypass_interval
            && (options.source != Source::Automatic || options.reason != "reset_credit_consume")
        {
            return skipped("invalid_query_reservation");
        }
        let Some(id) = integer(&row["id"]) else {
            return skipped("account_changed");
        };
        let (tx, pending) = {
            let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
            if flights.closed || self.actions.supervisor.is_stopping() {
                return skipped("service_stopping");
            }
            if flights.pending.contains_key(&id) {
                return skipped("query_busy");
            }
            if flights.pending.len() >= (self.capacity)().clamp(1, 16) {
                return Self::slot_wait(options.at);
            }
            let (tx, rx) = watch::channel(None);
            flights.pending.insert(id, rx.clone());
            (tx, rx)
        };
        let (response_tx, response_rx) = watch::channel(None);
        let this = self.clone();
        self.spawn_owned(id, async move {
            let mut admitted = false;
            let at = options.at;
            let reservation = at + TimeDelta::seconds(180);
            let identity = id.to_string();
            let mut result = skipped("query_state_unavailable");
            let admission = this.actions.queries.oauth.transaction(|state| {
                if !validate(state) {
                    return Ok(skipped("reset_state_changed"));
                }
                let saved = state["oauth_results"][&identity].clone();
                let meta = &mut state["scheduler"][&identity];
                if !meta.is_object() {
                    *meta = json!({});
                }
                let mut query = query_metadata(meta, &saved)?;
                if options.source == Source::Automatic {
                    let mut gate_row = row.clone();
                    gate_row["schedulable"] = true.into();
                    let (blocked, deadline) = automatic_gate(
                        &gate_row,
                        &saved,
                        meta,
                        at,
                        false,
                        bypass_interval,
                        required_budget,
                    )?;
                    if !blocked.is_empty() {
                        return Ok(oauth_admission::gate_result(blocked, deadline));
                    }
                    let mut attempts = query["automatic_attempts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|v| {
                            parse_datetime(v)
                                .is_some_and(|t| t > at - TimeDelta::seconds(AUTO_WINDOW_SECONDS))
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    attempts.push(datetime(reservation).into());
                    query["automatic_attempts"] = attempts.into();
                }
                query["last_query_at"] = datetime(reservation).into();
                query["last_source"] = options.source.as_str().into();
                query["last_reason"] = options.reason.clone().into();
                meta["quota_query"] = query;
                meta["last_attempt_at"] = datetime(at).into();
                Ok(json!({"admitted":true}))
            });
            if let Ok(decision) = admission {
                result = decision;
                if result["admitted"] == true {
                    admitted = true;
                    result = operation().await;
                    let completed = (this.clock)().max(at);
                    let recorded = if truthy(&result["uncertain"])
                        || matches!(
                            string(&result["error_code"]),
                            "timeout" | "result_uncertain"
                        ) {
                        completed.max(reservation)
                    } else {
                        completed
                    };
                    let finish = this.actions.queries.oauth.transaction(|state| {
                        let saved = state["oauth_results"][&identity].clone();
                        let meta = &mut state["scheduler"][&identity];
                        if !meta.is_object() {
                            *meta = json!({});
                        }
                        let mut query = query_metadata(meta, &saved)?;
                        if options.source == Source::Automatic {
                            query["automatic_attempts"] = json!(query["automatic_attempts"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|value| if value == &datetime(reservation) {
                                    json!(datetime(recorded))
                                } else {
                                    value.clone()
                                })
                                .collect::<Vec<_>>());
                        }
                        query["last_query_at"] =
                            datetime(if query["last_query_at"] == datetime(reservation) {
                                recorded
                            } else {
                                parse_datetime(&query["last_query_at"])
                                    .unwrap_or(recorded)
                                    .max(recorded)
                            })
                            .into();
                        let quota_result = &result["quota_result"];
                        let complete =
                            quota_result.is_object() && quota_complete(&row, quota_result);
                        if complete {
                            meta["last_success_at"] = datetime(completed).into();
                            meta["last_error_code"] = "".into();
                            query
                                .as_object_mut()
                                .unwrap()
                                .shift_remove("auth_fingerprint");
                            if truthy(&result["success"]) {
                                query["failure_count"] = 0.into();
                                query["retry_at"] = Value::Null;
                            }
                        }
                        if !result["reset_credit_observation"].is_null()
                            && truthy(&result["success"])
                        {
                            validate_observation(&result["reset_credit_observation"])?;
                            meta["reset_credit_observation"] =
                                result["reset_credit_observation"].clone();
                        }
                        let code = string(&result["error_code"]);
                        if AUTH_ERRORS.contains(&code) {
                            query["auth_fingerprint"] = credential_fingerprint(&row).into();
                            meta["last_error_at"] = datetime(completed).into();
                            meta["last_error_code"] = code.into();
                        }
                        if !truthy(&result["success"]) {
                            if !truthy(&result["skipped"]) {
                                meta["last_error_at"] = datetime(completed).into();
                                meta["last_error_code"] = code.into();
                            }
                            let failures = integer(&query["failure_count"])
                                .unwrap_or(0)
                                .saturating_add(1);
                            query["failure_count"] = failures.into();
                            query["retry_at"] = datetime(
                                recorded
                                    + TimeDelta::seconds(
                                        BACKOFF[(failures - 1).clamp(0, 3) as usize],
                                    ),
                            )
                            .into();
                        }
                        meta["quota_query"] = query;
                        if complete {
                            state["oauth_results"][&identity] = quota_result.clone();
                        }
                        Ok(())
                    });
                    if finish.is_err() {
                        result["success"] = false.into();
                        result["skipped"] = false.into();
                        result["uncertain"] = true.into();
                        result["error_code"] = "query_state_unavailable".into();
                    }
                }
            }
            this.audit(id, &options, admitted, &result);
            let shared = if result["quota_result"].is_object() {
                result["quota_result"].clone()
            } else {
                json!({"success":false,"error_code":"incomplete_quota"})
            };
            this.complete(id, &row, admitted, shared, tx);
            let _ = response_tx.send(Some(result));
        });
        drop(pending);
        wait(response_rx, 300).await
    }
    pub async fn drain(&self, seconds: u64) {
        let pending = {
            let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
            flights.closed = true;
            flights.pending.values().cloned().collect::<Vec<_>>()
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(seconds),
            futures_util::future::join_all(pending.into_iter().map(|rx| wait(rx, seconds))),
        )
        .await;
        let tasks = {
            let mut flights = self.flights.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut flights.tasks)
        };
        for task in tasks.values() {
            task.abort();
        }
        for (_, task) in tasks {
            let _ = task.await;
        }
        self.flights
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .clear();
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[tokio::test]
    async fn deleted_account_cleanup_keeps_live_results_and_pending_tasks() {
        let mut flights = Flights::default();
        for id in 1..=3 {
            flights.completed.insert(
                id,
                (Instant::now(), "fixture".into(), json!({"account_id":id})),
            );
            flights.deferred.insert(id, ("query_cooldown".into(), None));
        }
        let (result, receiver) = watch::channel(None);
        let (release, waiting) = tokio::sync::oneshot::channel();
        flights.pending.insert(2, receiver);
        flights.tasks.insert(
            2,
            tokio::spawn(async move {
                waiting.await.unwrap();
                result.send(Some(json!({"success":true}))).unwrap();
            }),
        );
        flights.forget(2);
        assert_eq!(
            flights.completed.keys().copied().collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(flights.deferred.keys().copied().collect::<Vec<_>>(), [1, 3]);
        flights.retain_accounts(&BTreeSet::from([3]));
        assert_eq!(flights.completed[&3].2, json!({"account_id":3}));
        assert_eq!(flights.completed.len(), 1);
        assert_eq!(flights.deferred.keys().copied().collect::<Vec<_>>(), [3]);
        assert_eq!(flights.pending.len(), 1);
        assert_eq!(flights.tasks.len(), 1);
        release.send(()).unwrap();
        assert_eq!(
            wait(flights.pending[&2].clone(), 1).await,
            json!({"success":true})
        );
        flights.tasks.remove(&2).unwrap().await.unwrap();
    }
}
