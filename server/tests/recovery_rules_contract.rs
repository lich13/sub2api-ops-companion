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

fn reset_fixture() -> (Value, Value, Value, DateTime<Utc>) {
    let at = date(&json!("2031-04-05T10:00:00Z"));
    let row = json!({"id":73,"platform":"openai","type":"oauth","status":"active","schedulable":true,
        "rate_limited_at":"2031-04-04T10:00:00Z","rate_limit_reset_at":"2031-04-05T10:04:00Z",
        "extra":{"codex_plan_type":"plus","codex_usage_updated_at":"2031-04-05T09:59:49Z",
            "codex_5h_used_percent":0,"codex_5h_reset_at":"2031-04-05T15:00:00Z",
            "codex_7d_used_percent":100,"codex_7d_reset_at":"2031-04-05T10:00:00Z"}});
    let result = sub2ops_server::quota::latest_result(&row, &Value::Null, at);
    let descriptor = recovery_rules::quota_descriptor(
        &sub2ops_server::quota::summary_from_result(&row, &result),
        at,
    );
    let intent = sub2ops_server::recovery_events::new_intent(&descriptor, "pending", None);
    let meta = json!({"recovery_intent":intent,"quota_query":{"last_query_at":result["queried_at"],
        "last_source":"manual","automatic_attempts":[]}});
    (row, result, meta, at)
}

#[test]
fn quota_reset_shortens_only_the_pre_reset_query_interval() {
    use chrono::Duration;
    let (row, result, mut meta, at) = reset_fixture();
    let gate = |meta: &Value, now| {
        oauth_admission::automatic_gate(&row, &result, meta, now, true, false, 1).unwrap()
    };
    assert_eq!(
        gate(&meta, at - Duration::seconds(1)),
        ("waiting_reset", Some(at))
    );
    assert_eq!(gate(&meta, at), ("", None));
    meta["quota_query"]["last_query_at"] = compat::datetime(at).into();
    assert_eq!(
        gate(&meta, at),
        ("query_cooldown", Some(at + Duration::hours(1)))
    );

    meta["quota_query"]["last_query_at"] = result["queried_at"].clone();
    meta["quota_query"]["retry_at"] = compat::datetime(at + Duration::minutes(30)).into();
    assert_eq!(
        gate(&meta, at),
        ("query_backoff", Some(at + Duration::minutes(30)))
    );
    meta["quota_query"]["retry_at"] = "".into();
    meta["quota_query"]["automatic_attempts"] =
        json!(vec![compat::datetime(at - Duration::hours(1)); 6]);
    assert_eq!(
        gate(&meta, at),
        ("query_budget", Some(at + Duration::hours(23)))
    );
    meta["quota_query"]["auth_fingerprint"] = oauth_admission::credential_fingerprint(&row).into();
    assert_eq!(gate(&meta, at), ("auth_paused", None));
}

