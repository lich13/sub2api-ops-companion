use serde_json::json;
use sub2ops_server::oauth_state;

#[test]
fn normalize_filters_invalid_scheduler_keys_and_keeps_recovered_history() {
    let prior_history = json!({"id":9,"status":"fixture-existing"});
    let raw = json!({
        "version": 5,
        "scheduler": {
            "fixture-text-key": {"discarded": true},
            "9223372036854775808": {"discarded": true},
            "0": {"discarded": true},
            "-1": {"discarded": true},
            "12": {
                "fixture_marker": "valid-account",
                "recovery_intent": {
                    "status": "recovered",
                    "fingerprint": "fixture-recovered-fingerprint"
                }
            }
        },
        "recovery_history": {
            "fixture-existing-recovery": prior_history
        }
    });

    // In particular, the overflowing key must be ignored without panicking.
    let normalized = oauth_state::normalize(&raw);
    assert_eq!(
        normalized["scheduler"],
        json!({
            "12": {
                "fixture_marker": "valid-account",
                "recovery_intent": {
                    "status": "recovered",
                    "fingerprint": "fixture-recovered-fingerprint"
                }
            }
        })
    );
    assert_eq!(
        normalized["recovery_history"]["fixture-existing-recovery"], prior_history,
        "normalization keeps valid historical records"
    );
    assert!(
        normalized["recovery_history"]["recovery:12:fixture-recovered-fingerprint:success"]
            .is_null()
    );
    assert_eq!(normalized["recovery_history"].as_object().unwrap().len(), 1);
}

#[test]
fn credit_history_limits_keep_active_pending_and_owned_receipts_in_order() {
    for count in [199_usize, 200, 203] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oauth.json");
        let store = oauth_state::OAuthState::new(path.clone(), false);
        let at = "2031-04-05T06:07:08+00:00";
        let receipt = json!({"http_status":502,"business_code":"unrecognized"});
        let mut episodes = json!({});
        let mut notices = json!({});
        for episode in ["fixture-active".to_owned(), "fixture-pending".to_owned()]
            .into_iter()
            .chain((0..count).map(|index| format!("fixture-{index:03}")))
        {
            episodes[&episode] =
                json!({"episode":episode,"stage":"finished","completed_at":at,"receipt":receipt});
            notices[&episode] = json!({"episode":episode,"account_id":7,"outcome":"unknown","status":"delivered","completed_at":at,"attempts":1});
        }
        episodes["fixture-active"]["stage"] = "confirming".into();
        episodes["fixture-owned"] = json!({"episode":"fixture-owned","stage":"finished","owns_pause":true,"receipt":receipt});
        notices["fixture-pending"]["status"] = "pending".into();
        notices["fixture-pending"]["next_at"] = at.into();
        let mut raw = store.snapshot().unwrap();
        raw["scheduler"]["7"] = json!({"auto_reset_credit":episodes["fixture-active"],
            "reset_credit_episodes":episodes,"credit_result_notifications":notices,
            "reset_credit_observation":{"fixture":"retired"}});
        raw["scheduler"]["7"]["auto_reset_credit"]["auth_fingerprint"] = "fixture-retired".into();
        raw["scheduler"]["7"]["auto_reset_credit"]["owned_version"] = "fixture-retired".into();
        let bytes = serde_json::to_vec(&raw).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let loaded = store.snapshot().unwrap();
        assert!(loaded["scheduler"]["7"]
            .get("reset_credit_episodes")
            .is_none());
        assert!(loaded["scheduler"]["7"]
            .get("reset_credit_observation")
            .is_none());
        assert!(loaded["scheduler"]["7"]["auto_reset_credit"]
            .get("auth_fingerprint")
            .is_none());
        assert!(loaded["scheduler"]["7"]["auto_reset_credit"]
            .get("owned_version")
            .is_none());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "loading does not rewrite state"
        );
        store.transaction(|_| Ok(())).unwrap();
        let state = store.snapshot().unwrap();
        let meta = &state["scheduler"]["7"];
        {
            let field = "credit_result_notifications";
            let kept: Vec<_> = meta[field]
                .as_object()
                .unwrap()
                .keys()
                .filter(|key| {
                    key.strip_prefix("fixture-")
                        .unwrap()
                        .parse::<usize>()
                        .is_ok()
                })
                .cloned()
                .collect();
            let expected: Vec<_> = (count.saturating_sub(200)..count)
                .map(|index| format!("fixture-{index:03}"))
                .collect();
            assert_eq!(kept, expected, "{field} count={count}");
            assert!(meta[field]["fixture-active"].is_object());
            assert!(meta[field]["fixture-pending"].is_object());
        }
        assert!(meta.get("reset_credit_episodes").is_none());
        assert_eq!(meta["auto_reset_credit"]["receipt"], receipt);
        assert_eq!(
            meta["credit_result_notifications"]["fixture-pending"],
            notices["fixture-pending"]
        );
        assert_eq!(
            oauth_state::OAuthState::new(path, false)
                .snapshot()
                .unwrap(),
            state
        );
    }
}
