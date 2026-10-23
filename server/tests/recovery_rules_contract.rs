use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use sub2ops_server::{
    bark, compat, daily_test, oauth_admission,
    oauth_state::{self, OAuthState, Update},
    recovery_policy, recovery_rules, reset_credit_observation,
};

const FIXTURES: &str = include_str!("../../tests/contracts/recovery-fixtures.json");

fn fixtures() -> Value {
    serde_json::from_str(FIXTURES).expect("pinned recovery fixtures must be valid JSON")
}

fn date(value: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value.as_str().expect("timestamp string"))
        .expect("fixture timestamp must carry an offset")
        .with_timezone(&Utc)
}

fn candidate_projection(value: &Value) -> Value {
    json!({
        "account_id": value["account_id"],
        "reason": value["reason"],
        "priority": value["priority"],
        "exact_fingerprint": value["exact_fingerprint"],
        "reuse_quota": !value["reuse_quota"].is_null(),
    })
}

fn integer_values(value: &Value) -> Vec<i64> {
    value
        .as_array()
        .expect("integer fixture array")
        .iter()
        .map(|item| item.as_i64().expect("integer fixture value"))
        .collect()
}

fn string_values(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("string fixture array")
        .iter()
        .map(|item| item.as_str().expect("string fixture value").to_owned())
        .collect()
}

