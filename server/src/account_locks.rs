use crate::{
    compat::integer,
    error::{Error, Result},
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

#[derive(Clone, Default)]
pub struct AccountLocks {
    accounts: Arc<Mutex<HashMap<i64, Arc<AsyncMutex<()>>>>>,
    controls: Arc<Mutex<HashMap<i64, Arc<AsyncMutex<()>>>>>,
}
pub struct Lease {
    _guards: Vec<OwnedMutexGuard<()>>,
}
pub fn busy() -> Error {
    Error::new(409, json!({"code":"account_busy","message":"等待账号空闲"}))
}
impl AccountLocks {
    fn lock(&self, id: i64, control: bool) -> Arc<AsyncMutex<()>> {
        let locks = if control {
            &self.controls
        } else {
            &self.accounts
        };
        locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id)
            .or_default()
            .clone()
    }
    pub fn try_ids(&self, ids: impl IntoIterator<Item = i64>) -> Result<Lease> {
        let mut guards = Vec::new();
        for id in ids
            .into_iter()
            .filter(|id| *id > 0)
            .collect::<BTreeSet<_>>()
        {
            guards.push(self.lock(id, false).try_lock_owned().map_err(|_| busy())?);
        }
        Ok(Lease { _guards: guards })
    }
    pub fn try_account(&self, row: &Value) -> Result<Lease> {
        self.try_ids([
            integer(&row["id"]).unwrap_or(0),
            integer(&row["parent_account_id"])
                .or_else(|| integer(&row["extra"]["parent_account_id"]))
                .unwrap_or(0),
        ])
    }
    pub async fn account(&self, id: i64) -> Lease {
        Lease {
            _guards: vec![self.lock(id, false).lock_owned().await],
        }
    }
    // Manual controls must not queue behind a billable upstream request.
    pub async fn control(&self, id: i64) -> Lease {
        Lease {
            _guards: vec![self.lock(id, true).lock_owned().await],
        }
    }
    pub fn try_control(&self, id: i64) -> Result<Lease> {
        Ok(Lease {
            _guards: vec![self.lock(id, true).try_lock_owned().map_err(|_| busy())?],
        })
    }
}