#[test]
fn post_reset_quota_supersedes_only_the_old_rate_limit_estimate() {
    use chrono::Duration;
    let (mut row, _, meta, reset) = reset_fixture();
    let at = reset + Duration::seconds(1);
    row["extra"]["codex_usage_updated_at"] = compat::datetime(at).into();
    row["extra"]["codex_7d_used_percent"] = 0.into();
    row["extra"]["codex_7d_reset_at"] = compat::datetime(reset + Duration::days(7)).into();
    let result = sub2ops_server::quota::latest_result(&row, &Value::Null, at);
    assert!(recovery_rules::quota_ready(&row, &result, &meta, at));
    assert_eq!(
        recovery_rules::reset_not_before(&row, &result, &meta, at),
        None
    );
    let candidates = recovery_rules::candidates(
        std::slice::from_ref(&row),
        &json!({"73":result}),
        &json!({"73":meta}),
        at,
        true,
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0]["reuse_quota"].is_object());
    let root = tempfile::tempdir().unwrap();
    let store = OAuthState::new(root.path().join("state.json"), false);
    store
        .transaction(|state| {
            state["scheduler"]["73"] = meta.clone();
            Ok(())
        })
        .unwrap();
    sub2ops_server::recovery_events::discover(
        &store,
        std::slice::from_ref(&row),
        &json!({"oauth_recovery_connection_account_ids":[73]}),
        at,
    )
    .unwrap();
    let discovered = store.snapshot().unwrap();
    assert_eq!(
        discovered["scheduler"]["73"]["recovery_intent"]["kind"],
        "quota_exhausted"
    );
    assert_eq!(
        recovery_rules::candidates(
            std::slice::from_ref(&row),
            &json!({"73":result}),
            &discovered["scheduler"],
            at,
            true
        )
        .unwrap()
        .len(),
        1
    );

    for patch in [
        json!({"rate_limited_at":compat::datetime(reset)}),
        json!({"overload_until":compat::datetime(at + Duration::minutes(5))}),
        json!({"temp_unschedulable_until":compat::datetime(at + Duration::minutes(5)),"temp_unschedulable_reason":"fixture manual pause"}),
    ] {
        let mut changed = row.clone();
        changed
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(!recovery_rules::quota_ready(&changed, &result, &meta, at));
    }
    let mut before = result.clone();
    before["queried_at"] = compat::datetime(reset - Duration::seconds(1)).into();
    before["oauth_quota"]["ui_windows"][1]["reset_at"] = compat::datetime(reset).into();
    assert!(!recovery_rules::quota_ready(&row, &before, &meta, at));
    let mut incomplete = result.clone();
    incomplete["oauth_quota"]["ui_windows"]
        .as_array_mut()
        .unwrap()
        .retain(|window| window["key"] != "codex_5h");
    assert!(!recovery_rules::quota_ready(&row, &incomplete, &meta, at));
    let mut rate_only = meta.clone();
    rate_only["recovery_intent"]["kind"] = "rate_limit_block".into();
    assert!(!recovery_rules::quota_ready(&row, &result, &rate_only, at));
    let mut failed = meta.clone();
    failed["last_error_at"] = result["queried_at"].clone();
    failed["last_error_code"] = "http_429".into();
    assert!(!recovery_rules::quota_ready(&row, &result, &failed, at));
}

#[test]
fn manual_cancellation_does_not_follow_the_account_into_a_new_quota_cycle() {
    use chrono::Duration;
    use sub2ops_server::{quota, recovery_events};
    let (mut row, result, mut meta, at) = reset_fixture();
    row["rate_limited_at"] = compat::datetime(at - Duration::days(10)).into();
    meta["manual_control"] = json!({"at":compat::datetime(at - Duration::days(8)),"enabled":true,
        "generation":1,"evidence_at":compat::datetime(at - Duration::days(8))});
    meta["recovery_intent"]["status"] = "cancelled".into();
    assert!(
        !recovery_rules::control_allows(&row, &result, &meta, at),
        "a new observation alone cannot revive the cancelled cycle"
    );
    let mut corrected = meta.clone();
    corrected["recovery_intent"]["fingerprint"] = "codex_7d@2031-04-05T10:01:00+00:00".into();
    corrected["recovery_intent"]["due_at"] = "2031-04-05T10:01:00Z".into();
    assert!(
        !recovery_rules::control_allows(&row, &result, &corrected, at),
        "correcting a pending reset estimate does not establish a new cycle"
    );

    let mut early_reset = row.clone();
    early_reset["extra"]["codex_7d_reset_at"] = compat::datetime(at + Duration::days(7)).into();
    let early_result = quota::latest_result(&early_reset, &Value::Null, at);
    assert!(
        recovery_rules::control_allows(&early_reset, &early_result, &corrected, at),
        "an early reset followed by depletion belongs to the later window"
    );

    meta["recovery_intent"]["fingerprint"] = "codex_7d@2031-03-29T10:00:00+00:00".into();
    meta["recovery_intent"]["due_at"] = "2031-03-29T10:00:00Z".into();
    assert!(recovery_rules::control_allows(&row, &result, &meta, at));
    let root = tempfile::tempdir().unwrap();
    let store = OAuthState::new(root.path().join("state.json"), false);
    store
        .transaction(|state| {
            state["scheduler"]["73"] = meta.clone();
            Ok(())
        })
        .unwrap();
    recovery_events::discover(
        &store,
        std::slice::from_ref(&row),
        &json!({"oauth_recovery_connection_account_ids":[73]}),
        at,
    )
    .unwrap();
    let state = store.snapshot().unwrap();
    let next = &state["scheduler"]["73"];
    assert_eq!(next["recovery_intent"]["status"], "pending");
    assert_eq!(
        next["recovery_intent"]["fingerprint"],
        recovery_rules::quota_descriptor(&quota::summary_from_result(&row, &result), at)
            ["fingerprint"]
    );
    assert_eq!(next["manual_control"], meta["manual_control"]);
    assert!(recovery_rules::control_allows(&row, &result, next, at));
    assert_eq!(
        recovery_rules::candidates(
            std::slice::from_ref(&row),
            &json!({"73":result}),
            &state["scheduler"],
            at,
            true
        )
        .unwrap()
        .len(),
        1
    );
    let mut disabled = row.clone();
    disabled["schedulable"] = false.into();
    assert!(!recovery_rules::control_allows(
        &disabled, &result, next, at
    ));
    meta.as_object_mut().unwrap().remove("recovery_intent");
    assert!(recovery_rules::control_allows(&row, &result, &meta, at));
}

