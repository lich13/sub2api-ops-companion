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
    use super::*;
    use std::{io::Read, net::TcpListener};

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
        let (bad, _) = serve(
            "200 OK",
            "Content-Type: application/json",
            r#"{"accounts":null}"#,
            false,
        )
        .await;
        assert!(bad.unwrap_err().contains("数据结构不兼容"));
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
}
