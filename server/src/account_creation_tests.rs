use super::*;

#[test]
fn creation_supported_combinations_and_defaults_are_valid() {
    let expected = [
        (
            "anthropic",
            vec![
                "oauth",
                "setup-token",
                "apikey",
                "bedrock",
                "service_account",
            ],
        ),
        ("openai", vec!["oauth", "apikey"]),
        ("gemini", vec!["oauth", "apikey", "service_account"]),
        ("antigravity", vec!["oauth", "apikey"]),
        ("grok", vec!["oauth", "apikey"]),
        ("kimi", vec!["apikey"]),
        ("zhipu", vec!["apikey"]),
        ("deepseek", vec!["apikey"]),
        ("minimax", vec!["apikey"]),
        ("opencode_go", vec!["apikey"]),
    ];
    assert_eq!(
        SCHEMA["platforms"].as_array().unwrap().len(),
        expected.len()
    );
    for (platform, types) in expected {
        for kind in [
            "oauth",
            "setup-token",
            "apikey",
            "bedrock",
            "service_account",
            "upstream",
        ] {
            assert_eq!(
                supported(platform, kind),
                types.contains(&kind),
                "{platform}/{kind}"
            );
            if !types.contains(&kind) {
                continue;
            }
            let mut value = json!({"name":"Fixture","platform":platform,"type":kind,"credentials":{},"extra":{},"concurrency":10,"priority":0,"rate_multiplier":0,"load_factor":null,"proxy_id":null,"expires_at":null,"group_ids":[],"auto_pause_on_expired":false});
            for f in SCHEMA["fields"].as_array().unwrap() {
                if applies(f, platform, kind) {
                    if let Some(default) = f.get("default") {
                        insert(&mut value, string(&f["path"]), default.clone());
                    }
                }
            }
            validate(&["account-creation"], &value).unwrap();
            let mut unsafe_value = value.clone();
            unsafe_value["credentials"]["unrecognized_identity"] = json!("fixture");
            assert!(validate(&["account-creation"], &unsafe_value).is_err());
        }
    }
}

#[test]
fn creation_reuse_preserves_zero_false_null_nested_config_and_excludes_identity_runtime() {
    let row = json!({"name":"Existing","platform":"openai","type":"apikey","notes":null,"priority":0,"concurrency":1,"load_factor":null,"rate_multiplier":0,"proxy_id":null,"group_ids":[3],"expires_at":null,"auto_pause_on_expired":false,
        "credentials":{"api_key":"fixture-secret","access_token":"fixture-access","model_mapping":{"alias":"fixture-model"},"pool_mode":false,"pool_mode_retry_count":0,"temp_unschedulable_rules":[{"error_code":429,"keywords":["fixture"],"duration_minutes":1,"description":""}],"header_overrides":{"X-Fixture":""}},
        "extra":{"quota_limit":0,"quota_used":999,"quota_daily_reset_hour":0,"openai_passthrough":false,"upstream_billing_probe_enabled":false,"sub2ops_detection":true,"email":"fixture@example.invalid","drive_storage_usage":9},"status":"error","last_used_at":123});
    let result = reusable(&row).unwrap();
    for key in [
        "notes",
        "priority",
        "concurrency",
        "load_factor",
        "rate_multiplier",
        "proxy_id",
        "group_ids",
        "expires_at",
        "auto_pause_on_expired",
    ] {
        assert_eq!(result.get(key), row.get(key), "{key}");
    }
    assert_eq!(
        result["credentials"]["model_mapping"],
        row["credentials"]["model_mapping"]
    );
    assert_eq!(
        result["credentials"]["temp_unschedulable_rules"],
        row["credentials"]["temp_unschedulable_rules"]
    );
    assert_eq!(result["credentials"]["pool_mode"], false);
    assert_eq!(result["credentials"]["pool_mode_retry_count"], 0);
    assert_eq!(result["credentials"]["header_overrides"]["X-Fixture"], "");
    assert_eq!(result["extra"]["quota_limit"], 0);
    assert_eq!(result["extra"]["openai_passthrough"], false);
    assert_eq!(result["upstream_billing_probe_enabled"], false);
    for key in ["name", "status", "last_used_at"] {
        assert!(result.get(key).is_none());
    }
    for key in [
        "quota_used",
        "email",
        "drive_storage_usage",
        "sub2ops_detection",
    ] {
        assert!(result["extra"].get(key).is_none());
    }
    assert!(!result.to_string().contains("fixture-secret"));
    assert!(!result.to_string().contains("fixture-access"));
    let grok = reusable(&json!({"platform":"grok","type":"oauth","credentials":{"access_token":"fixture-secret","header_override_enabled":true,"header_overrides":{"X-Fixture":""}}})).unwrap();
    assert_eq!(
        grok["credentials"],
        json!({"header_override_enabled":true,"header_overrides":{"X-Fixture":""}})
    );
    let claude = reusable(&json!({"platform":"anthropic","type":"oauth","credentials":{"model_mapping":{"alias":"fixture-model"}}})).unwrap();
    assert_eq!(claude["credentials"], json!({}));
    let service=reusable(&json!({"platform":"gemini","type":"service_account","credentials":{"service_account_json":"fixture-secret","project_id":"fixture-private","client_email":"fixture@example.invalid","location":"global"},"groups":[{"id":4}]})).unwrap();
    assert_eq!(service["credentials"], json!({"location":"global"}));
    assert_eq!(service["group_ids"], json!([4]));
}

