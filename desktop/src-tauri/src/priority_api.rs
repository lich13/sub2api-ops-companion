#[cfg(test)]
mod tests {
    use serde_json::json;
    use sub2ops_protocol::response::client_response;
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
        let summary = json!({"id":id,"platform":"openai","base":1,"waiting_reason":"等待账号空闲","waiting_account_ids":[1],"status":"running","cancel_requested":false,"reason":"正在保存","total":1,"change_count":1,"verified_count":0,"created_at":"2026-10-21T00:00:00Z","updated_at":"2026-10-21T00:00:00Z","completed_at":null});
        let path = format!("/account-priority-orders/{id}");
        let overview = format!("{path}?schema=2&details=0");
        assert!(client_response("GET", &overview, summary.clone()).is_some());
        assert!(client_response("GET", &path, summary.clone()).is_none());
        let mut detail = summary.clone();
        detail["items"] = json!([{"account_id":1,"account_name":"fixture","platform":"openai","original_priority":8,"target_priority":1,"observed_priority":null,"status":"pending"}]);
        assert!(client_response("GET", &path, detail.clone()).is_some());
        assert!(
            client_response("GET", &path.replace(&id, &"b".repeat(32)), detail.clone()).is_none()
        );
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
}
