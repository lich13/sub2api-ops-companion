use crate::compat::now;
use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
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
fn lock(path: &Path, exclusive: bool) -> std::io::Result<fs::File> {
    // Lock a stable inode before opening either data file: rename cannot leave
    // a waiting writer appending to a retired file.
    let file = if exclusive {
        let file = options().open(sibling(path, ".lock"))?;
        crate::store::private_permissions(&file)?;
        file
    } else {
        fs::File::open(sibling(path, ".lock"))?
    };
    if exclusive {
        file.lock_exclusive()?;
    } else {
        FileExt::lock_shared(&file)?;
    }
    Ok(file)
}
pub fn write(path: &Path, event: &str, payload: Value, read_only: bool) {
    if read_only {
        return;
    }
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let _lock = lock(path, true)?;
        let mut file = options().open(path)?;
        crate::store::private_permissions(&file)?;
        let payload = crate::security::redact_json(&payload);
        let mut bytes = serde_json::to_vec(&json!({"ts":now(),"event":event,"payload":payload}))?;
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
        }
        file.write_all(&bytes)
    })();
    let _ = result; // Audit failure must not undo an already confirmed operation.
}
fn tail(file: &mut fs::File, mut visit: impl FnMut(&[u8]) -> bool) -> std::io::Result<()> {
    let mut offset = file.metadata()?.len();
    let mut buffer = [0; 8192];
    let mut line = Vec::new();
    while offset > 0 {
        let count = offset.min(buffer.len() as u64) as usize;
        offset -= count as u64;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut buffer[..count])?;
        for byte in buffer[..count].iter().rev() {
            if *byte == b'\n' {
                if !line.is_empty() {
                    line.reverse();
                    if !visit(&line) {
                        return Ok(());
                    }
                    line.clear();
                }
            } else {
                line.push(*byte);
            }
        }
    }
    if !line.is_empty() {
        line.reverse();
        visit(&line);
    }
    Ok(())
}

pub fn read(path: &Path, limit: usize, prefix: Option<&str>) -> Vec<Value> {
    if limit == 0 {
        return vec![];
    }
    let _lock = match lock(path, false) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return vec![],
    };
    let mut records = Vec::new();
    for path in [path.to_path_buf(), sibling(path, ".1")] {
        let Ok(mut file) = fs::File::open(path) else {
            continue;
        };
        let _ = tail(&mut file, |line| {
            if let Ok(value) = serde_json::from_slice::<Value>(line) {
                if prefix
                    .is_none_or(|prefix| value["event"].as_str().unwrap_or("").starts_with(prefix))
                {
                    records.push(value);
                }
            }
            records.len() < limit
        });
        if records.len() == limit {
            break;
        }
    }
    records
}
