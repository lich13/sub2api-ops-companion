use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sub2ops_server::{
    compat, credit_rules,
    error::Result,
    error_evidence, key_fallback, model_catalog, model_rules, model_target,
    oauth_state::{self, OAuthState},
    queries, quota, recovery_events, recovery_rules, reset_credit_observation,
};

fn time(value: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value.as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc)
}

fn execute(name: &str, args: &Value) -> Result<Value> {
    Ok(match name {
        "account_dto" => queries::account_dto(
            &args["row"],
            time(&args["now"]),
            &args["managed"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| id.as_i64().unwrap())
                .collect(),
            &args["quota_result"],
        ),
        "group_dtos" => json!(queries::group_dtos(
            args["rows"].as_array().unwrap(),
            &args["account_groups"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(id, groups)| (
                    id.parse::<i64>().unwrap(),
                    groups
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|id| id.as_i64().unwrap())
                        .collect(),
                ))
                .collect(),
        )),
        "error_dto" => queries::error_dto(&args["row"], args["detail"].as_bool().unwrap()),
        "safe_error_body" => json!(sub2ops_server::security::safe_error_body(&args["value"])),
        "error_evidence.match_message" => json!(error_evidence::match_message(&args["row"])),
        "error_evidence.is_local_throttle" => json!(error_evidence::local_throttle(&args["row"])),
        "parse_iso_datetime" => json!(compat::parse_datetime(&args["value"]).map(compat::datetime)),
        "oauth_reset_at" => json!(
            quota::reset_details(
                &args["explicit_reset_at"],
                &args["reset_after_seconds"],
                &args["updated_at"],
                compat::parse_datetime(&args["now"]),
            )
            .0
        ),
        "oauth_quota_from_usage_data" => {
            quota::from_usage_data(&args["data"], &args["account_row"], time(&args["now"]))
        }
        "oauth_windows_by_key" => json!(quota::windows_by_key(&args["raw_windows"])),
        "oauth_recovery_transition" => recovery_events::recovery_transition(
            &args["previous_summary"],
            &args["refreshed_summary"],
            time(&args["now"]),
        ),
        "_retry_at" => json!(recovery_rules::retry_at(
            time(&args["now"]),
            args["attempt_count"].as_i64().unwrap()
        )),
        "automatic_recovery_eligible" => json!(recovery_rules::eligible(
            &args["row"],
            &args["exhausted_window_keys"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
        )),
        "recovery_block_signature" => json!(oauth_state::block_signature(&args["row"])),
        "recovery_block_change_is_safe" => json!(recovery_rules::change_is_safe(
            &args["before"],
            &args["after"]
        )),
        "account_recovery_confirmed" => json!(recovery_rules::confirmed(&args["row"])),
        "receipt_diagnostics" => reset_credit_observation::diagnostics(
            args["status"]
                .as_u64()
                .map(|value| u16::try_from(value).unwrap()),
            &args["data"],
        ),
        "observation_from_usage" => reset_credit_observation::from_usage(
            &args["before"],
            &args["after"],
            &args["result"],
            time(&args["started"]),
            time(&args["completed"]),
        )
        .unwrap_or(Value::Null),
        "classify_oauth_account" => json!(key_fallback::oauth_state(
            &args["row"],
            &args["result"],
            time(&args["now"]),
            args["freshness_seconds"].as_i64().unwrap()
        )),
        "classify_grok_oauth_account" => {
            json!(key_fallback::grok_state(&args["row"], time(&args["now"])))
        }
        "desired_key_schedulable" => json!(key_fallback::desired(
            &args["states"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>()
        )),
        "latest_completed_oauth_result" => {
            queries::latest_completed_result(&args["result"], &args["scheduler_row"])
        }
        "oauth_quota_summary_from_result" => {
            quota::summary_from_result(&args["account_row"], &args["result"])
        }
        "usage_windows" => json!(quota::usage_windows(
            &args["row"],
            time(&args["now"]),
            &args["result"]
        )),
        "latest_openai_result" => {
            quota::latest_result(&args["row"], &args["result"], time(&args["now"]))
        }
        "build_monitor_candidates" => json!(recovery_rules::candidates(
            args["accounts"].as_array().unwrap(),
            &args["results"],
            &args["scheduler"],
            time(&args["now"]),
            args["recovery_monitor_enabled"].as_bool().unwrap()
        )?),
        "merge" => model_rules::merge(&args["base"], &args["patch"]),
        "validate_overrides" => {
            model_rules::validate_overrides(&args["overrides"], &args["bases"])?;
            Value::Null
        }
        "transform" => model_rules::transform(
            &args["body"],
            &args["overrides"],
            &args["allowlist"],
            &args["aliases"],
        )?,
        "admitted" => json!(model_rules::admitted(
            &args["allowlist"],
            args["model"].as_str().unwrap()
        )),
        "import_fields" => model_catalog::import_fields(&args["entry"]),
        "targets" => json!(model_rules::targets(
            &args["group"],
            args["model"].as_str().unwrap(),
            args["accounts"].as_array().unwrap(),
            args["routes"].as_array().unwrap()
        )),
        "composite_target" => json!(model_rules::composite_target(
            args["model"].as_str().unwrap(),
            args["routes"].as_array().unwrap(),
            args["accounts"].as_array().unwrap()
        )),
        "codex_identity" => json!(model_target::identity(&args["values"])),
        "standard_codex_user_agent" => json!(model_target::standard_agent(
            args["version"].as_str().unwrap()
        )),
        "synced_codex_user_agent" => json!(model_target::synced_agent(
            &args["custom"],
            args["version"].as_str().unwrap()
        )),
        "project_state" => {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("state.json");
            let store = OAuthState::new(path.clone(), false);
            store.transaction(|state| {
                state["scheduler"]["31"] = json!({"auto_reset_credit":args["value"]});
                Ok(())
            })?;
            let before = std::fs::read(&path).unwrap();
            let result = credit_rules::project(&args["value"], &args["notices"]);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(
                OAuthState::new(path, false).snapshot()?["scheduler"]["31"]["auto_reset_credit"],
                args["value"]
            );
            result
        }
        other => panic!("unhandled pinned rule {other}"),
    })
}

fn assert_same(actual: &Value, expected: &Value, context: &str) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            assert!(
                (a - b).abs() <= 1e-10 * b.abs().max(1.0),
                "{context}: {a} != {b}"
            );
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{context}: array length");
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                assert_same(a, b, &format!("{context}[{i}]"));
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(
                a.len(),
                b.len(),
                "{context}: object fields {actual} != {expected}"
            );
            for (key, b) in b {
                assert!(a.contains_key(key), "{context}: missing {key}");
                assert_same(&a[key], b, &format!("{context}.{key}"));
            }
        }
        _ => assert_eq!(actual, expected, "{context}"),
    }
}

