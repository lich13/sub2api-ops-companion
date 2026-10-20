use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use sub2ops_server::{
    compat, model_catalog, model_config, model_rules,
    oauth_state::{self, OAuthState, Update},
    quota, store, usage,
};

const VERSION_VECTORS: &str = include_str!("../../tests/contracts/version-vectors.json");
const FOUNDATION_FIXTURES: &str = include_str!("../../tests/contracts/foundation-fixtures.json");

fn fixture() -> Value {
    serde_json::from_str(FOUNDATION_FIXTURES)
        .expect("generated foundation fixtures must be valid JSON")
}

fn vectors() -> Value {
    serde_json::from_str(VERSION_VECTORS).expect("version-vector contract must be valid JSON")
}

fn python_value(value: &Value) -> Value {
    match value {
        Value::Object(object)
            if object.get("$type").and_then(Value::as_str) == Some("datetime") =>
        {
            object
                .get("python_str")
                .cloned()
                .expect("datetime vector needs python_str")
        }
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), python_value(value)))
                .collect::<Map<String, Value>>(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(python_value).collect()),
        value => value.clone(),
    }
}

fn datetime(value: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value.as_str().expect("fixture timestamp must be a string"))
        .expect("fixture timestamp must include a UTC offset")
        .with_timezone(&Utc)
}

fn assert_hash(actual: String, expected: &Value, context: &str) {
    assert_eq!(Some(actual.as_str()), expected.as_str(), "{context}");
}

#[test]
fn all_version_vectors_match_the_pinned_python_contract() {
    let document = vectors();
    let mut tested = BTreeSet::new();
    for item in document["vectors"].as_array().expect("vector array") {
        let id = item["id"].as_str().expect("vector id");
        assert!(tested.insert(id.to_owned()), "duplicate vector id {id}");
        match id {
            "account_template_version_from_operation_version" => {
                let input = python_value(&item["input"]);
                let actual = compat::operation_versions(&input, false);
                assert_hash(
                    actual["account_template"].as_str().unwrap().to_owned(),
                    &item["expected_version"],
                    id,
                );
            }
            "account_versions_parent_child_identity" => {
                let mappings = [
                    (
                        "child_direct_parent_id",
                        "child_direct",
                        &["account_template", "model_config", "priority"][..],
                    ),
                    (
                        "child_extra_parent_id",
                        "child_extra",
                        &["account_template", "model_config", "priority"][..],
                    ),
                    ("parent", "parent", &["account_template", "priority"][..]),
                ];
                for (case_name, expected_prefix, fields) in mappings {
                    let input = python_value(&item["cases"][case_name]);
                    let actual = compat::operation_versions(&input, false);
                    for field in fields {
                        let expected_key = format!("{expected_prefix}_{field}_version");
                        assert_hash(
                            actual[*field].as_str().unwrap().to_owned(),
                            &item["expected"][expected_key],
                            &format!("{id}/{case_name}/{field}"),
                        );
                    }
                }
            }
            "config_revision_unicode_null_number_datetime" => {
                let input = python_value(&item["input"]);
                assert_hash(
                    compat::config_revision(&input),
                    &item["expected_revision"],
                    id,
                );
            }
            "desktop_account_dto_version_group_order_invariance" => {
                for case_name in ["group_ids_3_9", "group_ids_9_3"] {
                    let input = python_value(&item["cases"][case_name]);
                    assert_hash(
                        compat::account_version(&input),
                        &item["expected_versions"][case_name],
                        &format!("{id}/{case_name}"),
                    );
                }
            }
            "model_catalog_account_cache_identity" => {
                let input = python_value(&item["input"]);
                assert_hash(
                    model_catalog::source_identity(&input),
                    &item["expected_revision"],
                    id,
                );
            }
            "model_catalog_accounts_routes_revision" => {
                let input = python_value(&item["input"]);
                let accounts = input[0].as_array().expect("accounts array");
                let routes = input[1].as_array().expect("routes array");
                assert_hash(
                    model_catalog::routes_revision(accounts, routes),
                    &item["expected_revision"],
                    id,
                );
            }
            "model_catalog_final_response_etag" => {
                let input = python_value(&item["input"]);
                let actual = model_catalog::response_etag(&input);
                assert_hash(actual.clone(), &item["expected_etag_header"], id);
                assert_hash(
                    actual.trim_matches('"').to_owned(),
                    &item["expected_etag_value"],
                    &format!("{id}/unquoted"),
                );
            }
            "model_catalog_group_version" => {
                let input = python_value(&item["input"]);
                assert_hash(
                    model_config::group_version(&input),
                    &item["expected_version"],
                    id,
                );
            }
            "model_catalog_native_body_revision" => {
                let input = python_value(&item["input"]);
                assert_hash(
                    model_rules::revision(&input),
                    &item["expected_revision"],
                    id,
                );
            }
            "model_rules_revision_compact_unicode_null_number" => {
                let input = python_value(&item["input"]);
                assert_hash(
                    model_rules::revision(&input),
                    &item["expected_revision"],
                    id,
                );
            }
            "operation_digest_unicode_null_number_datetime" => {
                let input = python_value(&item["input"]);
                assert_hash(compat::digest(&input), &item["expected_sha256"], id);
            }
            "operation_versions_full_synthetic_account" => {
                let input = python_value(&item["input"]);
                assert_eq!(
                    compat::operation_versions(&input, false),
                    item["expected_versions"],
                    "{id}"
                );
            }
            "operation_versions_group_order_invariance" => {
                let mut actual_versions = BTreeMap::new();
                for case_name in ["a", "b"] {
                    let input = python_value(&item["cases"][case_name]);
                    let actual = compat::operation_versions(&input, false);
                    assert_eq!(
                        actual, item["expected_versions"][case_name],
                        "{id}/{case_name}"
                    );
                    actual_versions.insert(case_name, actual);
                }
                assert_eq!(
                    actual_versions["a"], actual_versions["b"],
                    "{id}/order-invariance"
                );
            }
            "operation_versions_model_config_sensitivity" => {
                for (case_name, expected) in
                    item["expected_model_config_versions"].as_object().unwrap()
                {
                    let input = python_value(&item["cases"][case_name]);
                    let actual = compat::operation_versions(&input, false);
                    assert_hash(
                        actual["model_config"].as_str().unwrap().to_owned(),
                        expected,
                        &format!("{id}/{case_name}"),
                    );
                }
            }
            _ => panic!("unmapped version vector {id}; it must not be silently skipped"),
        }
    }

    let expected: BTreeSet<String> = [
        "account_template_version_from_operation_version",
        "account_versions_parent_child_identity",
        "config_revision_unicode_null_number_datetime",
        "desktop_account_dto_version_group_order_invariance",
        "model_catalog_account_cache_identity",
        "model_catalog_accounts_routes_revision",
        "model_catalog_final_response_etag",
        "model_catalog_group_version",
        "model_catalog_native_body_revision",
        "model_rules_revision_compact_unicode_null_number",
        "operation_digest_unicode_null_number_datetime",
        "operation_versions_full_synthetic_account",
        "operation_versions_group_order_invariance",
        "operation_versions_model_config_sensitivity",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        tested, expected,
        "all 14 declared Python/Rust vectors must be mapped"
    );
}

