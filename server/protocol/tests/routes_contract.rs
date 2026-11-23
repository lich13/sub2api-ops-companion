use serde_json::{json, Value};
use sub2ops_protocol::{
    creation::authorization_url, response::client_response, routes::client_endpoint,
};

fn allowed(method: &str, path: &str) -> bool {
    client_endpoint(method, path).is_some()
}

fn valid_get_or_post(path: &str, v: &Value) -> bool {
    ["GET", "POST"]
        .iter()
        .any(|m| client_response(m, path, v.clone()).is_some())
}

#[test]
fn account_quota_projects_only_presence_and_revision() {
    let mut raw = json!({"account_id":1,"has_access_token":true,"revision":"a".repeat(64)});
    let path = "/accounts/1/key-quota-settings";
    let public = raw.clone();
    raw["access_token"] = "fixture".into();
    assert_eq!(client_response("GET", path, raw.clone()), Some(public));
    assert!(client_response("GET", "/accounts/2/key-quota-settings", raw).is_none());
}

#[test]
fn exact_priority_routes_and_task_identity_are_required() {
    let id = "a".repeat(32);
    for (method, path) in [
        ("GET", "/account-priorities".to_owned()),
        ("PUT", "/account-priorities".to_owned()),
        ("POST", "/account-priority-orders".to_owned()),
        ("GET", format!("/account-priority-orders/{id}")),
        ("POST", format!("/account-priority-orders/{id}/cancel")),
    ] {
        assert!(allowed(method, &format!("{path}?schema=2")));
        assert!(!allowed(method, &format!("{path}/?schema=2")));
    }
    for (method, path) in [
        ("GET", "/account-priority-orders?schema=2".to_owned()),
        ("POST", "/account-priorities?schema=2".to_owned()),
        ("PUT", format!("/account-priority-orders/{id}?schema=2")),
        (
            "GET",
            "/account-priority-orders/invalid?schema=2".to_owned(),
        ),
    ] {
        assert!(!allowed(method, &path));
    }
}

#[test]
fn platform_protocol_validates_summary_detail_and_identity() {
    let id = "a".repeat(32);
    assert!(allowed(
        "GET",
        &format!("/account-priority-orders/{id}?details=0&schema=2")
    ));
    let summary = json!({"id":id,"platform":"openai","base":1,"waiting_reason":"等待账号空闲","waiting_account_ids":[1],"status":"running","cancel_requested":false,"reason":"正在保存","total":1,"change_count":1,"verified_count":0,"created_at":"2026-10-21T00:00:00Z","updated_at":"2026-10-21T00:00:00Z","completed_at":null});
    let path = format!("/account-priority-orders/{id}");
    let overview = format!("{path}?schema=2&details=0");
    assert!(client_response("GET", &overview, summary.clone()).is_some());
    assert!(client_response("GET", &path, summary.clone()).is_none());
    let mut detail = summary.clone();
    detail["items"] = json!([{"account_id":1,"account_name":"fixture","platform":"openai","original_priority":8,"target_priority":1,"observed_priority":null,"status":"pending"}]);
    assert!(client_response("GET", &path, detail.clone()).is_some());
    assert!(client_response("GET", &path.replace(&id, &"b".repeat(32)), detail.clone()).is_none());
    detail["items"][0]["platform"] = "grok".into();
    assert!(client_response("GET", &path, detail).is_none());

    let mut view = json!({"platforms":[{"platform":"openai","version":"b".repeat(64),"items":[{"id":1,"name":"fixture","platform":"openai","type":"oauth","priority":8,"version":"c".repeat(64),"editable":true,"reason":"","status":"active","schedulable":true}],"last_task":summary}],"active_task":summary});
    let original = view.clone();
    view["platforms"][0]["items"][0]["credentials"] = json!({"unexpected":"fixture"});
    assert_eq!(
        client_response("GET", "/account-priorities?schema=2", view.clone()),
        Some(original)
    );
    let row = view["platforms"][0]["items"][0].clone();
    view["platforms"][0]["items"]
        .as_array_mut()
        .unwrap()
        .push(row);
    assert!(client_response("GET", "/account-priorities?schema=2", view).is_none());
}

#[test]
fn management_routes_and_credentials_are_explicit() {
    for (m, p) in [
        ("GET", "/proxy-management?page=1&search=fixture"),
        ("POST", "/proxy-management/batch"),
        ("PUT", "/proxy-management/7"),
        ("GET", "/proxy-management/7/credentials"),
        ("GET", "/proxy-management/data?ids=7%2C8"),
    ] {
        assert!(allowed(m, p));
    }
    for (m, p) in [
        ("GET", "/proxy-management/0"),
        ("POST", "/proxy-management/7/credentials"),
        ("DELETE", "/proxy-management/data"),
    ] {
        assert!(!allowed(m, p));
    }
    let mut r = json!({"id":7,"name":"fixture","host":"127.0.0.1","port":1080,"protocol":"socks5","status":"active","username_set":true,"password_set":true});
    assert!(client_response("GET", "/proxy-management/7", r.clone()).is_some());
    r["password"] = "fixture".into();
    assert!(client_response("GET", "/proxy-management/7", r)
        .unwrap()
        .get("password")
        .is_none());
    assert!(client_response(
        "GET",
        "/proxy-management/7/credentials",
        json!({"username":"fixture","password":"fixture"})
    )
    .is_some());
}