fn check(legacy_id: &str) {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/contracts/rule-traces/fixtures.json"
    ))
    .unwrap();
    assert_eq!(
        fixture["reference_sha"],
        "6be7c291ed38dcc337cb88f693639040a3463899"
    );
    let cases = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["legacy_test_id"] == legacy_id)
        .collect::<Vec<_>>();
    assert!(!cases.is_empty(), "missing oracle vectors for {legacy_id}");
    for (index, case) in cases.iter().enumerate() {
        if let Some(raw) = case["invalid_json_arguments"].as_str() {
            assert!(serde_json::from_str::<Value>(raw).is_err());
            assert!(case["error"] == "invalid_value" || case["expected"].is_null());
            continue;
        }
        let args = case["arguments"].clone();
        let actual = execute(case["function"].as_str().unwrap(), &args);
        if case["error"] == "invalid_value" {
            assert!(actual.is_err(), "{legacy_id} vector {index} must reject");
        } else {
            assert_same(
                &actual.unwrap(),
                &case["expected"],
                &format!("{legacy_id} vector {index}"),
            );
            assert_eq!(case["input_unchanged"], true);
            assert_eq!(args, case["arguments"]);
        }
    }
}

// The individual tests below correspond to pinned Python test identities.

#[test]
fn key_fallback_oauthclassificationtests_test_available_requires_gates_fresh_success_and_required_windows(
) {
    check("py:tests.test_key_fallback:OAuthClassificationTests.test_available_requires_gates_fresh_success_and_required_windows");
}