#[test]
fn quota_windows_and_read_only_usage_projections_match_python() {
    let document = fixture();
    let usage_fixture = &document["usage"];
    let at = datetime(&document["at"]);
    let oauth_row = &usage_fixture["oauth_row"];
    assert_eq!(
        Value::Array(quota::usage_windows(oauth_row, at, &Value::Null)),
        usage_fixture["oauth_expected_windows"],
        "OAuth 5h/7d windows, reset time, freshness, and passive source",
    );
    assert_eq!(
        usage::project(oauth_row, at, &Value::Null),
        usage_fixture["oauth_expected_projection"],
        "OAuth usage cells and expiring reset-credit projection",
    );
    assert_eq!(
        usage::project(&usage_fixture["apikey_row"], at, &Value::Null),
        usage_fixture["apikey_expected_projection"],
        "configured API-key windows, absent-used default, and UTC+8 day boundary",
    );
}

#[test]
fn account_quality_removal_contract_is_explicit() {
    let document = fixture();
    let contract = &document["account_quality_removed"];
    assert_eq!(contract["status"], "removed");
    assert_eq!(
        contract["route"],
        "GET /api/desktop/v1/accounts/{id}/quality"
    );
    assert_eq!(contract["expected_status"], 404);
    assert_eq!(
        contract["replacements"],
        json!(["proxy quality-check", "capacity slow_ttft evidence"])
    );
}

#[test]
fn credit_state_validation_errors_match_python() {
    let state_fixture = &fixture()["credit_state"];
    assert!(oauth_state::validate_credit(&state_fixture["valid_state"]).is_ok());
    assert_eq!(state_fixture["valid_expected_error"], Value::Null);
    for case in state_fixture["invalid_states"].as_array().unwrap() {
        let error =
            oauth_state::validate_credit(&case["state"]).expect_err("invalid state must fail");
        assert_eq!(error.status.as_u16(), 503, "state case {}", case["name"]);
        assert_eq!(
            error.detail.as_str(),
            case["expected_error"].as_str(),
            "state case {}",
            case["name"]
        );
    }
}