#[test]
fn oauth_admission_matches_pinned_fingerprint_and_time_budget_boundaries() {
    let fixture = fixtures();
    let admission = &fixture["oauth_admission"];
    let fingerprint = &admission["fingerprint"];
    assert_eq!(
        oauth_admission::credential_fingerprint(&fingerprint["row"]),
        fingerprint["expected"].as_str().unwrap(),
        "credential fingerprint must match Python's sorted JSON digest",
    );
    assert_eq!(
        oauth_admission::MIN_QUERY_SECONDS,
        admission["constants"]["min_query_seconds"]
            .as_i64()
            .unwrap(),
    );
    assert_eq!(
        oauth_admission::AUTO_WINDOW_SECONDS,
        admission["constants"]["automatic_window_seconds"]
            .as_i64()
            .unwrap(),
    );
    assert_eq!(
        oauth_admission::AUTO_QUERY_LIMIT,
        admission["constants"]["automatic_query_limit"]
            .as_u64()
            .unwrap() as usize,
    );

    for case in admission["gates"].as_array().unwrap() {
        let (reason, deadline) = oauth_admission::automatic_gate(
            &case["row"],
            &case["saved"],
            &case["metadata"],
            date(&fixture["at"]),
            case["wait_for_reset"].as_bool().unwrap(),
            case["bypass_interval"].as_bool().unwrap(),
            case["required_budget"].as_u64().unwrap() as usize,
        )
        .unwrap_or_else(|error| panic!("{}: {error}", case["name"]));
        assert_eq!(
            reason,
            case["expected_reason"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        assert_eq!(
            deadline.map(compat::datetime),
            case["expected_deadline"].as_str().map(str::to_owned),
            "{} deadline",
            case["name"],
        );
    }

    for case in admission["fresh_quota"].as_array().unwrap() {
        assert_eq!(
            oauth_admission::fresh_quota(&case["row"], &case["result"], date(&case["now"])),
            case["expected"].as_bool().unwrap(),
            "{}",
            case["name"],
        );
    }

    let query_metadata = &admission["query_metadata"];
    assert_eq!(
        oauth_state::query_metadata(&query_metadata["metadata"], &query_metadata["saved"]).unwrap(),
        query_metadata["expected"],
    );
}

#[test]
fn recovery_policy_selection_matches_pinned_python_rules() {
    let fixture = fixtures();
    let policy = &fixture["recovery_policy"];
    let ids = recovery_policy::account_ids(&policy["account_ids_input"]).unwrap();
    assert_eq!(
        ids.into_iter().collect::<Vec<_>>(),
        integer_values(&policy["account_ids_expected"]),
    );
    let eligible = recovery_policy::eligible_ids(policy["eligible_rows"].as_array().unwrap());
    assert_eq!(
        eligible.into_iter().collect::<Vec<_>>(),
        integer_values(&policy["eligible_expected"])
    );

    for case in policy["methods"].as_array().unwrap() {
        let actual = recovery_policy::method(&case["config"], case["id"].as_i64().unwrap());
        assert_eq!(actual, case["expected_method"].as_str(), "{}", case["name"],);
    }
}

#[test]
fn recovery_descriptors_and_block_signatures_match_pinned_python() {
    let fixture = fixtures();
    let rules = &fixture["recovery_rules"];
    assert_eq!(
        oauth_state::block_signature(&rules["signature"]["row"]),
        rules["signature"]["expected"].as_str().unwrap(),
    );

    for case in rules["change_safety"].as_array().unwrap() {
        assert_eq!(
            recovery_rules::change_is_safe(&case["before"], &case["after"]),
            case["expected"].as_bool().unwrap(),
            "{}",
            case["name"],
        );
    }

    let threshold = &rules["threshold"];
    let parsed = recovery_rules::threshold(&json!(threshold["value"]));
    assert_eq!(parsed, threshold["expected"]);
    assert_eq!(
        recovery_rules::eligible(
            &threshold["row"],
            &["codex_5h".to_owned()],
            date(&fixture["at"])
        ),
        threshold["eligibility_expected"].as_bool().unwrap(),
        "threshold evidence must name an observed quota window",
    );
    assert_eq!(
        recovery_rules::eligible(&threshold["row"], &[], date(&fixture["at"])),
        threshold["unobserved_window_expected"].as_bool().unwrap(),
    );
    let descriptor = recovery_rules::threshold_descriptor(
        &threshold["row"],
        &threshold["descriptor_summary"],
        date(&fixture["at"]),
    );
    assert_eq!(descriptor, threshold["descriptor_expected"]);
    assert_eq!(
        recovery_rules::threshold_available(&threshold["descriptor_summary"], &descriptor),
        threshold["quota_available_expected"].as_bool().unwrap(),
    );

    let confirmed = &rules["confirmed"];
    assert_eq!(
        recovery_rules::confirmed(&confirmed["row"]),
        confirmed["expected"].as_bool().unwrap(),
    );

    let quota = &rules["quota_descriptor"];
    assert_eq!(
        recovery_rules::quota_descriptor(&quota["summary"], date(&quota["at"])),
        quota["expected"],
        "quota reset fingerprint and source must match the pinned Python result",
    );
    let rate = &rules["rate_descriptor"];
    assert_eq!(
        recovery_rules::rate_descriptor(&rate["row"], &rate["summary"], date(&fixture["at"])),
        rate["expected"],
        "natural rate-limit reset evidence must retain the same fingerprint",
    );
    for case in rules["retry_at"].as_array().unwrap() {
        assert_eq!(
            recovery_rules::retry_at(date(&fixture["at"]), case["attempt"].as_i64().unwrap()),
            case["expected"].as_str().unwrap(),
            "attempt {}",
            case["attempt"],
        );
    }
}

#[test]
fn recovery_quota_and_manual_control_gates_match_pinned_python() {
    let fixture = fixtures();
    let rules = &fixture["recovery_rules"];
    for case in rules["quota_readiness"].as_array().unwrap() {
        assert_eq!(
            recovery_rules::quota_ready(
                &case["row"],
                &case["result"],
                &case["metadata"],
                date(&case["now"]),
            ),
            case["expected"].as_bool().unwrap(),
            "{}",
            case["name"],
        );
    }
    for case in rules["manual_control"].as_array().unwrap() {
        assert_eq!(
            recovery_rules::control_allows(
                &case["row"],
                &case["result"],
                &case["metadata"],
                date(&case["now"]),
            ),
            case["expected"].as_bool().unwrap(),
            "{}",
            case["name"],
        );
    }
}

#[test]
fn recovery_candidates_do_not_replay_dispatched_verification_without_receipt() {
    let fixture = fixtures();
    let candidate_fixture = &fixture["recovery_rules"]["candidates"];
    let candidates = recovery_rules::candidates(
        candidate_fixture["rows"].as_array().unwrap(),
        &candidate_fixture["results"],
        &candidate_fixture["scheduler"],
        date(&candidate_fixture["now"]),
        true,
    )
    .unwrap();
    let projected = candidates
        .iter()
        .map(candidate_projection)
        .collect::<Vec<_>>();
    assert_eq!(
        Value::Array(projected.clone()),
        candidate_fixture["expected"]
    );
    assert!(
        projected.iter().all(|item| item["account_id"] != 74),
        "a dispatched verification with no receipt must never become a retry candidate",
    );
    assert!(
        projected
            .iter()
            .any(|item| item["account_id"] == 73 && item["reuse_quota"] == true),
        "fresh post-reset quota evidence may be reused for an already due intent",
    );
    assert!(
        projected
            .iter()
            .any(|item| item["account_id"] == 76 && item["reason"] == "exact_reset"),
        "natural reset evidence should produce an exact-reset candidate",
    );
}

#[test]
fn daily_schedule_matches_pinned_on_time_missed_interrupted_and_config_change_states() {
    let fixture = fixtures();
    let daily = &fixture["daily_test"];
    for case in daily["times"].as_array().unwrap() {
        assert_eq!(
            compat::datetime(
                daily_test::next_time(date(&case["now"]), case["time"].as_str().unwrap()).unwrap(),
            ),
            case["expected"].as_str().unwrap(),
            "{}",
            case["name"],
        );
    }
    for case in daily["eligibility"].as_array().unwrap() {
        assert_eq!(
            daily_test::eligible(&case["row"], date(&case["now"])),
            case["expected"].as_bool().unwrap(),
            "{}",
            case["name"],
        );
    }

    let schedule_fixture = &daily["schedule"];
    let temporary = tempfile::tempdir().unwrap();
    let state = OAuthState::new(temporary.path().join("recovery-state.json"), false);
    let schedule = daily_test::Schedule::new(state.clone());
    let initial_config = json!({"oauth_daily_test_enabled":true,"oauth_daily_test_time":"05:00"});
    schedule
        .startup(
            &initial_config,
            date(&schedule_fixture["on_time_tick"]["at"]) - chrono::Duration::seconds(90),
        )
        .unwrap();
    assert_eq!(
        state.snapshot().unwrap()["daily_test"],
        schedule_fixture["initial"]
    );

    schedule
        .tick(
            &initial_config,
            date(&schedule_fixture["on_time_tick"]["at"]),
        )
        .unwrap();
    assert_eq!(
        state.snapshot().unwrap()["daily_test"],
        schedule_fixture["on_time_tick"]["expected"],
    );

    let changed_config = json!({"oauth_daily_test_enabled":true,"oauth_daily_test_time":"06:30"});
    schedule
        .tick(
            &changed_config,
            date(&schedule_fixture["configuration_change"]["at"]),
        )
        .unwrap();
    assert_eq!(
        state.snapshot().unwrap()["daily_test"],
        schedule_fixture["configuration_change"]["expected"],
        "changing the future schedule must preserve queued work",
    );

    let claimed = schedule
        .claim(&changed_config, date(&schedule_fixture["claim"]["at"]))
        .unwrap()
        .expect("on-time batch should be claimable");
    assert_eq!(claimed, schedule_fixture["claim"]["expected_batch"]);
    assert_eq!(
        state.snapshot().unwrap()["daily_test"],
        schedule_fixture["claim"]["expected_state"],
    );

    let restarted = daily_test::Schedule::new(state.clone());
    restarted
        .startup(&changed_config, date(&schedule_fixture["restart"]["at"]))
        .unwrap();
    assert_eq!(
        state.snapshot().unwrap()["daily_test"],
        schedule_fixture["restart"]["expected_state"],
        "queued or running work must become interrupted on restart",
    );

    let late_temporary = tempfile::tempdir().unwrap();
    let late_state = OAuthState::new(late_temporary.path().join("recovery-state.json"), false);
    let late_schedule = daily_test::Schedule::new(late_state.clone());
    late_schedule
        .startup(
            &initial_config,
            date(&daily["schedule"]["late_missed"]["at"]) - chrono::Duration::seconds(61),
        )
        .unwrap();
    late_schedule
        .tick(
            &initial_config,
            date(&schedule_fixture["late_missed"]["at"]),
        )
        .unwrap();
    assert_eq!(
        late_state.snapshot().unwrap()["daily_test"],
        schedule_fixture["late_missed"]["expected_state"],
        "a tick at or after the one-minute boundary must be marked missed",
    );
}

#[test]
fn reset_credit_expiry_and_fingerprints_match_pinned_python() {
    let fixture = fixtures();
    let reset = &fixture["reset_credit"];
    let normalized = oauth_state::normalized_credits(&reset["raw_credits"]).unwrap();
    assert_eq!(normalized, reset["normalized_expected"]);
    assert_eq!(
        oauth_state::available_credits(&reset["raw_credits"], date(&fixture["at"])),
        reset["available_at_now"].as_i64(),
        "credit expiring at the observation instant is no longer available",
    );
    assert_eq!(
        oauth_state::compact_digest(&reset["normalized"]),
        reset["digest"].as_str().unwrap(),
        "credit snapshot digest is part of the cross-language observation contract",
    );
    assert_eq!(
        oauth_state::limit_fingerprint(&reset["limit_fingerprint_row"]),
        reset["limit_fingerprint"].as_str().unwrap(),
    );
}

#[test]
fn reset_credit_observation_creation_and_evidence_gates_match_pinned_python() {
    let fixture = fixtures();
    let reset = &fixture["reset_credit"];
    let observation = &reset["observation"];
    let created = reset_credit_observation::create(
        &observation["row"],
        &observation["content"],
        date(&observation["observed_at"]),
        observation["credential_fingerprint"].as_str().unwrap(),
        date(&observation["reset_at"]),
        &observation["window_minutes"],
    )
    .unwrap();
    assert_eq!(created, observation["expected"]);

    for case in reset["observation_rejections"].as_array().unwrap() {
        let actual = reset_credit_observation::from_usage(
            &case["before"],
            &case["after"],
            &case["result"],
            date(&case["started"]),
            date(&case["completed"]),
        );
        assert_eq!(
            actual,
            (!case["expected"].is_null()).then(|| case["expected"].clone()),
            "{}",
            case["name"],
        );
    }
    let accepted = &reset["observation_from_usage"];
    let actual = reset_credit_observation::from_usage(
        &accepted["before"],
        &accepted["after"],
        &accepted["result"],
        date(&accepted["started"]),
        date(&accepted["completed"]),
    );
    assert_eq!(actual, Some(accepted["expected"].clone()));
}

#[test]
fn reset_credit_receipt_diagnostics_never_infer_uncertain_consumption() {
    let fixture = fixtures();
    let reset = &fixture["reset_credit"];
    for case in reset["diagnostics"].as_array().unwrap() {
        assert_eq!(
            reset_credit_observation::diagnostics(
                case["status"].as_u64().map(|status| status as u16),
                &case["data"],
            ),
            case["expected"],
            "{}",
            case["name"],
        );
    }
    for case in reset["consumption"].as_array().unwrap() {
        assert_eq!(
            reset_credit_observation::consumption_result(&case["data"]),
            case["expected"],
            "{}",
            case["name"],
        );
    }
}

#[test]
fn bark_urls_messages_and_utf8_chunks_match_pinned_python() {
    let fixture = fixtures();
    let bark_fixture = &fixture["bark"];
    for case in bark_fixture["urls"].as_array().unwrap() {
        let actual = bark::normalize_url(case["value"].as_str().unwrap());
        if case["expected_error"] == true {
            assert!(actual.is_err(), "{} should be rejected", case["name"]);
        } else {
            let normalized = actual.unwrap();
            assert_eq!(normalized, case["expected"], "{}", case["name"]);
            assert_eq!(
                format!("{normalized}/push"),
                case["push_url"],
                "{} push endpoint",
                case["name"],
            );
        }
    }

    for case in bark_fixture["oauth_messages"].as_array().unwrap() {
        let actual = bark::oauth_message(&case["event"]);
        assert_eq!(actual.0, case["expected"][0], "{} title", case["name"]);
        assert_eq!(actual.1, case["expected"][1], "{} body", case["name"]);
    }
    let credit = &bark_fixture["credit_message"];
    let actual = bark::credit_message(&credit["event"]);
    assert_eq!(actual.0, credit["expected"][0]);
    assert_eq!(actual.1, credit["expected"][1]);

    let chunks = &bark_fixture["chunks"];
    let text = chunks["text"].as_str().unwrap();
    let max = chunks["max_bytes"].as_u64().unwrap() as usize;
    assert_eq!(
        bark::split_chunks(text, max).unwrap(),
        string_values(&chunks["split_expected"]),
    );
    let parts = bark::body_parts(text, max).unwrap();
    assert_eq!(parts, string_values(&chunks["body_expected"]));
    assert!(parts.iter().all(|part| part.len() <= max));
    assert_eq!(
        bark::body_parts("", max).unwrap(),
        string_values(&chunks["empty_expected"]),
    );
}

#[test]
fn manual_generation_discards_stale_state_updates() {
    let fixture = fixtures();
    let expected = &fixture["manual_generation"];
    let temporary = tempfile::tempdir().unwrap();
    let state = OAuthState::new(temporary.path().join("recovery-state.json"), false);
    let generation = state
        .manual_control(&expected["row"], false, date(&expected["at"]), "manual")
        .unwrap();
    assert_eq!(
        generation,
        expected["expected_generation"].as_i64().unwrap()
    );

    let mut update = Update::default();
    update
        .results
        .insert(73, json!({"template_type":"oauth","stale_result":true}));
    update
        .scheduler
        .insert(73, json!({"stale_scheduler_marker":true}));
    update.generations = Some(BTreeMap::from([(73, generation - 1)]));
    state.commit(update).unwrap();

    let snapshot = state.snapshot().unwrap();
    assert_eq!(
        snapshot["scheduler"]["73"]["manual_control"],
        expected["expected_control"]
    );
    assert_eq!(
        snapshot["scheduler"]["73"]
            .get("stale_scheduler_marker")
            .cloned()
            .unwrap_or(Value::Null),
        expected["expected_stale_marker"],
    );
    assert_eq!(
        snapshot["oauth_results"]
            .get("73")
            .cloned()
            .unwrap_or(Value::Null),
        expected["expected_result"],
    );
}

#[test]
fn ordinary_temporary_restrictions_expire_only_at_a_valid_deadline() {
    let now = date(&json!("2026-01-01T12:00:00Z"));
    let base = json!({"id":73,"platform":"openai","type":"oauth","status":"active","schedulable":true,
        "rate_limited_at":"2026-01-01T10:00:00Z","rate_limit_reset_at":"2026-01-01T12:00:00Z",
        "temp_unschedulable_reason":"fixture network unavailable"});
    for (deadline, expected) in [
        (json!("2026-01-01T11:59:59Z"), true),
        (json!("2026-01-01T12:00:00Z"), true),
        (json!("2026-01-01T12:00:01Z"), false),
        (Value::Null, false),
        (json!(""), false),
        (json!("invalid"), false),
    ] {
        let mut row = base.clone();
        row["temp_unschedulable_until"] = deadline;
        assert_eq!(oauth_admission::automatic_eligible(&row, now), expected);
        assert_eq!(recovery_rules::eligible(&row, &[], now), expected);
        row["schedulable"] = false.into();
        assert!(!oauth_admission::automatic_eligible(&row, now));
        assert!(!recovery_rules::eligible(&row, &[], now));
    }
    let mut expired = base;
    expired["temp_unschedulable_until"] = "2026-01-01T11:00:00Z".into();
    expired["credentials"] = json!({"access_token":"fixture-token"});
    let auth = json!({"quota_query":{"auth_fingerprint":oauth_admission::credential_fingerprint(&expired)}});
    assert_eq!(
        oauth_admission::automatic_gate(&expired, &Value::Null, &auth, now, true, false, 1)
            .unwrap()
            .0,
        "auth_paused"
    );
    expired["status"] = "error".into();
    assert!(!recovery_rules::eligible(&expired, &[], now));
}

#[test]
fn rate_only_recovery_requires_fresh_evidence_and_preserves_real_blocks() {
    let now = date(&json!("2031-04-05T10:00:00Z"));
    let row = json!({"rate_limited_at":"2031-04-05T08:00:00Z", "rate_limit_reset_at":"2031-04-05T09:00:00Z", "last_success_at":"2031-04-05T09:30:00Z"});
    let summary = json!({"plan_type":"plus", "ui_windows":[{"key":"codex_5h","used_percent":20},{"key":"codex_7d","used_percent":30}]});
    assert!(recovery_rules::stale_rate_block(&row, &summary, now));
    assert!(recovery_rules::rate_descriptor(&row, &summary, now).is_null());
    let intent = json!({"kind":"rate_limit_block","status":"pending"});
    assert!(recovery_rules::obsolete_rate_intent(
        &row, &summary, &intent, now
    ));
    for patch in [
        json!({"last_success_at":null}),
        json!({"last_success_at":"2031-04-05T08:59:00Z"}),
        json!({"last_success_at":"2031-04-05T11:00:00Z"}),
        json!({"rate_limited_at":"2031-04-05T09:45:00Z"}),
        json!({"rate_limit_reset_at":"2031-04-05T11:00:00Z"}),
    ] {
        let mut changed = row.clone();
        changed
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(!recovery_rules::stale_rate_block(&changed, &summary, now));
    }
    let mut depleted = summary.clone();
    depleted["ui_windows"][1]["used_percent"] = 100.into();
    assert!(!recovery_rules::stale_rate_block(&row, &depleted, now));
    for patch in [
        json!({"kind":"quota"}),
        json!({"verification_dispatched":true}),
        json!({"verification_receipt":{"success":true}}),
        json!({"recovery_write_dispatched":true}),
    ] {
        let mut changed = intent.clone();
        changed
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(!recovery_rules::obsolete_rate_intent(
            &row, &summary, &changed, now
        ));
    }
}