#[test]
fn key_fallback_oauthclassificationtests_test_blocking_db_state_is_unavailable_and_stale_fields_do_not_block(
) {
    check("py:tests.test_key_fallback:OAuthClassificationTests.test_blocking_db_state_is_unavailable_and_stale_fields_do_not_block");
}

#[test]
fn key_fallback_oauthclassificationtests_test_desired_state_matrix() {
    check("py:tests.test_key_fallback:OAuthClassificationTests.test_desired_state_matrix");
}

#[test]
fn key_fallback_oauthclassificationtests_test_fresh_usage_data_windows_still_classify_available() {
    check("py:tests.test_key_fallback:OAuthClassificationTests.test_fresh_usage_data_windows_still_classify_available");
}

#[test]
fn key_fallback_oauthclassificationtests_test_freshness_and_ordinary_errors_are_unknown() {
    check("py:tests.test_key_fallback:OAuthClassificationTests.test_freshness_and_ordinary_errors_are_unknown");
}

#[test]
fn key_fallback_oauthclassificationtests_test_latest_completed_attempt_prefers_newer_success_over_leftover_error(
) {
    check("py:tests.test_key_fallback:OAuthClassificationTests.test_latest_completed_attempt_prefers_newer_success_over_leftover_error");
}

#[test]
fn key_fallback_oauthclassificationtests_test_quota_exhaustion_auth_errors_and_incomplete_windows()
{
    check("py:tests.test_key_fallback:OAuthClassificationTests.test_quota_exhaustion_auth_errors_and_incomplete_windows");
}

#[test]
fn key_fallback_oauthclassificationtests_test_success_without_fresh_quota_payload_is_unknown_despite_stale_extra(
) {
    check("py:tests.test_key_fallback:OAuthClassificationTests.test_success_without_fresh_quota_payload_is_unknown_despite_stale_extra");
}

#[test]
fn key_fallback_grokfallbacktests_test_grok_blocking_states_and_unknown_values() {
    check(
        "py:tests.test_key_fallback:GrokFallbackTests.test_grok_blocking_states_and_unknown_values",
    );
}

#[test]
fn quota_snapshot_passivequotatests_test_billing_and_money_budgets_are_not_usage_windows() {
    check("py:tests.test_quota_snapshot:PassiveQuotaTests.test_billing_and_money_budgets_are_not_usage_windows");
}

#[test]
fn quota_snapshot_passivequotatests_test_free_plan_has_only_seven_day_window_and_no_raw_data() {
    check("py:tests.test_quota_snapshot:PassiveQuotaTests.test_free_plan_has_only_seven_day_window_and_no_raw_data");
}

#[test]
fn quota_snapshot_passivequotatests_test_grok_error_stale_and_incomplete_are_not_fresh() {
    check("py:tests.test_quota_snapshot:PassiveQuotaTests.test_grok_error_stale_and_incomplete_are_not_fresh");
}

#[test]
fn quota_snapshot_passivequotatests_test_grok_requests_and_tokens_use_observed_headers_only() {
    check("py:tests.test_quota_snapshot:PassiveQuotaTests.test_grok_requests_and_tokens_use_observed_headers_only");
}

#[test]
fn quota_snapshot_passivequotatests_test_new_passive_snapshot_replaces_old_active_result() {
    check("py:tests.test_quota_snapshot:PassiveQuotaTests.test_new_passive_snapshot_replaces_old_active_result");
}

#[test]
fn quota_snapshot_passivequotatests_test_newer_error_wins_over_old_success_and_passive_snapshot() {
    check("py:tests.test_quota_snapshot:PassiveQuotaTests.test_newer_error_wins_over_old_success_and_passive_snapshot");
}

