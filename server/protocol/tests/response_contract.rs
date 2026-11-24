use serde_json::{json, Value};
use sub2ops_protocol::{
    response::client_response,
    semantics::{operation_valid, recoveries_valid},
};

fn accepts_reply(path: &str, value: &Value) -> bool {
    ["GET", "POST", "PUT", "DELETE"]
        .iter()
        .any(|method| client_response(method, path, value.clone()).is_some())
}

fn connection_valid(data: &Value) -> bool {
    accepts_reply("/connection-status", data)
}

#[test]
fn access_token_reveal_requires_the_requested_id_and_a_nonempty_token() {
    let path = "/accounts/7/key-quota-settings/access-token";
    assert!(accepts_reply(
        path,
        &json!({"account_id":7,"access_token":"fixture-token"})
    ));
    let public = client_response(
        "GET",
        path,
        json!({"account_id":7,"access_token":"fixture-token","extra":"unexpected"}),
    )
    .unwrap();
    assert!(public.get("extra").is_none());
    for value in [
        json!({"account_id":8,"access_token":"fixture-token"}),
        json!({"account_id":7,"access_token":""}),
    ] {
        assert!(!accepts_reply(path, &value));
    }
}

#[test]
fn native_usage_summary_validates_balances_windows_and_missing_values() {
    let data = json!({"branch":"native","native_read":true,"status":"known","actions":["query_usage"],"today":null,
        "windows":[{"key":"shared","label":"共享 1d","source":"local_estimate","estimated":true,"status":"known","used_percent":0,"observed_at":null,"reset_at":null}],
        "balances":[{"currency":"CNY","balance":0,"status":"stale","observed_at":null}]});
    assert!(accepts_reply("/accounts/1/usage", &data));
    assert!(accepts_reply(
        "/quota-refresh",
        &json!({"status":"completed","items":[{"usage":data}]})
    ));
    assert!(!accepts_reply(
        "/quota-refresh",
        &json!({"status":"completed","items":[{"usage":{"windows":[]}}]})
    ));
    for (field, value) in [
        ("balances", json!([{"currency":"USD","balance":"unknown"}])),
        (
            "windows",
            json!([{"key":"x","label":"x","source":"passive","status":"known","used_percent":"unknown"}]),
        ),
        ("today", json!({"requests":1})),
    ] {
        let mut invalid = data.clone();
        invalid[field] = value;
        assert!(!accepts_reply("/accounts/1/usage", &invalid));
    }
    for status in ["error", "unavailable", "authorization_required"] {
        let mut data = data.clone();
        data["status"] = status.into();
        assert!(accepts_reply("/accounts/1/usage", &data));
    }
}

#[test]
fn capabilities_accepts_existing_and_android_clients() {
    for platforms in [json!(["macos"]), json!(["macos", "android"])] {
        assert!(accepts_reply(
            "/capabilities",
            &json!({
                "api_version":1,"service":"Sub2API Ops Companion","version":"fixture","implementation":"rust","build_revision":"fixture","upstream_connection":{"state":"ok","endpoint":"none","http_status":null,"message":"","retryable":false,"checked_at":null}, "platforms":platforms
            })
        ));
    }
}

#[test]
fn proxy_diagnostic_responses_require_safe_known_shapes() {
    let test = json!({
        "success": true, "message": format!("https://example.invalid/?token=fixture\n  password=fixture\n{}", "原始错误".repeat(1000)), "latency_ms": 31,
        "ip_address": "203.0.113.7", "city": "Taipei", "region": "Taiwan",
        "country": "Taiwan", "country_code": "TW"
    });
    assert!(accepts_reply("/proxies/7/test", &test));
    for (key, value) in [
        ("success", json!("true")),
        ("latency_ms", json!("slow")),
        ("ip_address", json!(7)),
    ] {
        let mut bad = test.clone();
        bad[key] = value;
        assert!(!accepts_reply("/proxies/7/test", &bad), "{key}");
    }

    let quality = json!({
        "score": 88, "grade": "yellow", "summary": "完整质量报告".repeat(1000),
        "exit_ip": "203.0.113.8", "country": "Taiwan", "country_code": "TW",
        "base_latency_ms": 20, "passed_count": 1, "warn_count": 0,
        "failed_count": 0, "challenge_count": 0, "checked_at": null,
        "items": [{"target": "https://example.invalid", "status": "pass",
            "http_status": 200, "latency_ms": 20, "message": "ok"}]
    });
    assert!(accepts_reply("/proxies/7/quality-check", &quality));
    let mut bad = quality.clone();
    bad["score"] = json!("invalid");
    assert!(!accepts_reply("/proxies/7/quality-check", &bad));
    let mut bad = quality.clone();
    bad["items"][0]["status"] = json!(0);
    assert!(!accepts_reply("/proxies/7/quality-check", &bad));
    let mut bad = quality.clone();
    bad["items"][0]["password"] = json!("fixture-secret");
    assert_eq!(
        client_response("POST", "/proxies/7/quality-check", bad),
        Some(quality)
    );
}

