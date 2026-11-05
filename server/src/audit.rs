use crate::compat::now;
use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 20 * 1024 * 1024;

fn previous(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".1");
    name.into()
}

// Keep the current inode: writers may already have opened it before waiting
// for its lock. Copy only complete records from the bounded tail of old logs.
fn rotate(file: &mut fs::File, path: &Path, length: u64) -> std::io::Result<()> {
    let mut end = length;
    let mut buffer = [0; 8192];
    while end > 0 {
        let start = end.saturating_sub(buffer.len() as u64);
        let count = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..count])?;
        if let Some(index) = buffer[..count].iter().rposition(|&b| b == b'\n') {
            end = start + index as u64 + 1;
            break;
        }
        end = start;
    }
    let start = end.saturating_sub(MAX_BYTES);
    file.seek(SeekFrom::Start(start.saturating_sub(1)))?;
    let mut reader = BufReader::new(file.take(end - start.saturating_sub(1)));
    if start > 0 {
        reader.skip_until(b'\n')?;
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut old = options.open(previous(path))?;
    crate::store::private_permissions(&old)?;
    std::io::copy(&mut reader, &mut old)?;
    reader.into_inner().into_inner().set_len(0)
}

pub fn write(path: &Path, event: &str, payload: Value, read_only: bool) {
    if read_only {
        return;
    }
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.lock_exclusive()?;
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
            rotate(&mut file, path, length)?;
        }
        file.write_all(&bytes)
    })();
    let _ = result; // Audit failure must not undo an already confirmed operation.
}
pub fn read(path: &Path, limit: usize, prefix: Option<&str>) -> Vec<Value> {
    let current = fs::File::open(path).ok();
    if current
        .as_ref()
        .is_some_and(|file| FileExt::lock_shared(file).is_err())
    {
        return vec![];
    }
    let mut records = std::collections::VecDeque::new();
    for file in [
        fs::File::open(previous(path)).ok(),
        current.as_ref().and_then(|file| file.try_clone().ok()),
    ]
    .into_iter()
    .flatten()
    {
        for line in BufReader::new(file)
            .lines()
            .map_while(std::result::Result::ok)
        {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if prefix.is_some_and(|p| !value["event"].as_str().unwrap_or("").starts_with(p)) {
                continue;
            }
            records.push_back(crate::security::redact_json(&value));
            if records.len() > limit {
                records.pop_front();
            }
        }
    }
    records.into_iter().rev().collect()
}
