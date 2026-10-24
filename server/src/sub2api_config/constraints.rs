use super::get;
use serde_json::{json, Value};

fn text<'a>(values: &'a Value, path: &str) -> &'a str {
    get(values, path)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
}
fn enabled(values: &Value, path: &str) -> bool {
    get(values, path).and_then(Value::as_bool) == Some(true)
}
fn http_url(value: &str) -> bool {
    reqwest::Url::parse(value)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}
fn absolute_url(value: &str) -> bool {
    http_url(value) && reqwest::Url::parse(value).is_ok_and(|u| u.fragment().is_none())
}
fn frontend_url(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['\r', '\n'])
        && (value.starts_with('/') && !value.starts_with("//") || absolute_url(value))
}

pub(super) fn check(values: &Value) -> Vec<Value> {
    let mut errors = vec![];
    let mut add = |path: &str, message: &str| errors.push(json!({"path":path,"message":message}));
    if text(values, "gemini.oauth.client_id").is_empty()
        != text(values, "gemini.oauth.client_secret").is_empty()
    {
        add(
            "gemini.oauth.client_id",
            "client_id 与 client_secret 必须同时设置或同时留空",
        );
    }
    let frontend = text(values, "server.frontend_url");
    if !frontend.is_empty()
        && !reqwest::Url::parse(frontend).is_ok_and(|u| {
            http_url(frontend)
                && u.fragment().is_none()
                && u.query().is_none()
                && u.username().is_empty()
                && u.password().is_none()
        })
    {
        add(
            "server.frontend_url",
            "需要不含查询参数、片段及认证信息的 HTTP(S) URL",
        );
    }
    if enabled(values, "security.csp.enabled") && text(values, "security.csp.policy").is_empty() {
        add("security.csp.policy", "启用 CSP 时必须提供策略");
    }
    for group in ["linuxdo_connect", "oidc_connect"] {
        if !enabled(values, &format!("{group}.enabled")) {
            continue;
        }
        let required: &[&str] = if group == "linuxdo_connect" {
            &[
                "client_id",
                "authorize_url",
                "token_url",
                "userinfo_url",
                "redirect_url",
                "frontend_redirect_url",
            ]
        } else {
            &[
                "client_id",
                "issuer_url",
                "redirect_url",
                "frontend_redirect_url",
            ]
        };
        for key in required {
            let path = format!("{group}.{key}");
            if text(values, &path).is_empty() {
                add(&path, "启用此登录方式时必须填写");
            }
        }
        if text(values, &format!("{group}.token_auth_method")) != "none"
            && text(values, &format!("{group}.client_secret")).is_empty()
        {
            add(
                &format!("{group}.client_secret"),
                "当前客户端鉴权方式需要 client_secret",
            );
        }
        for key in [
            "issuer_url",
            "discovery_url",
            "authorize_url",
            "token_url",
            "userinfo_url",
            "jwks_url",
            "redirect_url",
        ] {
            let path = format!("{group}.{key}");
            let value = text(values, &path);
            if !value.is_empty() && !absolute_url(value) {
                add(&path, "需要不含片段的绝对 HTTP(S) URL");
            }
        }
        let path = format!("{group}.frontend_redirect_url");
        if !frontend_url(text(values, &path)) {
            add(&path, "需要合法的相对路径或绝对 HTTP(S) URL");
        }
    }
    if enabled(values, "oidc_connect.enabled") {
        if !text(values, "oidc_connect.scopes")
            .split_whitespace()
            .any(|s| s == "openid")
        {
            add("oidc_connect.scopes", "必须包含 openid");
        }
        if enabled(values, "oidc_connect.validate_id_token")
            && text(values, "oidc_connect.allowed_signing_algs").is_empty()
        {
            add(
                "oidc_connect.allowed_signing_algs",
                "验证 ID Token 时必须设置允许的签名算法",
            );
        }
    }
    if enabled(values, "dingtalk_connect.enabled") {
        if text(values, "dingtalk_connect.dingtalk_app_kind") != "internal_app" {
            add(
                "dingtalk_connect.dingtalk_app_kind",
                "当前版本仅允许 internal_app",
            );
        }
        if text(values, "dingtalk_connect.corp_restriction_policy") == "internal_only"
            && text(values, "dingtalk_connect.app_type") != "internal"
        {
            add(
                "dingtalk_connect.app_type",
                "internal_only 策略要求 internal",
            );
        }
    }
    if enabled(values, "webauthn.enabled") {
        if text(values, "webauthn.rp_display_name").is_empty() {
            add("webauthn.rp_display_name", "启用 Passkey 时必须填写");
        }
        let rp = text(values, "webauthn.rp_id").to_lowercase();
        if rp.is_empty() || rp.contains(['/', ':']) {
            add("webauthn.rp_id", "需要不含协议、端口和路径的域名");
        }
        let origins = get(values, "webauthn.rp_origins").and_then(Value::as_array);
        if origins.is_none_or(|a| a.is_empty()) {
            add("webauthn.rp_origins", "至少需要一个来源");
        }
        for value in origins.into_iter().flatten() {
            let valid = value
                .as_str()
                .map(str::trim)
                .filter(|s| {
                    s.split_once("://")
                        .is_some_and(|(_, rest)| !rest.contains('/'))
                })
                .and_then(|s| reqwest::Url::parse(s).ok())
                .is_some_and(|u| {
                    let h = u.host_str().unwrap_or("");
                    (h == rp || h.ends_with(&format!(".{rp}")))
                        && (u.scheme() == "https"
                            || u.scheme() == "http"
                                && matches!(h, "localhost" | "127.0.0.1" | "[::1]"))
                        && u.username().is_empty()
                        && u.password().is_none()
                        && u.query().is_none()
                        && u.fragment().is_none()
                        && u.path() == "/"
                });
            if !valid {
                add("webauthn.rp_origins","来源必须在 RP 域名内，使用 HTTPS（回环允许 HTTP），不能含认证、路径、查询或片段");
                break;
            }
        }
    }
    if enabled(values, "wechat_connect.enabled") {
        let any_enabled = ["open", "mp", "mobile"]
            .iter()
            .any(|m| enabled(values, &format!("wechat_connect.{m}_enabled")));
        for mode in ["open", "mp", "mobile"] {
            let selected = text(values, "wechat_connect.mode");
            if !enabled(values, &format!("wechat_connect.{mode}_enabled"))
                && (any_enabled
                    || mode
                        != if matches!(selected, "mp" | "mobile") {
                            selected
                        } else {
                            "open"
                        })
            {
                continue;
            }
            for key in ["app_id", "app_secret"] {
                let path = format!("wechat_connect.{mode}_{key}");
                if text(values, &path).is_empty()
                    && ["", "open_", "mp_", "mobile_"].iter().all(|prefix| {
                        text(values, &format!("wechat_connect.{prefix}{key}")).is_empty()
                    })
                {
                    add(&path, "启用此微信登录方式时必须填写");
                }
            }
        }
        let redirect = text(values, "wechat_connect.redirect_url");
        if !redirect.is_empty() && !absolute_url(redirect) {
            add(
                "wechat_connect.redirect_url",
                "需要不含片段的绝对 HTTP(S) URL",
            );
        }
        if !frontend_url(text(values, "wechat_connect.frontend_redirect_url")) {
            add(
                "wechat_connect.frontend_redirect_url",
                "需要合法的相对路径或绝对 HTTP(S) URL",
            );
        }
    }
    if enabled(values, "batch_image.vertex_enabled") {
        let bucket = text(values, "batch_image.vertex_managed_gcs_bucket");
        if bucket.is_empty() || bucket.contains("://") {
            add(
                "batch_image.vertex_managed_gcs_bucket",
                "需要桶名，不能使用 URI",
            );
        }
        if text(values, "batch_image.vertex_location").is_empty() {
            add("batch_image.vertex_location", "启用 Vertex 时必须填写");
        }
        if !text(values, "batch_image.vertex_managed_gcs_prefix").contains("{batch_id}") {
            add(
                "batch_image.vertex_managed_gcs_prefix",
                "必须包含 {batch_id}",
            );
        }
    }
    if enabled(values, "batch_image.queue_enabled") {
        for key in [
            "queue_ready_key",
            "queue_delayed_key",
            "queue_active_key",
            "inflight_key_prefix",
            "lock_key_prefix",
        ] {
            let path = format!("batch_image.{key}");
            if text(values, &path).is_empty() {
                add(&path, "启用队列时不能为空");
            }
        }
    }
    if enabled(values, "ops.cleanup.enabled") && text(values, "ops.cleanup.schedule").is_empty() {
        add("ops.cleanup.schedule", "启用清理时不能为空");
    }
    if enabled(values, "dashboard_aggregation.enabled")
        && enabled(values, "dashboard_aggregation.backfill_enabled")
        && get(values, "dashboard_aggregation.backfill_max_days").and_then(Value::as_i64) == Some(0)
    {
        add(
            "dashboard_aggregation.backfill_max_days",
            "启用聚合与历史回填时必须大于 0",
        );
    }
    if let Some(headers) =
        get(values, "security.forwarded_client_ip_headers").and_then(Value::as_array)
    {
        let names = headers
            .iter()
            .filter_map(Value::as_str)
            .map(|s| s.trim().to_lowercase())
            .collect::<std::collections::BTreeSet<_>>();
        if names.len() > 16
            || names
                .iter()
                .any(|s| reqwest::header::HeaderName::from_bytes(s.as_bytes()).is_err())
        {
            add(
                "security.forwarded_client_ip_headers",
                "需要合法 HTTP 字段名，最多 16 个不同名称",
            );
        }
    }
    if let Some(items) = get(values, "security.proxy_probe.urls").and_then(Value::as_array) {
        for item in items {
            let url = item["url"].as_str().unwrap_or("").trim();
            let parser = item["parser"].as_str().unwrap_or("").trim().to_lowercase();
            if !http_url(url) || !matches!(parser.as_str(), "ip-api" | "ipify" | "chatgpt-trace") {
                add(
                    "security.proxy_probe.urls",
                    "每项需要 HTTP(S) url 和 ip-api、ipify 或 chatgpt-trace parser",
                );
                break;
            }
        }
    }
    if let Some(tiers) = get(values, "gemini.quota.tiers").and_then(Value::as_object) {
        for tier in tiers.values() {
            if !tier.is_object()
                || ["pro_rpd", "flash_rpd", "cooldown_minutes"]
                    .iter()
                    .any(|key| tier.get(*key).is_some_and(|v| !v.is_null() && !v.is_i64()))
            {
                add(
                    "gemini.quota.tiers",
                    "每个档位需要映射，配额和冷却字段必须为整数或 null",
                );
                break;
            }
        }
    }
    errors
}