#[test]
fn quota_snapshot_passivequotatests_test_passive_exhaustion_schedules_active_recovery_at_reset() {
    check("py:tests.test_quota_snapshot:PassiveQuotaTests.test_passive_exhaustion_schedules_active_recovery_at_reset");
}

#[test]
fn quota_snapshot_passivequotatests_test_stale_incomplete_future_and_expired_snapshots_do_not_enable_fallback(
) {
    check("py:tests.test_quota_snapshot:PassiveQuotaTests.test_stale_incomplete_future_and_expired_snapshots_do_not_enable_fallback");
}

#[test]
fn quota_snapshot_passivequotatests_test_unknown_and_zero_are_distinct() {
    check("py:tests.test_quota_snapshot:PassiveQuotaTests.test_unknown_and_zero_are_distinct");
}

#[test]
fn model_catalog_ruletests_test_composite_exact_endpoint_priority_mapping_and_fixed_accounts() {
    check("py:tests.test_model_catalog:RuleTests.test_composite_exact_endpoint_priority_mapping_and_fixed_accounts");
}

#[test]
fn model_catalog_ruletests_test_recursive_merge_arrays_explicit_null_false_and_identity() {
    check("py:tests.test_model_catalog:RuleTests.test_recursive_merge_arrays_explicit_null_false_and_identity");
}

#[test]
fn model_catalog_ruletests_test_transform_filters_drafts_and_preserves_envelope_and_native_fields()
{
    check("py:tests.test_model_catalog:RuleTests.test_transform_filters_drafts_and_preserves_envelope_and_native_fields");
}

#[test]
fn model_catalog_ruletests_test_type_context_and_effort_validation() {
    check("py:tests.test_model_catalog:RuleTests.test_type_context_and_effort_validation");
}

#[test]
fn model_catalog_ruletests_test_upstream_import_does_not_infer_capabilities_or_default() {
    check("py:tests.test_model_catalog:RuleTests.test_upstream_import_does_not_infer_capabilities_or_default");
}

#[test]
fn codex_identity_codexidentitytests_test_empty_user_agent_uses_synced_version_in_standard_shape() {
    check("py:tests.test_codex_identity:CodexIdentityTests.test_empty_user_agent_uses_synced_version_in_standard_shape");
}

#[test]
fn codex_identity_codexidentitytests_test_invalid_versions_fall_back_without_accepting_newlines() {
    check("py:tests.test_codex_identity:CodexIdentityTests.test_invalid_versions_fall_back_without_accepting_newlines");
}

#[test]
fn codex_identity_codexidentitytests_test_manual_version_rewrites_custom_leading_and_trailing_versions(
) {
    check("py:tests.test_codex_identity:CodexIdentityTests.test_manual_version_rewrites_custom_leading_and_trailing_versions");
}

#[test]
fn recovery_config030_autoresetprojection030tests_test_active_recovery_steps_remain_visible() {
    check("py:tests.test_recovery_config030:AutoResetProjection030Tests.test_active_recovery_steps_remain_visible");
}

#[test]
fn recovery_config030_autoresetprojection030tests_test_completed_or_relinquished_states_are_hidden_without_deleting_history(
) {
    check("py:tests.test_recovery_config030:AutoResetProjection030Tests.test_completed_or_relinquished_states_are_hidden_without_deleting_history");
}

#[test]
fn recovery_config030_autoresetprojection030tests_test_manual_state_without_any_consumption_attempt_is_hidden(
) {
    check("py:tests.test_recovery_config030:AutoResetProjection030Tests.test_manual_state_without_any_consumption_attempt_is_hidden");
}

#[test]
fn recovery_config030_autoresetprojection030tests_test_unknown_consumption_remains_visible_after_manual_control(
) {
    check("py:tests.test_recovery_config030:AutoResetProjection030Tests.test_unknown_consumption_remains_visible_after_manual_control");
}

#[test]
fn auto_reset_resetcreditdiagnostictests_test_native_business_codes_are_preserved_without_extra_response_fields(
) {
    check("py:tests.test_auto_reset:ResetCreditDiagnosticTests.test_native_business_codes_are_preserved_without_extra_response_fields");
}

