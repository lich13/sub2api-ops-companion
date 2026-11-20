#[cfg(test)]
mod tests {
    use serde_json::json;
    use sub2ops_protocol::response::client_response;
    #[test]
    fn account_quota_projects_only_presence_and_revision() {
        let mut raw = json!({"account_id":1,"has_access_token":true,"revision":"a".repeat(64)});
        let path = "/accounts/1/key-quota-settings";
        let public = raw.clone();
        raw["access_token"] = "fixture".into();
        assert_eq!(client_response("GET", path, raw.clone()), Some(public));
        assert!(client_response("GET", "/accounts/2/key-quota-settings", raw).is_none());
    }
}
