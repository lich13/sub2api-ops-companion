//! Account-scoped admin directories. This is not the public Codex model catalog.
use crate::{
    error::{Error, Result},
    settings::Settings,
    store::Store,
    tasks::Tasks,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{watch, Semaphore};

const RETRY: Duration = Duration::from_secs(5);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_ENTRIES: usize = 256;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Failure {
    pub error: Error,
    pub transient: bool,
}
impl Failure {
    pub fn permanent(error: Error) -> Self {
        Self {
            error,
            transient: false,
        }
    }
    pub fn temporary(error: Error) -> Self {
        Self {
            error,
            transient: true,
        }
    }
}
type Outcome = std::result::Result<Arc<Vec<Value>>, Failure>;
type Flight = watch::Receiver<Option<Outcome>>;
struct Success {
    models: Arc<Vec<Value>>,
    bytes: usize,
}
struct Entry {
    success: Option<Success>,
    failure: Option<(Instant, Failure)>,
    flight: Option<(u64, Flight)>,
    used: Instant,
}
#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    serial: u64,
}
#[derive(Clone)]
pub(crate) struct Candidates {
    state: Arc<Mutex<State>>,
    slots: Arc<Semaphore>,
    tasks: Tasks,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    disk: Option<Store>,
}
fn unavailable() -> Error {
    Error::new(503, "模型目录暂不可用，请稍后重试")
}
fn evict(state: &mut State, inserting: bool) -> bool {
    while state.entries.len() + usize::from(inserting) > MAX_ENTRIES
        || state
            .entries
            .values()
            .filter_map(|e| e.success.as_ref())
            .map(|s| s.bytes)
            .sum::<usize>()
            > MAX_BYTES
    {
        let victim = state
            .entries
            .iter()
            .filter(|(_, e)| e.flight.is_none())
            .min_by_key(|(_, e)| e.used)
            .map(|(key, _)| key.clone());
        let Some(victim) = victim else {
            return false;
        };
        state.entries.remove(&victim);
    }
    true
}
impl Candidates {
    pub fn new(supervisor: crate::supervision::Supervisor) -> Self {
        Self::with_clock(supervisor, Arc::new(Instant::now))
    }
    pub(crate) fn with_clock(
        supervisor: crate::supervision::Supervisor,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self {
            state: Default::default(),
            slots: Arc::new(Semaphore::new(8)),
            tasks: Tasks::supervised(supervisor),
            clock,
            disk: None,
        }
    }
    pub fn persisted(mut self, settings: &Settings) -> Result<Self> {
        let disk = Store::policy(
            settings.data_dir().join("account-model-cache.json"),
            json!({"entries":{}}),
            settings.read_only,
        );
        let saved = disk.read_shared()?;
        let at = (self.clock)();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            for (identity, models) in saved["entries"].as_object().into_iter().flatten() {
                let Some(models) = models
                    .as_array()
                    .filter(|items| items.iter().all(|v| v["id"].is_string()))
                else {
                    continue;
                };
                let bytes = serde_json::to_vec(models)?.len();
                state.entries.insert(
                    identity.clone(),
                    Entry {
                        success: Some(Success {
                            models: Arc::new(models.clone()),
                            bytes,
                        }),
                        failure: None,
                        flight: None,
                        used: at,
                    },
                );
                evict(&mut state, false);
            }
        }
        self.disk = Some(disk);
        Ok(self)
    }
    fn persist(&self, identity: &str, models: Option<&Vec<Value>>) -> Result<()> {
        let Some(disk) = self.disk.as_ref().filter(|disk| !disk.is_read_only()) else {
            return Ok(());
        };
        disk.transaction(|data| {
            let entries = data["entries"]
                .as_object_mut()
                .ok_or_else(|| Error::new(503, "模型缓存格式无效"))?;
            entries.shift_remove(identity);
            if let Some(models) = models {
                entries.insert(identity.to_owned(), json!(models));
            }
            while entries.len() > MAX_ENTRIES
                || entries
                    .values()
                    .map(|v| {
                        serde_json::to_vec(v)
                            .map(|v| v.len())
                            .unwrap_or(MAX_BYTES + 1)
                    })
                    .sum::<usize>()
                    > MAX_BYTES
            {
                let Some(oldest) = entries.keys().next().cloned() else {
                    break;
                };
                entries.shift_remove(&oldest);
            }
            Ok(())
        })
    }
    pub async fn close(&self) {
        self.slots.close();
        self.tasks.close(0).await;
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .clear();
    }
    pub async fn get(
        &self,
        identity: String,
        refresh: bool,
        fetch: impl Future<Output = std::result::Result<Vec<Value>, Failure>> + Send + 'static,
    ) -> Result<Vec<Value>> {
        if self.tasks.is_closed() {
            return Err(Error::new(503, "服务正在停止"));
        }
        let at = (self.clock)();
        let mut receive = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = state.entries.get_mut(&identity) {
                entry.used = at;
                if !refresh {
                    if let Some(success) = &entry.success {
                        return Ok((*success.models).clone());
                    }
                    if let Some((failed_at, failure)) = &entry.failure {
                        if at.saturating_duration_since(*failed_at) < RETRY {
                            return Err(failure.error.clone());
                        }
                    }
                }
            }
            if let Some((_, flight)) = state.entries.get(&identity).and_then(|e| e.flight.as_ref())
            {
                flight.clone()
            } else {
                let permit = match self.slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => return Err(unavailable()),
                };
                let inserting = !state.entries.contains_key(&identity);
                if !evict(&mut state, inserting) {
                    return Err(unavailable());
                }
                state.serial = state.serial.wrapping_add(1);
                let serial = state.serial;
                let (send, receive) = watch::channel(None);
                let entry = state.entries.entry(identity.clone()).or_insert(Entry {
                    success: None,
                    failure: None,
                    flight: None,
                    used: at,
                });
                entry.flight = Some((serial, receive.clone()));
                let cache = self.clone();
                let task_identity = identity.clone();
                let weak = Arc::downgrade(&self.state);
                let cleanup_identity = identity.clone();
                // Construct the guard inside the task: spawn failure is handled below
                // while this mutex is held; dropping a guard here would deadlock.
                let task = async move {
                    let _permit = permit;
                    let _cleanup = crate::supervision::on_drop(move || {
                        if let Some(state) = weak.upgrade() {
                            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(entry) = state.entries.get_mut(&cleanup_identity) {
                                if entry.flight.as_ref().is_some_and(|(id, _)| *id == serial) {
                                    entry.flight = None;
                                }
                            }
                        }
                    });
                    let mut outcome = tokio::time::timeout(REFRESH_TIMEOUT, fetch)
                        .await
                        .unwrap_or_else(|_| {
                            Err(Failure::temporary(Error::new(
                                504,
                                "模型目录读取超时，请稍后重试",
                            )))
                        })
                        .map(Arc::new);
                    match &outcome {
                        Ok(models) => {
                            if cache.persist(&task_identity, Some(models)).is_err() {
                                outcome = Err(Failure::temporary(Error::new(
                                    503,
                                    "模型目录已读取，但缓存保存失败，请重试",
                                )));
                            }
                        }
                        Err(failure) if !failure.transient => {
                            let _ = cache.persist(&task_identity, None);
                        }
                        _ => (),
                    }
                    let now = (cache.clock)();
                    let mut state = cache.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(entry) = state.entries.get_mut(&task_identity) {
                        if entry.flight.as_ref().is_some_and(|(id, _)| *id == serial) {
                            match &outcome {
                                Ok(models) => {
                                    let bytes = serde_json::to_vec(models.as_ref())
                                        .map(|v| v.len())
                                        .unwrap_or(MAX_BYTES + 1);
                                    entry.success = Some(Success {
                                        models: models.clone(),
                                        bytes,
                                    });
                                    entry.failure = None;
                                }
                                Err(failure) => {
                                    if !failure.transient {
                                        entry.success = None;
                                    }
                                    entry.failure = Some((now, failure.clone()));
                                }
                            }
                            entry.flight = None;
                            entry.used = now;
                        }
                    }
                    evict(&mut state, false);
                    send.send_replace(Some(outcome));
                };
                if let Err(error) = self.tasks.spawn(task) {
                    if let Some(entry) = state.entries.get_mut(&identity) {
                        entry.flight = None;
                    }
                    return Err(error);
                }
                receive
            }
        };
        loop {
            if let Some(outcome) = receive.borrow_and_update().clone() {
                return outcome
                    .map(|models| (*models).clone())
                    .map_err(|failure| failure.error);
            }
            receive.changed().await.map_err(|_| unavailable())?;
        }
    }
}

#[cfg(test)]
#[path = "model_candidates_tests.rs"]
mod tests;
