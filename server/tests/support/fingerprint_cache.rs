use serde_json::json;
use std::path::Path;
use sub2ops_server::store::{valid_object, Store};

pub fn seed(root: &Path) {
    let raw = include_str!("../../resources/modeltrace/unified_bank.json");
    let (_, mut version) = sub2ops_server::modeltrace::bundled().unwrap();
    let manifest = sub2ops_server::modeltrace::manifest();
    for key in ["core", "challenge"] {
        version[format!("{key}_blob")] = manifest["files"][key]["gitBlob"].clone();
    }
    let now = chrono::Utc::now().timestamp();
    for (file, data) in [
        (
            "modeltrace-bank-state.json",
            json!({"version":1,"active":{"raw_bank":raw,"version":version},
          "source":"bundled","checked_at":now,"synced_at":now,"cooldown_until":null,"last_error":null,"result":"up-to-date"}),
        ),
        (
            "lm-detector/state.json",
            json!({"revision":sub2ops_server::lm_detector::manifest()["revision"],"checked_at":now}),
        ),
    ] {
        Store::new(root.join(file), json!({}), valid_object)
            .transaction(|state| {
                *state = data;
                Ok(())
            })
            .unwrap();
    }
}
