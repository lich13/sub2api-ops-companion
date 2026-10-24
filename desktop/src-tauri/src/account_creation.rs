use serde_json::Value;
use std::sync::LazyLock;

static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../server/src/account_creation_fields.json"
    ))
    .expect("account field catalog")
});

pub fn authorization_url(value: &str) -> bool {
    value.len() <= 16384
        && url::Url::parse(value).is_ok_and(|u| {
            u.scheme() == "https"
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
        })
}

pub fn allowed(method: &str, path: &str) -> bool {
    if path == "/account-creation?sync_models=true" {
        return method == "POST";
    }
    if path.contains(['?', '#', '\\']) || path.contains("..") {
        return false;
    }
    let p: Vec<_> = path.trim_start_matches('/').split('/').collect();
    match p.as_slice() {
        ["account-creation"] | ["account-creation", "check-mixed-channel" | "models"] => {
            method == "POST"
        }
        ["account-creation", "options"] => method == "GET",
        ["account-creation", "source", id] => {
            method == "GET" && id.parse::<u64>().is_ok_and(|n| n > 0)
        }
        ["account-creation", "catalog", "gemini" | "grok" | "tls" | "settings" | "web-search" | "antigravity-models"] => {
            method == "GET"
        }
        ["account-creation", "import", "codex-session" | "codex-pat" | "grok-sso"] => {
            method == "POST"
        }
        ["account-creation", "auth", provider, action] => {
            method == "POST"
                && matches!(
                    (*provider, *action),
                    (
                        "anthropic" | "setup-token",
                        "authorize" | "exchange" | "cookie"
                    ) | (
                        "openai" | "antigravity" | "grok",
                        "authorize" | "exchange" | "refresh"
                    ) | ("gemini", "authorize" | "exchange")
                )
        }
        _ => false,
    }
}
fn account(v: &Value) -> bool {
    v["id"].as_u64().is_some_and(|n| n > 0)
        && v["name"].is_string()
        && v["platform"].is_string()
        && v["type"].is_string()
        && v.as_object().is_some_and(|o| o.len() == 4)
}
fn choices(v: &Value) -> bool {
    v.as_array()
        .is_some_and(|a| a.iter().all(|v| v["id"].is_u64() && v["name"].is_string()))
}
fn source(v: &Value) -> bool {
    let Some(platform) = v["platform"].as_str() else {
        return false;
    };
    let Some(kind) = v["type"].as_str() else {
        return false;
    };
    if !SCHEMA["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == platform && p["types"].as_array().unwrap().iter().any(|t| t == kind))
    {
        return false;
    }
    let top = [
        "platform",
        "type",
        "notes",
        "proxy_id",
        "concurrency",
        "load_factor",
        "priority",
        "rate_multiplier",
        "group_ids",
        "expires_at",
        "auto_pause_on_expired",
        "upstream_billing_probe_enabled",
        "credentials",
        "extra",
    ];
    v.as_object()
        .is_some_and(|o| o.keys().all(|k| top.contains(&k.as_str())))
        && ["credentials", "extra"].iter().all(|section| {
            v[*section].as_object().is_some_and(|o| {
                o.keys().all(|key| {
                    SCHEMA["fields"].as_array().unwrap().iter().any(|f| {
                        f["path"] == format!("{section}.{key}")
                            && [("platforms", platform), ("types", kind)].iter().all(
                                |(key, value)| {
                                    f[*key]
                                        .as_array()
                                        .is_none_or(|a| a.iter().any(|v| v == *value))
                                },
                            )
                    })
                })
            })
        })
}
pub fn valid(path: &str, v: &Value) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    let p: Vec<_> = path.trim_start_matches('/').split('/').collect();
    match p.as_slice() {
        ["account-creation"] | ["account-creation", "import", "codex-pat"] => {
            account(&v["account"])
                && v["warnings"]
                    .as_array()
                    .is_some_and(|a| a.iter().all(Value::is_string))
        }
        ["account-creation", "options"] => choices(&v["groups"]) && choices(&v["proxies"]),
        ["account-creation", "source", _] => source(v),
        ["account-creation", "catalog", "tls"] => choices(v),
        ["account-creation", "catalog", "gemini"] => v["ai_studio_oauth_enabled"].is_boolean(),
        ["account-creation", "catalog", "grok"] => v["password_auth_enabled"].is_boolean(),
        ["account-creation", "catalog", "settings"] => {
            v["account_quota_notify_enabled"].is_boolean()
        }
        ["account-creation", "catalog", "web-search"] => v["enabled"].is_boolean(),
        ["account-creation", "catalog", "antigravity-models"] => v
            .as_object()
            .is_some_and(|o| o.values().all(Value::is_string)),
        ["account-creation", "check-mixed-channel"] => v["has_risk"].is_boolean(),
        ["account-creation", "models"] => v["models"]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string)),
        ["account-creation", "auth", _, "authorize"] => {
            v["auth_url"].as_str().is_some_and(authorization_url)
                && v["session_id"].as_str().is_some_and(|s| !s.is_empty())
        }
        ["account-creation", "auth", _, _] => {
            v["access_token"].as_str().is_some_and(|s| !s.is_empty())
        }
        ["account-creation", "import", "grok-sso"] => {
            v["created"].as_array().is_some_and(|a| {
                a.iter()
                    .all(|v| v["index"].is_u64() && account(&v["account"]))
            }) && v["failed"].is_array()
        }
        ["account-creation", "import", "codex-session"] => {
            ["created", "updated", "skipped", "failed"]
                .iter()
                .all(|k| v[*k].is_u64())
                && v["errors"].is_array()
                && v["warnings"].is_array()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn fixed_routes_and_response_boundaries() {
        assert!(allowed("POST", "/account-creation?sync_models=true"));
        assert!(!allowed("GET", "/account-creation?sync_models=true"));
        assert!(!allowed("POST", "/account-creation?sync_models=false"));
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
            "/account-creation/options?token=fixture",
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
            assert!(!valid("/account-creation/source/1", &bad));
        }
        let mut bad = source;
        bad["extra"]["quota_used"] = json!(1);
        assert!(!valid("/account-creation/source/1", &bad));
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
