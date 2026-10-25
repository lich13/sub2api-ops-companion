//! State files are shared with the previous release during rollback. Every
//! transaction rereads under the same advisory lock before atomic replacement.
use crate::error::{Error, Result};
use fs2::FileExt;
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

type Validator = fn(&Value) -> Result<()>;

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl From<&fs::Metadata> for FileStamp {
    fn from(metadata: &fs::Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        }
    }
}

struct Inner {
    path: PathBuf,
    lock_path: PathBuf,
    defaults: Arc<Value>,
    existed: AtomicBool,
    mutex: Mutex<()>,
    read_only: bool,
    validate: Validator,
    cache: Mutex<Option<(FileStamp, Arc<Value>)>>,
}

#[derive(Clone)]
pub struct Store(Arc<Inner>);

pub fn valid_object(value: &Value) -> Result<()> {
    if value.is_object() {
        Ok(())
    } else {
        Err(Error::new(503, "控制状态文件无效"))
    }
}

pub fn valid_policy(value: &Value) -> Result<()> {
    valid_object(value)?;
    if value["version"] == 1 {
        Ok(())
    } else {
        Err(Error::new(503, "控制状态文件版本无效"))
    }
}

impl Store {
    pub fn new(
        path: impl Into<PathBuf>,
        defaults: Value,
        validate: Validator,
        read_only: bool,
    ) -> Self {
        let path = path.into();
        let lock_path = path.with_extension("lock");
        Self::with_lock(path, lock_path, defaults, validate, read_only)
    }

    pub fn with_lock(
        path: PathBuf,
        lock_path: PathBuf,
        defaults: Value,
        validate: Validator,
        read_only: bool,
    ) -> Self {
        let existed = path.exists() || lock_path.exists();
        Self(Arc::new(Inner {
            path,
            lock_path,
            defaults: Arc::new(defaults),
            existed: AtomicBool::new(existed),
            mutex: Mutex::new(()),
            read_only,
            validate,
            cache: Mutex::new(None),
        }))
    }

    pub fn policy(path: impl Into<PathBuf>, mut defaults: Value, read_only: bool) -> Self {
        defaults["version"] = json!(1);
        Self::new(path, defaults, valid_policy, read_only)
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }
    pub fn is_read_only(&self) -> bool {
        self.0.read_only
    }

    pub fn read(&self) -> Result<Value> {
        Ok((*self.read_shared()?).clone())
    }

    /// Immutable snapshots outlive the cache lock and remain stable after a write.
    /// Callers can project a record without copying unrelated history.
    pub(crate) fn read_shared(&self) -> Result<Arc<Value>> {
        match fs::metadata(&self.0.path) {
            Ok(metadata) => {
                let stamp = FileStamp::from(&metadata);
                let cache = self.0.cache.lock().map_err(|_| Error::internal())?;
                if let Some((cached_stamp, value)) = cache.as_ref() {
                    if *cached_stamp == stamp {
                        return Ok(value.clone());
                    }
                }
            }
            Err(_) => {
                *self.0.cache.lock().map_err(|_| Error::internal())? = None;
            }
        }
        self.read_disk()
    }

