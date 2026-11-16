use serde_json::Value;
use std::collections::{HashMap, HashSet};

fn hex(value: &Value, length: usize) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == length
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn text(value: &Value) -> bool {
    value.is_string()
}
fn account_id(value: &Value) -> bool {
    value
        .as_u64()
        .is_some_and(|id| (1..=9_007_199_254_740_991).contains(&id))
}
fn timestamp(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| (20..=64).contains(&s.len()) && s.is_ascii() && s.as_bytes()[10] == b'T')
}
fn fields(value: &Value, allowed: &[&str]) -> bool {
    value.as_object().is_some_and(|map| {
        map.len() == allowed.len() && map.keys().all(|key| allowed.contains(&key.as_str()))
    })
}
fn platform(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 64)
}
fn parallelism(value: &Value) -> bool {
    value.as_u64().is_some_and(|width| (1..=8).contains(&width))
}
pub fn task(value: &Value, detail: bool) -> bool {
    let mut allowed = vec![
        "id",
        "status",
        "cancel_requested",
        "reason",
        "total",
        "change_count",
        "verified_count",
        "created_at",
        "updated_at",
        "completed_at",
        "platform",
        "base",
        "waiting_reason",
        "waiting_account_ids",
    ];
    if value.get("parallelism").is_some() {
        allowed.push("parallelism");
    }
    if detail {
        allowed.push("items");
    }
    fields(value, &allowed)
        && value.get("parallelism").is_none_or(parallelism)
        && value["base"] == 1
        && platform(&value["platform"])
        && text(&value["waiting_reason"])
        && value["waiting_account_ids"].as_array().is_some_and(|ids| {
            let mut seen = HashSet::new();
            ids.len() as u64 <= value["total"].as_u64().unwrap_or(0)
                && ids
                    .iter()
                    .all(|id| account_id(id) && seen.insert(id.as_u64().unwrap()))
        })
        && hex(&value["id"], 32)
        && matches!(
            value["status"].as_str(),
            Some(
                "queued"
                    | "running"
                    | "checking"
                    | "completed"
                    | "failed"
                    | "cancelled"
                    | "needs_confirmation"
            )
        )
        && value["cancel_requested"].is_boolean()
        && text(&value["reason"])
        && value["total"]
            .as_u64()
            .is_some_and(|n| n <= i32::MAX as u64)
        && ["change_count", "verified_count"].iter().all(|key| {
            value[*key]
                .as_u64()
                .is_some_and(|n| n <= value["total"].as_u64().unwrap_or(0))
        })
        && timestamp(&value["created_at"])
        && timestamp(&value["updated_at"])
        && (value["completed_at"].is_null() || timestamp(&value["completed_at"]))
        && (!detail
            || value["items"].as_array().is_some_and(|items| {
                let mut ids = HashSet::new();
                let mut counts = HashMap::<u64, u64>::new();
                items.len() as u64 == value["total"].as_u64().unwrap_or(u64::MAX)
                    && items.iter().enumerate().all(|(index, item)| {
                        fields(
                            item,
                            &[
                                "account_id",
                                "account_name",
                                "platform",
                                "original_priority",
                                "target_priority",
                                "observed_priority",
                                "status",
                            ],
                        ) && item["platform"] == value["platform"]
                            && account_id(&item["account_id"])
                            && ids.insert(item["account_id"].as_u64().unwrap())
                            && text(&item["account_name"])
                            && item["original_priority"].is_i64()
                            && (if value.get("parallelism").is_some() {
                                item["target_priority"].as_u64().is_some_and(|target| {
                                    let count = counts.entry(target).or_default();
                                    *count += 1;
                                    target <= i32::MAX as u64
                                        && *count <= value["parallelism"].as_u64().unwrap_or(1)
                                })
                            } else {
                                item["target_priority"].as_u64() == Some(index as u64 + 1)
                            })
                            && (item["observed_priority"].is_null()
                                || item["observed_priority"].is_i64())
                            && matches!(
                                item["status"].as_str(),
                                Some("pending" | "unchanged" | "dispatched" | "verified")
                            )
                            && (!matches!(item["status"].as_str(), Some("verified" | "unchanged"))
                                || item["observed_priority"] == item["target_priority"])
                    })
            }))
}