#[test]
fn creation_auth_and_import_routes_are_fixed_and_validate_each_branch() {
    for (provider, actions) in [
        ("anthropic", vec!["authorize", "exchange", "cookie"]),
        ("setup-token", vec!["authorize", "exchange", "cookie"]),
        ("openai", vec!["authorize", "exchange", "refresh"]),
        ("gemini", vec!["authorize", "exchange"]),
        ("antigravity", vec!["authorize", "exchange", "refresh"]),
        ("grok", vec!["authorize", "exchange", "refresh"]),
    ] {
        for action in actions {
            let path = ["account-creation", "auth", provider, action];
            assert_eq!(allowed(&path), Some(&["POST"][..]));
            let body = match action {
                "authorize" => json!({"proxy_id":null}),
                "exchange" => {
                    json!({"session_id":"fixture-session","code":"fixture-code","state":"fixture-state"})
                }
                "cookie" => json!({"code":"fixture-session-key"}),
                _ => json!({"refresh_token":"fixture-refresh"}),
            };
            validate(&path, &body).unwrap();
            let mut bad = body;
            bad["url"] = json!("https://example.invalid/escape");
            assert!(validate(&path, &bad).is_err());
        }
    }
    assert_eq!(
        auth_route("setup-token", "cookie"),
        Some("accounts/setup-token-cookie-auth")
    );
    assert_eq!(
        auth_route("anthropic", "cookie"),
        Some("accounts/cookie-auth")
    );
    assert_eq!(auth_route("gemini", "refresh"), None);
    for (kind, body) in [
        (
            "codex-session",
            json!({"content":"fixture-content","update_existing":true,"extra":{"openai_oauth_responses_websockets_v2_enabled":false},"credential_extras":{"model_mapping":{"alias":"fixture-model"}}}),
        ),
        ("codex-pat", json!({"access_token":"fixture-pat"})),
        (
            "grok-sso",
            json!({"sso_tokens":["fixture-sso"],"credentials":{"base_url":"https://example.invalid"}}),
        ),
    ] {
        validate(&["account-creation", "import", kind], &body).unwrap();
    }
    assert!(validate(
        &["account-creation", "import", "codex-session"],
        &json!({"content":"fixture","credential_extras":{"access_token":"fixture-disallowed"}})
    )
    .is_err());
    assert!(allowed(&["account-creation", "source", "0"]).is_none());
    assert!(allowed(&["account-creation", "auth", "openai", "../delete"]).is_none());
    assert!(allowed(&["account-creation", "catalog", "credentials"]).is_none());
}

#[test]
fn creation_invalid_values_fail_before_upstream_access() {
    let value = json!({"name":"Fixture","platform":"anthropic","type":"apikey","credentials":{},"extra":{}});
    for (path, bad) in [
        ("credentials.pool_mode_retry_count", json!(-1)),
        ("credentials.pool_mode_retry_status_codes", json!([99])),
        ("credentials.header_overrides", json!("invalid")),
        ("credentials.model_mapping", json!({"a":false})),
        ("extra.anthropic_apikey_auth_scheme", json!("unknown")),
        ("concurrency", json!(0)),
        ("rate_multiplier", json!(-1)),
        ("credentials", Value::Null),
    ] {
        let mut v = value.clone();
        insert(&mut v, path, bad);
        assert!(validate(&["account-creation"], &v).is_err(), "{path}");
    }
}

#[test]
fn model_warnings_are_projected_without_echoing_provider_data() {
    let value = json!({"warnings":[{"code":"upstream_model_metadata_partial","message":"fixture-secret"},{"code":"unknown","message":"fixture-secret"}]});
    let result = model_warnings(&value);
    assert_eq!(result.len(), 2);
    assert!(result[0].contains("部分"));
    assert!(!result.join(" ").contains("fixture-secret"));
}
