use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::Path};
use sub2ops_server::{
    account_templates::{self, Templates},
    compat, key_fallback, model_catalog,
    model_config::ModelConfig,
    model_rules,
    settings::Settings,
};

const FIXTURES: &str = include_str!("../../tests/contracts/config-catalog-fixtures.json");
const REFERENCE_SHA: &str = "6be7c291ed38dcc337cb88f693639040a3463899";

fn fixtures() -> Value {
    serde_json::from_str(FIXTURES).expect("pinned config/catalog fixtures must be valid JSON")
}

fn write_json(path: &Path, value: &Value) {
    fs::write(
        path,
        serde_json::to_vec(value).expect("fixture JSON should serialize"),
    )
    .expect("write synthetic fixture");
}

fn replace_id(value: &mut Value, old: &str, new: &str) {
    match value {
        Value::String(current) if current == old => *current = new.to_owned(),
        Value::Array(items) => {
            for item in items {
                replace_id(item, old, new);
            }
        }
        Value::Object(items) => {
            if let Some(item) = items.remove(old) {
                items.insert(new.to_owned(), item);
            }
            for item in items.values_mut() {
                replace_id(item, old, new);
            }
        }
        _ => {}
    }
}

fn canonical_template_view(
    templates: &Templates,
    mut view: Value,
    actual_id: &str,
    fixture_id: &str,
) -> Value {
    let mut data = templates.data().expect("template state should normalize");
    replace_id(&mut data, actual_id, fixture_id);
    replace_id(&mut view, actual_id, fixture_id);
    view["version"] = compat::digest(&data).into();
    view
}

#[test]
fn account_template_schema_upgrade_matches_pinned_python_and_preserves_read_only_or_corrupt_state()
{
    let fixture = fixtures();
    assert_eq!(fixture["reference_sha"].as_str(), Some(REFERENCE_SHA));
    let migration = &fixture["templates"]["migration"];
    let root = tempfile::tempdir().expect("temporary test root");

    let writable_path = root.path().join("templates-writable.json");
    write_json(&writable_path, &migration["input"]);
    let writable = Templates::new(writable_path, false);
    assert_eq!(
        writable
            .view(None)
            .expect("legacy template state should migrate"),
        migration["expected_view"]
    );
    assert_eq!(
        writable
            .store
            .read()
            .expect("migrated template state should be readable"),
        migration["expected_persisted"]
    );

    let readonly_path = root.path().join("templates-read-only.json");
    write_json(&readonly_path, &migration["input"]);
    let before_readonly = fs::read(&readonly_path).expect("read-only fixture bytes");
    let readonly = Templates::new(readonly_path.clone(), true);
    assert_eq!(
        readonly
            .view(None)
            .expect("read-only legacy state should normalize in memory"),
        migration["readonly_view"]
    );
    assert_eq!(
        fs::read(&readonly_path).expect("read-only state should remain present"),
        before_readonly,
        "read-only normalization must not persist a migration"
    );

    let corrupt_path = root.path().join("templates-corrupt.json");
    fs::write(&corrupt_path, migration["corrupt_json"].as_str().unwrap())
        .expect("write synthetic corrupt template state");
    let before_corrupt = fs::read(&corrupt_path).expect("corrupt fixture bytes");
    let corrupt = Templates::new(corrupt_path.clone(), false);
    assert!(
        corrupt.view(None).is_err(),
        "corrupt template state must be rejected"
    );
    assert_eq!(
        fs::read(&corrupt_path).expect("corrupt state should remain present"),
        before_corrupt,
        "rejected template state must not be overwritten"
    );
}