pub fn list(value: &Value) -> bool {
    // `legacy_last_task` is always null; servers keep the key for clients up to 0.1.92.
    fields(value, &["platforms", "active_task", "legacy_last_task"])
        && value["legacy_last_task"].is_null()
        && value["platforms"].as_array().is_some_and(|groups| {
            let mut platforms = HashSet::new();
            let mut ids = HashSet::new();
            groups.iter().all(|group| {
                let mut keys = vec!["platform", "version", "items", "last_task"];
                if group.get("parallelism").is_some() {
                    keys.push("parallelism");
                }
                fields(group, &keys)
                    && group.get("parallelism").is_none_or(parallelism)
                    && platform(&group["platform"])
                    && platforms.insert(group["platform"].as_str().unwrap())
                    && hex(&group["version"], 64)
                    && group["items"].as_array().is_some_and(|items| {
                        items.iter().all(|row| {
                            fields(
                                row,
                                &[
                                    "id",
                                    "name",
                                    "platform",
                                    "type",
                                    "priority",
                                    "version",
                                    "editable",
                                    "reason",
                                    "status",
                                    "schedulable",
                                ],
                            ) && account_id(&row["id"])
                                && ids.insert(row["id"].as_u64().unwrap())
                                && ["name", "type", "reason", "status"]
                                    .iter()
                                    .all(|key| text(&row[*key]))
                                && row["platform"] == group["platform"]
                                && row["priority"].is_i64()
                                && hex(&row["version"], 64)
                                && row["editable"].is_boolean()
                                && row["schedulable"].is_boolean()
                        })
                    })
                    && (group["last_task"].is_null()
                        || task(&group["last_task"], false)
                            && group["last_task"]["platform"] == group["platform"])
            })
        })
        && (value["active_task"].is_null()
            || task(&value["active_task"], false)
                && matches!(
                    value["active_task"]["status"].as_str(),
                    Some("queued" | "running" | "checking")
                ))
}
pub fn query(path: &str, detail: bool) -> bool {
    let Some((_, query)) = path.split_once('?') else {
        return false;
    };
    let mut seen = HashSet::new();
    query.split('&').all(|pair| {
        let Some((key, value)) = pair.split_once('=') else {
            return false;
        };
        seen.insert(key)
            && match key {
                "schema" => value == "2",
                "details" => detail && value == "0",
                _ => false,
            }
    }) && seen.contains("schema")
}
pub fn is_summary(path: &str) -> bool {
    path.split_once('?')
        .is_some_and(|(_, q)| q.split('&').any(|p| p == "details=0"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
            assert!(crate::allowed_request(method, &format!("{path}?schema=2")));
            assert!(!crate::allowed_request(method, &path));
            assert!(!crate::allowed_request(method, &format!("{path}?limit=1")));
            assert!(!crate::allowed_request(
                method,
                &format!("{path}?schema=2&limit=1")
            ));
            assert!(!crate::allowed_request(
                method,
                &format!("{path}/?schema=2")
            ));
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
            assert!(!crate::allowed_request(method, &path));
        }
    }
    #[test]
    fn platform_protocol_validates_summary_detail_and_identity() {
        let id = "a".repeat(32);
        assert!(crate::allowed_request(
            "GET",
            &format!("/account-priority-orders/{id}?details=0&schema=2")
        ));
        for path in [
            "/account-priorities?schema=2&details=0".to_owned(),
            format!("/account-priority-orders/{id}?details=0"),
            format!("/account-priority-orders/{id}?schema=2&schema=2"),
            format!("/account-priority-orders/{id}?schema=2&details=1"),
        ] {
            assert!(!crate::allowed_request("GET", &path));
        }
        assert!(!crate::allowed_request(
            "POST",
            "/account-priority-orders?schema=2&details=0"
        ));
        let summary = json!({"id":id,"platform":"openai","base":1,"waiting_reason":"等待账号空闲","waiting_account_ids":[1],"status":"running","cancel_requested":false,"reason":"正在保存","total":1,"change_count":1,"verified_count":0,"created_at":"2026-10-21T00:00:00Z","updated_at":"2026-10-21T00:00:00Z","completed_at":null});
        assert!(task(&summary, false));
        assert!(!task(&summary, true));
        for (key, bad) in [
            ("status", json!("unknown")),
            ("verified_count", json!(2)),
            ("updated_at", json!("invalid")),
            ("base", json!(0)),
            ("platform", Value::Null),
        ] {
            let mut invalid = summary.clone();
            invalid[key] = bad;
            assert!(!task(&invalid, false), "{key}");
        }
        let mut detail = summary.clone();
        detail["items"] = json!([{"account_id":1,"account_name":"fixture","platform":"openai","original_priority":8,"target_priority":1,"observed_priority":null,"status":"pending"}]);
        assert!(task(&detail, true));
        assert!(!task(&detail, false));
        let mut parallel = detail.clone();
        parallel["parallelism"] = 3.into();
        parallel["total"] = 2.into();
        let mut second = parallel["items"][0].clone();
        second["account_id"] = 2.into();
        parallel["items"].as_array_mut().unwrap().push(second);
        assert!(task(&parallel, true));
        parallel["parallelism"] = 1.into();
        assert!(!task(&parallel, true));
        detail["items"][0]["platform"] = "grok".into();
        assert!(!task(&detail, true));
        detail["items"][0]["platform"] = "openai".into();
        detail["items"][0]["target_priority"] = 0.into();
        assert!(!task(&detail, true));
        let mut view = json!({"platforms":[{"platform":"openai","version":"b".repeat(64),"items":[{"id":1,"name":"fixture","platform":"openai","type":"oauth","priority":8,"version":"c".repeat(64),"editable":true,"reason":"","status":"active","schedulable":true}],"last_task":summary}],"active_task":summary,"legacy_last_task":null});
        assert!(list(&view));
        view["platforms"][0]["parallelism"] = 3.into();
        assert!(list(&view));
        view["platforms"][0]["parallelism"] = 9.into();
        assert!(!list(&view));
        view["platforms"][0]["parallelism"] = 3.into();
        let mut invalid = view.clone();
        invalid["legacy_last_task"] = summary.clone();
        assert!(!list(&invalid));
        let mut invalid = view.clone();
        invalid["platforms"][0]["items"][0]["platform"] = "grok".into();
        assert!(!list(&invalid));
        let mut invalid = view.clone();
        let row = invalid["platforms"][0]["items"][0].clone();
        invalid["platforms"][0]["items"]
            .as_array_mut()
            .unwrap()
            .push(row);
        assert!(!list(&invalid));
        view["platforms"][0]["items"][0]["credentials"] = json!({});
        assert!(!list(&view));
    }
}
