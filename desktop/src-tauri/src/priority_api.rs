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
    if detail {
        allowed.push("items");
    }
    fields(value, &allowed)
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
                        fields(
                            item,
                            &[
                                "account_id",
                                "account_name",
                                "original_priority",
                                "target_priority",
                                "observed_priority",
                                "status",
                            ],
                        ) && account_id(&item["account_id"])
                            && ids.insert(item["account_id"].as_u64().unwrap())
                            && text(&item["account_name"])
                            && item["original_priority"].is_i64()
                            && item["target_priority"].as_u64() == Some(index as u64)
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
}