#[test]
fn auto_reset_resetcreditdiagnostictests_test_only_whitelisted_status_business_code_and_boolean_fields_are_retained(
) {
    check("py:tests.test_auto_reset:ResetCreditDiagnosticTests.test_only_whitelisted_status_business_code_and_boolean_fields_are_retained");
}

#[test]
fn auto_reset_resetcreditdiagnostictests_test_unknown_business_code_is_replaced_and_boolean_reset_count_is_omitted(
) {
    check("py:tests.test_auto_reset:ResetCreditDiagnosticTests.test_unknown_business_code_is_replaced_and_boolean_reset_count_is_omitted");
}

#[test]
fn auto_reset038_autoreset038observationtests_test_observation_from_usage_requires_fresh_explicit_matching_card_data(
) {
    check("py:tests.test_auto_reset038:AutoReset038ObservationTests.test_observation_from_usage_requires_fresh_explicit_matching_card_data");
}

#[test]
fn usage_query_oauthusagetests_test_active_usage_does_not_infer_plus_from_five_hour_window() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_active_usage_does_not_infer_plus_from_five_hour_window");
}

#[test]
fn usage_query_oauthusagetests_test_available_fresh_windows_remove_stale_recovery_metadata() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_available_fresh_windows_remove_stale_recovery_metadata");
}

#[test]
fn usage_query_oauthusagetests_test_cached_free_summary_filters_label_only_five_hour_window() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_cached_free_summary_filters_label_only_five_hour_window");
}

#[test]
fn usage_query_oauthusagetests_test_cached_invalid_reset_falls_back_to_remaining_and_updated_at() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_cached_invalid_reset_falls_back_to_remaining_and_updated_at");
}

#[test]
fn usage_query_oauthusagetests_test_current_account_plan_overrides_stale_cached_plan() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_current_account_plan_overrides_stale_cached_plan");
}

#[test]
fn usage_query_oauthusagetests_test_exact_server_reset_accepts_epoch_and_exposes_source() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_exact_server_reset_accepts_epoch_and_exposes_source");
}

#[test]
fn usage_query_oauthusagetests_test_free_recovery_metadata_ignores_depleted_five_hour_window() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_free_recovery_metadata_ignores_depleted_five_hour_window");
}

#[test]
fn usage_query_oauthusagetests_test_free_recovery_only_requires_seven_day() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_free_recovery_only_requires_seven_day");
}

#[test]
fn usage_query_oauthusagetests_test_fresh_response_does_not_inherit_an_entire_missing_window() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_fresh_response_does_not_inherit_an_entire_missing_window");
}

#[test]
fn usage_query_oauthusagetests_test_fresh_window_clears_stale_reset_and_window_fields() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_fresh_window_clears_stale_reset_and_window_fields");
}

#[test]
fn usage_query_oauthusagetests_test_invalid_explicit_reset_uses_remaining_seconds_from_query_time()
{
    check("py:tests.test_usage_query:OAuthUsageTests.test_invalid_explicit_reset_uses_remaining_seconds_from_query_time");
}

#[test]
fn usage_query_oauthusagetests_test_invalid_remaining_seconds_uses_reset_after_seconds_alias() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_invalid_remaining_seconds_uses_reset_after_seconds_alias");
}

#[test]
fn usage_query_oauthusagetests_test_nonfree_recovery_requires_both_windows_and_seven_day_balance() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_nonfree_recovery_requires_both_windows_and_seven_day_balance");
}

#[test]
fn usage_query_oauthusagetests_test_oauth_reset_at_keeps_legacy_interface_and_falls_back_from_invalid_explicit(
) {
    check("py:tests.test_usage_query:OAuthUsageTests.test_oauth_reset_at_keeps_legacy_interface_and_falls_back_from_invalid_explicit");
}

#[test]
fn usage_query_oauthusagetests_test_recovery_metadata_uses_depleted_required_keys_and_datetime_max()
{
    check("py:tests.test_usage_query:OAuthUsageTests.test_recovery_metadata_uses_depleted_required_keys_and_datetime_max");
}