#[test]
fn active_recovery_keeps_query_failures_and_unusable_available_quota() {
    use chrono::Duration;
    use sub2ops_server::{quota, recovery_events};
    let (mut row, _, mut meta, at) = reset_fixture();
    row["rate_limited_at"] = compat::datetime(at - Duration::days(10)).into();
    row["rate_limit_reset_at"] = compat::datetime(at - Duration::seconds(1)).into();
    meta["manual_control"] = json!({"at":compat::datetime(at - Duration::days(8)),
        "evidence_at":compat::datetime(at - Duration::days(8)),"enabled":true,"generation":1});
    for status in [
        "pending",
        "retry",
        "waiting_quota",
        "retry_exhausted",
        "blocked",
        "auth_failed",
    ] {
        meta["recovery_intent"]["status"] = status.into();
        for code in ["network_error", "incomplete_quota", "http_401"] {
            // A local state failure may not include a provider observation timestamp.
            let failed = json!({"success":false,"error_code":code,"error":"fixture failure"});
            let (update, runnable, _) = recovery_events::quota_update(
                &json!({"account_id":73,"row":row,"reason":"recovery_intent"}),
                &failed,
                &json!({"scheduler":{"73":meta}}),
                at,
                true,
                false,
            );
            assert!(runnable.is_none());
            let saved = &update.scheduler[&73];
            assert_eq!(saved["last_error_code"], code);
            assert_eq!(saved["recovery_intent"]["last_error_code"], code);
            if code == "http_401" {
                assert_eq!(saved["recovery_intent"]["status"], "auth_failed");
                assert_eq!(update.pending.len(), 1);
            } else {
                assert_eq!(saved["recovery_intent"]["status"], "retry");
                assert_eq!(saved["recovery_intent"]["attempt_count"], 1);
                assert_eq!(
                    saved["recovery_intent"]["next_retry_at"],
                    recovery_rules::retry_at(at, 1)
                );
            }
        }
    }
    meta["recovery_intent"]["status"] = "pending".into();
    row["extra"]["codex_usage_updated_at"] = compat::datetime(at).into();
    row["extra"]["codex_7d_used_percent"] = 0.into();
    row["extra"]["codex_7d_reset_at"] = compat::datetime(at + Duration::days(7)).into();
    let available = quota::latest_result(&row, &Value::Null, at);
    for status in ["pending", "retry", "waiting_quota"] {
        meta["recovery_intent"]["status"] = status.into();
        meta["recovery_intent"]["next_retry_at"] = compat::datetime(at).into();
        let stale_at = at + Duration::hours(2);
        assert!(!recovery_rules::quota_ready(
            &row, &available, &meta, stale_at
        ));
        assert!(recovery_rules::control_allows(
            &row, &available, &meta, stale_at
        ));
        let candidates = recovery_rules::candidates(
            &[row.clone()],
            &json!({"73":available}),
            &json!({"73":meta}),
            stale_at,
            true,
        )
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0]["reuse_quota"].is_null());
        let mut overloaded = row.clone();
        overloaded["overload_until"] = compat::datetime(at + Duration::minutes(5)).into();
        assert!(recovery_rules::control_allows(
            &overloaded,
            &available,
            &meta,
            at
        ));
        assert!(!recovery_rules::quota_ready(
            &overloaded,
            &available,
            &meta,
            at
        ));
        let mut failed = meta.clone();
        failed["last_error_at"] = compat::datetime(at).into();
        failed["last_error_code"] = "http_429".into();
        assert!(recovery_rules::control_allows(
            &row, &available, &failed, at
        ));
        assert!(!recovery_rules::quota_ready(&row, &available, &failed, at));
    }
}

