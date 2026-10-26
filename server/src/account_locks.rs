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
        let mut locks = locks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lock) = locks.get(&id) {
            return lock.clone();
        }
        // Owned guards and queued acquisitions retain an Arc. The registry
        // mutex prevents another caller from obtaining a lock during removal.
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        locks.entry(id).or_default().clone()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_cleanup_preserves_owned_guards_and_queued_acquisitions() {
        let locks = AccountLocks::default();
        for control in [false, true] {
            let held = locks.lock(1, control).lock_owned().await;
            let mut waiting = Box::pin(locks.lock(1, control).lock_owned());
            assert!(futures_util::poll!(&mut waiting).is_pending());
            drop(locks.lock(2, control));
            assert!(locks.lock(1, control).try_lock_owned().is_err());
            drop(held);
            // A queued future still owns the lock even before it is polled again.
            drop(locks.lock(3, control));
            let acquired = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
                .await
                .unwrap();
            assert!(locks.lock(1, control).try_lock_owned().is_err());
            drop(acquired);
            drop(locks.lock(4, control));
            let registry = if control {
                &locks.controls
            } else {
                &locks.accounts
            };
            assert_eq!(
                registry.lock().unwrap().keys().copied().collect::<Vec<_>>(),
                [4]
            );
        }
    }

    #[tokio::test]
    async fn cleanup_preserves_parent_child_exclusion_and_separate_controls() {
        let locks = AccountLocks::default();
        let parent = locks.account(10).await;
        let child = json!({"id":11,"parent_account_id":10});
        assert!(locks.try_account(&child).is_err());
        drop(locks.try_ids([12]).unwrap());
        drop(locks.try_control(10).unwrap());
        assert!(locks.try_account(&child).is_err());
        drop(parent);
        let child = locks.try_account(&child).unwrap();
        drop(locks.try_ids([13]).unwrap());
        assert!(locks.try_ids([10]).is_err());
        assert!(locks.try_ids([11]).is_err());
        drop(child);
        assert!(locks.try_ids([10, 11]).is_ok());
    }
}