#[test]
fn account_template_custom_crud_mapping_and_request_validation_match_pinned_python() {
    let fixture = fixtures();
    let contract = &fixture["templates"]["crud"];
    let root = tempfile::tempdir().expect("temporary test root");
    let path = root.path().join("account-templates.json");
    let templates = Templates::new(path, false);

    let initial = templates.view(None).expect("default templates should load");
    assert_eq!(initial, contract["initial_view"]);

    let create = json!({
        "expected_version": initial["version"],
        "name": contract["create"]["name"],
        "whitelist": contract["create"]["whitelist"],
        "mappings": contract["create"]["mappings"],
    });
    let created = templates
        .edit(None, &create, false)
        .expect("custom template create should succeed");
    let actual_id = created["custom_templates"][0]["id"]
        .as_str()
        .expect("new custom template has an ID")
        .to_owned();
    assert!(actual_id.starts_with("custom-") && actual_id.len() == 31);
    assert!(actual_id[7..]
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    assert_eq!(
        canonical_template_view(
            &templates,
            created.clone(),
            &actual_id,
            contract["custom_id"].as_str().unwrap()
        ),
        contract["created_view"]
    );
    let desired = templates
        .desired(&json!({
            "template_id": actual_id,
            "template_version": created["template_versions"][actual_id.as_str()],
        }))
        .expect("custom mapping should resolve from its profile version");
    assert_eq!(desired, contract["desired_mapping"]);

    let update = json!({
        "expected_version": created["version"],
        "name": contract["update"]["name"],
        "whitelist": contract["update"]["whitelist"],
        "mappings": contract["update"]["mappings"],
    });
    let updated = templates
        .edit(Some(&actual_id), &update, false)
        .expect("custom template update should succeed");
    assert_eq!(
        canonical_template_view(
            &templates,
            updated.clone(),
            &actual_id,
            contract["custom_id"].as_str().unwrap()
        ),
        contract["updated_view"]
    );

    let delete = json!({"expected_version": updated["version"]});
    let deleted = templates
        .edit(Some(&actual_id), &delete, true)
        .expect("custom template delete should succeed");
    assert_eq!(
        canonical_template_view(
            &templates,
            deleted,
            &actual_id,
            contract["custom_id"].as_str().unwrap()
        ),
        contract["deleted_view"]
    );

    let state_before_stale = templates
        .store
        .read()
        .expect("template state before stale write");
    let stale = json!({
        "expected_version": "0".repeat(64),
        "name": "fixture stale edit",
        "whitelist": [],
        "mappings": [],
    });
    assert!(templates.edit(Some("full"), &stale, false).is_err());
    assert_eq!(
        templates
            .store
            .read()
            .expect("template state after stale write"),
        state_before_stale,
        "stale template request must not partially update state"
    );

    for case in contract["invalid_profiles"].as_array().unwrap() {
        let profile = account_templates::profile(&case["input"]);
        assert_eq!(
            profile.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "template request case {}",
            case["id"]
        );
        if let Some(expected) = case.get("combined") {
            assert_eq!(
                account_templates::combine(&profile.unwrap()).expect("valid profile combines"),
                *expected,
                "template mapping case {}",
                case["id"]
            );
        }
    }
}

#[test]
fn model_catalog_reasoning_import_model_rules_revision_and_priority_match_pinned_python() {
    let fixture = fixtures();
    assert_eq!(fixture["reference_sha"].as_str(), Some(REFERENCE_SHA));
    let model = &fixture["model"];

    let revision = &model["revision"];
    assert_eq!(
        model_rules::revision(&revision["input"]),
        revision["expected"].as_str().unwrap(),
        "model config revision must match legacy JSON canonicalization"
    );

    let transform = &model["transform"];
    assert_eq!(
        model_rules::transform(
            &transform["body"],
            &transform["overrides"],
            &transform["allowlist"],
            &transform["aliases"],
        )
        .expect("valid catalog transform"),
        transform["expected"],
        "saved override values should take precedence on the matching model"
    );

    for case in model["field_validation"].as_array().unwrap() {
        assert_eq!(
            model_rules::validate_fields(&case["fields"], &case["base"]).is_ok(),
            case["accepted"].as_bool().unwrap(),
            "model field request case {}",
            case["id"]
        );
    }
    for case in model["override_validation"].as_array().unwrap() {
        assert_eq!(
            model_rules::validate_overrides(&case["input"], &Value::Null).is_ok(),
            case["accepted"].as_bool().unwrap(),
            "model override request case {}",
            case["id"]
        );
    }
    for case in model["reasoning_import"].as_array().unwrap() {
        assert_eq!(
            model_catalog::import_fields(&case["input"]),
            case["expected"],
            "model directory reasoning parse case {}",
            case["id"]
        );
    }
}

#[test]
fn model_config_revision_read_only_and_corrupt_state_match_pinned_python() {
    let fixture = fixtures();
    let contract = &fixture["model"]["model_config"];
    let root = tempfile::tempdir().expect("temporary test root");
    let raw = serde_json::to_vec(&contract["state"]).expect("model config fixture serializes");

    let writable_path = root.path().join("model-config-writable.json");
    fs::write(&writable_path, &raw).expect("write model config fixture");
    let writable = ModelConfig::new(writable_path, false);
    assert_eq!(
        writable.read().expect("valid model config should read"),
        contract["state"]
    );
    assert_eq!(
        writable
            .group(contract["group_id"].as_i64().unwrap())
            .expect("model group should load"),
        contract["expected_group"],
        "group revision must match the pinned Python store"
    );

    let readonly_path = root.path().join("model-config-read-only.json");
    fs::write(&readonly_path, &raw).expect("write read-only model config fixture");
    let before_readonly = fs::read(&readonly_path).expect("read-only model config bytes");
    let readonly = ModelConfig::new(readonly_path.clone(), true);
    assert_eq!(
        readonly.read().expect("read-only model config should read"),
        contract["state"]
    );
    let current_group = readonly
        .group(contract["group_id"].as_i64().unwrap())
        .expect("read-only group should load");
    assert!(
        readonly
            .save(
                contract["group_id"].as_i64().unwrap(),
                &json!({"fixture-model":{"display_name":"fixture read-only attempt"}}),
                current_group["revision"].as_str().unwrap(),
            )
            .is_err(),
        "read-only model config must reject a write"
    );
    assert_eq!(
        fs::read(&readonly_path).expect("read-only model config should remain present"),
        before_readonly,
        "read-only model config must not be overwritten"
    );
    assert_eq!(contract["read_unchanged"].as_bool(), Some(true));

    let stale_path = root.path().join("model-config-stale.json");
    fs::write(&stale_path, &raw).expect("write stale-write fixture");
    let stale_before = fs::read(&stale_path).expect("stale-write fixture bytes");
    let stale_store = ModelConfig::new(stale_path.clone(), false);
    assert!(
        stale_store
            .save(
                contract["group_id"].as_i64().unwrap(),
                &json!({"fixture-model":{"display_name":"fixture stale attempt"}}),
                &"0".repeat(64),
            )
            .is_err(),
        "stale model config revision must be rejected"
    );
    assert_eq!(fs::read(&stale_path).unwrap(), stale_before);

    let corrupt_path = root.path().join("model-config-corrupt.json");
    let corrupt_bytes = contract["corrupt_json"].as_str().unwrap().as_bytes();
    fs::write(&corrupt_path, corrupt_bytes).expect("write invalid-schema model config");
    let before_corrupt = fs::read(&corrupt_path).expect("invalid model config bytes");
    let corrupt = ModelConfig::new(corrupt_path.clone(), false);
    assert!(
        corrupt.read().is_err(),
        "invalid model config schema must be rejected"
    );
    assert_eq!(fs::read(&corrupt_path).unwrap(), before_corrupt);
    assert_eq!(contract["corrupt_rejected"].as_bool(), Some(true));
    assert_eq!(contract["corrupt_unchanged"].as_bool(), Some(true));
}

#[test]
fn settings_priority_and_key_fallback_legacy_parsing_match_pinned_python() {
    let fixture = fixtures();
    let root = tempfile::tempdir().expect("temporary test root");
    let priority = &fixture["settings_priority"];
    let oauth_path = root.path().join("oauth-config.json");
    let bark_path = root.path().join("bark-config.json");
    write_json(&oauth_path, &priority["oauth_config"]);
    write_json(&bark_path, &priority["bark_config"]);
    let oauth_before = fs::read(&oauth_path).expect("oauth config bytes");
    let bark_before = fs::read(&bark_path).expect("bark config bytes");
    let mut env: BTreeMap<String, String> = priority["env"]
        .as_object()
        .expect("synthetic environment is an object")
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap().to_owned()))
        .collect();
    env.insert("OAUTH_CONFIG_PATH".into(), oauth_path.display().to_string());
    env.insert("BARK_CONFIG_PATH".into(), bark_path.display().to_string());
    let settings = Settings::from_env(&env, false).expect("synthetic settings should parse");
    assert_eq!(
        settings.oauth["model_detection_model_id"],
        priority["rust_additions_0_1_48"]["model_detection_model_id"],
        "the newly approved detection model key should use its default"
    );
    let mut legacy_projection = settings.oauth.clone();
    legacy_projection
        .as_object_mut()
        .unwrap()
        .remove("model_detection_model_id");
    assert_eq!(
        legacy_projection,
        priority["expected"],
        "persisted config should override environment, while absent fields use environment or defaults; the added detection-model key is an approved difference"
    );
    assert_eq!(fs::read(&oauth_path).unwrap(), oauth_before);
    assert_eq!(fs::read(&bark_path).unwrap(), bark_before);

    let fallback = &fixture["key_fallback"];
    let parsed = key_fallback::parse_config(&fallback["legacy_input"]);
    assert!(parsed.valid);
    assert_eq!(
        parsed.enabled,
        fallback["expected"]["enabled"].as_bool().unwrap()
    );
    assert_eq!(
        parsed.openai,
        fallback["expected"]["openai_enabled"].as_bool().unwrap()
    );
    assert_eq!(
        parsed.grok,
        fallback["expected"]["grok_enabled"].as_bool().unwrap()
    );
    assert_eq!(
        json!(parsed.ids),
        fallback["expected"]["managed_account_ids"]
    );
    assert_eq!(
        json!(parsed.coexist),
        fallback["expected"]["coexist_account_ids"]
    );
    assert_eq!(
        parsed.version,
        fallback["expected"]["config_version"].as_i64().unwrap()
    );
    assert_eq!(
        parsed.updated_at,
        fallback["expected"]["updated_at"].as_str().unwrap()
    );
    assert_eq!(
        parsed.updated_by,
        fallback["expected"]["updated_by"].as_str().unwrap()
    );
    assert_eq!(parsed.panel(), fallback["expected"]["panel"]);

    let invalid = key_fallback::parse_config(&fallback["invalid_input"]);
    assert!(!invalid.valid);
    assert_eq!(invalid.panel(), fallback["invalid_expected"]["panel"]);
    assert_eq!(fallback["invalid_expected"]["valid"].as_bool(), Some(false));

    for case in fallback["managed_id_cases"].as_array().unwrap() {
        let result = key_fallback::managed_ids(&case["input"]);
        assert_eq!(
            result.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "managed account ID case {}",
            case["id"]
        );
        if let Some(expected) = case.get("expected") {
            assert_eq!(
                json!(result.unwrap()),
                *expected,
                "managed account ID case {}",
                case["id"]
            );
        }
    }
}

