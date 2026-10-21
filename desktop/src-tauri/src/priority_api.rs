use serde_json::Value;
use std::collections::HashSet;

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
pub fn task(value: &Value, detail: bool) -> bool {
    task_schema(value, detail, false)
}
pub fn task_v2(value: &Value, detail: bool) -> bool {
    task_schema(value, detail, true)
}
fn task_schema(value: &Value, detail: bool, v2: bool) -> bool {
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
    ];
    if v2 {
        allowed.extend(["platform", "base", "waiting_reason", "waiting_account_ids"]);
    }
    let base = if v2 {
        value["base"].as_u64().unwrap_or(2)
    } else {
        0
    };
    if detail {
        allowed.push("items");
    }
    fields(value, &allowed)
        && (!v2
            || base <= 1
                && (if base == 1 {
                    platform(&value["platform"])
                } else {
                    value["platform"].is_null()
                })
                && text(&value["waiting_reason"])
                && value["waiting_account_ids"].as_array().is_some_and(|ids| {
                    let mut seen = HashSet::new();
                    ids.len() as u64 <= value["total"].as_u64().unwrap_or(0)
                        && ids
                            .iter()
                            .all(|id| account_id(id) && seen.insert(id.as_u64().unwrap()))
                }))
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
                items.len() as u64 == value["total"].as_u64().unwrap_or(u64::MAX)
                    && items.iter().enumerate().all(|(index, item)| {
                        let mut item_fields = vec![
                            "account_id",
                            "account_name",
                            "original_priority",
                            "target_priority",
                            "observed_priority",
                            "status",
                        ];
                        if v2 && base == 1 {
                            item_fields.push("platform");
                        }
                        fields(item, &item_fields)
                            && (!v2 || base == 0 || item["platform"] == value["platform"])
                            && account_id(&item["account_id"])
                            && ids.insert(item["account_id"].as_u64().unwrap())
                            && text(&item["account_name"])
                            && item["original_priority"].is_i64()
                            && item["target_priority"].as_u64() == Some(index as u64 + base)
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
    fields(value, &["items", "version", "active_task", "last_task"])
        && hex(&value["version"], 64)
        && value["items"].as_array().is_some_and(|items| {
            let mut ids = HashSet::new();
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
                    && ["name", "platform", "type", "reason", "status"]
                        .iter()
                        .all(|key| text(&row[*key]))
                    && row["priority"].is_i64()
                    && hex(&row["version"], 64)
                    && row["editable"].is_boolean()
                    && row["schedulable"].is_boolean()
            })
        })
        && ["active_task", "last_task"]
            .iter()
            .all(|key| value[*key].is_null() || task(&value[*key], false))
        && (value["active_task"].is_null()
            || matches!(
                value["active_task"]["status"].as_str(),
                Some("queued" | "running" | "checking")
            ))
}

