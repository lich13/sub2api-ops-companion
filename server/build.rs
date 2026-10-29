use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from("resources/lm-detector");
    println!("cargo:rerun-if-changed=resources/lm-detector/manifest.json");
    let manifest: Value =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).expect("LM manifest"))
            .expect("valid LM manifest");
    let mut bank = Value::Null;
    for name in [
        "unified_bank.json",
        "shared_detector.bin",
        "tokenizer_bank.json",
    ] {
        println!("cargo:rerun-if-changed=resources/lm-detector/{name}");
        let bytes = fs::read(root.join(name)).expect("LM source data");
        let expected = &manifest["files"][format!("data/{name}")];
        assert_eq!(
            expected["bytes"].as_u64(),
            Some(bytes.len() as u64),
            "{name} size"
        );
        assert_eq!(
            expected["sha256"].as_str(),
            Some(format!("{:x}", Sha256::digest(&bytes)).as_str()),
            "{name} checksum"
        );
        if name == "unified_bank.json" {
            bank = serde_json::from_slice(&bytes).expect("LM reference bank");
        }
    }
    // These are exactly the fields consumed by the offline SDK's Bank/BankModel.
    // Keep the original file and its hashes as the public source identity.
    let models = bank["models"]
        .as_array()
        .expect("LM models")
        .iter()
        .map(|model| {
            let mut compact = serde_json::Map::new();
            for key in [
                "id",
                "display_name",
                "family",
                "family_name",
                "response_count",
            ] {
                compact.insert(key.into(), model.get(key).expect("LM model field").clone());
            }
            Value::Object(compact)
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&json!({"built_at":bank["built_at"],"models":models}))
        .expect("LM metadata");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    fs::write(
        output.join("lm-detector-bank.sha256"),
        format!("{:x}", Sha256::digest(&bytes)),
    )
    .expect("LM metadata checksum");
    fs::write(output.join("lm-detector-bank.json"), bytes).expect("LM metadata output");
}