#[test]
fn neutral_settings_ignore_retired_sources_keep_defaults_and_prefer_explicit_json() {
    let root = tempfile::tempdir().unwrap();
    let oauth = root.path().join("oauth.json");
    let legacy = root.path().join("retired.json");
    let connection = root.path().join("connection.json");
    fs::write(
        &legacy,
        r#"{"oauth_daily_test_time":"05:15","bot_token":"fixture-private-retired"}"#,
    )
    .unwrap();
    fs::write(&oauth, r#"{"oauth_early_probe_interval_seconds":15}"#).unwrap();
    let mut env = BTreeMap::from([
        (
            "DATABASE_URL".into(),
            "postgresql://fixture:fixture@127.0.0.1:1/fixture".into(),
        ),
        ("OAUTH_CONFIG_PATH".into(), oauth.display().to_string()),
        (
            "BARK_CONFIG_PATH".into(),
            root.path().join("bark.json").display().to_string(),
        ),
        (
            "SUB2API_CONFIG_PATH".into(),
            connection.display().to_string(),
        ),
        ("TELEGRAM_CONFIG_PATH".into(), legacy.display().to_string()),
        ("TELEGRAM_OAUTH_DAILY_TEST_TIME".into(), "05:15".into()),
        (
            "TELEGRAM_BOT_TOKEN".into(),
            "fixture-private-retired".into(),
        ),
    ]);
    let original = fs::read(&legacy).unwrap();
    let defaults = Settings::from_env(&env, false).unwrap();
    assert_eq!(
        defaults.oauth["oauth_recovery_test_model_id"],
        "gpt-5.6-luna"
    );
    assert_eq!(defaults.oauth["oauth_daily_test_enabled"], true);
    assert_eq!(defaults.oauth["oauth_daily_test_time"], "05:00");
    for name in [
        "oauth_regular_refresh_interval_seconds",
        "oauth_7d_probe_interval_seconds",
        "oauth_early_probe_interval_seconds",
        "guard_enabled",
        "telegram_bot_token",
    ] {
        assert!(defaults.oauth.get(name).is_none());
    }
    env.insert("OAUTH_DAILY_TEST_TIME".into(), "08:12".into());
    assert_eq!(
        Settings::from_env(&env, false).unwrap().oauth["oauth_daily_test_time"],
        "08:12"
    );
    assert_eq!(fs::read(&legacy).unwrap(), original);
    env.insert("OAUTH_DAILY_TEST_ENABLED".into(), "true".into());
    fs::write(&oauth, r#"{"oauth_daily_test_enabled":false}"#).unwrap();
    assert_eq!(
        Settings::from_env(&env, false).unwrap().oauth["oauth_daily_test_enabled"],
        false
    );
    fs::write(&connection,r#"{"base_url":"https://public.example.invalid/","verify_base_url":"http://127.0.0.1:8080/"}"#).unwrap();
    env.insert(
        "SUB2API_BASE_URL".into(),
        "https://ignored.example.invalid".into(),
    );
    let settings = Settings::from_env(&env, false).unwrap();
    assert_eq!(
        settings.connection_config().unwrap(),
        json!({"base_url":"https://public.example.invalid","verify_base_url":"http://127.0.0.1:8080"})
    );
    assert_eq!(
        sub2ops_server::upstream::Upstream::new(std::sync::Arc::new(settings))
            .unwrap()
            .active_url(),
        "http://127.0.0.1:8080"
    );
}
