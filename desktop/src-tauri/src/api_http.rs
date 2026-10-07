use serde_json::Value;
use std::{io::Write, path::Path, time::Duration};

const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;

fn snapshot_valid(data: &Value) -> bool {
    data.get("observed_at").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
        && data.get("accounts").and_then(Value::as_array).is_some_and(|rows| rows.iter().all(|a| {
            a["id"].as_i64().is_some()
                && ["name", "platform", "type", "version"].iter().all(|k| a[*k].is_string())
                && ["group_ids", "usage_windows", "blockers"].iter().all(|k| a[*k].is_array())
                && a["schedulable"].is_boolean() && a["available"].is_boolean()
                && a["priority"].as_i64().is_some()
                && (a["usage"].is_null() || a["usage"]["windows"].is_array())
        }))
        && data.get("groups").and_then(Value::as_array).is_some_and(|rows| rows.iter().all(|g| {
            g["id"].as_i64().is_some() && g["name"].is_string() && g["recent_accounts"].is_array()
        }))
        && ["errors", "recoveries"].iter().all(|k| data[*k].is_array())
}

fn operation_valid(data: &Value) -> bool {
    data["id"].as_str().is_some_and(|s| s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        && data["account_id"].as_u64().is_some_and(|id| id > 0)
        && data["action"].is_string()
        && data["status"].as_str().is_some_and(|s| matches!(s, "queued" | "running" | "checking" | "completed" | "needs_confirmation" | "failed" | "cancelled" | "superseded"))
        && data["requested"].is_object()
}

fn connection_valid(data: &Value) -> bool {
    ["ok", "network_unreachable", "auth_rejected", "route_not_found", "protocol_mismatch", "upstream_error"]
        .contains(&data["state"].as_str().unwrap_or(""))
        && ["verify", "public", "none"].contains(&data["endpoint"].as_str().unwrap_or(""))
        && (data["http_status"].is_null() || data["http_status"].as_u64().is_some_and(|n| (100..=599).contains(&n)))
        && data["message"].is_string()
        && data["retryable"].is_boolean()
        && (data["checked_at"].is_null() || data["checked_at"].is_string())
}

fn templates_valid(data: &Value) -> bool {
    let Some(templates) = data["templates"].as_object() else { return false; };
    data["configured"].is_boolean() && data["version"].is_string()
        && (3..=32).contains(&templates.len())
        && ["full", "degraded", "takeover"].iter().all(|id| templates.contains_key(*id))
        && templates.iter().all(|(id, value)| {
            let builtin = matches!(id.as_str(), "full" | "degraded" | "takeover");
            let custom = id.strip_prefix("custom-").is_some_and(|v| v.len() == 24 && v.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
            (builtin || custom) && value["whitelist"].as_array().is_some_and(|v| v.iter().all(Value::is_string))
                && value["mappings"].as_array().is_some_and(|v| v.iter().all(|m| m["source"].is_string() && m["target"].is_string()))
        })
        && (data["template_order"].is_null() || data["template_order"].as_array().is_some_and(|values| {
            let ids = values.iter().filter_map(Value::as_str).collect::<std::collections::HashSet<_>>();
            ids.len() == values.len() && ids.len() == templates.len() && ids.iter().all(|id| templates.contains_key(*id))
        }))
}

fn shape_valid(path: &str, data: &Value) -> bool {
    match path.split('?').next().unwrap_or("") {
        "/snapshot" => snapshot_valid(data),
        "/config" => ["oauth", "bark", "key_fallback"].iter().all(|k| data[*k]["revision"].is_string()),
        "/capabilities" => data["api_version"].as_u64() == Some(1)
            && (data["upstream_connection"].is_null() || connection_valid(&data["upstream_connection"])),
        "/connection-status" => connection_valid(data),
        "/errors" | "/recoveries" => data["items"].is_array(),
        "/account-operations" => data["pending"].is_u64() && data["items"].as_array().is_some_and(|rows| rows.iter().all(operation_valid)),
        p if p.starts_with("/account-operations/") || p.ends_with("/operations") => operation_valid(data),
        p if p.ends_with("/model-tests/latest") && data.is_null() => true,
        p if p.starts_with("/model-tests/") || p.ends_with("/model-tests") || p.ends_with("/model-tests/latest") => data["id"].is_string() && data["status"].is_string() && data["account_id"].is_u64(),
        p if p == "/account-templates" || p.starts_with("/account-templates/") => templates_valid(data),
        p if p.ends_with("/model-detection") => data["account_id"].is_u64() && data["version"].is_string()
            && data["enabled"].is_boolean() && data["interval_minutes"].as_u64().is_some_and(|v| v > 0)
            && data["model_id"].is_string() && data["status"].is_string(),
        "/modeltrace/fingerprint-bank" | "/modeltrace/fingerprint-bank/sync" => data["version"]["revision"].is_string()
            && data["version"]["sha256"].is_string() && data["version"]["analyzer_version"].is_u64()
            && data["source"].is_string() && data["status"].is_string(),
        "/quota-refresh" => data["status"].is_string() && data["items"].is_array(),
        p if p.ends_with("/models") => data.as_array().is_some_and(|m| m.iter().all(|v| v["id"].is_string())),
        _ => data.is_object(),
    }
}

fn response_kind(value: &str) -> &'static str {
    match value.split(';').next().unwrap_or("").trim() {
        "application/json" => "json",
        "text/html" => "html",
        "text/plain" => "text",
        "" => "missing",
        _ => "other",
    }
}

fn diagnostic(file: &Path, method: &str, path: &str, status: u16, kind: &str, stage: &str, request_id: &str) {
    use std::os::unix::fs::OpenOptionsExt;
    let Some(parent) = file.parent() else { return; };
    if std::fs::create_dir_all(parent).is_err() { return; }
    let truncate = std::fs::metadata(file).is_ok_and(|m| m.len() > 64 * 1024);
    let route = path.split('?').next().unwrap_or("").split('/').map(|s| {
        if s.parse::<u64>().is_ok() { ":id" } else if s.bytes().all(|c| c.is_ascii_lowercase() || c == b'-' || c == b'_') { s } else { "unknown" }
    }).collect::<Vec<_>>().join("/");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).write(true).append(!truncate).truncate(truncate).mode(0o600).open(file) {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
        let _ = writeln!(f, "{stamp} method={method} path={route} status={status} type={kind} stage={stage} request_id={request_id}");
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HttpFailure {
    pub message: String,
    pub connection: Option<Value>,
}
impl From<String> for HttpFailure {
    fn from(message: String) -> Self { Self { message, connection: None } }
}
impl From<&str> for HttpFailure {
    fn from(message: &str) -> Self { message.to_string().into() }
}
impl From<HttpFailure> for String {
    fn from(error: HttpFailure) -> Self { error.message }
}

pub(crate) async fn http(
    client: &reqwest::Client, base: &str, key: &str, method: &str, path: &str,
    body: Option<Value>, log: &Path,
) -> Result<Value, String> {
    http_detailed(client, base, key, method, path, body, log).await.map_err(String::from)
}

pub(crate) async fn http_detailed(
    client: &reqwest::Client, base: &str, key: &str, method: &str, path: &str,
    body: Option<Value>, log: &Path,
) -> Result<Value, HttpFailure> {
    let parsed_method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "请求方式无效")?;
    let mut req = client.request(parsed_method, format!("{base}/api/desktop/v1{path}"))
        .header("x-api-key", key).header("Accept", "application/json");
    if path.ends_with("/usage-action") { req = req.timeout(Duration::from_secs(105)); }
    if method == "DELETE" || path.ends_with("/recover-state") { req = req.timeout(Duration::from_secs(20)); }
    if path.starts_with("/model-groups/") { req = req.timeout(Duration::from_secs(65)); }
    if let Some(body) = body { req = req.json(&body); }
    let mut response = req.send().await.map_err(|e| {
        diagnostic(log, method, path, 0, "missing", if e.is_timeout() { "headers_timeout" } else { "connect" }, "-");
        if e.is_timeout() { "连接超时，未重放操作" } else { "连接暂时中断" }.to_string()
    })?;
    let status = response.status().as_u16();
    let kind = response_kind(response.headers().get("content-type").and_then(|h| h.to_str().ok()).unwrap_or(""));
    let request_id = response.headers().get("x-request-id").and_then(|h| h.to_str().ok())
        .filter(|s| s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
        .unwrap_or("-").to_string();
    let fail = |stage: &str, message: &str| {
        diagnostic(log, method, path, status, kind, stage, &request_id);
        HttpFailure::from(message)
    };
    let limit = if path.starts_with("/account-operations/") { 134_217_728 } else { MAX_JSON_BYTES };
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| fail(
        if e.is_timeout() { "body_timeout" } else { "body_interrupted" },
        if e.is_timeout() { "响应读取超时，未重放操作" } else { "响应读取中断" },
    ))? {
        if bytes.len() + chunk.len() > limit { return Err(fail("size", "服务响应超过大小限制")); }
        bytes.extend_from_slice(&chunk);
    }
    let parsed = serde_json::from_slice::<Value>(&bytes);
    if !(200..300).contains(&status) {
        let fallback = match status { 401 | 403 => "管理员 API Key 被拒绝，请重新连接", 429 => "请求过于频繁", 502..=504 => "网关暂不可用", 500..=599 => "服务暂不可用", _ => "请求失败" };
        let detail = parsed.as_ref().ok().and_then(|d| d.get("detail"));
        let message = detail.and_then(Value::as_str).or_else(|| detail.and_then(|d| d["message"].as_str())).unwrap_or(fallback);
        let suffix = if detail.is_some_and(|d| d["detached"].as_bool() == Some(true)) { "（该账号已解除托管）" } else { "" };
        let mut error = fail(if status == 401 || status == 403 { "auth" } else { "http" },
            &format!("{}{} [{}]", message.chars().take(240).collect::<String>(), suffix, status));
        error.connection = detail.and_then(|d| d.get("connection")).filter(|v| connection_valid(v)).cloned();
        return Err(error);
    }
    if bytes.iter().all(u8::is_ascii_whitespace) { return Err(fail("empty", "服务返回空响应")); }
    if kind != "json" { return Err(fail("content_type", "服务返回了非 JSON 响应")); }
    let data = parsed.map_err(|_| fail("json", "服务 JSON 响应不完整或格式无效"))?;
    if !shape_valid(path, &data) { return Err(fail("schema", "服务数据结构不兼容，请检查客户端与服务版本")); }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, net::TcpListener};

    async fn serve(status: &str, headers: &str, body: &str, delay: bool) -> (Result<Value, String>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let reply = format!("HTTP/1.1 {status}\r\n{headers}\r\nx-request-id: test-request\r\nConnection: close\r\n\r\n{body}");
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096]; let _ = stream.read(&mut request);
            let _ = stream.write_all(reply.as_bytes());
            if delay { std::thread::sleep(Duration::from_millis(200)); }
        });
        let temp = tempfile::tempdir().unwrap(); let log = temp.path().join("network.log");
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_millis(100)).build().unwrap();
        let result = http(&client, &base, "secret-admin-key", "GET", "/snapshot", None, &log).await;
        thread.join().unwrap();
        (result, std::fs::read_to_string(log).unwrap_or_default())
    }

    #[tokio::test]
    async fn classifies_failures_without_leaking_body_or_key() {
        for (status, headers, body, expected, stage, delay) in [
            ("502 Bad Gateway", "Content-Type: text/html", "<html>secret-body</html>", "网关暂不可用 [502]", "http", false),
            ("200 OK", "Content-Type: application/json", "", "空响应", "empty", false),
            ("200 OK", "Content-Type: text/html", "secret-body", "非 JSON", "content_type", false),
            ("200 OK", "Content-Type: application/json", "{", "JSON 响应不完整", "json", false),
            ("200 OK", "Content-Type: application/json", "{}", "数据结构不兼容", "schema", false),
            ("401 Unauthorized", "Content-Type: text/html", "secret-body", "API Key 被拒绝", "auth", false),
            ("200 OK", "Content-Type: application/json\r\nContent-Length: 1000", "{", "读取中断", "body_interrupted", false),
            ("200 OK", "Content-Type: application/json\r\nContent-Length: 1000", "{", "读取超时", "body_timeout", true),
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
        let body = serde_json::json!({"detail":{"code":"auth_rejected","message":"管理员 Key 被拒绝","connection":connection}}).to_string();
        let reply = format!("HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096]; let _ = stream.read(&mut request);
            let _ = stream.write_all(reply.as_bytes());
        });
        let temp = tempfile::tempdir().unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let error = http_detailed(&client, &base, "fixture-key", "GET", "/snapshot", None, &temp.path().join("network.log")).await.unwrap_err();
        thread.join().unwrap();
        assert_eq!(error.connection, Some(connection));
        assert!(error.message.contains("管理员 Key 被拒绝"));
        assert!(!std::fs::read_to_string(temp.path().join("network.log")).unwrap().contains("fixture-key"));
    }

    #[tokio::test]
    async fn valid_snapshot_and_structure_guard() {
        let body = r#"{"observed_at":"2026-09-25T00:00:00Z","accounts":[],"groups":[],"errors":[],"recoveries":[]}"#;
        let (result, log) = serve("200 OK", "Content-Type: application/json", body, false).await;
        assert!(result.is_ok()); assert!(log.is_empty());
        assert!(!snapshot_valid(&serde_json::json!({"accounts":null})));
    }

    #[test]
    fn detection_templates_and_bank_response_guards() {
        let detection = serde_json::json!({"account_id":1,"version":"fixture","enabled":false,"interval_minutes":15,"model_id":"gpt-6-luna","status":"disabled"});
        assert!(shape_valid("/accounts/1/model-detection", &detection));
        let mut bad = detection.clone(); bad["interval_minutes"] = serde_json::json!(0);
        assert!(!shape_valid("/accounts/1/model-detection", &bad));
        let profile = serde_json::json!({"whitelist":[],"mappings":[]});
        let templates = serde_json::json!({"version":"fixture","configured":false,"templates":{"full":profile,"degraded":profile,"takeover":profile}});
        assert!(shape_valid("/account-templates", &templates));
        assert!(!shape_valid("/account-templates", &serde_json::json!({})));
        let bank = serde_json::json!({"version":{"revision":"fixture","sha256":"fixture","analyzer_version":1},"source":"bundled","status":"idle"});
        assert!(shape_valid("/modeltrace/fingerprint-bank", &bank));
        assert!(!shape_valid("/modeltrace/fingerprint-bank/sync", &serde_json::json!({})));
    }

    #[test]
    fn connection_diagnostics_require_redacted_known_shape() {
        let status = serde_json::json!({"state":"auth_rejected","endpoint":"verify","http_status":403,
            "message":"管理员 API Key 被上游拒绝，请重新连接","retryable":false,"checked_at":"2026-10-07T00:00:00Z"});
        assert!(connection_valid(&status));
        assert!(shape_valid("/connection-status", &status));
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
        assert!(shape_valid("/accounts/421/operations", &job));
        assert!(shape_valid("/account-operations", &serde_json::json!({"pending":1,"items":[job.clone()]})));
        let mut wrong = job.clone(); wrong["status"] = serde_json::json!("unknown");
        assert!(!shape_valid("/accounts/421/operations", &wrong));
        wrong = job.clone(); wrong["account_id"] = serde_json::json!("421");
        assert!(!shape_valid("/accounts/421/operations", &wrong));
        assert!(!shape_valid("/account-operations", &serde_json::json!({"items":[job]})));
    }
}
