pub use sub2ops_protocol::creation::authorization_url;
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    fn valid(path: &str, v: &Value) -> bool {
        ["GET", "POST"]
            .iter()
            .any(|m| sub2ops_protocol::response::client_response(m, path, v.clone()).is_some())
    }
    use crate::allowed_request as allowed;
    use serde_json::json;
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
        assert!(valid("/account-creation/source/1", &source));
        for key in [
            "access_token",
            "api_key",
            "refresh_token",
            "email",
            "private_key",
        ] {
            let mut bad = source.clone();
            bad["credentials"][key] = json!("fixture");
            let clean = sub2ops_protocol::response::client_response(
                "GET",
                "/account-creation/source/1",
                bad,
            )
            .unwrap();
            assert!(clean["credentials"].get(key).is_none());
        }
        let mut bad = source;
        bad["extra"]["quota_used"] = json!(1);
        let clean =
            sub2ops_protocol::response::client_response("GET", "/account-creation/source/1", bad)
                .unwrap();
        assert!(clean["extra"].get("quota_used").is_none());
        assert!(valid(
            "/account-creation",
            &json!({"account":{"id":1,"name":"Fixture","platform":"openai","type":"apikey"},"warnings":[]})
        ));
        assert!(!valid(
            "/account-creation",
            &json!({"account":{"id":0,"name":"Fixture","platform":"openai","type":"apikey"},"warnings":[]})
        ));
        assert!(valid(
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
}