#[test]
fn recovery_response_accepts_legacy_and_complete_card_recovery_metadata() {
    let legacy = json!([{"id":1,"account_id":1,"legacy":true}]);
    let ordinary = json!([{"id":2,"account_id":1,"kind":"quota_recovery","legacy":false}]);
    let connection_test = json!([{"id":4,"account_id":1,"kind":"connection_test","result_status":"completed","legacy":false}]);
    let card = json!([{"id":3,"account_id":1,"kind":"reset_credit","legacy":false,
        "test_completed_at":"2026-10-01T01:11:00Z","recovered_at":"2026-10-01T01:12:00Z",
        "reset_credit":{"completed_at":"2026-10-01T01:10:00Z","verification_method":"connection"}}]);

    assert!(recoveries_valid(&legacy));
    assert!(recoveries_valid(&ordinary));
    assert!(recoveries_valid(&connection_test));
    assert!(recoveries_valid(&card));
    assert!(accepts_reply("/recoveries", &json!({"items":card})));
}

#[test]
fn recovery_response_rejects_partial_or_invalid_card_recovery_metadata() {
    let valid = json!({"id":1,"account_id":1,"kind":"reset_credit","legacy":false,
        "test_completed_at":"2026-10-01T01:11:00Z","recovered_at":"2026-10-01T01:12:00Z",
        "reset_credit":{"completed_at":"2026-10-01T01:10:00Z","verification_method":"model"}});
    let mut invalid = Vec::new();
    for (path, val) in [
        (&["reset_credit"][..], Value::Null),
        (&["reset_credit", "completed_at"][..], json!("")),
        (
            &["reset_credit", "verification_method"][..],
            json!("unknown"),
        ),
        (&["reset_credit", "consumed"][..], json!(false)),
        (&["legacy"][..], json!(true)),
        (&["legacy"][..], Value::Null),
        (&["test_completed_at"][..], json!("")),
        (&["recovered_at"][..], Value::Null),
        (&["kind"][..], json!("unknown")),
    ] {
        let mut row = valid.clone();
        if path.len() == 2 {
            row[path[0]][path[1]] = val;
        } else {
            row[path[0]] = val;
        }
        invalid.push(row);
    }
    for row in invalid {
        assert!(!recoveries_valid(&json!([row])));
    }
    assert!(!recoveries_valid(&json!({"items":[]})));
}

#[test]
fn detection_platforms_and_templates_and_bank_response_guards() {
    let mut config = json!({"oauth":{"revision":"fixture","model_detection_model_id":"gpt-6-luna","oauth_early_reset_enabled":false,"oauth_early_reset_interval_seconds":60,"model_detection_platforms":{"openai":{"engine":"modeltrace","sample_count":1}}},"bark":{"revision":"fixture"}});
    assert!(accepts_reply("/config", &config));
    for platform in ["anthropic", "grok", "deepseek"] {
        config["oauth"]["model_detection_platforms"][platform] =
            json!({"engine":"lm_detector","sample_count":3});
    }
    assert!(accepts_reply("/config", &config));
    config["oauth"]["model_detection_platforms"]["anthropic"]["engine"] = "modeltrace".into();
    assert!(!accepts_reply("/config", &config));

    let detection = json!({"account_id":1,"version":"fixture","enabled":false,"interval_minutes":15,"schedule_mode":"fixed","min_interval_minutes":5,"model_id":"gpt-6-luna","model_source":"global","model_override":null,"global_model_id":"gpt-6-luna","status":"disabled"});
    assert!(accepts_reply("/accounts/1/model-detection", &detection));
    for field in [
        "schedule_mode",
        "min_interval_minutes",
        "model_source",
        "version",
        "global_model_id",
    ] {
        let mut missing = detection.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            !accepts_reply("/accounts/1/model-detection", &missing),
            "{field}"
        );
    }
    let mut smart = detection.clone();
    smart["schedule_mode"] = json!("smart");
    smart["effective_interval_minutes"] = json!(10);
    smart["recent_request_count"] = json!(77);
    smart["verdict_mode"] = json!("relaxed");
    smart["degraded_model_id"] = json!("gpt-5.6-luna");
    assert!(accepts_reply("/accounts/1/model-detection", &smart));
    for (field, value) in [
        ("schedule_mode", json!("other")),
        ("min_interval_minutes", json!(5.5)),
        ("effective_interval_minutes", json!("31")),
        ("recent_request_count", json!("many")),
        ("degraded_model_id", json!(false)),
        ("sample_count", json!("two")),
        ("model_override", json!(false)),
        ("model_source", Value::Null),
        ("last_automatic_started_at", json!(123)),
        ("interval_minutes", json!("0")),
    ] {
        let mut invalid = smart.clone();
        invalid[field] = value;
        assert!(!accepts_reply("/accounts/1/model-detection", &invalid));
    }
    let profile = json!({"whitelist":[],"mappings":[]});
    let templates = json!({"version":"fixture","configured":false,"templates":{"full":profile,"degraded":profile,"takeover":profile}});
    assert!(accepts_reply("/account-templates", &templates));
    assert!(!accepts_reply("/account-templates", &json!({})));
    let bank = json!({"version":{"revision":"fixture","sha256":"fixture","analyzer_version":1},"source":"bundled","status":"idle"});
    assert!(accepts_reply("/modeltrace/fingerprint-bank", &bank));
    assert!(!accepts_reply("/modeltrace/fingerprint-bank/sync", &json!({})));
}