#[test]
fn manual_cancellation_uses_each_window_and_captures_the_current_cycle() {
    use chrono::Duration;
    use sub2ops_server::{quota, recovery_events};
    let (mut row, _, mut meta, at) = reset_fixture();
    row["rate_limited_at"] = compat::datetime(at - Duration::days(10)).into();
    row["extra"]["codex_usage_updated_at"] = compat::datetime(at - Duration::days(7)).into();
    meta["manual_control"] = json!({"at":compat::datetime(at - Duration::days(7) - Duration::hours(1)),
        "evidence_at":compat::datetime(at - Duration::days(8)),"enabled":true,"generation":1});
    meta["recovery_intent"] = json!({"status":"cancelled","fingerprint":"codex_5h@2031-03-29T12:40:00+00:00",
        "due_at":"2031-03-29T12:40:00Z","window_keys":["codex_5h"]});
    let result = quota::latest_result(&row, &Value::Null, at);
    assert!(
        recovery_rules::control_allows(&row, &result, &meta, at),
        "a later weekly depletion is independent of the cancelled five-hour boundary"
    );
    // With two cancelled windows, only the matching window's boundary matters.
    meta["recovery_intent"]["fingerprint"] =
        "codex_5h@2031-04-12T12:40:00+00:00|codex_7d@2031-03-29T09:00:00+00:00".into();
    meta["recovery_intent"]["due_at"] = "2031-04-12T12:40:00Z".into();
    assert!(recovery_rules::control_allows(&row, &result, &meta, at));
    let root = tempfile::tempdir().unwrap();
    let store = OAuthState::new(root.path().join("state.json"), false);
    store
        .transaction(|state| {
            state["scheduler"]["73"] = meta.clone();
            Ok(())
        })
        .unwrap();
    store
        .manual_control(&row, true, at - Duration::minutes(1), "manual")
        .unwrap();
    let captured = store.snapshot().unwrap()["scheduler"]["73"].clone();
    let descriptor =
        recovery_rules::quota_descriptor(&quota::summary_from_result(&row, &result), at);
    assert_eq!(
        captured["recovery_intent"]["fingerprint"],
        descriptor["fingerprint"]
    );
    assert_eq!(captured["recovery_intent"]["status"], "cancelled");
    row["extra"]["codex_usage_updated_at"] = compat::datetime(at).into();
    let refreshed = quota::latest_result(&row, &Value::Null, at);
    assert!(!recovery_rules::control_allows(
        &row, &refreshed, &captured, at
    ));
    store.manual_control(&row, false, at, "manual").unwrap();
    row["schedulable"] = false.into();
    assert!(!recovery_rules::control_allows(
        &row,
        &refreshed,
        &store.snapshot().unwrap()["scheduler"]["73"],
        at
    ));
    // Manual control also captures a cycle before discovery has created an intent.
    let empty = OAuthState::new(root.path().join("empty.json"), false);
    row["schedulable"] = true.into();
    empty.manual_control(&row, true, at, "manual").unwrap();
    assert_eq!(
        empty.snapshot().unwrap()["scheduler"]["73"]["recovery_intent"]["fingerprint"],
        descriptor["fingerprint"]
    );
    let mut threshold_row = row.clone();
    threshold_row["schedulable"] = false.into();
    threshold_row["rate_limited_at"] = Value::Null;
    threshold_row["rate_limit_reset_at"] = Value::Null;
    threshold_row["extra"]["codex_7d_used_percent"] = 0.into();
    threshold_row["extra"]["codex_5h_used_percent"] = 80.into();
    let until = at + Duration::hours(1);
    threshold_row["temp_unschedulable_until"] = compat::datetime(until).into();
    threshold_row["temp_unschedulable_reason"] = json!({"source":"account_scheduling_threshold",
        "platform":"openai","window":"5h","threshold_percent":80,"until_unix":until.timestamp()})
    .to_string()
    .into();
    let threshold_store = OAuthState::new(root.path().join("threshold.json"), false);
    threshold_store
        .manual_control(&threshold_row, false, at, "manual")
        .unwrap();
    let captured =
        threshold_store.snapshot().unwrap()["scheduler"]["73"]["recovery_intent"].clone();
    assert_eq!(captured["status"], "cancelled");
    assert_eq!(captured["kind"], "account_scheduling_threshold");
    assert_eq!(captured["window_keys"], json!(["codex_5h"]));
    assert_eq!(captured["due_at"], compat::datetime(until));
    recovery_events::discover(
        &empty,
        &[row],
        &json!({"oauth_recovery_connection_account_ids":[73]}),
        at,
    )
    .unwrap();
    assert_eq!(
        empty.snapshot().unwrap()["scheduler"]["73"]["recovery_intent"]["status"],
        "cancelled"
    );
}

