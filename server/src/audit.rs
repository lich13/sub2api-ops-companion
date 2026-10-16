use crate::compat::now;
use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
};

pub fn write(path: &Path, event: &str, payload: Value, read_only: bool) {
    if read_only {
        return;
    }
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.lock_exclusive()?;
        crate::store::private_permissions(&file)?;
        let mut bytes = serde_json::to_vec(&json!({"ts":now(),"event":event,"payload":payload}))?;
        bytes.push(b'\n');
        file.write_all(&bytes)
    })();
    let _ = result; // Audit failure must not undo an already confirmed operation.
}
pub fn read(path: &Path, limit: usize, prefix: Option<&str>) -> Vec<Value> {
    let Ok(file) = fs::File::open(path) else {
        return vec![];
    };
    let mut records = std::collections::VecDeque::new();
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
        records.push_back(value);
        if records.len() > limit {
            records.pop_front();
        }
    }
    records.into_iter().rev().collect()
}
