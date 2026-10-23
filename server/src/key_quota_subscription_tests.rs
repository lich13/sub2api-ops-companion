// 覆盖套餐、钱包及额度窗口的独立解析。
//! v0.21 subscription metadata parsing for read-only quota responses.
use super::{newapi, sub2, subscriptions};
use serde_json::json;

#[test]
fn subscriptions_keep_multiple_named_states_expiry_and_nested_plans() {
    let value = json!({
        "subscriptions": [
            {
                "name": "Pro",
                "status": "active",
                "active": true,
                "expiresAt": "2026-12-31T00:00:00Z",
                "amount_total": 100,
                "amount_used": 25,
                "unit": "USD"
            },
            {
                "plan": {"name": "Team"},
                "status": "expired",
                "end_time": 1
            },
            {"name": "bad\nname"}
        ]
    });
    let parsed = subscriptions(&value, "额度单位");
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0].name.as_deref(), Some("Pro"));
    assert_eq!(parsed[0].status.as_deref(), Some("active"));
    assert_eq!(parsed[0].active, Some(true));
    assert_eq!(
        parsed[0].expires_at.as_deref(),
        Some("2026-12-31T00:00:00+00:00")
    );
    assert_eq!(parsed[0].plans.len(), 1);
    assert_eq!(parsed[0].plans[0].remaining, Some(75.));
    assert_eq!(parsed[0].plans[0].used, Some(25.));
    assert_eq!(parsed[0].plans[0].total, Some(100.));
    assert_eq!(parsed[1].name.as_deref(), Some("Team"));
    assert_eq!(parsed[1].status.as_deref(), Some("expired"));
    assert_eq!(parsed[1].active, Some(false));
    assert_eq!(
        parsed[1].expires_at.as_deref(),
        Some("1970-01-01T00:00:01+00:00")
    );
    assert_eq!(parsed[2].name, None);
}

#[test]
fn subscription_expiry_accepts_valid_values_without_fabricating_sentinels() {
    for (label, value, expected) in [
        (
            "positive Unix seconds",
            json!(1),
            Some("1970-01-01T00:00:01+00:00"),
        ),
        (
            "RFC3339",
            json!("2026-12-31T00:00:00Z"),
            Some("2026-12-31T00:00:00+00:00"),
        ),
        ("zero sentinel", json!(0), None),
        ("negative sentinel", json!(-1), None),
        ("invalid string", json!("not-a-date"), None),
    ] {
        let parsed = subscriptions(
            &json!({
                "subscriptions": [{"name": "Fixture", "end_time": value}]
            }),
            "USD",
        );
        assert_eq!(parsed[0].expires_at.as_deref(), expected, "{label}");
    }
}

#[test]
fn missing_and_zero_subscription_values_do_not_become_balance_or_key_expiry() {
    let parsed = subscriptions(
        &json!({
            "subscriptions": [
                {"name": "Zero", "amount_total": 0, "amount_used": 0},
                {"name": "Missing"}
            ]
        }),
        "USD",
    );
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].plans.len(), 1);
    assert_eq!(parsed[0].plans[0].remaining, Some(0.));
    assert_eq!(parsed[0].plans[0].total, Some(0.));
    assert!(parsed[1].plans.is_empty());

    let wallet = sub2(&json!({
        "isValid": true,
        "remaining": 0,
        "subscriptions": [{"name": "Pro", "status": "active"}]
    }))
    .expect("sub2 response");
    assert_eq!(wallet.subscriptions.len(), 1);
    assert_eq!(wallet.subscriptions[0].name.as_deref(), Some("Pro"));
    assert_eq!(wallet.plans[0].name, "钱包余额");
    assert_eq!(wallet.plans[0].remaining, Some(0.));
    assert_eq!(wallet.expires_at, None);
    assert_eq!(wallet.expires_at_unix, None);
}

#[test]
fn newapi_key_expiry_stays_on_key_metadata_and_does_not_create_subscription_expiry() {
    let data = newapi(&json!({
        "code": true,
        "data": {
            "object": "token_usage",
            "total_available": 80,
            "total_used": 20,
            "total_granted": 100,
            "unlimited_quota": false,
            "expires_at": 1,
            "subscriptions": [{"name": "Pro", "status": "active"}]
        }
    }))
    .expect("newapi response");
    assert_eq!(data.expires_at_unix, Some(1));
    assert_eq!(data.subscriptions.len(), 1);
    assert_eq!(data.subscriptions[0].name.as_deref(), Some("Pro"));
    assert_eq!(data.subscriptions[0].expires_at, None);
    assert_eq!(data.plans[0].name, "Key 额度");
    assert_eq!(data.plans[0].remaining, Some(80.));
}
