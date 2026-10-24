use super::*;
use sub2ops_server::account_creation::{auth_route, SCHEMA};

pub(super) fn mock(method: &Method, path: &str, body: &Value) -> Option<Response> {
    if method == Method::GET && path == "/api/v1/admin/settings/web-search-emulation" {
        return Some(response(
            StatusCode::OK,
            json!({"code":0,"data":{"enabled":true,"providers":[],"api_key":"fixture-secret"}}),
        ));
    }
    if method != Method::POST {
        return None;
    }
    let route = path.strip_prefix("/api/v1/admin/")?;
    let data = if route == "accounts" {
        if body["name"] == "fixture-rejected" {
            return Some(response(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"code":422,"message":"fixture-secret-echo"}),
            ));
        }
        if body["name"] == "fixture-unknown" {
            return Some(response(
                StatusCode::BAD_GATEWAY,
                json!({"code":502,"message":"fixture-secret-echo"}),
            ));
        }
        json!({"id":777,"name":body["name"],"platform":body["platform"],"type":body["type"],"credentials":{"access_token":"fixture-native-secret"},"extra":{"private":"fixture-private"}})
    } else if route == "accounts/check-mixed-channel" {
        json!({"has_risk":true,"private":"fixture-private"})
    } else if route == "accounts/777/models/sync-upstream"
        || route == "accounts/777/upstream-billing-probe"
    {
        json!({})
    } else if route == "accounts/models/sync-upstream-preview" {
        json!({"models":["fixture-model"],"warnings":[]})
    } else if route == "accounts/import/codex-session" {
        json!({"created":1,"updated":1,"skipped":0,"failed":1,"errors":[{"index":3,"message":"fixture-secret-echo"}],"warnings":[]})
    } else if route == "openai/create-from-codex-pat" {
        json!({"id":777,"name":"Fixture","platform":"openai","type":"oauth","credentials":{"access_token":"fixture-native-secret"}})
    } else if route == "grok/sso-to-oauth" {
        json!({"created":[{"index":0,"account":{"id":777,"name":"Fixture","platform":"grok","type":"oauth","credentials":{"access_token":"fixture-native-secret"}}}],"failed":[{"index":1,"error":"fixture-secret-echo"}]})
    } else {
        let auth = [
            "anthropic",
            "setup-token",
            "openai",
            "gemini",
            "antigravity",
            "grok",
        ]
        .iter()
        .flat_map(|p| ["authorize", "exchange", "refresh", "cookie"].map(|a| (*p, a)))
        .find(|(p, a)| auth_route(p, a) == Some(route));
        let (_, action) = auth?;
        if action == "authorize" {
            json!({"auth_url":"https://example.invalid/authorize?state=fixture","session_id":"fixture-session","state":"fixture","private":"fixture-native-secret"})
        } else {
            json!({"access_token":"fixture-access","refresh_token":"fixture-refresh","expires_at":2000000000,"extra":{"drive_storage_limit":0,"private":"fixture-native-secret"},"sso_token":"fixture-native-secret"})
        }
    };
    Some(response(StatusCode::OK, json!({"code":0,"data":data})))
}

