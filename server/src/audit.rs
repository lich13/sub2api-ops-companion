use crate::compat::datetime;
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 20 * 1024 * 1024;

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    name.into()
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}
fn lock(path: &Path) -> std::io::Result<fs::File> {
    // Lock a stable inode before opening either data file: rename cannot leave
    // a waiting writer appending to a retired file.
    let file = options().open(sibling(path, ".lock"))?;
    crate::store::private_permissions(&file)?;
    file.lock_exclusive()?;
    Ok(file)
}

pub fn write_at(path: &Path, event: &str, payload: Value, at: DateTime<Utc>) {
    let result = crate::store::blocking_file_io(|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let _lock = lock(path)?;
        let mut file = options().open(path)?;
        crate::store::private_permissions(&file)?;
        let payload = crate::security::redact_json(&payload);
        let mut bytes =
            serde_json::to_vec(&json!({"ts":datetime(at),"event":event,"payload":payload}))?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "audit record too large",
            ));
        }
        let length = file.metadata()?.len();
        if length + bytes.len() as u64 > MAX_BYTES {
            drop(file);
            fs::rename(path, sibling(path, ".1"))?;
            file = options().open(path)?;
            crate::store::private_permissions(&file)?;
        }
        file.write_all(&bytes)
    });
    if let Err(error) = result {
        tracing::error!(category = "audit_write", kind = ?error.kind(), "audit_write_failed");
    }
}