fn platform(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 64)
}
pub fn list_v2(value: &Value) -> bool {
    fields(value, &["platforms", "active_task", "legacy_last_task"])
        && value["platforms"].as_array().is_some_and(|groups| {
            let mut platforms = HashSet::new();
            let mut ids = HashSet::new();
            groups.iter().all(|group| {
                fields(group, &["platform", "version", "items", "last_task"])
                    && platform(&group["platform"])
                    && platforms.insert(group["platform"].as_str().unwrap())
                    && list(&serde_json::json!({"items":group["items"],"version":group["version"],"active_task":null,"last_task":null}))
                    && group["items"].as_array().is_some_and(|items| items.iter().all(|item| item["platform"] == group["platform"] && ids.insert(item["id"].as_u64())))
                    && (group["last_task"].is_null() || task_v2(&group["last_task"], false) && group["last_task"]["base"] == 1 && group["last_task"]["platform"] == group["platform"])
            })
        })
        && (value["active_task"].is_null() || task_v2(&value["active_task"], false)
            && value["active_task"]["base"] == 1
            && matches!(value["active_task"]["status"].as_str(),Some("queued" | "running" | "checking")))
        && (value["legacy_last_task"].is_null() || task_v2(&value["legacy_last_task"], false) && value["legacy_last_task"]["base"] == 0)
}
pub fn query(path: &str, detail: bool) -> bool {
    let Some((_, query)) = path.split_once('?') else {
        return true;
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
pub fn is_v2(path: &str) -> bool {
    path.split_once('?')
        .is_some_and(|(_, q)| q.split('&').any(|p| p == "schema=2"))
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
            ("POST", "/account-priority-orders".to_owned()),
            ("GET", format!("/account-priority-orders/{id}")),
            ("POST", format!("/account-priority-orders/{id}/cancel")),
        ] {
            assert!(crate::allowed_request(method, &path));
            assert!(!crate::allowed_request(method, &format!("{path}?limit=1")));
            assert!(!crate::allowed_request(method, &format!("{path}/")));
        }
        for (method, path) in [
            ("GET", "/account-priority-orders".to_owned()),
            ("POST", "/account-priorities".to_owned()),
            ("PUT", format!("/account-priority-orders/{id}")),
            ("GET", "/account-priority-orders/invalid".to_owned()),
        ] {
            assert!(!crate::allowed_request(method, &path));
        }
    }
    #[test]
    fn validates_full_account_sets_and_order_progress_without_accepting_extra_data() {
        let mut view = json!({"items":[{"id":1,"name":"fixture","platform":"other","type":"apikey","priority":9,"version":"a".repeat(64),"editable":true,"reason":"","status":"active","schedulable":false}],"version":"b".repeat(64),"active_task":null,"last_task":null});
        assert!(list(&view));
        view["items"][0]["credentials"] = json!({});
        assert!(!list(&view));
        view["items"][0]
            .as_object_mut()
            .unwrap()
            .remove("credentials");
        let row = view["items"][0].clone();
        view["items"].as_array_mut().unwrap().push(row);
        assert!(!list(&view));
        let job = json!({"id":"a".repeat(32),"status":"checking","cancel_requested":false,"reason":"","total":1,"change_count":1,"verified_count":0,"created_at":"2026-10-21T00:00:00Z","updated_at":"2026-10-21T00:00:00Z","completed_at":null,"items":[{"account_id":1,"account_name":"fixture","original_priority":9,"target_priority":0,"observed_priority":null,"status":"dispatched"}]});
        assert!(task(&job, true));
        assert!(!task(&job, false));
        for (key, bad) in [
            ("status", json!("unknown")),
            ("verified_count", json!(2)),
            ("updated_at", json!("invalid")),
        ] {
            let mut invalid = job.clone();
            invalid[key] = bad;
            assert!(!task(&invalid, true));
        }
        let mut invalid = job;
        invalid["items"][0]["target_priority"] = 1.into();
        assert!(!task(&invalid, true));
    }
    #[test]
    fn platform_protocol_keeps_legacy_exact_and_validates_summary_and_identity() {
        let id = "a".repeat(32);
        for path in [
            "/account-priorities?schema=2".to_owned(),
            format!("/account-priority-orders/{id}?schema=2"),
            format!("/account-priority-orders/{id}?details=0&schema=2"),
        ] {
            assert!(crate::allowed_request("GET", &path));
        }
        for path in [
            "/account-priorities?schema=2&details=0".to_owned(),
            format!("/account-priority-orders/{id}?details=0"),
            format!("/account-priority-orders/{id}?schema=2&schema=2"),
            format!("/account-priority-orders/{id}?schema=2&details=1"),
        ] {
            assert!(!crate::allowed_request("GET", &path));
        }
        assert!(crate::allowed_request(
            "POST",
            "/account-priority-orders?schema=2"
        ));
        assert!(!crate::allowed_request(
            "POST",
            "/account-priority-orders?schema=2&details=0"
        ));
        let summary = json!({"id":id,"platform":"openai","base":1,"waiting_reason":"等待账号空闲","waiting_account_ids":[1],"status":"running","cancel_requested":false,"reason":"正在保存","total":1,"change_count":1,"verified_count":0,"created_at":"2026-10-21T00:00:00Z","updated_at":"2026-10-21T00:00:00Z","completed_at":null});
        assert!(task_v2(&summary, false));
        assert!(!task(&summary, false));
        assert!(!task_v2(&summary, true));
        let mut detail = summary.clone();
        detail["items"] = json!([{"account_id":1,"account_name":"fixture","platform":"openai","original_priority":8,"target_priority":1,"observed_priority":null,"status":"pending"}]);
        assert!(task_v2(&detail, true));
        detail["items"][0]["platform"] = "grok".into();
        assert!(!task_v2(&detail, true));
        detail["items"][0]["platform"] = "openai".into();
        detail["items"][0]["target_priority"] = 0.into();
        assert!(!task_v2(&detail, true));
        let mut view = json!({"platforms":[{"platform":"openai","version":"b".repeat(64),"items":[{"id":1,"name":"fixture","platform":"openai","type":"oauth","priority":8,"version":"c".repeat(64),"editable":true,"reason":"","status":"active","schedulable":true}],"last_task":summary}],"active_task":summary,"legacy_last_task":null});
        assert!(list_v2(&view));
        assert!(!list(&view));
        view["platforms"][0]["items"][0]["credentials"] = json!({});
        assert!(!list_v2(&view));
        let mut legacy = summary;
        legacy["base"] = 0.into();
        legacy["platform"] = Value::Null;
        assert!(task_v2(&legacy, false));
    }
}