#[test]
fn detection_log_and_system_update_and_reauthorization_and_keepalive_guards() {
    let valid_log = json!({
        "items":[{
            "job_id":"0123456789abcdef0123456789abcdef",
            "account_id":7,
            "account_name":"fixture account",
            "model_id":"gpt-6-sol",
            "status":"completed",
            "created_at":"2026-10-20T12:00:00Z",
            "started_at":null,
            "completed_at":"2026-10-20T12:00:00Z",
            "triggers":["scheduled"],
            "error":"",
            "details_available":false,
            "summary":{
                "outcome":"unavailable",
                "analysis_available":false,
                "first_prediction":"",
                "prediction":"",
                "probability":null,
                "first_probability":null,
                "used_outputs":null,
                "first_downgrade":null,
                "aggregate_downgrade":null,
                "disposition":null,
                "recovery":null,
                "verification":null,
                "restoration":null
            },
            "notification":{
                "status":"delivered",
                "merged":false,
                "delivered_at":"2026-10-20T12:00:01Z",
                "reason":""
            }
        }],
        "next_cursor":"eyJmaXh0dXJlIjoxfQ"
    });
    assert!(accepts_reply("/detection-logs", &valid_log));
    let mut sample_family_log = valid_log.clone();
    sample_family_log["items"][0]["summary"]["disposition"] = json!({
        "status":"marked",
        "basis":"sample_family",
        "group_indexes":[1, 2],
        "marked":true,
        "schedule_verified":true,
        "mark_cleared":null,
        "reason":"",
        "completed_at":"2026-10-20T12:00:00Z"
    });
    assert!(accepts_reply("/detection-logs", &sample_family_log));
    let mut bad_basis = sample_family_log;
    bad_basis["items"][0]["summary"]["disposition"]["basis"] = json!("unknown_basis");
    assert!(!accepts_reply("/detection-logs", &bad_basis));

    let model_test = json!({
        "id":"0123456789abcdef0123456789abcdef",
        "account_id":7,
        "account_name":"fixture account",
        "requested_model":"gpt-6-sol",
        "forwarded_model":"gpt-6-sol",
        "returned_models":["gpt-6-sol"],
        "status":"completed",
        "engine":"modeltrace",
        "automatic_disposition":{
            "basis":"sample_family",
            "group_indexes":[1],
            "status":"marked",
            "marked":true,
            "schedule_verified":true,
            "reason":""
        }
    });
    assert!(accepts_reply(
        "/model-tests/0123456789abcdef0123456789abcdef",
        &model_test
    ));
    let mut missing_account_id = model_test;
    missing_account_id.as_object_mut().unwrap().remove("account_id");
    assert!(!accepts_reply(
        "/model-tests/0123456789abcdef0123456789abcdef",
        &missing_account_id
    ));
    for mut bad in [
        {
            let mut r = valid_log.clone();
            r["items"][0]["status"] = json!("running");
            r
        },
        {
            let mut r = valid_log.clone();
            r["items"][0]["summary"]["probability"] = json!(1.01);
            r
        },
        {
            let mut r = valid_log.clone();
            r["items"][0]["job_id"] = json!("invalid");
            r
        },
        {
            let mut r = valid_log.clone();
            r["next_cursor"] = json!("");
            r
        },
    ] {
        assert!(!accepts_reply("/detection-logs", &bad));
        bad["items"] = json!([]);
    }

    assert!(accepts_reply(
        "/sub2api-system/update",
        &json!({"message":"Already up to date","already_up_to_date":true,"current_version":"0.2.25","latest_version":"0.2.25"})
    ));
    assert!(accepts_reply(
        "/sub2api-system/update",
        &json!({"message":"Updated","need_restart":true})
    ));
    assert!(!accepts_reply(
        "/sub2api-system/update",
        &json!({"message":"Unknown"})
    ));
    assert!(accepts_reply(
        "/sub2api-system/version",
        &json!({"version":"0.2.25"})
    ));

    let reauth_path = "/accounts/7/apply-oauth-credentials";
    let reauth = json!({"account_id":7,"verified":true,"status":"active"});
    assert!(accepts_reply(reauth_path, &reauth));
    let mut extra = reauth.clone();
    extra["credentials"] = json!({"access_token":"fixture-token"});
    assert_eq!(client_response("POST", reauth_path, extra), Some(reauth));
    assert!(!accepts_reply(
        "/accounts/8/apply-oauth-credentials",
        &json!({"account_id":7,"verified":true,"status":"active"})
    ));

    let ka_path = "/accounts/7/keepalive";
    let mut row = json!({
        "account_id":7,"enabled":true,"generation":1,"version":"a".repeat(64),
        "status":"waiting","reason":"","paused_reason":"",
        "restore_count":2,"budget_used":3,
        "next_allowed_at":"2026-10-15T12:31:00.123456+00:00","last_restore_at":"2026-10-15T12:30:00Z"
    });
    row["future_field"] = true.into();
    assert!(accepts_reply(ka_path, &row));
    for status in [
        "disabled",
        "waiting",
        "paused",
        "running",
        "needs_confirmation",
    ] {
        let mut s = row.clone();
        s["status"] = json!(status);
        assert!(accepts_reply(ka_path, &s), "{status}");
    }
    assert!(!accepts_reply("/accounts/8/keepalive", &row));
    for (key, value) in [
        ("enabled", json!("true")),
        ("version", Value::Null),
        ("version", json!("a".repeat(63))),
        ("version", json!("A".repeat(64))),
        ("generation", json!(-1)),
        ("generation", json!(1.5)),
        ("generation", json!(9_007_199_254_740_992u64)),
        ("status", json!("unknown")),
        ("restore_count", json!("2")),
        ("restore_count", json!(-1)),
        ("restore_count", json!(6)),
        ("budget_used", json!(6)),
        ("budget_used", json!(1)),
        ("next_allowed_at", json!(7)),
        ("next_allowed_at", json!("not-a-date")),
        ("last_restore_at", json!("2026-02-30T00:00:00Z")),
    ] {
        let mut invalid = row.clone();
        if key == "restore_count" && value == json!(6) {
            invalid["budget_used"] = json!(6);
        }
        invalid[key] = value;
        assert!(!accepts_reply(ka_path, &invalid), "{key}");
    }
}

