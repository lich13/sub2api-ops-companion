use crate::error::{Error, Result};
use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

#[derive(Clone)]
pub struct TestSlots {
    active: Arc<AtomicUsize>,
    limit: Arc<dyn Fn() -> usize + Send + Sync>,
}
pub struct TestPermit(Arc<AtomicUsize>);
impl Drop for TestPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl TestSlots {
    pub fn new(limit: Arc<dyn Fn() -> usize + Send + Sync>) -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }
    pub fn acquire(&self) -> Option<TestPermit> {
        let limit = (self.limit)().clamp(1, 8);
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                if n < limit {
                    Some(n + 1)
                } else {
                    None
                }
            })
            .ok()
            .map(|_| TestPermit(self.active.clone()))
    }
    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}
pub struct DiskLease(File);
impl DiskLease {
    pub fn try_acquire(state: &Path, suffix: &str, read_only: bool) -> Result<Self> {
        if read_only {
            return Err(Error::read_only());
        }
        if let Some(parent) = state.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(format!("{}.{suffix}", state.display()))?;
        file.try_lock_exclusive()
            .map_err(|_| crate::account_locks::busy())?;
        Ok(Self(file))
    }
}
impl Drop for DiskLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