    fn read_disk(&self) -> Result<Arc<Value>> {
        match File::open(&self.0.path) {
            Ok(mut file) => {
                let before = FileStamp::from(&file.metadata()?);
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                let value: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| Error::new(503, "控制状态文件无效"))?;
                (self.0.validate)(&value)?;
                let value = Arc::new(value);
                self.0.existed.store(true, Ordering::Release);
                // Never label bytes with metadata from a replacement file.
                // Transactions always read under the file lock; read snapshots
                // may observe either side of a concurrent atomic replacement.
                let stable = FileStamp::from(&file.metadata()?) == before
                    && fs::metadata(&self.0.path)
                        .is_ok_and(|metadata| FileStamp::from(&metadata) == before);
                *self.0.cache.lock().map_err(|_| Error::internal())? =
                    stable.then(|| (before, value.clone()));
                Ok(value)
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && !self.0.existed.load(Ordering::Acquire) =>
            {
                Ok(self.0.defaults.clone())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::new(503, "控制状态文件丢失"))
            }
            Err(_) => Err(Error::new(503, "无法读取控制状态文件")),
        }
    }

    pub fn transaction<T>(&self, action: impl FnOnce(&mut Value) -> Result<T>) -> Result<T> {
        // Waiting for the cross-process lock and syncing a file are blocking
        // operations. Let Tokio replace this worker so cancellation, timers and
        // other account requests can continue while a write drains.
        if tokio::runtime::Handle::try_current().is_ok_and(|handle| {
            handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread
        }) {
            tokio::task::block_in_place(|| self.transaction_blocking(action))
        } else {
            self.transaction_blocking(action)
        }
    }

    fn transaction_blocking<T>(&self, action: impl FnOnce(&mut Value) -> Result<T>) -> Result<T> {
        if self.0.read_only {
            return Err(Error::read_only());
        }
        let _mutex = self.0.mutex.lock().map_err(|_| Error::internal())?;
        let parent = self.0.path.parent().ok_or_else(Error::internal)?;
        fs::create_dir_all(parent)?;
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let lock = options.open(&self.0.lock_path)?;
        lock.lock_exclusive()?;
        private_permissions(&lock)?;
        // A write must always reconcile the current on-disk value after taking
        // the shared advisory lock, even when a read snapshot is cached.
        let previous = self.read_disk()?;
        let mut value = (*previous).clone();
        let result = action(&mut value)?;
        (self.0.validate)(&value)?;
        if value != *previous || !self.0.existed.load(Ordering::Acquire) {
            write_json(&self.0.path, &value)?;
            *self.0.cache.lock().map_err(|_| Error::internal())? = None;
        }
        self.0.existed.store(true, Ordering::Release);
        // File drop releases the OS lock on every error path too.
        Ok(result)
    }
}

pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().ok_or_else(Error::internal)?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".sub2ops-state-")
        .tempfile_in(parent)?;
    private_permissions(temporary.as_file())?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.write_all(b"\n")?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|_| Error::internal())?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Tighten the opened file, without following a path that could be replaced.
