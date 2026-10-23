use serde_json::Value;
fn id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<i64>().is_ok_and(|n| n > 0)
}
pub fn allowed(method: &str, path: &str) -> bool {
    let (plain, query) = path
        .split_once('?')
        .map_or((path, None), |(p, q)| (p, Some(q)));
    let parts = plain.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let route = match parts.as_slice() {
        ["proxy-management"] => matches!(method, "GET" | "POST"),
        ["proxy-management", "data"] => matches!(method, "GET" | "POST"),
        ["proxy-management", "batch" | "batch-delete"] => method == "POST",
        ["proxy-management", n] if id(n) => matches!(method, "GET" | "PUT" | "DELETE"),
        ["proxy-management", n, "credentials" | "accounts"] if id(n) => method == "GET",
        _ => false,
    };
    if !route {
        return false;
    }
    let Some(query) = query else {
        return true;
    };
    if method != "GET"
        || !matches!(plain, "/proxy-management" | "/proxy-management/data")
        || query.is_empty()
    {
        return false;
    }
    let export = plain.ends_with("/data");
    let mut seen = std::collections::HashSet::new();
    url::form_urlencoded::parse(query.as_bytes()).all(|(k, v)| {
        seen.insert(k.to_string())
            && match k.as_ref() {
                "page" => !export && v.parse::<u32>().is_ok_and(|n| n > 0),
                "page_size" => !export && v.parse::<u32>().is_ok_and(|n| (1..=200).contains(&n)),
                "protocol" => matches!(v.as_ref(), "http" | "https" | "socks5" | "socks5h"),
                "status" => matches!(v.as_ref(), "active" | "inactive" | "expired"),
                "sort_by" => matches!(v.as_ref(), "id" | "name" | "protocol" | "account_count"),
                "sort_order" => matches!(v.as_ref(), "asc" | "desc"),
                "search" => v.chars().count() <= 100 && !v.chars().any(char::is_control),
                "ids" => export && v.split(',').count() <= 1000 && v.split(',').all(id),
                _ => false,
            }
    })
}
fn only(v: &Value, keys: &[&str]) -> bool {
    v.as_object()
        .is_some_and(|o| o.keys().all(|k| keys.contains(&k.as_str())))
}
fn positive(v: &Value) -> bool {
    v.as_i64().is_some_and(|n| n > 0)
}
fn row(v: &Value) -> bool {
    only(
        v,
        &[
            "id",
            "name",
            "protocol",
            "host",
            "port",
            "status",
            "created_at",
            "updated_at",
            "expires_at",
            "fallback_mode",
            "backup_proxy_id",
            "expiry_warn_days",
            "account_count",
            "latency_ms",
            "latency_status",
            "ip_address",
            "country",
            "country_code",
            "region",
            "city",
            "quality_status",
            "quality_score",
            "quality_grade",
            "quality_checked",
            "username_set",
            "password_set",
        ],
    ) && positive(&v["id"])
        && ["name", "protocol", "host", "status"]
            .iter()
            .all(|k| v[*k].is_string())
        && v["port"].as_u64().is_some_and(|n| (1..=65535).contains(&n))
        && v["username_set"].is_boolean()
        && v["password_set"].is_boolean()
        && [
            "created_at",
            "updated_at",
            "expires_at",
            "fallback_mode",
            "latency_status",
            "ip_address",
            "country",
            "country_code",
            "region",
            "city",
            "quality_status",
            "quality_grade",
        ]
        .iter()
        .all(|k| v[*k].is_null() || v[*k].is_string())
        && [
            "backup_proxy_id",
            "expiry_warn_days",
            "account_count",
            "latency_ms",
            "quality_score",
            "quality_checked",
        ]
        .iter()
        .all(|k| v[*k].is_null() || v[*k].is_u64())
}
pub fn valid(path: &str, v: &Value) -> bool {
    let p = path.split('?').next().unwrap_or(path);
    if p.ends_with("/credentials") {
        return only(v, &["username", "password"])
            && v["username"].is_string()
            && v["password"].is_string();
    }
    if p.ends_with("/accounts") {
        return v.as_array().is_some_and(|a| {
            a.iter().all(|v| {
                only(v, &["id", "name", "platform", "type"])
                    && positive(&v["id"])
                    && ["name", "platform", "type"]
                        .iter()
                        .all(|k| v[*k].is_string())
            })
        });
    }
    if p.ends_with("/batch") {
        return only(v, &["created", "skipped"]) && v["created"].is_u64() && v["skipped"].is_u64();
    }
    if p.ends_with("/batch-delete") {
        return only(v, &["deleted_ids", "skipped"])
            && v["deleted_ids"]
                .as_array()
                .is_some_and(|a| a.iter().all(positive))
            && v["skipped"].as_array().is_some_and(|a| {
                a.iter().all(|v| {
                    only(v, &["id", "reason"]) && positive(&v["id"]) && v["reason"].is_string()
                })
            });
    }
    if p.ends_with("/data") {
        return if v.get("proxies").is_some() {
            only(v, &["exported_at", "proxies", "accounts"])
                && v["exported_at"].is_string()
                && v["accounts"].as_array().is_some_and(|a| a.is_empty())
                && v["proxies"].as_array().is_some_and(|a| {
                    a.iter().all(|v| {
                        only(
                            v,
                            &[
                                "proxy_key",
                                "name",
                                "protocol",
                                "host",
                                "port",
                                "username",
                                "password",
                                "status",
                                "expires_at",
                                "fallback_mode",
                                "backup_proxy_name",
                                "expiry_warn_days",
                            ],
                        ) && v["name"].is_string()
                            && v["host"].is_string()
                            && v["port"].is_u64()
                    })
                })
        } else {
            only(
                v,
                &["proxy_created", "proxy_reused", "proxy_failed", "errors"],
            ) && ["proxy_created", "proxy_reused", "proxy_failed"]
                .iter()
                .all(|k| v[*k].is_u64())
                && v["errors"].as_array().is_some_and(|a| {
                    a.iter().all(|v| {
                        only(v, &["name", "message"])
                            && v["name"].is_string()
                            && v["message"].is_string()
                    })
                })
        };
    }
    if v.get("items").is_some() {
        return p == "/proxy-management"
            && only(v, &["items", "page", "page_size", "total", "pages"])
            && ["page", "page_size", "total", "pages"]
                .iter()
                .all(|k| v[*k].is_u64())
            && v["items"]
                .as_array()
                .is_some_and(|a| a.len() <= 200 && a.iter().all(row));
    }
    only(v, &["deleted"]) && v["deleted"] == true || row(v)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
            ("GET", "/proxy-management?include_password=true"),
            ("GET", "/proxy-management?page=1&page=2"),
            ("DELETE", "/proxy-management/data"),
        ] {
            assert!(!allowed(m, p));
        }
        let mut r = json!({"id":7,"name":"fixture","host":"127.0.0.1","port":1080,"protocol":"socks5","status":"active","username_set":true,"password_set":true});
        assert!(valid("/proxy-management/7", &r));
        r["password"] = "fixture".into();
        assert!(!valid("/proxy-management/7", &r));
        assert!(valid(
            "/proxy-management/7/credentials",
            &json!({"username":"fixture","password":"fixture"})
        ));
    }
}