#[test]
fn usage_query_oauthusagetests_test_recovery_transition_fingerprint_is_keyed_canonical_and_uses_time_max(
) {
    check("py:tests.test_usage_query:OAuthUsageTests.test_recovery_transition_fingerprint_is_keyed_canonical_and_uses_time_max");
}

#[test]
fn usage_query_oauthusagetests_test_reset_time_parser_normalizes_rfc3339_and_epoch_to_utc() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_reset_time_parser_normalizes_rfc3339_and_epoch_to_utc");
}

#[test]
fn usage_query_oauthusagetests_test_reset_time_parser_rejects_non_finite_and_out_of_range_epoch() {
    check("py:tests.test_usage_query:OAuthUsageTests.test_reset_time_parser_rejects_non_finite_and_out_of_range_epoch");
}

#[test]
fn oauth_monitor_oauthmonitorschedulingtests_test_available_accounts_never_trigger_periodic_refresh(
) {
    check("py:tests.test_oauth_monitor:OAuthMonitorSchedulingTests.test_available_accounts_never_trigger_periodic_refresh");
}

#[test]
fn oauth_monitor_oauthmonitorschedulingtests_test_candidates_are_ordered_by_recovery_priority() {
    check("py:tests.test_oauth_monitor:OAuthMonitorSchedulingTests.test_candidates_are_ordered_by_recovery_priority");
}

#[test]
fn oauth_monitor_oauthmonitorschedulingtests_test_exact_reset_uses_latest_full_required_window() {
    check("py:tests.test_oauth_monitor:OAuthMonitorSchedulingTests.test_exact_reset_uses_latest_full_required_window");
}

#[test]
fn oauth_monitor_oauthmonitorschedulingtests_test_one_account_is_only_scheduled_once() {
    check("py:tests.test_oauth_monitor:OAuthMonitorSchedulingTests.test_one_account_is_only_scheduled_once");
}

#[test]
fn oauth_monitor_oauthmonitorschedulingtests_test_seven_day_probe_is_retired() {
    check(
        "py:tests.test_oauth_monitor:OAuthMonitorSchedulingTests.test_seven_day_probe_is_retired",
    );
}

#[test]
fn oauth_monitor_oauthrecoverygatetests_test_automatic_recovery_is_strict_and_fail_closed() {
    check("py:tests.test_oauth_monitor:OAuthRecoveryGateTests.test_automatic_recovery_is_strict_and_fail_closed");
}

#[test]
fn oauth_monitor_oauthrecoverygatetests_test_block_signature_detects_concurrency_change() {
    check("py:tests.test_oauth_monitor:OAuthRecoveryGateTests.test_block_signature_detects_concurrency_change");
}

#[test]
fn oauth_monitor_oauthrecoverygatetests_test_recovery_backoff_schedule_caps_at_30_minutes() {
    check("py:tests.test_oauth_monitor:OAuthRecoveryGateTests.test_recovery_backoff_schedule_caps_at_30_minutes");
}

#[test]
fn events030_eventevidence030tests_test_deleted_account_history_keeps_known_upstream_identity() {
    check("py:tests.test_events030:EventEvidence030Tests.test_deleted_account_history_keeps_known_upstream_identity");
}

#[test]
fn events030_eventevidence030tests_test_explicit_client_concurrency_limit_is_local() {
    check("py:tests.test_events030:EventEvidence030Tests.test_explicit_client_concurrency_limit_is_local");
}

#[test]
fn events030_eventevidence030tests_test_explicit_local_request_limit_is_recognized_in_recorded_fields(
) {
    check("py:tests.test_events030:EventEvidence030Tests.test_explicit_local_request_limit_is_recognized_in_recorded_fields");
}

#[test]
fn events030_eventevidence030tests_test_gateway_wrapped_capacity_keeps_structured_upstream_evidence(
) {
    check("py:tests.test_events030:EventEvidence030Tests.test_gateway_wrapped_capacity_keeps_structured_upstream_evidence");
}

