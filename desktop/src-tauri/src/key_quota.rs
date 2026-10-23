use serde_json::Value;

fn text(v: &Value) -> bool {
    v.is_null() || v.is_string()
}
fn number(v: &Value) -> bool {
    v.is_null() || v.as_f64().is_some_and(f64::is_finite)
}
fn timestamp(v: &Value) -> bool {
    v.is_null() || v.as_u64().is_some()
}
fn plans(v: &Value) -> bool {
    v.as_array().is_some_and(|items| {
        items.iter().all(|p| {
            p["name"].is_string()
                && p["unit"].is_string()
                && p["unlimited"].is_boolean()
                && ["remaining", "used", "total"]
                    .iter()
                    .all(|k| number(&p[*k]))
                && text(&p["reset_at"])
        })
    })
}
pub fn valid(v: &Value) -> bool {
    matches!(
        v["state"].as_str(),
        Some("idle" | "loading" | "ok" | "error" | "unsupported")
    ) && (v["source"].is_null() || matches!(v["source"].as_str(), Some("sub2api" | "newapi")))
        && v["stale"].is_boolean()
        && ["error", "key_status", "expires_at"]
            .iter()
            .all(|k| text(&v[*k]))
        && [
            "checked_at",
            "success_at",
            "retry_at",
            "next_refresh_at",
            "expires_at_unix",
        ]
        .iter()
        .all(|k| timestamp(&v[*k]))
        && plans(&v["plans"])
        && v["subscriptions"].as_array().is_some_and(|items| {
            items.iter().all(|s| {
                ["name", "status", "expires_at"]
                    .iter()
                    .all(|k| text(&s[*k]))
                    && (s["active"].is_null() || s["active"].is_boolean())
                    && plans(&s["plans"])
            })
        })
        && ["today", "total_usage"].iter().all(|k| {
            v[*k].is_null()
                || v[*k].is_object()
                    && ["requests", "tokens", "cost"]
                        .iter()
                        .all(|field| number(&v[*k][*field]))
        })
}
pub fn settings_valid(v: &Value, id: Option<u64>) -> bool {
    v["revision"]
        .as_str()
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        && if let Some(id) = id {
            v["account_id"].as_u64() == Some(id)
                && v["has_access_token"].is_boolean()
                && v.as_object().is_some_and(|o| {
                    o.keys().all(|k| {
                        ["account_id", "has_access_token", "revision"].contains(&k.as_str())
                    })
                })
        } else {
            v["seconds"]
                .as_u64()
                .is_some_and(|n| n == 0 || (10..=86400).contains(&n))
                && v.as_object().is_some_and(|o| {
                    o.keys()
                        .all(|k| ["seconds", "revision"].contains(&k.as_str()))
                })
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_quota_and_never_accepts_a_returned_token() {
        let mut v = json!({"state":"ok","source":"newapi","stale":false,"plans":[{"name":"余额","unit":"USD","unlimited":false,"remaining":0}],"subscriptions":[],"today":{"requests":0}});
        assert!(valid(&v));
        v["plans"][0]["remaining"] = "unknown".into();
        assert!(!valid(&v));
        let mut s = json!({"account_id":1,"has_access_token":true,"revision":"a".repeat(64)});
        assert!(settings_valid(&s, Some(1)));
        assert!(!settings_valid(&s, Some(2)));
        s["access_token"] = "fixture".into();
        assert!(!settings_valid(&s, Some(1)));
        assert!(settings_valid(
            &json!({"seconds":0,"revision":"a".repeat(64)}),
            None
        ));
        assert!(!settings_valid(
            &json!({"seconds":1,"revision":"a".repeat(64)}),
            None
        ));
    }
}
