#[cfg(test)]
mod tests {
    use crate::allowed_request as allowed;
    use serde_json::json;
    use sub2ops_protocol::response::client_response;
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
            ("DELETE", "/proxy-management/data"),
        ] {
            assert!(!allowed(m, p));
        }
        let mut r = json!({"id":7,"name":"fixture","host":"127.0.0.1","port":1080,"protocol":"socks5","status":"active","username_set":true,"password_set":true});
        assert!(client_response("GET", "/proxy-management/7", r.clone()).is_some());
        r["password"] = "fixture".into();
        assert!(client_response("GET", "/proxy-management/7", r)
            .unwrap()
            .get("password")
            .is_none());
        assert!(client_response(
            "GET",
            "/proxy-management/7/credentials",
            json!({"username":"fixture","password":"fixture"})
        )
        .is_some());
    }
}