#[test]
fn fixed_routes_and_response_boundaries() {
    assert!(allowed("POST", "/account-creation?sync_models=true"));
    assert!(!allowed("GET", "/account-creation?sync_models=true"));
    for (provider, actions) in [
        ("anthropic", vec!["authorize", "exchange", "cookie"]),
        ("setup-token", vec!["authorize", "exchange", "cookie"]),
        ("openai", vec!["authorize", "exchange", "refresh"]),
        ("gemini", vec!["authorize", "exchange"]),
        ("antigravity", vec!["authorize", "exchange", "refresh"]),
        ("grok", vec!["authorize", "exchange", "refresh"]),
    ] {
        for action in actions {
            let path = format!("/account-creation/auth/{provider}/{action}");
            assert!(allowed("POST", &path));
            assert!(!allowed("GET", &path));
        }
    }
    for path in [
        "/account-creation/unknown",
        "/account-creation/source/0",
        "/account-creation/auth/gemini/refresh",
        "/account-creation/auth/openai/../delete",
    ] {
        assert!(!allowed("GET", path));
        assert!(!allowed("POST", path));
    }
    let source = json!({"platform":"openai","type":"apikey","credentials":{"model_mapping":{"a":"b"},"pool_mode":false},"extra":{"quota_limit":0},"proxy_id":null});
    assert!(valid_get_or_post("/account-creation/source/1", &source));
    for key in [
        "access_token",
        "api_key",
        "refresh_token",
        "email",
        "private_key",
    ] {
        let mut bad = source.clone();
        bad["credentials"][key] = json!("fixture");
        let clean = client_response("GET", "/account-creation/source/1", bad).unwrap();
        assert!(clean["credentials"].get(key).is_none());
    }
    let mut bad = source;
    bad["extra"]["quota_used"] = json!(1);
    let clean = client_response("GET", "/account-creation/source/1", bad).unwrap();
    assert!(clean["extra"].get("quota_used").is_none());
    assert!(valid_get_or_post(
        "/account-creation",
        &json!({"account":{"id":1,"name":"Fixture","platform":"openai","type":"apikey"},"warnings":[]})
    ));
    assert!(!valid_get_or_post(
        "/account-creation",
        &json!({"account":{"id":0,"name":"Fixture","platform":"openai","type":"apikey"},"warnings":[]})
    ));
    assert!(valid_get_or_post(
        "/account-creation/import/codex-session",
        &json!({"created":1,"updated":0,"skipped":0,"failed":1,"errors":[],"warnings":[]})
    ));
}

#[test]
fn browser_authorization_is_https_without_embedded_credentials() {
    assert!(authorization_url(
        "https://example.invalid/authorize?state=fixture"
    ));
    for value in [
        "javascript:alert(1)",
        "file:///fixture",
        "http://example.invalid/authorize",
        "https://user:fixture@example.invalid/authorize",
    ] {
        assert!(!authorization_url(value));
    }
}