#[test]
fn connection_operations_settings_proxies_batches_and_snapshot_guards() {
    let status = json!({"state":"auth_rejected","endpoint":"verify","http_status":403,
        "message":"管理员 API Key 被上游拒绝，请重新连接","retryable":false,"checked_at":"2026-10-07T00:00:00Z"});
    assert!(connection_valid(&status));
    let mut bad_conn = status.clone();
    bad_conn["endpoint"] = json!("https://private.invalid");
    assert!(!connection_valid(&bad_conn));
    bad_conn = status;
    bad_conn["state"] = json!("unknown");
    assert!(!connection_valid(&bad_conn));

    let job = json!({"id":"0123456789abcdef0123456789abcdef", "account_id":421,
        "action":"priority", "status":"queued", "requested":{"priority":2}});
    assert!(accepts_reply("/accounts/421/operations", &job));
    assert!(accepts_reply(
        "/account-operations",
        &json!({"pending":1,"items":[job.clone()]})
    ));
    let mut scheduled = job.clone();
    scheduled["action"] = "test".into();
    scheduled["scheduled_at"] = "2030-01-02T01:31:38+00:00".into();
    assert!(operation_valid(&scheduled));
    for invalid in [
        "2030-02-30T01:31:00+00:00",
        "2030-01-02T01:31:60+00:00",
        "2030-01-02T01:31:38.001+00:00",
        "invalid",
    ] {
        scheduled["scheduled_at"] = invalid.into();
        assert!(!operation_valid(&scheduled));
    }

    let mut settings = json!({"account_id":7,"priority":0,"concurrency":2,
        "load_factor":null,"effective_load_factor":2,"proxy_id":null,
        "model_config":{"whitelist":["fixture-text"],"mappings":[{"source":"fixture-*","target":"fixture-text"}]},
        "capabilities":{},"versions":{}});
    for action in [
        "priority",
        "concurrency",
        "load_factor",
        "model_config",
        "proxy",
        "sync_models",
    ] {
        settings["capabilities"][action] = json!({"editable":true,"reason":""});
        settings["versions"][action] = json!("a".repeat(64));
    }
    assert!(accepts_reply("/accounts/7/settings", &settings));
    let proxies = json!([{"id":7,"name":"fixture proxy","protocol":"socks5","status":"active"}]);
    for key in ["password", "username", "url", "host", "port"] {
        let mut unsafe_proxy = proxies.clone();
        unsafe_proxy[0][key] = json!("fixture");
        assert_eq!(
            client_response("GET", "/proxies", unsafe_proxy),
            Some(proxies.clone())
        );
    }

    let id = "0123456789abcdef0123456789abcdef";
    let empty_tpl = json!({"version":"fixture", "configured":true, "templates":{},
        "template_order":[],"template_meta":{},"template_versions":{}});
    assert!(accepts_reply("/account-templates/full", &empty_tpl));
    let tpl_job = json!({"id":id,"batch_id":id,"account_id":1,"action":"account_template","status":"completed","requested":{}});
    assert!(accepts_reply(
        "/account-templates/apply",
        &json!({"batch_id":id,"pending":0,"items":vec![tpl_job; 4] })
    ));

    let proxy_batch = json!({"batch_id":id,"pending":1,"items":[{"id":id,"batch_id":id,"account_id":1,"action":"proxy","status":"queued","requested":{"proxy_id":null,"extra":true}}]});
    let projected = client_response("POST", "/account-operations/batch", proxy_batch).unwrap();
    assert_eq!(projected["items"][0]["requested"], json!({"proxy_id":null}));

    let groups_batch = json!({"batch_id":id,"pending":0,"items":[{"id":id,"batch_id":id,"account_id":7,"action":"groups","status":"completed","requested":{"scope_group_ids":[1,2,9],"group_ids":[1,9]}}]});
    assert!(accepts_reply("/account-operations/batch", &groups_batch));
    let mut outside = groups_batch;
    outside["items"][0]["requested"]["group_ids"] = json!([3]);
    assert!(!accepts_reply("/account-operations/batch", &outside));

    let conn_batch = json!({"batch_id":id,"pending":0,"items":[{"id":"abcdefabcdefabcdefabcdefabcdefab","batch_id":id,"account_id":7,"action":"test","status":"completed","requested":{"confirmed":true,"mode":"text","model_id":"fixture-model"},"result":{"success":true,"requested_model":"fixture-model","actual_model":"fixture-mapped-model","duration_ms":12,"completed_at":"2026-10-13T00:00:00Z","error":null,"uncertain":false}}]});
    assert!(accepts_reply("/account-operations/batch", &conn_batch));
    let mut bad_conn_batch = conn_batch.clone();
    bad_conn_batch["items"][0]["requested"]["confirmed"] = json!(false);
    assert!(!accepts_reply("/account-operations/batch", &bad_conn_batch));
    let mut recover_batch = conn_batch;
    recover_batch["items"][0]["action"] = json!("recover");
    recover_batch["items"][0]["requested"] = json!({});
    recover_batch["items"][0]["result"] = json!({"verified":true,"unchanged":true});
    assert!(accepts_reply("/account-operations/batch", &recover_batch));

    let account = json!({
        "id": 1, "name": "fixture", "platform": "openai", "type": "oauth", "version": "v1",
        "group_ids": [], "usage_windows": [], "blockers": [], "schedulable": true,
        "available": true, "priority": 0, "operation_versions": {"test": "v1"}
    });
    let mut snapshot = json!({
        "observed_at": "2026-10-11T00:00:00Z", "accounts": [account], "groups": [],
        "errors": [], "recoveries": []
    });
    assert!(accepts_reply("/snapshot", &snapshot));
    snapshot["accounts"][0]["recovery_monitor"] = json!(3600);
    assert!(!accepts_reply("/snapshot", &snapshot));
}
