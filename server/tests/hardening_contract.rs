use serde_json::json;
use sub2ops_server::oauth_state;

#[test]
fn normalize_filters_invalid_scheduler_keys_and_keeps_recovered_history() {
    let prior_history = json!({"id":9,"status":"fixture-existing"});
    let raw = json!({
        "version": 4,
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
    assert_eq!(
        normalized["recovery_history"]["recovery:12:fixture-recovered-fingerprint:success"]["id"],
        10,
        "a valid recovered account still receives its deduplicated history record"
    );
    assert_eq!(normalized["recovery_history"].as_object().unwrap().len(), 2);
}
