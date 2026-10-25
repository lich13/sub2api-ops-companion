use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sub2ops_server::{
    account_usage::{apply_native, provider},
    usage,
};

fn at() -> DateTime<Utc> {
    "2026-10-23T12:00:00Z".parse().unwrap()
}
fn row(platform: &str, kind: &str, mode: &str) -> Value {
    json!({"id":1,"platform":platform,"type":kind,"usage_account_mode":mode,"extra":{}})
}
fn native(row: &Value, data: Value) -> Value {
    let mut result = usage::project(row, at(), &Value::Null);
    apply_native(row, &mut result, &data, at()).unwrap();
    result
}

#[test]
fn oauth_cost_windows_use_actual_duration_and_never_start_in_the_future() {
    use chrono::Duration;
    use sub2ops_server::compat::{datetime, parse_datetime};
    for (key, minutes, label, fallback) in [
        ("codex_5h", json!(300), "5h", 300),
        ("codex_7d", json!(10080), "7d", 10080),
        ("codex_7d", json!(43200), "30d", 10080),
        ("codex_7d", json!(90), "90m", 10080),
        ("codex_7d", Value::Null, "7d", 10080),
        ("codex_7d", json!(-1), "7d", 10080),
        ("codex_7d", json!(5256001), "7d", 10080),
    ] {
        let duration = minutes
            .as_i64()
            .filter(|m| (1..=5256000).contains(m))
            .unwrap_or(fallback);
        let mut account = row("openai", "oauth", "");
        account["quota_plan_type"] = "plus".into();
        account["extra"][format!("{key}_used_percent")] = json!(32);
        account["extra"]["codex_usage_updated_at"] = datetime(at()).into();
        account["extra"][format!("{key}_window_minutes")] = minutes;
        for (reset, start) in [
            (
                json!(datetime(at() + Duration::minutes(duration - 30))),
                at() - Duration::minutes(30),
            ),
            (
                json!(datetime(at() + Duration::minutes(duration + 1))),
                at() - Duration::minutes(duration),
            ),
            (
                json!(datetime(at() - Duration::minutes(1))),
                at() - Duration::minutes(duration),
            ),
            (Value::Null, at() - Duration::minutes(duration)),
        ] {
            account["extra"][format!("{key}_reset_at")] = reset;
            let mut usage = usage::project(&account, at(), &Value::Null);
            let window = usage["windows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|w| w["key"] == key)
                .unwrap();
            assert_eq!(window["label"], label);
            assert_eq!(parse_datetime(&window["stats_start"]), Some(start));
            let stats = usage::stats_rows(vec![
                json!({"account_id":1,"key":key,"requests":2,"tokens":100,"cost":8,"standard_cost":9,"user_cost":10}),
            ]);
            usage::attach_stats(1, &mut usage, &stats, 500_000);
            let window = usage["windows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|w| w["key"] == key)
                .unwrap();
            assert_eq!(window["stats"]["cost"], 8.0);
            if key == "codex_7d" {
                assert_eq!(window["estimated_total_cost"], 25.0);
            }
        }
    }
}

#[test]
fn every_native_platform_and_valid_account_type_uses_its_upstream_source() {
    for (platform, kind, mode, source) in [
        ("anthropic", "apikey", "", ""),
        ("anthropic", "bedrock", "", ""),
        ("anthropic", "oauth", "", "anthropic"),
        ("anthropic", "setup-token", "", "anthropic"),
        ("openai", "apikey", "", ""),
        ("openai", "upstream", "", ""),
        ("openai", "oauth", "", ""),
        ("grok", "apikey", "", ""),
        ("grok", "oauth", "", ""),
        ("gemini", "apikey", "", "gemini"),
        ("gemini", "oauth", "", "gemini"),
        ("gemini", "service_account", "", "gemini"),
        ("antigravity", "oauth", "", "antigravity"),
        ("kimi", "apikey", "coding", "cn_quota"),
        ("kimi", "apikey", "payg", "cn_balance"),
        ("zhipu", "apikey", "coding", "cn_quota"),
        ("zhipu", "apikey", "payg", ""),
        ("minimax", "apikey", "coding", "cn_quota"),
        ("minimax", "apikey", "payg", ""),
        ("deepseek", "apikey", "payg", "cn_balance"),
        ("opencode_go", "apikey", "go", "opencode"),
        ("opencode_go", "apikey", "zen", ""),
    ] {
        assert_eq!(
            provider(&row(platform, kind, mode)),
            source,
            "{platform}/{kind}/{mode}"
        );
    }
    for platform in [
        "openai",
        "anthropic",
        "kimi",
        "zhipu",
        "deepseek",
        "minimax",
    ] {
        let mut account = row(platform, "apikey", "coding");
        for (url, source) in [
            ("https://ollama.com/v1", "ollama"),
            ("https://www.ollama.com/", "ollama"),
            ("https://opencode.ai/zen/go/v1/", "opencode"),
            ("https://opencode.ai/ZEN/GO", "opencode"),
        ] {
            account["usage_base_url"] = url.into();
            assert_eq!(provider(&account), source);
        }
        for url in [
            "https://example.invalid/v1",
            "https://ollama.com.example.invalid/v1",
            "https://opencode.ai/zen/gone",
            "https://opencode.ai/zen/go?token=fixture",
            "http://ollama.com/v1",
        ] {
            account["usage_base_url"] = url.into();
            assert!(!matches!(provider(&account), "ollama" | "opencode"));
        }
    }
}

#[test]
fn ordinary_accounts_have_today_stats_without_a_platform_whitelist_and_keep_account_ownership() {
    for platform in [
        "anthropic",
        "openai",
        "grok",
        "gemini",
        "kimi",
        "zhipu",
        "minimax",
        "deepseek",
        "opencode_go",
        "future",
    ] {
        for kind in ["apikey", "upstream", "bedrock", "service_account"] {
            let mut account = row(platform, kind, "payg");
            account["parent_account_id"] = 2.into();
            account["extra"] = json!({"quota_daily_limit":10,"quota_daily_used":0,"quota_weekly_limit":100,"quota_weekly_used":5,"quota_limit":200,"quota_used":20});
            let mut result = usage::project(&account, at(), &Value::Null);
            let specs = usage::stats_specs(1, &result);
            assert_eq!(
                specs.last().unwrap(),
                &json!({"account_id":1,"key":"today","start_at":"2026-10-23T00:00:00+08:00"})
            );
            assert_eq!(result["windows"][0]["used_percent"], 0.0);
            assert_eq!(result["windows"][1]["used_percent"], 5.0);
            assert_eq!(result["windows"][2]["used_percent"], 10.0);
            let stats = usage::stats_rows(vec![
                json!({"account_id":1,"key":"today","requests":0,"tokens":0,"cost":0,"standard_cost":0,"user_cost":0}),
                json!({"account_id":2,"key":"today","requests":99}),
            ]);
            usage::attach_stats(1, &mut result, &stats, 500_000);
            assert_eq!(result["today"]["requests"], 0);
            assert_eq!(result["today"]["cost"], 0.0);
            assert!(!result.to_string().contains("parent_account_id"));
        }
    }
    for (stamp, day) in [
        ("2026-10-22T15:59:59Z", "2026-10-22"),
        ("2026-10-22T16:00:00Z", "2026-10-23"),
    ] {
        let result = usage::project(
            &row("anthropic", "apikey", ""),
            stamp.parse().unwrap(),
            &Value::Null,
        );
        assert_eq!(result["today_start"], format!("{day}T00:00:00+08:00"));
    }
}

#[test]
fn native_windows_preserve_zero_percent_source_timestamps_and_estimates() {
    let stats = json!({"requests":2,"tokens":100,"cost":0.1,"standard_cost":0.2,"user_cost":0.3,"private":"fixture-hidden"});
    let mut data = json!({"source":"passive","updated_at":"2026-10-23T11:55:00Z","credentials":{"api_key":"fixture-hidden"}});
    for key in [
        "five_hour",
        "seven_day",
        "seven_day_sonnet",
        "seven_day_fable",
    ] {
        data[key] =
            json!({"utilization":0,"resets_at":"2026-10-23T15:00:00Z","window_stats":stats});
    }
    for kind in ["oauth", "setup-token"] {
        let result = native(&row("anthropic", kind, ""), data.clone());
        assert_eq!(result["windows"].as_array().unwrap().len(), 4);
        for w in result["windows"].as_array().unwrap() {
            assert_eq!(w["used_percent"], 0.0);
            assert_eq!(w["status"], "known");
            assert_eq!(w["source"], "passive");
            assert_eq!(w["stats"]["user_cost"], 0.3);
        }
        assert!(!result.to_string().contains("fixture-hidden"));
    }
    let missing = native(
        &row("anthropic", "oauth", ""),
        json!({"source":"passive","five_hour":{"utilization":0}}),
    );
    assert_eq!(missing["status"], "unavailable");
    let mut gemini = json!({"updated_at":"2026-10-23T11:59:50Z"});
    for key in [
        "gemini_shared_daily",
        "gemini_pro_daily",
        "gemini_flash_daily",
        "gemini_shared_minute",
        "gemini_pro_minute",
        "gemini_flash_minute",
    ] {
        gemini[key] =
            json!({"utilization":12.5,"resets_at":"2026-10-23T12:01:00Z","window_stats":stats});
    }
    for kind in ["oauth", "apikey", "service_account"] {
        let result = native(&row("gemini", kind, ""), gemini.clone());
        assert_eq!(result["windows"].as_array().unwrap().len(), 6);
        assert!(result["windows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["estimated"] == true && w["source"] == "local_estimate"));
    }
}

#[test]
fn passive_anthropic_and_stale_native_data_do_not_become_fresh_zeroes() {
    let mut account = row("anthropic", "setup-token", "");
    account["usage_session_window_end"] = "2026-10-23T15:00:00Z".into();
    account["extra"] = json!({"session_window_utilization":0,"passive_usage_sampled_at":"2026-10-23T11:59:00Z","passive_usage_7d_utilization":0.5,"passive_usage_7d_reset":1793361600});
    let result = usage::project(&account, at(), &Value::Null);
    assert_eq!(result["windows"][0]["used_percent"], 0.0);
    assert_eq!(result["windows"][1]["used_percent"], 50.0);
    let mut result = native(
        &account,
        json!({"updated_at":"2026-10-23T10:00:00Z","seven_day":{"utilization":42,"resets_at":"2026-10-24T12:00:00Z"}}),
    );
    assert_eq!(result["windows"][0]["status"], "stale");
    apply_native(
        &account,
        &mut result,
        &json!({"error":"fixture-hidden","error_code":"unauthenticated","needs_reauth":true}),
        at(),
    )
    .unwrap();
    assert_eq!(result["status"], "authorization_required");
    assert_eq!(result["windows"][0]["used_percent"], 42.0);
    assert_eq!(result["windows"][0]["status"], "error");
    assert!(!result.to_string().contains("fixture-hidden"));
    let unknown = native(
        &account,
        json!({"seven_day":{"utilization":"invalid","resets_at":"invalid"}}),
    );
    assert_eq!(unknown["windows"][0]["used_percent"], Value::Null);
    assert_eq!(unknown["windows"][0]["status"], "unknown");
}

#[test]
fn subscription_quotas_multicurrency_balances_and_cloud_authorization_are_projected() {
    for platform in ["kimi", "zhipu", "minimax"] {
        let result = native(
            &row(platform, "apikey", "coding"),
            json!({"success":true,"fetched_at":at().timestamp(),"tiers":[{"window":"5h","used_percent":0,"reset_at":"2026-10-23T17:00:00Z"},{"window":"weekly","used_percent":10},{"window":"monthly","used_percent":20}]}),
        );
        assert_eq!(result["windows"].as_array().unwrap().len(), 3);
        assert_eq!(result["windows"][0]["status"], "known");
        for status in [401, 403] {
            assert_eq!(
                native(
                    &row(platform, "apikey", "coding"),
                    json!({"success":false,"credential_valid":false,"status_code":status,"error":"fixture-hidden"}),
                )["status"],
                "authorization_required"
            );
        }
    }
    for platform in ["kimi", "deepseek"] {
        let result = native(
            &row(platform, "apikey", "payg"),
            json!({"success":true,"available":false,"fetched_at":at().timestamp(),"balances":[{"currency":"USD","balance":0},{"currency":"CNY","balance":3.5}]}),
        );
        assert_eq!(result["balances"][0]["balance"], 0.0);
        assert_eq!(result["balances"][1]["currency"], "CNY");
        assert_eq!(result["balances"][1]["status"], "known");
    }
    let result = native(
        &row("antigravity", "oauth", ""),
        json!({"updated_at":"2026-10-23T12:00:00Z","antigravity_quota":{"fixture-model":{"utilization":33,"reset_time":"2026-10-24T12:00:00Z"}},"ai_credits":[{"credit_type":"GOOGLE_ONE_AI","amount":0}]}),
    );
    assert_eq!(result["windows"][0]["used_percent"], 33.0);
    assert_eq!(result["balances"][0]["currency"], "GOOGLE_ONE_AI");
    let mut ollama = row("openai", "apikey", "");
    ollama["usage_base_url"] = "https://ollama.com/v1".into();
    let snapshot = json!({"status":"ok","fetched_at":"2026-10-23T12:00:00Z","data":{"five_hour":{"used_percent":0,"reset_at":"2026-10-23T17:00:00Z"},"seven_day":{"used_percent":25}}});
    let result = native(&ollama, json!({"configured":true,"snapshot":snapshot}));
    assert_eq!(result["windows"].as_array().unwrap().len(), 2);
    assert_eq!(
        native(&ollama, json!({"configured":false}))["status"],
        "authorization_required"
    );
    let result = native(
        &row("opencode_go", "apikey", "go"),
        json!({"snapshot":{"status":"ok","fetched_at":"2026-10-23T12:00:00Z","data":{"rolling":{"percent":1},"weekly":{"percent":2},"monthly":{"percent":3}}}}),
    );
    assert_eq!(result["windows"].as_array().unwrap().len(), 3);
    let missing = native(
        &row("opencode_go", "apikey", "go"),
        json!({"snapshot":{"status":"ok","fetched_at":"2026-10-23T12:00:00Z","data":{"rolling":{"status":"","percent":0,"resets_at":"0001-01-01T00:00:00Z"},"weekly":{"status":"","percent":0,"resets_at":"0001-01-01T00:00:00Z"},"monthly":{"status":"","percent":0,"resets_at":"0001-01-01T00:00:00Z"}}}}),
    );
    assert_eq!(missing["status"], "unavailable");
    assert!(missing["windows"].as_array().unwrap().is_empty());
    for status in ["failed", "unauthorized"] {
        let result = native(
            &row("opencode_go", "apikey", "go"),
            json!({"snapshot":{"status":status}}),
        );
        assert_eq!(
            result["status"],
            if status == "failed" {
                "error"
            } else {
                "authorization_required"
            }
        );
    }
}