#[tokio::test]
async fn account_creation_http_forwards_supported_types_and_native_auth_without_secret_echoes() {
    let mut h = Harness::new().await;
    let (_, cap, _) = h.request(Method::GET, "capabilities", None).await;
    assert_eq!(cap["features"]["account_creation"], true);
    let (status, options, text) = h
        .request(Method::GET, "account-creation/options", None)
        .await;
    assert_eq!(status, 200);
    assert!(options["groups"].is_array());
    assert!(!text.contains("fixture-proxy-password"));
    assert!(!text.contains("proxy-address"));
    let (status, web, text) = h
        .request(Method::GET, "account-creation/catalog/web-search", None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(web, json!({"enabled":false}));
    assert!(!text.contains("fixture-secret"));
    for p in SCHEMA["platforms"].as_array().unwrap() {
        for kind in p["types"].as_array().unwrap() {
            let body = json!({"name":"Fixture","platform":p["id"],"type":kind,"credentials":{"access_token":"fixture-access"},"extra":{},"priority":0,"concurrency":1,"rate_multiplier":0,"load_factor":null,"auto_pause_on_expired":false,"group_ids":[]});
            let (status, result, text) = h
                .request(Method::POST, "account-creation", Some(body.clone()))
                .await;
            assert_eq!(status, 200, "{} {}", p["id"], kind);
            assert_eq!(result["account"]["id"], 777);
            assert!(!text.contains("fixture-native-secret"));
            let calls = h.upstream.matching(Method::POST, "/api/v1/admin/accounts");
            assert_eq!(calls.last().unwrap().body, body);
            assert_eq!(calls.last().unwrap().api_key, ADMIN_KEY);
        }
    }
    for provider in [
        "anthropic",
        "setup-token",
        "openai",
        "gemini",
        "antigravity",
        "grok",
    ] {
        for action in ["authorize", "exchange", "refresh", "cookie"] {
            let Some(native) = auth_route(provider, action) else {
                continue;
            };
            let body = match action {
                "authorize" => json!({}),
                "exchange" => {
                    json!({"session_id":"fixture-session","code":"fixture-code","state":"fixture"})
                }
                "refresh" => json!({"refresh_token":"fixture-refresh"}),
                _ => json!({"code":"fixture-cookie"}),
            };
            let path = format!("account-creation/auth/{provider}/{action}");
            let (status, result, text) = h.request(Method::POST, &path, Some(body.clone())).await;
            assert_eq!(status, 200, "{path}");
            assert!(!text.contains("fixture-native-secret"));
            assert!(result
                .get(if action == "authorize" {
                    "auth_url"
                } else {
                    "access_token"
                })
                .is_some());
            assert_eq!(
                h.upstream
                    .matching(Method::POST, &format!("/api/v1/admin/{native}"))
                    .last()
                    .unwrap()
                    .body,
                body
            );
        }
    }
    let body = json!({"name":"Fixture","platform":"openai","type":"apikey","credentials":{"api_key":"fixture-key","model_mapping":{"fixture-model":"fixture-model"}},"upstream_billing_probe_enabled":true});
    assert_eq!(
        h.request(Method::POST, "account-creation", Some(body))
            .await
            .0,
        200
    );
    assert_eq!(
        h.upstream
            .matching(
                Method::POST,
                "/api/v1/admin/accounts/777/models/sync-upstream"
            )
            .len(),
        1
    );
    assert_eq!(
        h.upstream
            .matching(
                Method::POST,
                "/api/v1/admin/accounts/777/upstream-billing-probe"
            )
            .len(),
        1
    );
    let body = json!({"name":"Fixture preview","platform":"grok","type":"apikey","credentials":{"api_key":"fixture-key"}});
    assert_eq!(
        h.request(
            Method::POST,
            "account-creation?sync_models=true",
            Some(body.clone())
        )
        .await
        .0,
        200
    );
    assert_eq!(
        h.upstream
            .matching(
                Method::POST,
                "/api/v1/admin/accounts/777/models/sync-upstream"
            )
            .len(),
        2
    );
    assert_eq!(
        h.upstream
            .matching(Method::POST, "/api/v1/admin/accounts")
            .last()
            .unwrap()
            .body,
        body
    );
    for (name, status) in [("fixture-rejected", 422), ("fixture-unknown", 502)] {
        let count = h
            .upstream
            .matching(Method::POST, "/api/v1/admin/accounts")
            .len();
        let (got,_,text)=h.request(Method::POST,"account-creation",Some(json!({"name":name,"platform":"openai","type":"apikey","credentials":{"api_key":"fixture-key"}}))).await;
        assert_eq!(got, status);
        assert!(!text.contains("fixture-secret-echo"));
        assert_eq!(
            h.upstream
                .matching(Method::POST, "/api/v1/admin/accounts")
                .len(),
            count + 1
        );
    }
    for (kind, body) in [
        (
            "codex-session",
            json!({"content":"fixture-content","update_existing":true}),
        ),
        ("codex-pat", json!({"access_token":"fixture-pat"})),
        ("grok-sso", json!({"sso_tokens":["fixture-sso"]})),
    ] {
        let (status, result, text) = h
            .request(
                Method::POST,
                &format!("account-creation/import/{kind}"),
                Some(body),
            )
            .await;
        assert_eq!(status, 200);
        assert!(!text.contains("fixture-native-secret"));
        assert!(!text.contains("fixture-secret-echo"));
        if kind == "codex-session" {
            assert_eq!(result["failed"], 1);
            assert_eq!(result["errors"][0]["index"], 3);
        }
    }
    let (_, risk, text) = h
        .request(
            Method::POST,
            "account-creation/check-mixed-channel",
            Some(json!({"platform":"anthropic","group_ids":[]})),
        )
        .await;
    assert_eq!(risk, json!({"has_risk":true}));
    assert!(!text.contains("private"));
    assert_eq!(
        h.request_with_key(
            Method::GET,
            "account-creation/options",
            None,
            "fixture-invalid"
        )
        .await
        .0,
        401
    );
    assert_eq!(
        h.request(Method::GET, "account-creation/options?extra=1", None)
            .await
            .0,
        422
    );
    assert_eq!(
        h.request(
            Method::POST,
            "account-creation/auth/gemini/refresh",
            Some(json!({}))
        )
        .await
        .0,
        404
    );
    h.finish().await;
}

#[tokio::test]
async fn account_creation_reuse_and_access_token_http_are_uncached_and_isolated() {
    let mut h = Harness::new().await;
    let id=h.insert_account("openai","apikey",json!({"alias":"fixture-model"}),json!({"quota_limit":0,"quota_used":9,"email":"fixture@example.invalid","openai_passthrough":false}),None,&[]).await;
    let url = format!("{}/api/desktop/v1/account-creation/source/{id}", h.base_url);
    let response = h
        .client
        .get(&url)
        .header("x-api-key", ADMIN_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let value: Value = response.json().await.unwrap();
    assert_eq!(
        value["credentials"]["model_mapping"],
        json!({"alias":"fixture-model"})
    );
    assert_eq!(value["extra"]["quota_limit"], 0);
    assert_eq!(value["extra"]["openai_passthrough"], false);
    assert!(value["extra"].get("quota_used").is_none());
    assert!(value["extra"].get("email").is_none());
    assert!(value["credentials"].get("api_key").is_none());
    let token_path = format!("accounts/{id}/key-quota-settings");
    let (_, settings, _) = h.request(Method::GET, &token_path, None).await;
    let (status,_,text)=h.request(Method::PUT,&token_path,Some(json!({"expected_revision":settings["revision"],"access_token":"fixture-reveal-secret"}))).await;
    assert_eq!(status, 200);
    assert!(!text.contains("fixture-reveal-secret"));
    let reveal = format!("{token_path}/access-token");
    assert_eq!(
        h.request_with_key(Method::GET, &reveal, None, "fixture-invalid")
            .await
            .0,
        401
    );
    assert_eq!(
        h.request(Method::POST, &reveal, Some(json!({}))).await.0,
        405
    );
    let response = h
        .client
        .get(format!("{}/api/desktop/v1/{reveal}", h.base_url))
        .header("x-api-key", ADMIN_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"account_id":id,"access_token":"fixture-reveal-secret"})
    );
    assert!(!h
        .request(Method::GET, &token_path, None)
        .await
        .2
        .contains("fixture-reveal-secret"));
    sqlx::query("DELETE FROM accounts WHERE id=$1")
        .bind(id)
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    assert_eq!(
        h.request(Method::GET, &format!("account-creation/source/{id}"), None)
            .await
            .0,
        404
    );
    h.finish().await;
}