#[test]
fn events030_eventevidence030tests_test_local_event_does_not_hide_independent_capacity_evidence() {
    check("py:tests.test_events030:EventEvidence030Tests.test_local_event_does_not_hide_independent_capacity_evidence");
}

#[test]
fn events030_eventevidence030tests_test_only_complete_capacity_messages_on_supported_upstream_accounts_match(
) {
    check("py:tests.test_events030:EventEvidence030Tests.test_only_complete_capacity_messages_on_supported_upstream_accounts_match");
}

#[test]
fn events030_eventevidence030tests_test_other_platforms_local_errors_and_partial_phrases_are_not_degradation(
) {
    check("py:tests.test_events030:EventEvidence030Tests.test_other_platforms_local_errors_and_partial_phrases_are_not_degradation");
}

#[test]
fn events030_eventevidence030tests_test_request_payload_cannot_supply_capacity_or_local_limiter_evidence(
) {
    check("py:tests.test_events030:EventEvidence030Tests.test_request_payload_cannot_supply_capacity_or_local_limiter_evidence");
}

#[test]
fn events030_eventevidence030tests_test_upstream_429_and_all_capacity_messages_are_not_local_concurrency_limits(
) {
    check("py:tests.test_events030:EventEvidence030Tests.test_upstream_429_and_all_capacity_messages_are_not_local_concurrency_limits");
}

#[test]
fn desktop_api_desktopevidencetests_test_account_dto_never_returns_credentials() {
    check(
        "py:tests.test_desktop_api:DesktopEvidenceTests.test_account_dto_never_returns_credentials",
    );
}

#[test]
fn desktop_api_desktopevidencetests_test_empty_error_body() {
    check("py:tests.test_desktop_api:DesktopEvidenceTests.test_empty_error_body");
}

#[test]
fn desktop_api_desktopevidencetests_test_error_body_excludes_request_and_credentials() {
    check("py:tests.test_desktop_api:DesktopEvidenceTests.test_error_body_excludes_request_and_credentials");
}

#[test]
fn desktop_api_desktopevidencetests_test_error_detail_is_bounded_and_does_not_return_raw_columns() {
    check("py:tests.test_desktop_api:DesktopEvidenceTests.test_error_detail_is_bounded_and_does_not_return_raw_columns");
}

#[test]
fn desktop_api_desktopevidencetests_test_expired_blockers_and_success_after_error() {
    check("py:tests.test_desktop_api:DesktopEvidenceTests.test_expired_blockers_and_success_after_error");
}

#[test]
fn desktop_api_desktopevidencetests_test_group_with_only_removed_or_deleted_candidates_clears_legacy_summary(
) {
    check("py:tests.test_desktop_api:DesktopEvidenceTests.test_group_with_only_removed_or_deleted_candidates_clears_legacy_summary");
}

#[test]
fn desktop_api_desktopevidencetests_test_new_error_is_not_hidden_by_older_success() {
    check("py:tests.test_desktop_api:DesktopEvidenceTests.test_new_error_is_not_hidden_by_older_success");
}

#[test]
fn desktop_api_desktopevidencetests_test_no_error_time_is_invented() {
    check("py:tests.test_desktop_api:DesktopEvidenceTests.test_no_error_time_is_invented");
}

#[test]
fn desktop_api_desktopevidencetests_test_recent_accounts_are_distinct_and_group_scoped() {
    check("py:tests.test_desktop_api:DesktopEvidenceTests.test_recent_accounts_are_distinct_and_group_scoped");
}

#[test]
fn desktop_api_desktopevidencetests_test_stringified_request_dump_is_hidden_in_detail_and_summary()
{
    check("py:tests.test_desktop_api:DesktopEvidenceTests.test_stringified_request_dump_is_hidden_in_detail_and_summary");
}

#[test]
fn desktop_api_desktopevidencetests_test_switch_on_is_not_availability() {
    check("py:tests.test_desktop_api:DesktopEvidenceTests.test_switch_on_is_not_availability");
}