#[test]
fn fresh_release_before_the_old_boundary_accepts_fractional_429_deadlines() {
    use chrono::Duration;
    use sub2ops_server::{quota, recovery_events};
    let (mut row, _, mut meta, reset) = reset_fixture();
    let at = reset - Duration::minutes(4);
    row["rate_limit_reset_at"] = compat::datetime(reset + Duration::microseconds(1_280_691)).into();
    row["extra"]["codex_usage_updated_at"] = compat::datetime(at - Duration::seconds(1)).into();
    row["extra"]["codex_7d_used_percent"] = 0.into();
    row["extra"]["codex_7d_reset_at"] = compat::datetime(at + Duration::days(7)).into();
    let result = quota::latest_result(&row, &Value::Null, at);
    meta["quota_query"]["last_query_at"] = result["queried_at"].clone();
    assert!(recovery_rules::quota_release(&row, &result, &meta, at));
    let root = tempfile::tempdir().unwrap();
    let store = OAuthState::new(root.path().join("state.json"), false);
    store
        .transaction(|state| {
            state["scheduler"]["73"] = meta.clone();
            Ok(())
        })
        .unwrap();
    recovery_events::discover(
        &store,
        &[row.clone()],
        &json!({"oauth_recovery_connection_account_ids":[73]}),
        at,
    )
    .unwrap();
    let state = store.snapshot().unwrap();
    assert_eq!(
        state["scheduler"]["73"]["recovery_intent"]["due_at"],
        result["queried_at"]
    );
    let candidates = recovery_rules::candidates(
        &[row.clone()],
        &json!({"73":result}),
        &state["scheduler"],
        at,
        true,
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["reuse_quota"], result);
    let (update, runnable, _) = recovery_events::quota_update(
        &candidates[0],
        &result,
        &json!({"scheduler":{"73":meta}}),
        at,
        true,
        true,
    );
    assert!(runnable.is_some());
    assert_eq!(
        update.scheduler[&73]["recovery_intent"]["due_at"],
        result["queried_at"]
    );
    for reason in [
        "worker_slots",
        "model_verification_pending",
        "model_cooldown",
        "model_busy",
        "test_slots",
        "account_busy",
        "",
    ] {
        let mut waiting = state["scheduler"]["73"].clone();
        let intent = &mut waiting["recovery_intent"];
        intent["waiting_reason"] = reason.into();
        intent["waiting_until"] = Value::Null;
        if reason != "worker_slots" {
            intent["status"] = "retry".into();
            intent["tested_at"] = compat::datetime(at - Duration::seconds(2)).into();
            intent["next_retry_at"] = compat::datetime(at + Duration::seconds(60)).into();
            if reason.is_empty() {
                intent["test_completed_at"] = intent["tested_at"].clone();
                intent["last_error_code"] = "model_test_failed".into();
            }
        }
        store
            .transaction(|state| {
                state["scheduler"]["73"] = waiting.clone();
                Ok(())
            })
            .unwrap();
        let saved = std::fs::read(store.0.path()).unwrap();
        recovery_events::discover(
            &store,
            std::slice::from_ref(&row),
            &json!({"oauth_recovery_model_account_ids":[73]}),
            at,
        )
        .unwrap();
        if reason.is_empty() {
            assert_eq!(
                store.snapshot().unwrap()["scheduler"]["73"]["recovery_intent"]["status"],
                "ready"
            );
        } else {
            assert_eq!(std::fs::read(store.0.path()).unwrap(), saved);
        }
        if reason != "worker_slots" {
            let (update, runnable, _) = recovery_events::quota_update(
                &candidates[0],
                &result,
                &json!({"scheduler":{"73":waiting}}),
                at,
                true,
                true,
            );
            assert_eq!(runnable.is_some(), reason.is_empty(), "{reason}");
            assert_eq!(
                update.scheduler[&73]["recovery_intent"]["next_retry_at"],
                if reason.is_empty() {
                    json!("")
                } else {
                    waiting["recovery_intent"]["next_retry_at"].clone()
                }
            );
            assert_eq!(
                update.scheduler[&73]["recovery_intent"]["waiting_reason"],
                reason
            );
        }
    }
    let until = at + Duration::hours(1);
    row["temp_unschedulable_until"] = compat::datetime(until).into();
    row["temp_unschedulable_reason"] = json!({"source":"account_scheduling_threshold",
        "platform":"openai","window":"5h","threshold_percent":80,"until_unix":until.timestamp()})
    .to_string()
    .into();
    meta["recovery_intent"]["attempt_count"] = 2.into();
    meta["recovery_intent"]["last_error_code"] = "model_test_failed".into();
    let (update, runnable, _) = recovery_events::quota_update(
        &json!({"account_id":73,"row":row,"reason":"recovery_intent"}),
        &result,
        &json!({"scheduler":{"73":meta}}),
        at,
        true,
        true,
    );
    assert!(runnable.is_some());
    for field in ["kind", "fingerprint", "attempt_count", "last_error_code"] {
        assert_eq!(
            update.scheduler[&73]["recovery_intent"][field], meta["recovery_intent"][field],
            "{field}"
        );
    }
    row["rate_limited_at"] = result["queried_at"].clone();
    assert!(!recovery_rules::quota_release(&row, &result, &meta, at));
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
        // 0.1.80: the old cancellation cycle only gates an explicitly cancelled intent.
        let expected =
            case["name"] == "observation_at_manual_change" || case["expected"].as_bool().unwrap();
        assert_eq!(
            recovery_rules::control_allows(
                &case["row"],
                &case["result"],
                &case["metadata"],
                date(&case["now"]),
            ),
            expected,
            "{}",
            case["name"],
        );
    }
}