/// Callers of persistent lock files must hold the lock before changing its mode.
pub(crate) fn private_permissions(file: &File) -> std::io::Result<()> {
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

pub fn read_config(path: &Path) -> Result<Value> {
    match fs::read(path) {
        Ok(bytes) => {
            let value =
                serde_json::from_slice(&bytes).map_err(|_| Error::new(503, "配置文件无效"))?;
            valid_object(&value)?;
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(_) => Err(Error::new(503, "无法读取配置文件")),
    }
}

/// Compatibility diagnostic only: never change modes or reject old files.
pub(crate) fn diagnose_permissions(path: &Path, resource: &'static str) {
    match fs::metadata(path) {
        #[cfg(unix)]
        Ok(metadata) if metadata.permissions().mode() & 0o777 != 0o600 => {
            eprintln!("startup_diagnostic resource={resource} error_code=existing_permissions");
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            eprintln!("startup_diagnostic resource={resource} error_code=metadata_unavailable");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_reads_observe_atomic_and_in_place_changes_without_hiding_damage() {
        use std::sync::atomic::AtomicUsize;
        static VALIDATIONS: AtomicUsize = AtomicUsize::new(0);
        fn validate(value: &Value) -> Result<()> {
            VALIDATIONS.fetch_add(1, Ordering::SeqCst);
            valid_policy(value)
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let store = Store::new(&path, json!({"version":1,"value":1}), validate, false);
        store.transaction(|_| Ok(())).unwrap();
        let first = store.read_shared().unwrap();
        assert_eq!(first["value"], 1);
        let validations = VALIDATIONS.load(Ordering::SeqCst);
        assert!(Arc::ptr_eq(&first, &store.read_shared().unwrap()));
        assert_eq!(store.read().unwrap()["value"], 1);
        assert_eq!(VALIDATIONS.load(Ordering::SeqCst), validations);
        write_json(&path, &json!({"version":1,"value":2})).unwrap();
        assert_eq!(store.read().unwrap()["value"], 2);
        assert_eq!(
            first["value"], 1,
            "external replacement cannot mutate retained snapshots"
        );
        assert_eq!(VALIDATIONS.load(Ordering::SeqCst), validations + 1);
        fs::write(&path, b"{\"version\":1,\"value\":3}").unwrap();
        assert_eq!(store.read().unwrap()["value"], 3);
        fs::write(&path, b"{fixture-corrupt-state}").unwrap();
        assert!(store.read_shared().is_err());
        assert!(store
            .transaction(|value| {
                value["value"] = 4.into();
                Ok(())
            })
            .is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{fixture-corrupt-state}");
        fs::remove_file(&path).unwrap();
        assert!(store.read().is_err());
        assert!(store.0.cache.lock().unwrap().is_none());
    }
    #[test]
    fn shared_snapshots_remain_stable_across_concurrent_and_rejected_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::policy(dir.path().join("state.json"), json!({"value":1}), false);
        store.transaction(|_| Ok(())).unwrap();
        let retained = store.read_shared().unwrap();
        let writer = store.clone();
        std::thread::spawn(move || {
            writer.transaction(|data| {
                data["value"] = json!(2);
                Ok(())
            })
        })
        .join()
        .unwrap()
        .unwrap();
        let current = store.read_shared().unwrap();
        assert_eq!(retained["value"], 1);
        assert_eq!(current["value"], 2);
        let mut owned = store.read().unwrap();
        owned["value"] = json!(3);
        assert_eq!(store.read_shared().unwrap()["value"], 2);
        let rejected: Result<()> = store.transaction(|data| {
            data["value"] = json!(4);
            Err(Error::invalid())
        });
        assert!(rejected.is_err());
        assert_eq!(retained["value"], 1);
        assert_eq!(current["value"], 2);
        assert_eq!(store.read_shared().unwrap()["value"], 2);
    }
    #[test]
    fn unchanged_admin_token_does_not_enter_a_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::oauth_state::OAuthState::new(dir.path().join("state.json"), false);
        state.save_admin_token("fixture-admin").unwrap();
        let before = state.0.read_shared().unwrap();
        state.save_admin_token("  fixture-admin  ").unwrap();
        assert!(Arc::ptr_eq(&before, &state.0.read_shared().unwrap()));
        state.save_admin_token("fixture-replacement").unwrap();
        assert_eq!(state.admin_token().unwrap(), "fixture-replacement");
        assert_eq!(before["settings"]["sub2api_admin_token"], "fixture-admin");
        fs::write(state.0.path(), b"{broken").unwrap();
        assert!(state.save_admin_token("fixture-replacement").is_err());
        assert_eq!(fs::read(state.0.path()).unwrap(), b"{broken");
    }
    #[test]
    fn transactions_merge_disk_changes_and_reject_lost_or_corrupt_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let a = Store::policy(&path, json!({"jobs":{}}), false);
        a.transaction(|value| {
            value["jobs"]["one"] = json!(1);
            Ok(())
        })
        .unwrap();
        let b = Store::policy(&path, json!({"jobs":{}}), false);
        b.transaction(|value| {
            value["jobs"]["two"] = json!(2);
            Ok(())
        })
        .unwrap();
        a.transaction(|value| {
            value["jobs"]["three"] = json!(3);
            Ok(())
        })
        .unwrap();
        assert_eq!(a.read().unwrap()["jobs"].as_object().unwrap().len(), 3);
        fs::write(&path, b"{broken").unwrap();
        assert!(a.transaction(|_| Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
        fs::remove_file(&path).unwrap();
        assert!(a.read().is_err());
        assert!(Store::policy(&path, json!({}), false).read().is_err());
    }
    #[test]
    fn read_only_does_not_create_state_or_lock_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::policy(dir.path().join("absent/state.json"), json!({}), true);
        assert_eq!(store.read().unwrap(), json!({"version":1}));
        assert!(store.transaction(|_| Ok(())).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[test]
    fn rejected_transaction_keeps_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = Store::policy(&path, json!({}), false);
        store.transaction(|_| Ok(())).unwrap();
        let before = fs::read(&path).unwrap();
        let result: Result<()> = store.transaction(|value| {
            value["version"] = json!(2);
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(before, fs::read(&path).unwrap());
    }
}