#[test]
fn manual_control_generation_rejects_stale_scheduler_results() {
    let manual = &fixture()["credit_state"]["manual_generation"];
    let temporary = tempfile::tempdir().unwrap();
    let state = OAuthState::new(temporary.path().join("oauth-state.json"), false);
    state
        .transaction(|value| {
            value["pending_events"]["keep-active"] =
                json!({"account_id":73,"stage":"active_usage"});
            value["pending_events"]["drop-ready"] = json!({"account_id":73,"stage":"ready"});
            value["pending_events"]["keep-other"] = json!({"account_id":74,"stage":"ready"});
            Ok(())
        })
        .unwrap();

    let at = datetime(&fixture()["at"]);
    let generation = state
        .manual_control(&manual["row"], false, at, "manual")
        .unwrap();
    assert_eq!(generation, manual["expected_generation"].as_i64().unwrap());

    let mut update = Update::default();
    update
        .results
        .insert(73, json!({"template_type":"oauth","stale_result":true}));
    update.scheduler.insert(
        73,
        json!({
            "manual_control":{"generation":99,"enabled":true,"at":fixture()["at"]},
            "stale_scheduler_marker":true
        }),
    );
    update.pending.insert(
        "stale-ready".to_owned(),
        json!({"account_id":73,"stage":"ready"}),
    );
    update.generations = Some(BTreeMap::from([(73, 0)]));
    state.commit(update).unwrap();

    let snapshot = state.snapshot().unwrap();
    assert_eq!(
        snapshot["scheduler"]["73"]["manual_control"], manual["expected_control"],
        "stale scheduler data cannot replace a newer manual generation",
    );
    assert_eq!(
        snapshot["scheduler"]["73"]
            .get("stale_scheduler_marker")
            .cloned()
            .unwrap_or(Value::Null),
        manual["expected_stale_marker"],
    );
    assert_eq!(
        snapshot["oauth_results"]
            .get("73")
            .cloned()
            .unwrap_or(Value::Null),
        manual["expected_oauth_result"],
        "stale OAuth result is discarded with the scheduler update",
    );
    let pending_stages: Map<String, Value> = snapshot["pending_events"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, event)| (key.clone(), event["stage"].clone()))
        .collect();
    assert_eq!(
        Value::Object(pending_stages),
        manual["expected_pending_stages"]
    );
}

#[test]
fn state_transactions_preserve_peer_updates_and_detect_lost_files() {
    let persistence = &fixture()["persistence"];
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("shared-state.json");
    let first = OAuthState::new(path.clone(), false);
    let second = OAuthState::new(path.clone(), false);

    first
        .transaction(|value| {
            value["scheduler"]["73"]["first"] = json!(true);
            Ok(())
        })
        .unwrap();
    second
        .transaction(|value| {
            value["scheduler"]["73"]["second"] = json!(true);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        first.snapshot().unwrap()["scheduler"]["73"],
        persistence["lost_update_expected_scheduler"],
        "each transaction rereads the latest state under the shared lock",
    );

    std::fs::remove_file(&path).unwrap();
    let error = first
        .snapshot()
        .expect_err("previously persisted state cannot disappear silently");
    assert_eq!(error.status.as_u16(), 503);
    assert_eq!(error.detail.as_str(), Some("控制状态文件丢失"));
}

#[test]
fn failed_atomic_save_preserves_target_and_cleans_temporary_file() {
    let expected = &fixture()["persistence"]["atomic_write_failure"];
    assert!(expected["target_is_directory"].as_bool().unwrap());
    assert_eq!(expected["temporary_artifacts_after"], json!([]));

    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("blocked-target");
    std::fs::create_dir(&target).unwrap();
    let marker = target.join("marker.txt");
    std::fs::write(&marker, expected["marker_after"].as_str().unwrap()).unwrap();

    let error = store::write_json(&target, &json!({"synthetic":true}))
        .expect_err("renaming a state file over a directory must fail");
    assert_eq!(error.status.as_u16(), 500);
    assert!(target.is_dir());
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        expected["marker_after"].as_str().unwrap()
    );
    let temporary_artifacts: Vec<String> = std::fs::read_dir(temporary.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(".sub2ops-state-"))
        .collect();
    assert!(
        temporary_artifacts.is_empty(),
        "failed save left temporary files: {temporary_artifacts:?}"
    );
}

#[test]
fn readonly_state_reads_defaults_without_creating_disk_state() {
    let expected = &fixture()["persistence"]["read_only_absent_state_expectation"];
    assert!(!expected["initial_path_exists"].as_bool().unwrap());
    assert!(expected["snapshot_must_not_create_parent"]
        .as_bool()
        .unwrap());

    let temporary = tempfile::tempdir().unwrap();
    let parent = temporary.path().join("not-created");
    let path = parent.join("oauth-state.json");
    let state = OAuthState::new(path, true);
    assert!(state.snapshot().unwrap().is_object());
    assert!(
        !parent.exists(),
        "readonly snapshot must not create parent directories"
    );

    let error = state
        .transaction(|_| Ok(()))
        .expect_err("readonly writes must be rejected");
    assert_eq!(error.status.as_u16(), 409);
    assert_eq!(error.detail["code"], expected["transaction_error_code"]);
    assert!(
        !parent.exists(),
        "readonly transaction must not create a lock or state file"
    );
}