#[test]
fn recovery_candidates_retry_interrupted_verification_without_private_dispatch_flags() {
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
    let mut expected = candidate_fixture["expected"].as_array().unwrap().clone();
    expected.push(json!({"account_id":74,"reason":"recovery_intent","priority":0,"exact_fingerprint":"fixture-reset-boundary","reuse_quota":true}));
    assert_eq!(projected, expected);
    assert!(projected.iter().any(|item| item["account_id"] == 74));
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
}

#[test]
fn reset_credit_observation_creation_and_evidence_gates_match_pinned_python() {
    let fixture = fixtures();
    let reset = &fixture["reset_credit"];
    let observation = &reset["observation"];
    let compact = |value: &Value| {
        compat::pick(
            value,
            &["version", "observed_at", "available_count", "expires_at"],
        )
    };
    let created = reset_credit_observation::create(
        &observation["content"],
        date(&observation["observed_at"]),
    )
    .unwrap();
    assert_eq!(created, compact(&observation["expected"]));
    let accepted = &reset["observation_from_usage"];
    let expected = compact(&accepted["expected"]);
    assert_eq!(
        reset_credit_observation::from_usage(
            &accepted["result"],
            date(&accepted["started"]),
            date(&accepted["completed"])
        ),
        Some(expected)
    );
    for case in reset["observation_rejections"].as_array().unwrap() {
        let actual = reset_credit_observation::from_usage(
            &case["result"],
            date(&case["started"]),
            date(&case["completed"]),
        );
        let rejects = matches!(
            case["name"].as_str().unwrap(),
            "cache_not_persisted" | "fetched_before_start_window"
        );
        assert_eq!(actual.is_none(), rejects, "{}", case["name"]);
        if let Some(actual) = actual {
            assert_eq!(actual.as_object().unwrap().len(), 4);
        }
    }
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
        json!({"status":"testing"}),
        json!({"status":"retry","waiting_reason":"model_verification_pending"}),
        json!({"status":"retry","waiting_reason":"account_busy"}),
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

#[test]
fn early_quota_release_retires_old_waits_without_erasing_query_budget_or_live_restrictions() {
    use sub2ops_server::{quota, recovery_events};
    let at = date(&json!("2031-04-05T10:00:00Z"));
    let row = json!({"id":73,"platform":"openai","type":"oauth","status":"active","schedulable":true,
        "extra":{"codex_plan_type":"plus","codex_usage_updated_at":"2031-04-05T09:59:00Z","codex_5h_used_percent":0,"codex_7d_used_percent":0,"codex_5h_reset_at":"2031-04-05T15:00:00Z","codex_7d_reset_at":"2031-04-12T10:00:00Z"}});
    let result = quota::latest_result(&row, &Value::Null, at);
    let intent = json!({"status":"pending","kind":"quota_exhausted","fingerprint":"fixture-old-cycle",
        "due_at":"2031-04-06T10:00:00Z","next_retry_at":"2031-04-05T11:00:00Z","waiting_reason":"query_cooldown","waiting_until":"2031-04-05T11:00:00Z"});
    let meta = json!({"recovery_intent":intent,"quota_query":{"last_query_at":"2031-04-05T09:50:00Z","automatic_attempts":["2031-04-05T09:50:00Z"]}});
    assert!(recovery_rules::obsolete_healthy_intent(
        &row, &result, &meta, at
    ));
    for patch in [
        json!({"schedulable":false}),
        json!({"rate_limit_reset_at":"2031-04-05T12:00:00Z"}),
        json!({"temp_unschedulable_reason":"fixture manual restriction"}),
        json!({"status":"error"}),
    ] {
        let mut changed = row.clone();
        changed
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(!recovery_rules::obsolete_healthy_intent(
            &changed, &result, &meta, at
        ));
    }
    for patch in [
        json!({"success":false}),
        json!({"queried_at":"2031-04-05T08:00:00Z"}),
        json!({"queried_at":"2031-04-05T11:00:00Z"}),
        json!({"oauth_quota":{"plan_type":"plus","ui_windows":[{"key":"codex_7d","used_percent":0}]}}),
    ] {
        let mut changed = result.clone();
        changed
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(!recovery_rules::obsolete_healthy_intent(
            &row, &changed, &meta, at
        ));
    }
    let mut inflight = meta.clone();
    inflight["recovery_intent"]["status"] = "testing".into();
    assert!(!recovery_rules::obsolete_healthy_intent(
        &row, &result, &inflight, at
    ));
    let directory = tempfile::tempdir().unwrap();
    let store = OAuthState::new(directory.path().join("state.json"), false);
    store
        .transaction(|state| {
            state["scheduler"]["73"] = meta.clone();
            Ok(())
        })
        .unwrap();
    recovery_events::discover(
        &store,
        std::slice::from_ref(&row),
        &json!({"oauth_recovery_connection_account_ids":[73]}),
        at,
    )
    .unwrap();
    let state = store.snapshot().unwrap();
    let saved = &state["scheduler"]["73"];
    assert_eq!(saved["recovery_intent"]["status"], "cancelled");
    for field in ["due_at", "next_retry_at", "waiting_until", "waiting_reason"] {
        assert_eq!(saved["recovery_intent"][field], "");
    }
    assert!(recovery_events::project(saved).is_null());
    assert_eq!(saved["quota_query"], meta["quota_query"]);
    assert!(recovery_rules::candidates(
        std::slice::from_ref(&row),
        &json!({"73":result}),
        &json!({"73":meta}),
        at,
        true
    )
    .unwrap()
    .is_empty());
    let (update, runnable, _) = recovery_events::quota_update(
        &json!({"account_id":73,"row":row,"reason":"manual"}),
        &result,
        &json!({"scheduler":{"73":meta}}),
        at,
        true,
        false,
    );
    assert!(runnable.is_none());
    assert_eq!(
        update.scheduler[&73]["recovery_intent"]["status"],
        "cancelled"
    );
}

#[test]
fn early_new_cycle_bypasses_only_obsolete_quota_waits_before_recovery_verification() {
    use sub2ops_server::{quota, recovery_events};
    let at = date(&json!("2031-04-05T10:00:00Z"));
    let row = json!({"id":74,"platform":"openai","type":"oauth","status":"active","schedulable":true,
        "rate_limited_at":"2031-04-05T08:00:00Z","rate_limit_reset_at":"2031-04-06T10:00:00Z",
        "extra":{"codex_plan_type":"plus","codex_usage_updated_at":"2031-04-05T09:59:00Z","codex_5h_used_percent":0,"codex_7d_used_percent":0,"codex_5h_reset_at":"2031-04-05T15:00:00Z","codex_7d_reset_at":"2031-04-12T10:00:00Z"}});
    let result = quota::latest_result(&row, &Value::Null, at);
    let meta = json!({"recovery_intent":{"kind":"quota_exhausted","status":"pending","fingerprint":"codex_7d@2031-04-06T10:00:00+00:00","window_keys":["codex_7d"],"due_at":"2031-04-06T10:00:00Z","next_retry_at":"2031-04-06T10:00:00Z","waiting_reason":"waiting_reset","waiting_until":"2031-04-06T10:00:00Z"},"quota_query":{"last_query_at":"2031-04-05T09:59:00Z","automatic_attempts":[]}});
    assert!(recovery_rules::quota_release(&row, &result, &meta, at));
    assert!(recovery_rules::quota_ready(&row, &result, &meta, at));
    assert!(!recovery_rules::obsolete_healthy_intent(
        &row, &result, &meta, at
    ));
    let mut verifying = meta.clone();
    verifying["recovery_intent"]["status"] = "testing".into();
    verifying["recovery_intent"]["tested_at"] = "2031-04-05T10:00:00Z".into();
    verifying["recovery_intent"]["verification_dispatched"] = true.into();
    assert!(recovery_rules::quota_ready(&row, &result, &verifying, at));
    verifying["recovery_intent"]["status"] = "retry".into();
    verifying["recovery_intent"]["waiting_reason"] = "model_verification_pending".into();
    assert!(recovery_rules::quota_ready(&row, &result, &verifying, at));
    verifying["recovery_intent"]["test_completed_at"] = "2031-04-05T10:00:00Z".into();
    assert!(!recovery_rules::quota_ready(&row, &result, &verifying, at));
    let directory = tempfile::tempdir().unwrap();
    let store = OAuthState::new(directory.path().join("state.json"), false);
    store
        .transaction(|state| {
            state["scheduler"]["74"] = meta.clone();
            Ok(())
        })
        .unwrap();
    recovery_events::discover(
        &store,
        std::slice::from_ref(&row),
        &json!({"oauth_recovery_connection_account_ids":[74]}),
        at,
    )
    .unwrap();
    let scheduler = store.snapshot().unwrap()["scheduler"].clone();
    assert_eq!(scheduler["74"]["recovery_intent"]["status"], "ready");
    assert_eq!(scheduler["74"]["recovery_intent"]["waiting_until"], "");
    let candidates = recovery_rules::candidates(
        std::slice::from_ref(&row),
        &json!({"74":result}),
        &scheduler,
        at,
        true,
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0]["reuse_quota"].is_object());
    let (_, runnable, _) = recovery_events::quota_update(
        &candidates[0],
        &result,
        &json!({"scheduler":scheduler}),
        at,
        true,
        true,
    );
    assert!(runnable.is_some());
    for patch in [
        json!({"rate_limited_at":"2031-04-05T10:00:00Z"}),
        json!({"rate_limit_reset_at":"2031-04-07T10:00:00Z", "rate_limited_at":"2031-04-05T10:00:00Z"}),
        json!({"schedulable":false}),
        json!({"overload_until":"2031-04-05T11:00:00Z"}),
    ] {
        let mut changed = row.clone();
        changed
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(!recovery_rules::quota_release(&changed, &result, &meta, at));
    }
    let mut stale = meta.clone();
    stale["recovery_intent"]["fingerprint"] = "codex_7d@2031-04-12T10:00:00+00:00".into();
    assert!(!recovery_rules::quota_release(&row, &result, &stale, at));
    stale = meta.clone();
    stale["last_error_at"] = "2031-04-05T10:00:00Z".into();
    assert!(!recovery_rules::quota_release(&row, &result, &stale, at));
}