#[test]
fn client_route_allowlist_covers_configuration_settings_keepalive_proxies_errors_templates_and_batches(
) {
    for (method, path) in [
        ("GET", "/sub2api-config"),
        ("GET", "/sub2api-config/operation"),
        ("POST", "/sub2api-config/validate"),
        ("POST", "/sub2api-config/apply"),
        ("GET", "/accounts/7/settings"),
        ("GET", "/proxies"),
        ("GET", "/accounts/7/keepalive"),
        ("PUT", "/accounts/7/keepalive"),
        ("POST", "/proxies/7/test"),
        ("POST", "/proxies/7/quality-check"),
        ("GET", "/errors"),
        ("GET", "/errors?category=degradation"),
        ("GET", "/errors?category=other&before_id=25&limit=50"),
        ("GET", "/errors/25?category=other"),
        ("POST", "/account-templates"),
        ("GET", "/account-templates"),
        ("GET", "/account-templates?account_id=421"),
        ("POST", "/account-templates/apply"),
        ("POST", "/account-operations/batch"),
        ("POST", "/accounts/7/apply-oauth-credentials"),
        ("GET", "/accounts/421/model-detection"),
        ("PUT", "/accounts/421/model-detection"),
        ("GET", "/modeltrace/fingerprint-bank"),
        ("POST", "/modeltrace/fingerprint-bank/sync"),
        ("GET", "/lm-detector/fingerprint-bank"),
        ("POST", "/lm-detector/fingerprint-bank/sync"),
        ("GET", "/accounts/7/models"),
        ("GET", "/accounts/7/models?purpose=model_test&refresh=true"),
        ("GET", "/accounts/7/models?refresh=true&purpose=model_test"),
        ("GET", "/accounts/7/models?refresh=false"),
        ("GET", "/accounts/7/key-quota-settings/access-token"),
        ("GET", "/config/key-quota"),
        ("GET", "/accounts/7/usage"),
        ("GET", "/usage-records?limit=50&after_id=12"),
        ("GET", "/usage-record-options?kind=api_keys&user_id=7"),
        ("GET", "/usage-records/12"),
        ("GET", "/connection-status"),
        ("GET", "/detection-logs?before=YWJj&limit=100"),
        ("POST", "/accounts/421/operations"),
        ("GET", "/account-operations"),
    ] {
        assert!(allowed(method, path), "{method} {path}");
    }
    let id = "0123456789abcdef0123456789abcdef";
    for (method, path) in [
        ("GET", format!("/account-operations?batch_id={id}")),
        ("POST", format!("/account-operations/batches/{id}/cancel")),
        ("GET", format!("/account-operations/{id}")),
        ("GET", format!("/account-operations/{id}?after_event=100")),
        ("POST", format!("/account-operations/{id}/cancel")),
        ("POST", format!("/model-tests/{id}/retry")),
    ] {
        assert!(allowed(method, &path), "{method} {path}");
    }
    let custom_id = "custom-0123456789abcdef01234567";
    for method in ["PUT", "DELETE"] {
        assert!(allowed(method, &format!("/account-templates/{custom_id}")));
        for legacy in ["full", "degraded", "takeover"] {
            assert!(allowed(method, &format!("/account-templates/{legacy}")));
        }
        for bad in [
            "unknown",
            "custom-not-an-id",
            "custom-0123456789ABCDEF01234567",
        ] {
            assert!(!allowed(method, &format!("/account-templates/{bad}")));
        }
    }
    for (method, path) in [
        ("DELETE", "/sub2api-config"),
        ("POST", "/sub2api-config/operation"),
        ("POST", "/sub2api-config/restart"),
        ("POST", "/accounts/7/settings"),
        ("GET", "/accounts/0/settings"),
        ("GET", "/accounts/-1/settings"),
        ("GET", "/accounts/a/settings"),
        ("GET", "/accounts/7/settings/"),
        ("GET", "/accounts/7/settings/../credentials"),
        ("GET", "/accounts/7%2fsettings"),
        ("GET", "/proxies/all"),
        ("GET", "/accounts/18446744073709551616/settings"),
        ("POST", "/accounts/7/keepalive"),
        ("DELETE", "/accounts/7/keepalive"),
        ("GET", "/accounts/0/keepalive"),
        ("GET", "/accounts/+7/keepalive"),
        ("GET", "/accounts/7/keepalive/"),
        ("GET", "/proxies/7/test"),
        ("POST", "/proxies/0/test"),
        ("POST", "/proxies/7/unknown"),
        ("POST", "/errors"),
        ("GET", "/errors/0?category=other"),
        ("GET", "/errors/25?category=other#fragment"),
        ("POST", "/account-templates/custom"),
        ("GET", "/account-templates/full"),
        ("PUT", "/account-templates"),
        ("PUT", "/account-templates/apply"),
        ("GET", "/account-operations/batch"),
        ("POST", "/account-operations/batch/"),
        ("POST", "/account-operations/batches/invalid/cancel"),
        (
            "POST",
            "/account-operations/batches/0123456789ABCDEF0123456789ABCDEF/cancel",
        ),
        ("GET", "/accounts/7/apply-oauth-credentials"),
        ("POST", "/accounts/0/apply-oauth-credentials"),
        ("PUT", "/modeltrace/fingerprint-bank"),
        ("PUT", "/lm-detector/fingerprint-bank"),
        ("GET", "/snapshot"),
        ("GET", "/capabilities"),
        ("GET", "//example.invalid/config"),
        ("GET", "https://example.invalid/config"),
        ("GET", "/config/../snapshot"),
        ("GET", "/accounts/7/%2e%2e/settings"),
        ("GET", "/config#fragment"),
        ("GET", "/accounts/0/key-quota-settings/access-token"),
        ("POST", "/accounts/7/key-quota-settings/access-token"),
        ("GET", "/usage-records/12/credentials"),
        ("POST", "/accounts/7/recover-state"),
        ("PUT", "/accounts/7/groups"),
        ("DELETE", "/accounts/7"),
        ("POST", "/detection-logs"),
        ("GET", "/detection-logs/1"),
        ("GET", "/model-tests/invalid"),
        ("POST", "/model-tests/invalid/cancel"),
        ("GET", "/account-operations/invalid"),
    ] {
        assert!(!allowed(method, path), "{method} {path}");
    }
}
