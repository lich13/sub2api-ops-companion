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

    println!("cargo:rerun-if-changed=resources/sub2api-config.json");
    let config_raw =
        fs::read("resources/sub2api-config.json").expect("sub2api-config.json resource");
    let catalog: Value =
        serde_json::from_slice(&config_raw).expect("valid sub2api-config.json catalogue");
    let compact_catalog = serde_json::to_vec(&catalog).expect("compact sub2api-config.json");
    fs::write(output.join("sub2api-config.compact.json"), compact_catalog)
        .expect("sub2api-config.compact.json output");

    let fields = catalog["fields"]
        .as_array()
        .expect("sub2api-config.json fields");
    let mut editable_paths: Vec<String> = fields
        .iter()
        .filter(|f| f["editable"] == Value::Bool(true))
        .filter_map(|f| f["path"].as_str().map(str::to_owned))
        .collect();
    editable_paths.sort();
    editable_paths.dedup();
    let mut generated = format!(
        "const CATALOG_FIELD_COUNT: usize = {};\nstatic EDITABLE_CONFIG_PATHS: &[&str] = &[\n",
        fields.len()
    );
    for path in &editable_paths {
        generated.push_str(&format!("    {path:?},\n"));
    }
    generated.push_str("];\n");
    fs::write(output.join("sub2api-config-paths.rs"), generated)
        .expect("sub2api-config-paths.rs output");
}
