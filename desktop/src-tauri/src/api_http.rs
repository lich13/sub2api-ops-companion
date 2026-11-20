use serde_json::Value;
use std::{io::Write, path::Path, time::Duration};

const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_OPERATION_JSON_BYTES: usize = 128 * 1024 * 1024;

fn response_kind(value: &str) -> &'static str {
    match value.split(';').next().unwrap_or("").trim() {
        "application/json" => "json",
        "text/html" => "html",
        "text/plain" => "text",
        "" => "missing",
        _ => "other",
    }
}

fn diagnostic(
    file: &Path,
    method: &str,
    path: &str,
    status: u16,
    kind: &str,
    stage: &str,
    request_id: &str,
) {
    use std::os::unix::fs::OpenOptionsExt;
    let Some(parent) = file.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let truncate = std::fs::metadata(file).is_ok_and(|m| m.len() > 64 * 1024);
    let route = path
        .split('?')
        .next()
        .unwrap_or("")
        .split('/')
        .map(|s| {
            if s.parse::<u64>().is_ok() {
                ":id"
            } else if s
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c == b'-' || c == b'_')
            {
                s
            } else {
                "unknown"
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(!truncate)
        .truncate(truncate)
        .mode(0o600)
        .open(file)
    {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let _ = writeln!(f, "{stamp} method={method} path={route} status={status} type={kind} stage={stage} request_id={request_id}");
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct HttpFailure {
    #[serde(flatten)]
    pub failure: sub2ops_protocol::dto::BridgeFailure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<Value>,
}
impl HttpFailure {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            failure: sub2ops_protocol::dto::BridgeFailure::new(code, message),
            connection: None,
        }
    }
    pub fn background() -> Self {
        Self {
            failure: sub2ops_protocol::dto::BridgeFailure::background(),
            connection: None,
        }
    }
}
impl std::ops::Deref for HttpFailure {
    type Target = sub2ops_protocol::dto::BridgeFailure;
    fn deref(&self) -> &Self::Target {
        &self.failure
    }
}
impl From<String> for HttpFailure {
    fn from(message: String) -> Self {
        Self::new("client_error", message)
    }
}
impl From<&str> for HttpFailure {
    fn from(message: &str) -> Self {
        Self::new("client_error", message)
    }
}

fn model_directory_request(method: &str, path: &str) -> bool {
    use sub2ops_protocol::routes::{client_endpoint, Reply};
    match client_endpoint(method, path).map(|e| e.reply) {
        Some(Reply::Models) => path.split_once('?').is_some_and(|(_, q)| {
            url::form_urlencoded::parse(q.as_bytes())
                .any(|(k, v)| k == "purpose" && v == "model_test")
        }),
        Some(Reply::ModelTest) => {
            method == "POST"
                && path
                    .split('?')
                    .next()
                    .is_some_and(|p| p.ends_with("/model-tests"))
        }
        Some(Reply::Detection) => method == "PUT",
        _ => false,
    }
}

pub(crate) async fn http_detailed(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    log: &Path,
) -> Result<Value, HttpFailure> {
    let parsed_method =
        reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "请求方式无效")?;
    let mut req = client
        .request(parsed_method, format!("{base}/api/desktop/v1{path}"))
        .header("x-api-key", key)
        .header("Accept", "application/json");
    if path == "/sub2api-system/update" {
        req = req.timeout(Duration::from_secs(930));
    }
    if path.starts_with("/sub2api-config") {
        req = req.timeout(Duration::from_secs(65));
    }
    if path.starts_with("/proxies/") || path.starts_with("/proxy-management") {
        req = req.timeout(Duration::from_secs(105));
    }
    if path.ends_with("/apply-oauth-credentials")
        || path == "/account-creation"
        || path.starts_with("/account-creation/")
        || path == "/account-creation?sync_models=true"
    {
        req = req.timeout(Duration::from_secs(390));
    }
    if path.ends_with("/usage") {
        req = req.timeout(Duration::from_secs(105));
    }
    if method == "DELETE" {
        req = req.timeout(Duration::from_secs(20));
    }
    if model_directory_request(method, path) {
        req = req.timeout(Duration::from_secs(20));
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let mut response = req.send().await.map_err(|e| {
        diagnostic(
            log,
            method,
            path,
            0,
            "missing",
            if e.is_timeout() {
                "headers_timeout"
            } else {
                "connect"
            },
            "-",
        );
        if e.is_timeout() {
            HttpFailure::new("network_timeout", "连接超时，未重放操作")
        } else {
            HttpFailure::new("network_unavailable", "连接暂时中断")
        }
    })?;
    let status = response.status().as_u16();
    let kind = response_kind(
        response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or(""),
    );
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .filter(|s| s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
        .unwrap_or("-")
        .to_string();
    let fail = |stage: &str, message: &str| {
        diagnostic(log, method, path, status, kind, stage, &request_id);
        let mut error = HttpFailure::new(stage, message);
        error.failure.status = Some(status);
        error.failure.request_id = (request_id != "-").then(|| request_id.clone());
        error
    };
    let limit = if path.starts_with("/account-operations/") {
        MAX_OPERATION_JSON_BYTES
    } else {
        MAX_JSON_BYTES
    };
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| {
        fail(
            if e.is_timeout() {
                "body_timeout"
            } else {
                "body_interrupted"
            },
            if e.is_timeout() {
                "响应读取超时，未重放操作"
            } else {
                "响应读取中断"
            },
        )
    })? {
        if bytes.len() + chunk.len() > limit {
            return Err(fail("size", "服务响应超过大小限制"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let parsed = serde_json::from_slice::<Value>(&bytes);
    if !(200..300).contains(&status) {
        let fallback = match status {
            401 | 403 => "管理员 API Key 被拒绝，请重新连接",
            429 => "请求过于频繁",
            502..=504 => "网关暂不可用",
            500..=599 => "服务暂不可用",
            _ => "请求失败",
        };
        let detail = parsed.as_ref().ok().and_then(|d| d.get("detail"));
        let message = detail
            .and_then(Value::as_str)
            .or_else(|| detail.and_then(|d| d["message"].as_str()))
            .unwrap_or(fallback);
        let mut error = fail(
            if status == 401 || status == 403 {
                "auth"
            } else {
                "http"
            },
            &format!(
                "{} [{}]",
                if path.starts_with("/proxies/") || path.starts_with("/sub2api-config") {
                    message.to_string()
                } else {
                    message.chars().take(240).collect::<String>()
                },
                status
            ),
        );
        error.failure.code = match status {
            401 | 403 if detail.is_some_and(|d| d["code"] == "model_directory_revoked") => {
                "model_directory_revoked"
            }
            401 | 403 => "auth_rejected",
            409 => "conflict",
            _ => detail
                .and_then(|d| d["code"].as_str())
                .filter(|code| {
                    code.len() <= 64 && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                })
                .unwrap_or("http_error"),
        }
        .to_owned();
        error.connection = detail
            .and_then(|d| d.get("connection"))
            .and_then(sub2ops_protocol::response::connection);
        return Err(error);
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(fail("empty", "服务返回空响应"));
    }
    if kind != "json" {
        return Err(fail("content_type", "服务返回了非 JSON 响应"));
    }
    let data = parsed.map_err(|_| fail("json", "服务 JSON 响应不完整或格式无效"))?;
    sub2ops_protocol::response::client_response(method, path, data)
        .ok_or_else(|| fail("schema", "服务数据结构不兼容，请检查客户端与服务版本"))
}

#[cfg(test)]
mod tests {
    use super::http_detailed as http;
    use sub2ops_protocol::semantics::{operation_valid, recoveries_valid};
    fn accepts_reply(path: &str, value: &Value) -> bool {
        ["GET", "POST", "PUT", "DELETE"].iter().any(|method| {
            sub2ops_protocol::response::client_response(method, path, value.clone()).is_some()
        })
    }
    fn connection_valid(data: &Value) -> bool {
        accepts_reply("/connection-status", data)
    }
    #[test]
    fn access_token_reveal_requires_the_requested_id_and_a_nonempty_token() {
        let path = "/accounts/7/key-quota-settings/access-token";
        assert!(accepts_reply(
            path,
            &serde_json::json!({"account_id":7,"access_token":"fixture-token"})
        ));
        let public = sub2ops_protocol::response::client_response(
            "GET",
            path,
            serde_json::json!({"account_id":7,"access_token":"fixture-token","extra":"unexpected"}),
        )
        .unwrap();
        assert!(public.get("extra").is_none());
        for value in [
            serde_json::json!({"account_id":8,"access_token":"fixture-token"}),
            serde_json::json!({"account_id":7,"access_token":""}),
        ] {
            assert!(!accepts_reply(path, &value));
        }
    }
    use super::*;
    use std::{io::Read, net::TcpListener};

    #[test]
    fn native_usage_summary_validates_balances_windows_and_missing_values() {
        let data = serde_json::json!({"branch":"native","native_read":true,"status":"known","actions":["query_usage"],"today":null,
            "windows":[{"key":"shared","label":"共享 1d","source":"local_estimate","estimated":true,"status":"known","used_percent":0,"observed_at":null,"reset_at":null}],
            "balances":[{"currency":"CNY","balance":0,"status":"stale","observed_at":null}]});
        assert!(accepts_reply("/accounts/1/usage", &data));
        assert!(accepts_reply(
            "/quota-refresh",
            &serde_json::json!({"status":"completed","items":[{"usage":data}]})
        ));
        assert!(!accepts_reply(
            "/quota-refresh",
            &serde_json::json!({"status":"completed","items":[{"usage":{"windows":[]}}]})
        ));
        for (field, value) in [
            (
                "balances",
                serde_json::json!([{"currency":"USD","balance":"unknown"}]),
            ),
            (
                "windows",
                serde_json::json!([{"key":"x","label":"x","source":"passive","status":"known","used_percent":"unknown"}]),
            ),
            ("today", serde_json::json!({"requests":1})),
        ] {
            let mut invalid = data.clone();
            invalid[field] = value;
            assert!(!accepts_reply("/accounts/1/usage", &invalid));
        }
        for status in ["error", "unavailable", "authorization_required"] {
            let mut data = data.clone();
            data["status"] = status.into();
            assert!(accepts_reply("/accounts/1/usage", &data));
        }
    }

    #[test]
    fn capabilities_accepts_existing_and_android_clients() {
        for platforms in [
            serde_json::json!(["macos"]),
            serde_json::json!(["macos", "android"]),
        ] {
            assert!(accepts_reply(
                "/capabilities",
                &serde_json::json!({
                    "api_version":1,"service":"Sub2API Ops Companion","version":"fixture","implementation":"rust","build_revision":"fixture","upstream_connection":{"state":"ok","endpoint":"none","http_status":null,"message":"","retryable":false,"checked_at":null}, "platforms":platforms
                })
            ));
        }
    }

    #[tokio::test]
    async fn response_limits_cover_exact_boundaries_and_chunked_overflow() {
        for (path, limit) in [
            ("/snapshot", MAX_JSON_BYTES),
            ("/account-operations/fixture", MAX_OPERATION_JSON_BYTES),
        ] {
            for (extra, chunked) in [(0, false), (0, true), (1, false), (1, true)] {
                let length = limit + extra;
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let base = format!("http://{}", listener.local_addr().unwrap());
                let server = std::thread::spawn(move || {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(20)))
                        .unwrap();
                    let mut request = [0; 4096];
                    let _ = socket.read(&mut request);
                    let framing = if chunked {
                        "Transfer-Encoding: chunked".to_owned()
                    } else {
                        format!("Content-Length: {length}")
                    };
                    // An HTTP failure is intentional: it proves that a body at
                    // the boundary reaches ordinary status handling, not size rejection.
                    let headers = format!("HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\n{framing}\r\nConnection: close\r\n\r\n");
                    if socket.write_all(headers.as_bytes()).is_err() {
                        return;
                    }
                    let mut block = vec![b' '; 65_536];
                    let mut sent = 0;
                    while sent < length {
                        if sent == 0 {
                            block[..2].copy_from_slice(b"{}");
                        } else {
                            block[..2].fill(b' ');
                        }
                        let size = block.len().min(length - sent);
                        if chunked && write!(socket, "{size:x}\r\n").is_err() {
                            return;
                        }
                        if socket.write_all(&block[..size]).is_err() {
                            return;
                        }
                        if chunked && socket.write_all(b"\r\n").is_err() {
                            return;
                        }
                        sent += size;
                    }
                    if chunked {
                        let _ = socket.write_all(b"0\r\n\r\n");
                    }
                });
                let directory = tempfile::tempdir().unwrap();
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(30))
                    .build()
                    .unwrap();
                let error = http(
                    &client,
                    &base,
                    "fixture-admin-key",
                    "GET",
                    path,
                    None,
                    &directory.path().join("network.log"),
                )
                .await
                .unwrap_err();
                server.join().unwrap();
                if extra == 0 {
                    assert!(
                        error.message.contains("网关暂不可用 [502]"),
                        "at-limit body must reach HTTP status handling"
                    );
                } else {
                    assert!(
                        error.message.contains("服务响应超过大小限制"),
                        "oversize body must be rejected"
                    );
                }
                assert!(
                    !std::fs::read_to_string(directory.path().join("network.log"))
                        .unwrap_or_default()
                        .contains("fixture-admin-key")
                );
            }
        }
    }

    async fn serve(
        status: &str,
        headers: &str,
        body: &str,
        delay: bool,
    ) -> (Result<Value, String>, String) {
        serve_path("/snapshot", status, headers, body, delay).await
    }

    async fn serve_path(
        path: &str,
        status: &str,
        headers: &str,
        body: &str,
        delay: bool,
    ) -> (Result<Value, String>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let reply = format!("HTTP/1.1 {status}\r\n{headers}\r\nx-request-id: test-request\r\nConnection: close\r\n\r\n{body}");
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(reply.as_bytes());
            if delay {
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("network.log");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let result = http(&client, &base, "secret-admin-key", "GET", path, None, &log).await;
        thread.join().unwrap();
        (
            result.map_err(|error| error.failure.message),
            std::fs::read_to_string(log).unwrap_or_default(),
        )
    }

    #[tokio::test]
    async fn http_projects_token_reply_and_checks_requested_account() {
        let path = "/accounts/7/key-quota-settings/access-token";
        for id in [7, 8] {
            let body = serde_json::json!({"account_id":id,"access_token":"fixture","debug_field":"hidden"}).to_string();
            let (result, _) = serve_path(
                path,
                "200 OK",
                "Content-Type: application/json",
                &body,
                false,
            )
            .await;
            if id == 7 {
                assert_eq!(
                    result.unwrap(),
                    serde_json::json!({"account_id":7,"access_token":"fixture"})
                );
            } else {
                assert!(result.unwrap_err().contains("服务数据结构不兼容"));
            }
        }
    }

    #[tokio::test]
    async fn classifies_failures_without_leaking_body_or_key() {
        for (status, headers, body, expected, stage, delay) in [
            (
                "502 Bad Gateway",
                "Content-Type: text/html",
                "<html>secret-body</html>",
                "网关暂不可用 [502]",
                "http",
                false,
            ),
            (
                "200 OK",
                "Content-Type: application/json",
                "",
                "空响应",
                "empty",
                false,
            ),
            (
                "200 OK",
                "Content-Type: text/html",
                "secret-body",
                "非 JSON",
                "content_type",
                false,
            ),
            (
                "200 OK",
                "Content-Type: application/json",
                "{",
                "JSON 响应不完整",
                "json",
                false,
            ),
            (
                "200 OK",
                "Content-Type: application/json",
                "{}",
                "数据结构不兼容",
                "schema",
                false,
            ),
            (
                "401 Unauthorized",
                "Content-Type: text/html",
                "secret-body",
                "API Key 被拒绝",
                "auth",
                false,
            ),
            (
                "200 OK",
                "Content-Type: application/json\r\nContent-Length: 1000",
                "{",
                "读取中断",
                "body_interrupted",
                false,
            ),
            (
                "200 OK",
                "Content-Type: application/json\r\nContent-Length: 1000",
                "{",
                "读取超时",
                "body_timeout",
                true,
            ),
        ] {
            let (result, log) = serve(status, headers, body, delay).await;
            assert!(result.unwrap_err().contains(expected));
            assert!(log.contains(&format!("stage={stage}")));
            assert!(!log.contains("secret"));
        }
    }

    #[tokio::test]
    async fn structured_upstream_failure_survives_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let connection = serde_json::json!({"state":"auth_rejected","endpoint":"verify","http_status":403,
            "message":"管理员 Key 被拒绝","retryable":false,"checked_at":"2026-10-07T00:00:00Z"});
        let mut received = connection.clone();
        received["debug_field"] = "fixture-hidden".into();
        let body = serde_json::json!({"detail":{"code":"auth_rejected","message":"管理员 Key 被拒绝","connection":received}}).to_string();
        let reply = format!("HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(reply.as_bytes());
        });
        let temp = tempfile::tempdir().unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let error = http_detailed(
            &client,
            &base,
            "fixture-key",
            "GET",
            "/snapshot",
            None,
            &temp.path().join("network.log"),
        )
        .await
        .unwrap_err();
        thread.join().unwrap();
        assert_eq!(error.connection, Some(connection));
        assert!(error.message.contains("管理员 Key 被拒绝"));
        assert!(!std::fs::read_to_string(temp.path().join("network.log"))
            .unwrap()
            .contains("fixture-key"));
    }

    #[tokio::test]
    async fn valid_snapshot_and_structure_guard() {
        let body = r#"{"observed_at":"2026-09-25T00:00:00Z","accounts":[],"groups":[],"errors":[],"recoveries":[]}"#;
        let (result, log) = serve("200 OK", "Content-Type: application/json", body, false).await;
        assert!(result.is_ok());
        assert!(log.is_empty());
        assert!(!accepts_reply(
            "/snapshot",
            &serde_json::json!({"accounts":null})
        ));
    }

    #[tokio::test]
    async fn proxy_diagnostics_use_the_extended_request_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let body = serde_json::json!({
            "success": true, "message": "fixture ok", "latency_ms": 31,
            "ip_address": "203.0.113.7", "city": "Taipei", "region": "Taiwan",
            "country": "Taiwan", "country_code": "TW"
        })
        .to_string();
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            std::thread::sleep(Duration::from_millis(200));
            let _ = stream.write_all(reply.as_bytes());
        });
        let temp = tempfile::tempdir().unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let result = http_detailed(
            &client,
            &base,
            "fixture-key",
            "POST",
            "/proxies/7/test",
            Some(serde_json::json!({})),
            &temp.path().join("network.log"),
        )
        .await;
        thread.join().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert!(!temp.path().join("network.log").exists());
    }

    #[tokio::test]
    async fn model_directory_routes_extend_timeout_without_replaying_writes() {
        for (method, path) in [
            ("GET", "/accounts/7/models?purpose=model_test"),
            ("GET", "/accounts/7/models?purpose=model_test&refresh=true"),
            ("POST", "/accounts/7/model-tests"),
            ("PUT", "/accounts/7/model-detection"),
        ] {
            assert!(model_directory_request(method, path));
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let expected_method = method.to_owned();
            let thread = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let size = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..size]).starts_with(&expected_method));
                std::thread::sleep(Duration::from_millis(200));
                stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
                listener.set_nonblocking(true).unwrap();
                std::thread::sleep(Duration::from_millis(50));
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            });
            let directory = tempfile::tempdir().unwrap();
            let client = reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .timeout(Duration::from_millis(100))
                .build()
                .unwrap();
            let error = http_detailed(
                &client,
                &base,
                "fixture-admin-key",
                method,
                path,
                (method != "GET").then(|| serde_json::json!({})),
                &directory.path().join("network.log"),
            )
            .await
            .unwrap_err();
            assert!(
                error.message.contains("503"),
                "extended timeout must receive the HTTP response"
            );
            thread.join().unwrap();
        }
        for query in [
            "refresh=true&purpose=model_test",
            "purpose=model_test&refresh=false",
        ] {
            assert!(model_directory_request(
                "GET",
                &format!("/accounts/7/models?{query}")
            ));
        }
        for (method, path) in [
            ("GET", "/accounts/7/models"),
            ("GET", "/accounts/7/model-detection"),
            ("POST", "/accounts/7/model-detection"),
            ("GET", "/snapshot"),
            ("GET", "/accounts/0/models?purpose=model_test"),
        ] {
            assert!(!model_directory_request(method, path));
        }
    }

    #[test]
    fn proxy_diagnostic_responses_require_safe_known_shapes() {
        let test = serde_json::json!({
            "success": true, "message": format!("https://example.invalid/?token=fixture\n  password=fixture\n{}", "原始错误".repeat(1000)), "latency_ms": 31,
            "ip_address": "203.0.113.7", "city": "Taipei", "region": "Taiwan",
            "country": "Taiwan", "country_code": "TW"
        });
        assert!(accepts_reply("/proxies/7/test", &test));
        for (key, value) in [
            ("success", serde_json::json!("true")),
            ("latency_ms", serde_json::json!("slow")),
            ("ip_address", serde_json::json!(7)),
        ] {
            let mut bad = test.clone();
            bad[key] = value;
            assert!(!accepts_reply("/proxies/7/test", &bad), "{key}");
        }

        let quality = serde_json::json!({
            "score": 88, "grade": "yellow", "summary": "完整质量报告".repeat(1000),
            "exit_ip": "203.0.113.8", "country": "Taiwan", "country_code": "TW",
            "base_latency_ms": 20, "passed_count": 1, "warn_count": 0,
            "failed_count": 0, "challenge_count": 0, "checked_at": null,
            "items": [{"target": "https://example.invalid", "status": "pass",
                "http_status": 200, "latency_ms": 20, "message": "ok"}]
        });
        assert!(accepts_reply("/proxies/7/quality-check", &quality));
        let mut bad = quality.clone();
        bad["score"] = serde_json::json!("invalid");
        assert!(!accepts_reply("/proxies/7/quality-check", &bad));
        let mut bad = quality.clone();
        bad["items"][0]["status"] = serde_json::json!(0);
        assert!(!accepts_reply("/proxies/7/quality-check", &bad));
        let mut bad = quality.clone();
        bad["items"][0]["password"] = serde_json::json!("fixture-secret");
        assert_eq!(
            sub2ops_protocol::response::client_response("POST", "/proxies/7/quality-check", bad),
            Some(quality)
        );
    }

    #[test]
    fn recovery_response_accepts_legacy_and_complete_card_recovery_metadata() {
        let legacy = serde_json::json!([{"id":1,"account_id":1,"legacy":true}]);
        let ordinary =
            serde_json::json!([{"id":2,"account_id":1,"kind":"quota_recovery","legacy":false}]);
        let connection_test = serde_json::json!([{"id":4,"account_id":1,"kind":"connection_test","result_status":"completed","legacy":false}]);
        let card = serde_json::json!([{"id":3,"account_id":1,"kind":"reset_credit","legacy":false,
            "test_completed_at":"2026-10-01T01:11:00Z","recovered_at":"2026-10-01T01:12:00Z",
            "reset_credit":{"completed_at":"2026-10-01T01:10:00Z","verification_method":"connection"}}]);

        assert!(recoveries_valid(&legacy));
        assert!(recoveries_valid(&ordinary));
        assert!(recoveries_valid(&connection_test));
        assert!(recoveries_valid(&card));
        assert!(accepts_reply(
            "/recoveries",
            &serde_json::json!({"items":card})
        ));
    }

    #[test]
    fn recovery_response_rejects_partial_or_invalid_card_recovery_metadata() {
        let valid = serde_json::json!({"id":1,"account_id":1,"kind":"reset_credit","legacy":false,
            "test_completed_at":"2026-10-01T01:11:00Z","recovered_at":"2026-10-01T01:12:00Z",
            "reset_credit":{"completed_at":"2026-10-01T01:10:00Z","verification_method":"model"}});
        let mut invalid = Vec::new();
        let mut row = valid.clone();
        row["reset_credit"] = serde_json::Value::Null;
        invalid.push(row);
        let mut row = valid.clone();
        row["reset_credit"]["completed_at"] = serde_json::json!("");
        invalid.push(row);
        let mut row = valid.clone();
        row["reset_credit"]["verification_method"] = serde_json::json!("unknown");
        invalid.push(row);
        let mut row = valid.clone();
        row["reset_credit"]["consumed"] = serde_json::json!(false);
        invalid.push(row);
        let mut row = valid.clone();
        row["legacy"] = serde_json::json!(true);
        invalid.push(row);
        let mut row = valid.clone();
        row["legacy"] = serde_json::Value::Null;
        invalid.push(row);
        let mut row = valid.clone();
        row["test_completed_at"] = serde_json::json!("");
        invalid.push(row);
        let mut row = valid.clone();
        row["recovered_at"] = serde_json::Value::Null;
        invalid.push(row);
        let mut row = valid;
        row["kind"] = serde_json::json!("unknown");
        invalid.push(row);

        for row in invalid {
            assert!(!recoveries_valid(&serde_json::json!([row])));
        }
        assert!(!recoveries_valid(&serde_json::json!({"items":[]})));
    }

    #[test]
    fn detection_platforms_accept_partial_and_four_platform_configuration() {
        use serde_json::json;
        let mut config = json!({"oauth":{"revision":"fixture","model_detection_model_id":"gpt-6-luna","oauth_early_reset_enabled":false,"oauth_early_reset_interval_seconds":60,"model_detection_platforms":{"openai":{"engine":"modeltrace","sample_count":1}}},"bark":{"revision":"fixture"}});
        assert!(accepts_reply("/config", &config));
        for platform in ["anthropic", "grok", "deepseek"] {
            config["oauth"]["model_detection_platforms"][platform] =
                json!({"engine":"lm_detector","sample_count":3});
        }
        assert!(accepts_reply("/config", &config));
        config["oauth"]["model_detection_platforms"]["anthropic"]["engine"] = "modeltrace".into();
        assert!(!accepts_reply("/config", &config));
        let mut detection = json!({"account_id":1,"version":"fixture","enabled":false,"interval_minutes":15,"schedule_mode":"smart","min_interval_minutes":5,"model_id":"","model_source":"account","global_model_id":"","model_override":null,"status":"disabled","engine":"lm_detector","sample_count":1});
        assert!(accepts_reply("/accounts/1/model-detection", &detection));
        let mut policy = detection.clone();
        policy["verdict_mode"] = "relaxed".into();
        policy["degraded_model_id"] = "gpt-5.6-luna".into();
        assert!(accepts_reply("/accounts/1/model-detection", &policy));
        policy["degraded_model_id"] = json!(false);
        assert!(!accepts_reply("/accounts/1/model-detection", &policy));
        detection["sample_count"] = "two".into();
        assert!(!accepts_reply("/accounts/1/model-detection", &detection));
    }

    #[test]
    fn detection_templates_and_bank_response_guards() {
        let detection = serde_json::json!({"account_id":1,"version":"fixture","enabled":false,"interval_minutes":15,"schedule_mode":"fixed","min_interval_minutes":5,"model_id":"gpt-6-luna","model_source":"global","model_override":null,"global_model_id":"gpt-6-luna","status":"disabled"});
        assert!(accepts_reply("/accounts/1/model-detection", &detection));
        for field in [
            "schedule_mode",
            "min_interval_minutes",
            "model_source",
            "version",
            "global_model_id",
        ] {
            let mut missing = detection.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                !accepts_reply("/accounts/1/model-detection", &missing),
                "{field}"
            );
        }
        let mut smart_detection = detection.clone();
        smart_detection["schedule_mode"] = serde_json::json!("smart");
        smart_detection["min_interval_minutes"] = serde_json::json!(5);
        smart_detection["effective_interval_minutes"] = serde_json::json!(10);
        smart_detection["recent_request_count"] = serde_json::json!(77);
        assert!(accepts_reply(
            "/accounts/1/model-detection",
            &smart_detection
        ));
        for (field, value) in [
            ("schedule_mode", serde_json::json!("other")),
            ("min_interval_minutes", serde_json::json!(5.5)),
            ("effective_interval_minutes", serde_json::json!("31")),
            ("recent_request_count", serde_json::json!("many")),
        ] {
            let mut invalid = smart_detection.clone();
            invalid[field] = value;
            assert!(!accepts_reply("/accounts/1/model-detection", &invalid));
        }
        smart_detection["effective_interval_minutes"] = serde_json::Value::Null;
        smart_detection["recent_request_count"] = serde_json::Value::Null;
        assert!(accepts_reply(
            "/accounts/1/model-detection",
            &smart_detection
        ));
        let mut global_detection = detection.clone();
        global_detection["model_source"] = serde_json::json!("global");
        assert!(accepts_reply(
            "/accounts/1/model-detection",
            &global_detection
        ));
        let mut account_detection = global_detection.clone();
        account_detection["model_source"] = serde_json::json!("account");
        account_detection["model_override"] = serde_json::json!("fixture-provider-model");
        account_detection["global_model_id"] = serde_json::json!("gpt-6-luna");
        assert!(accepts_reply(
            "/accounts/1/model-detection",
            &account_detection
        ));
        account_detection["model_override"] = serde_json::json!(false);
        assert!(!accepts_reply(
            "/accounts/1/model-detection",
            &account_detection
        ));
        for invalid_source in [serde_json::json!(null), serde_json::json!(true)] {
            let mut invalid = detection.clone();
            invalid["model_source"] = invalid_source;
            assert!(!accepts_reply("/accounts/1/model-detection", &invalid));
        }
        let config = serde_json::json!({
            "oauth":{"revision":"fixture","model_detection_model_id":"gpt-6-luna","model_detection_platforms":{"openai":{"engine":"modeltrace","sample_count":1}},"oauth_early_reset_enabled":false,"oauth_early_reset_interval_seconds":60},
            "bark":{"revision":"fixture"}
        });
        assert!(accepts_reply("/config", &config));
        for field in [
            "model_detection_model_id",
            "model_detection_platforms",
            "oauth_early_reset_enabled",
            "oauth_early_reset_interval_seconds",
        ] {
            let mut missing = config.clone();
            missing["oauth"].as_object_mut().unwrap().remove(field);
            assert!(!accepts_reply("/config", &missing), "{field}");
        }
        let mut invalid_config = config.clone();
        invalid_config["oauth"]["model_detection_model_id"] = serde_json::json!(7);
        assert!(!accepts_reply("/config", &invalid_config));
        for at in [
            serde_json::Value::Null,
            serde_json::json!("2026-10-01T02:30:00Z"),
        ] {
            let mut timed = detection.clone();
            timed["last_automatic_started_at"] = at;
            assert!(accepts_reply("/accounts/1/model-detection", &timed));
        }
        let mut bad = detection.clone();
        bad["last_automatic_started_at"] = serde_json::json!(123);
        assert!(!accepts_reply("/accounts/1/model-detection", &bad));
        let mut bad = detection.clone();
        bad["interval_minutes"] = serde_json::json!("0");
        assert!(!accepts_reply("/accounts/1/model-detection", &bad));

        let profile = serde_json::json!({"whitelist":[],"mappings":[]});
        let templates = serde_json::json!({"version":"fixture","configured":false,"templates":{"full":profile,"degraded":profile,"takeover":profile}});
        assert!(accepts_reply("/account-templates", &templates));
        assert!(!accepts_reply("/account-templates", &serde_json::json!({})));
        let bank = serde_json::json!({"version":{"revision":"fixture","sha256":"fixture","analyzer_version":1},"source":"bundled","status":"idle"});
        assert!(accepts_reply("/modeltrace/fingerprint-bank", &bank));
        assert!(!accepts_reply(
            "/modeltrace/fingerprint-bank/sync",
            &serde_json::json!({})
        ));
    }

    #[test]
    fn detection_log_response_guard_accepts_the_projection_and_rejects_bad_fields() {
        let valid = serde_json::json!({
            "items":[{
                "job_id":"0123456789abcdef0123456789abcdef",
                "account_id":7,
                "account_name":"fixture account",
                "model_id":"gpt-6-sol",
                "status":"completed",
                "created_at":"2026-10-20T12:00:00Z",
                "started_at":null,
                "completed_at":"2026-10-20T12:00:00Z",
                "triggers":["scheduled"],
                "error":"",
                "details_available":false,
                "summary":{
                    "outcome":"unavailable",
                    "analysis_available":false,
                    "first_prediction":"",
                    "prediction":"",
                    "probability":null,
                    "first_probability":null,
                    "used_outputs":null,
                    "first_downgrade":null,
                    "aggregate_downgrade":null,
                    "disposition":null,
                    "recovery":null,
                    "verification":null,
                    "restoration":null
                },
                "notification":{
                    "status":"delivered",
                    "merged":false,
                    "delivered_at":"2026-10-20T12:00:01Z",
                    "reason":""
                }
            }],
            "next_cursor":"eyJmaXh0dXJlIjoxfQ"
        });
        assert!(accepts_reply("/detection-logs", &valid));

        let mut malformed = Vec::new();
        let mut row = valid.clone();
        row["items"][0]["status"] = serde_json::json!("running");
        malformed.push(row);
        let mut row = valid.clone();
        row["items"][0]["summary"]["analysis_available"] = serde_json::json!("false");
        malformed.push(row);
        let mut row = valid.clone();
        row["items"][0]["summary"]["probability"] = serde_json::json!(1.01);
        malformed.push(row);
        let mut row = valid.clone();
        row["items"][0]["notification"]["merged"] = serde_json::json!("false");
        malformed.push(row);
        let mut row = valid.clone();
        row["items"][0]["summary"]["recovery"] = serde_json::json!({
            "status":"completed","reason":"fixture","completed_at":123,
            "marked":true,"schedule_verified":true,"mark_cleared":true
        });
        malformed.push(row);
        let mut row = valid.clone();
        row["items"][0]["job_id"] = serde_json::json!("invalid");
        malformed.push(row);
        let mut row = valid.clone();
        row["next_cursor"] = serde_json::json!("");
        malformed.push(row);

        for response in malformed {
            assert!(
                !accepts_reply("/detection-logs", &response),
                "accepted malformed detection log response: {response}"
            );
        }
    }

    #[tokio::test]
    async fn system_update_outlives_the_default_http_timeout_without_replay() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let count = socket.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..count])
                .starts_with("POST /api/desktop/v1/sub2api-system/update "));
            std::thread::sleep(Duration::from_millis(120));
            let body = r#"{"message":"Updated","need_restart":true}"#;
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err());
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(30))
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let result = http(
            &client,
            &base,
            "fixture-key",
            "POST",
            "/sub2api-system/update",
            None,
            &dir.path().join("network.log"),
        )
        .await
        .unwrap();
        assert_eq!(result["need_restart"], true);
        server.join().unwrap();
    }

    #[test]
    fn native_update_accepts_latest_without_restart_and_requires_known_outcome() {
        assert!(accepts_reply(
            "/sub2api-system/update",
            &serde_json::json!({"message":"Already up to date","already_up_to_date":true,"current_version":"0.2.20","latest_version":"0.2.20"})
        ));
        assert!(accepts_reply(
            "/sub2api-system/update",
            &serde_json::json!({"message":"Updated","need_restart":true})
        ));
        assert!(!accepts_reply(
            "/sub2api-system/update",
            &serde_json::json!({"message":"Unknown"})
        ));
        assert!(accepts_reply(
            "/sub2api-system/version",
            &serde_json::json!({"version":"0.2.20"})
        ));
    }

    #[test]
    fn oauth_reauthorization_response_requires_matching_account_and_no_credentials() {
        let path = "/accounts/7/apply-oauth-credentials";
        let valid = serde_json::json!({"account_id":7,"verified":true,"status":"active"});
        assert!(accepts_reply(path, &valid));
        let mut extra = valid.clone();
        extra["credentials"] = serde_json::json!({"access_token":"fixture-token"});
        assert_eq!(
            sub2ops_protocol::response::client_response("POST", path, extra),
            Some(valid.clone())
        );
        assert!(!accepts_reply(
            "/accounts/8/apply-oauth-credentials",
            &valid
        ));
        for (field, value) in [
            ("verified", serde_json::json!(false)),
            ("status", serde_json::Value::Null),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(!accepts_reply(path, &invalid));
        }
    }

    #[test]
    fn keepalive_requires_account_identity_and_typed_fields() {
        let path = "/accounts/7/keepalive";
        let mut row = serde_json::json!({"account_id":7,"enabled":true,"generation":1,"version":"a".repeat(64),"status":"waiting","reason":"","paused_reason":"","restore_count":2,"budget_used":3,"next_allowed_at":null,"last_restore_at":null});
        row["future_field"] = true.into();
        assert!(accepts_reply(path, &row));
        assert!(!accepts_reply("/accounts/8/keepalive", &row));
        for (key, value) in [
            ("enabled", "true".into()),
            ("version", Value::Null),
            ("restore_count", "2".into()),
            ("next_allowed_at", 7.into()),
        ] {
            let mut invalid = row.clone();
            invalid[key] = value;
            assert!(!accepts_reply(path, &invalid), "{key}");
        }
    }

    #[test]
    fn connection_diagnostics_require_redacted_known_shape() {
        let status = serde_json::json!({"state":"auth_rejected","endpoint":"verify","http_status":403,
            "message":"管理员 API Key 被上游拒绝，请重新连接","retryable":false,"checked_at":"2026-10-07T00:00:00Z"});
        assert!(connection_valid(&status));
        assert!(accepts_reply("/connection-status", &status));
        let mut invalid = status.clone();
        invalid["endpoint"] = serde_json::json!("https://private.invalid");
        assert!(!connection_valid(&invalid));
        invalid = status;
        invalid["state"] = serde_json::json!("unknown");
        assert!(!connection_valid(&invalid));
    }

    #[test]
    fn operation_response_requires_identity_and_known_state() {
        let job = serde_json::json!({"id":"0123456789abcdef0123456789abcdef", "account_id":421,
            "action":"priority", "status":"queued", "requested":{"priority":2}});
        assert!(accepts_reply("/accounts/421/operations", &job));
        assert!(accepts_reply(
            "/account-operations",
            &serde_json::json!({"pending":1,"items":[job.clone()]})
        ));
        let mut scheduled = job.clone();
        scheduled["action"] = "test".into();
        scheduled["scheduled_at"] = "2030-01-02T01:31:00+00:00".into();
        assert!(operation_valid(&scheduled));
        scheduled["scheduled_at"] = "2030-01-02T01:31:38+00:00".into();
        assert!(operation_valid(&scheduled));
        for invalid in [
            "2030-02-30T01:31:00+00:00",
            "2030-01-02T01:31:60+00:00",
            "2030-01-02T01:31:38.001+00:00",
            "invalid",
        ] {
            scheduled["scheduled_at"] = invalid.into();
            assert!(!operation_valid(&scheduled));
        }
        let mut wrong = job.clone();
        wrong["status"] = serde_json::json!("unknown");
        assert!(!accepts_reply("/accounts/421/operations", &wrong));
        wrong = job.clone();
        wrong["account_id"] = serde_json::json!("421");
        assert!(!accepts_reply("/accounts/421/operations", &wrong));
        assert!(!accepts_reply(
            "/account-operations",
            &serde_json::json!({"items":[job]})
        ));
    }

    #[test]
    fn account_settings_and_proxies_reject_unsafe_or_malformed_fields() {
        let mut settings = serde_json::json!({"account_id":7,"priority":0,"concurrency":2,
            "load_factor":null,"effective_load_factor":2,"proxy_id":null,
            "model_config":{"whitelist":["fixture-text"],"mappings":[{"source":"fixture-*","target":"fixture-text"}]},
            "capabilities":{},"versions":{}});
        for action in [
            "priority",
            "concurrency",
            "load_factor",
            "model_config",
            "proxy",
            "sync_models",
        ] {
            settings["capabilities"][action] = serde_json::json!({"editable":true,"reason":""});
            settings["versions"][action] = serde_json::json!("a".repeat(64));
        }
        assert!(accepts_reply("/accounts/7/settings", &settings));
        for (field, value) in [
            ("load_factor", serde_json::json!("invalid")),
            ("proxy_id", serde_json::json!("invalid")),
            ("priority", serde_json::json!("2")),
        ] {
            let mut bad = settings.clone();
            bad[field] = value;
            assert!(!accepts_reply("/accounts/7/settings", &bad), "{field}");
        }
        let mut bad = settings.clone();
        bad["versions"]["proxy"] = serde_json::json!(0);
        assert!(!accepts_reply("/accounts/7/settings", &bad));
        bad = settings.clone();
        bad["model_config"]["mappings"][0]["target"] = serde_json::json!(3);
        assert!(!accepts_reply("/accounts/7/settings", &bad));
        let proxies = serde_json::json!([{"id":7,"name":"fixture proxy","protocol":"socks5","status":"active"}]);
        assert!(accepts_reply("/proxies", &proxies));
        assert!(accepts_reply("/proxies", &serde_json::json!([])));
        for key in ["password", "username", "url", "host", "port"] {
            let mut unsafe_proxy = proxies.clone();
            unsafe_proxy[0][key] = serde_json::json!("fixture");
            assert_eq!(
                sub2ops_protocol::response::client_response("GET", "/proxies", unsafe_proxy),
                Some(proxies.clone()),
                "{key}"
            );
        }
    }

    #[test]
    fn empty_templates_and_full_batch_responses_are_validated() {
        let empty = serde_json::json!({"version":"fixture", "configured":true, "templates":{},
            "template_order":[],"template_meta":{},"template_versions":{}});
        assert!(accepts_reply("/account-templates/full", &empty));
        let mut bad = empty.clone();
        bad["template_order"] = serde_json::json!(["full"]);
        assert!(!accepts_reply("/account-templates", &bad));
        let id = "0123456789abcdef0123456789abcdef";
        let job = serde_json::json!({"id":id,"batch_id":id,"account_id":1,"action":"account_template","status":"completed","requested":{}});
        let mut batch = serde_json::json!({"batch_id":id,"pending":0,"items":vec![job; 40]});
        assert!(accepts_reply("/account-templates/apply", &batch));
        assert!(accepts_reply(
            &format!("/account-operations?batch_id={id}"),
            &batch
        ));
        batch["items"][0]["batch_id"] = serde_json::json!("ffffffffffffffffffffffffffffffff");
        assert!(!accepts_reply("/account-templates/apply", &batch));
        assert!(!accepts_reply("/account-templates/apply", &empty));
    }
    #[test]
    fn proxy_batch_response_requires_batch_not_single_operation() {
        let id = "0123456789abcdef0123456789abcdef";
        let job = serde_json::json!({"id":id,"batch_id":id,"account_id":1,"action":"proxy",
            "status":"queued","requested":{"proxy_id":null}});
        let batch = serde_json::json!({"batch_id":id,"pending":1,"items":[job.clone()]});
        assert!(accepts_reply("/account-operations/batch", &batch));
        assert!(!accepts_reply("/account-operations/batch", &job));
        let mut bad = batch;
        bad["items"][0]["batch_id"] = serde_json::Value::Null;
        assert!(!accepts_reply("/account-operations/batch", &bad));
    }

    #[test]
    fn proxy_batch_response_rejects_wrong_action_and_proxy_payload_shape() {
        let id = "0123456789abcdef0123456789abcdef";
        let job = serde_json::json!({"id":id,"batch_id":id,"account_id":1,"action":"proxy",
            "status":"queued","requested":{"proxy_id":null}});
        let batch = serde_json::json!({"batch_id":id,"pending":1,"items":[job]});

        let mut wrong_action = batch.clone();
        wrong_action["items"][0]["action"] = serde_json::json!("account_template");
        assert!(!accepts_reply("/account-operations/batch", &wrong_action));
        for value in [
            serde_json::json!(-1),
            serde_json::json!(0),
            serde_json::json!("2"),
        ] {
            let mut bad_proxy = batch.clone();
            bad_proxy["items"][0]["requested"]["proxy_id"] = value;
            assert!(!accepts_reply("/account-operations/batch", &bad_proxy));
        }
        let mut missing_requested = batch.clone();
        missing_requested["items"][0]
            .as_object_mut()
            .unwrap()
            .remove("requested");
        assert!(!accepts_reply(
            "/account-operations/batch",
            &missing_requested
        ));
        let mut missing_proxy_id = batch.clone();
        missing_proxy_id["items"][0]["requested"] = serde_json::json!({});
        assert!(!accepts_reply(
            "/account-operations/batch",
            &missing_proxy_id
        ));
        let mut extra_proxy_field = batch;
        extra_proxy_field["items"][0]["requested"]["extra"] = serde_json::json!(true);
        let projected = sub2ops_protocol::response::client_response(
            "POST",
            "/account-operations/batch",
            extra_proxy_field,
        )
        .unwrap();
        assert_eq!(
            projected["items"][0]["requested"],
            serde_json::json!({"proxy_id":null})
        );
    }

    #[test]
    fn account_batch_response_validates_delete_and_group_payloads() {
        let id = "0123456789abcdef0123456789abcdef";
        let item = |action: &str, requested: Value| {
            serde_json::json!({
                "id": "abcdefabcdefabcdefabcdefabcdefab",
                "batch_id": id,
                "account_id": 7,
                "action": action,
                "status": "completed",
                "requested": requested,
            })
        };
        let batch = |job: Value| serde_json::json!({"batch_id": id, "pending": 0, "items": [job]});

        let delete = batch(item("delete", serde_json::json!({})));
        assert!(accepts_reply("/account-operations/batch", &delete));
        let mut bad_delete = delete.clone();
        bad_delete["items"][0]["requested"]["unexpected"] = serde_json::json!(true);
        assert!(accepts_reply("/account-operations/batch", &bad_delete));
        let mut extra_delete = delete.clone();
        extra_delete["items"][0]["requested"]["extra"] = serde_json::json!(false);
        assert!(accepts_reply("/account-operations/batch", &extra_delete));

        let groups = batch(item(
            "groups",
            serde_json::json!({"scope_group_ids": [1, 2, 9], "group_ids": [1, 9]}),
        ));
        assert!(accepts_reply("/account-operations/batch", &groups));
        let mut outside_scope = groups.clone();
        outside_scope["items"][0]["requested"]["group_ids"] = serde_json::json!([3]);
        assert!(!accepts_reply("/account-operations/batch", &outside_scope));
        let mut empty_scope = groups.clone();
        empty_scope["items"][0]["requested"]["scope_group_ids"] = serde_json::json!([]);
        assert!(!accepts_reply("/account-operations/batch", &empty_scope));
        let mut bad_id = groups.clone();
        bad_id["items"][0]["requested"]["scope_group_ids"] = serde_json::json!([0, 1]);
        assert!(!accepts_reply("/account-operations/batch", &bad_id));
        let mut too_many = groups.clone();
        too_many["items"][0]["requested"]["scope_group_ids"] =
            serde_json::Value::Array((1..=101).map(|value| serde_json::json!(value)).collect());
        assert!(!accepts_reply("/account-operations/batch", &too_many));
    }

    fn connection_test_batch_fixture() -> Value {
        serde_json::json!({
            "batch_id": "0123456789abcdef0123456789abcdef",
            "pending": 0,
            "items": [{
                "id": "abcdefabcdefabcdefabcdefabcdefab",
                "batch_id": "0123456789abcdef0123456789abcdef",
                "account_id": 7,
                "action": "test",
                "status": "completed",
                "requested": {"confirmed": true, "mode": "text", "model_id": "fixture-model"},
                "result": {
                    "success": true,
                    "requested_model": "fixture-model",
                    "actual_model": "fixture-mapped-model",
                    "duration_ms": 12,
                    "completed_at": "2026-10-13T00:00:00Z",
                    "error": null,
                    "uncertain": false
                }
            }]
        })
    }

    #[test]
    fn connection_batch_responses_accept_queue_results_and_cancellation() {
        let batch = connection_test_batch_fixture();
        for path in [
            "/account-operations/batch",
            "/account-operations?batch_id=0123456789abcdef0123456789abcdef",
            "/account-operations/batches/0123456789abcdef0123456789abcdef/cancel",
        ] {
            assert!(accepts_reply(path, &batch), "{path}");
            for status in ["queued", "running", "cancelled"] {
                let mut pending = batch.clone();
                pending["items"][0]["status"] = serde_json::json!(status);
                pending["items"][0]["result"] = Value::Null;
                assert!(accepts_reply(path, &pending), "{path} {status}");
            }
            let mut failed = batch.clone();
            failed["items"][0]["status"] = serde_json::json!("failed");
            failed["items"][0]["result"]["success"] = serde_json::json!(false);
            failed["items"][0]["result"]["actual_model"] = Value::Null;
            failed["items"][0]["result"]["error"] = serde_json::json!("fixture failure");
            failed["items"][0]["result"]["uncertain"] = serde_json::json!(true);
            assert!(accepts_reply(path, &failed), "{path}");
        }
        let mut default_mode = batch;
        default_mode["items"][0]["requested"]["mode"] = serde_json::json!("default");
        default_mode["items"][0]["requested"]["model_id"] = serde_json::json!("");
        default_mode["items"][0]["result"]["requested_model"] = serde_json::json!("");
        assert!(accepts_reply("/account-operations/batch", &default_mode));
    }

    #[test]
    fn recovery_batch_accepts_empty_requests_and_typed_results() {
        let mut batch = connection_test_batch_fixture();
        batch["items"][0]["action"] = serde_json::json!("recover");
        batch["items"][0]["requested"] = serde_json::json!({});
        batch["items"][0]["result"] = serde_json::json!({"verified":true,"unchanged":true});
        for path in [
            "/account-operations/batch",
            "/account-operations?batch_id=0123456789abcdef0123456789abcdef",
        ] {
            assert!(accepts_reply(path, &batch));
            for invalid in [
                serde_json::json!({"verified":"true"}),
                serde_json::json!({"verified":true,"unchanged":"true"}),
            ] {
                let mut bad = batch.clone();
                bad["items"][0]["result"] = invalid;
                assert!(!accepts_reply(path, &bad));
            }
            let mut bad = batch.clone();
            bad["items"][0]["requested"] = serde_json::json!({"schedulable":true});
            assert!(!accepts_reply(path, &bad));
            let mut queued = batch.clone();
            queued["items"][0]["status"] = serde_json::json!("queued");
            queued["items"][0]["result"] = Value::Null;
            assert!(accepts_reply(path, &queued));
        }
    }

    #[test]
    fn connection_batch_responses_reject_invalid_request_and_batch_shapes() {
        let batch = connection_test_batch_fixture();
        for path in [
            "/account-operations/batch",
            "/account-operations/batches/0123456789abcdef0123456789abcdef/cancel",
        ] {
            assert!(!accepts_reply(path, &batch["items"][0]), "{path}");
            for requested in [
                serde_json::json!({"confirmed": false, "mode": "text", "model_id": "fixture-model"}),
                serde_json::json!({"confirmed": "true", "mode": "text", "model_id": "fixture-model"}),
                serde_json::json!({"confirmed": true, "mode": "image", "model_id": "fixture-model"}),
                serde_json::json!({"confirmed": true, "mode": "text", "model_id": 7}),
                serde_json::json!({"confirmed": true, "mode": "text", "model_id": "x".repeat(201)}),
                serde_json::json!({"confirmed": true, "mode": "text"}),
            ] {
                let mut bad = batch.clone();
                bad["items"][0]["requested"] = requested;
                assert!(!accepts_reply(path, &bad), "{path}");
            }
            let mut extended = batch.clone();
            extended["items"][0]["requested"]["extra"] = true.into();
            let projected =
                sub2ops_protocol::response::client_response("POST", path, extended).unwrap();
            assert!(projected["items"][0]["requested"].get("extra").is_none());
            let mut wrong_batch = batch.clone();
            wrong_batch["items"][0]["batch_id"] =
                serde_json::json!("ffffffffffffffffffffffffffffffff");
            assert!(!accepts_reply(path, &wrong_batch), "{path}");
        }
    }

    #[test]
    fn connection_batch_responses_reject_malformed_result_fields() {
        let batch = connection_test_batch_fixture();
        for path in [
            "/account-operations/batch",
            "/account-operations?batch_id=0123456789abcdef0123456789abcdef",
            "/account-operations/batches/0123456789abcdef0123456789abcdef/cancel",
        ] {
            for (field, value) in [
                ("success", serde_json::json!("true")),
                ("requested_model", serde_json::json!(7)),
                ("requested_model", serde_json::json!("x".repeat(201))),
                ("actual_model", serde_json::json!(7)),
                ("actual_model", serde_json::json!("x".repeat(201))),
                ("duration_ms", serde_json::json!(-1)),
                ("duration_ms", serde_json::json!(1.5)),
                ("completed_at", Value::Null),
                ("error", serde_json::json!({"message": "fixture failure"})),
                ("error", serde_json::json!("x".repeat(1025))),
                ("uncertain", serde_json::json!("true")),
            ] {
                let mut bad = batch.clone();
                bad["items"][0]["result"][field] = value;
                assert!(!accepts_reply(path, &bad), "{path} {field}");
            }
            for field in ["success", "requested_model", "duration_ms", "completed_at"] {
                let mut missing = batch.clone();
                missing["items"][0]["result"]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                assert!(!accepts_reply(path, &missing), "{path} {field}");
            }
            let mut wrong_result = batch.clone();
            wrong_result["items"][0]["result"] = serde_json::json!([]);
            assert!(!accepts_reply(path, &wrong_result), "{path}");
        }
    }

    #[test]
    fn snapshot_recovery_monitor_is_optional_but_typed_when_present() {
        let account = serde_json::json!({
            "id": 1, "name": "fixture", "platform": "openai", "type": "oauth", "version": "v1",
            "group_ids": [], "usage_windows": [], "blockers": [], "schedulable": true,
            "available": true, "priority": 0, "operation_versions": {"test": "v1"}
        });
        let mut snapshot = serde_json::json!({
            "observed_at": "2026-10-11T00:00:00Z", "accounts": [account], "groups": [],
            "errors": [], "recoveries": []
        });
        assert!(accepts_reply("/snapshot", &snapshot));
        for versions in [serde_json::Value::Null, serde_json::json!({"test": 1})] {
            let mut invalid = snapshot.clone();
            invalid["accounts"][0]["operation_versions"] = versions;
            assert!(!accepts_reply("/snapshot", &invalid));
        }
        snapshot["accounts"][0]["recovery_monitor"] = serde_json::Value::Null;
        assert!(accepts_reply("/snapshot", &snapshot));
        snapshot["accounts"][0]["recovery_monitor"] = serde_json::json!({
            "status": "scheduled", "label": "额度恢复监控", "reason": "等待下次检查", "next_at": null
        });
        assert!(accepts_reply("/snapshot", &snapshot));
        snapshot["accounts"][0]["recovery_monitor"]["next_at"] =
            serde_json::json!("2026-10-11T01:00:00Z");
        assert!(accepts_reply("/snapshot", &snapshot));
        snapshot["accounts"][0]["recovery_monitor"]["next_at"] = serde_json::json!(3600);
        assert!(!accepts_reply("/snapshot", &snapshot));
    }
}
