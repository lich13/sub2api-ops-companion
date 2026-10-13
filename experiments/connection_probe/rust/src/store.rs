use crate::model::{now, Failure, Job};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
pub struct Batch {
    pub signature: String,
    pub jobs: Vec<String>,
}
#[derive(Default, Deserialize, Serialize)]
pub struct Data {
    pub jobs: Vec<Job>,
    pub batches: BTreeMap<String, Batch>,
    pub requests: BTreeMap<String, String>,
}
impl Data {
    pub fn listing(&self, bid: &str) -> Result<Value, Failure> {
        let batch = self
            .batches
            .get(bid)
            .ok_or(Failure(404, "batch_missing", "批量测试不存在"))?;
        let items: Vec<_> = batch
            .jobs
            .iter()
            .filter_map(|id| self.jobs.iter().find(|j| &j.id == id))
            .collect();
        Ok(
            json!({"batch_id":bid,"pending":items.iter().filter(|j|j.pending()).count(),"items":items.iter().map(|j|j.public()).collect::<Vec<_>>()}),
        )
    }
}
pub struct Store {
    path: PathBuf,
    _lock: File,
}
impl Store {
    pub fn open(dir: &Path) -> Result<Self, Failure> {
        fs::create_dir_all(dir)?;
        let path = dir.join("account-operations.json");
        if dir.join("prototype.lock").exists() && !path.exists() {
            return Err(Failure(503, "state_missing", "已保存的操作状态丢失"));
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("prototype.lock"))?;
        lock.try_lock_exclusive()?;
        let store = Self { path, _lock: lock };
        if !store.path.exists() {
            store.write(&Data::default())?;
        }
        let mut data = store.read()?;
        for job in &mut data.jobs {
            if matches!(job.status.as_str(), "running" | "checking") {
                job.change("needs_confirmation", "已派发请求的结果未确认，未重复执行");
            }
        }
        store.write(&data)?;
        Ok(store)
    }
    pub fn read(&self) -> Result<Data, Failure> {
        let data: Data = serde_json::from_slice(&fs::read(&self.path)?)?;
        if data.batches.iter().any(|(bid, b)| {
            b.jobs
                .iter()
                .any(|jid| !data.jobs.iter().any(|j| &j.id == jid && &j.batch_id == bid))
        }) || data
            .requests
            .values()
            .any(|bid| !data.batches.contains_key(bid))
        {
            return Err(Failure(503, "state_invalid", "批量状态无法读取"));
        }
        Ok(data)
    }
    pub fn write(&self, data: &Data) -> Result<(), Failure> {
        let tmp = self.path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        serde_json::to_writer(&mut file, data)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, &self.path)?;
        File::open(self.path.parent().unwrap())?.sync_all()?;
        Ok(())
    }
    pub fn update(&self, jid: &str, change: impl FnOnce(&mut Job)) -> Result<(), Failure> {
        let mut data = self.read()?;
        let job = data.jobs.iter_mut().find(|j| j.id == jid).ok_or(Failure(
            404,
            "job_missing",
            "任务不存在",
        ))?;
        if job.status == "cancelled" {
            return Ok(());
        }
        change(job);
        job.updated_at = now();
        self.write(&data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_job_preserves_terminal_state_against_late_worker_updates() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let job: Job = serde_json::from_value(json!({
            "id": "fixture-cancelled-job",
            "batch_id": "fixture-batch",
            "account_id": 1,
            "account_name": "fixture-account",
            "action": "test",
            "parent_account_id": null,
            "expected_version": "fixture-version",
            "requested": {"confirmed": true},
            "status": "queued",
            "reason": "fixture-queued",
            "created_at": "2026-10-13T02:00:00Z",
            "updated_at": "2026-10-13T02:00:00Z",
            "started_at": null,
            "result": null
        }))
        .unwrap();
        store
            .write(&Data {
                jobs: vec![job],
                ..Data::default()
            })
            .unwrap();
        store
            .update("fixture-cancelled-job", |job| {
                job.change("cancelled", "fixture-cancelled-before-dispatch");
            })
            .unwrap();
        let cancelled = fs::read(&store.path).unwrap();

        for status in ["needs_confirmation", "running", "completed", "failed"] {
            store
                .update("fixture-cancelled-job", |job| {
                    job.change(status, "fixture-late-worker-result");
                    job.started_at = Some(now());
                    job.result = Some(json!({"success": true}));
                })
                .unwrap();
            assert_eq!(fs::read(&store.path).unwrap(), cancelled, "{status}");
        }

        drop(store);
        let restored = Store::open(dir.path()).unwrap();
        assert_eq!(fs::read(&restored.path).unwrap(), cancelled);
        assert_eq!(restored.read().unwrap().jobs[0].status, "cancelled");
    }

    #[test]
    fn missing_state_fails_closed_and_single_owner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert!(Store::open(dir.path()).is_err());
        fs::remove_file(&store.path).unwrap();
        assert!(store.read().is_err());
    }
    #[test]
    fn corrupt_state_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account-operations.json");
        fs::write(&path, b"{").unwrap();
        assert!(Store::open(dir.path()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{");
    }
}
