use crate::{
    actions::Actions,
    compat::{integer, string, text, truthy},
    security::{clean, safe_error},
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const MAX_TEST_LINE_BYTES: usize = 1024 * 1024;
const MAX_TEST_TEXT_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct TestSseParser {
    texts: String,
    success: bool,
    executed: String,
    terminal_error: Option<Value>,
}

impl TestSseParser {
    fn abort_overflow(&mut self) {
        if self.terminal_error.is_none() {
            self.terminal_error = Some(json!({
                "success": false,
                "error": "测活事件单行超出上限，结果待核对",
                "error_code": "account_test_error",
                "uncertain": true,
                "executed_model": self.executed,
            }));
        }
    }

    fn feed_line(&mut self, line: &str) -> bool {
        if self.terminal_error.is_some() {
            return true;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.len() > MAX_TEST_LINE_BYTES {
            self.abort_overflow();
            return true;
        }
        let Some(data) = line.trim().strip_prefix("data:") else {
            return false;
        };
        let Ok(event) = serde_json::from_str::<Value>(data.trim()) else {
            return false;
        };
        if !event.is_object() {
            return false;
        }
        if matches!(string(&event["type"]), "test_start" | "test_complete") {
            let model = ["model_id", "model"]
                .iter()
                .find_map(|key| event[*key].as_str().filter(|v| !v.is_empty()));
            if let Some(model) = model {
                self.executed = clean(&json!(model), 200);
            }
        }
        if event["type"] == "error" {
            let nested = &event["error"];
            let code = [
                &event["code"],
                &event["error_code"],
                &event["status_code"],
                &nested["code"],
            ]
            .into_iter()
            .find(|v| truthy(v))
            .map(text)
            .unwrap_or_else(|| "unknown_test_error".into());
            let error = [
                &nested["message"],
                &nested["error"],
                &nested["detail"],
                &event["message"],
                &event["detail"],
                &event["error"],
            ]
            .into_iter()
            .find(|v| truthy(v))
            .map(|v| safe_error(v, 1000))
            .unwrap_or_else(|| "账号测试失败".into());
            let mut result = json!({
                "success": false,
                "error": error,
                "error_code": code,
                "executed_model": self.executed
            });
            // A transport failure reported inside SSE is just as ambiguous as a
            // dropped HTTP stream: the upstream may already have spent tokens.
            if matches!(
                code.trim().to_ascii_lowercase().as_str(),
                "network_error" | "timeout" | "account_test_error" | "missing_success_event"
            ) {
                result["uncertain"] = true.into();
            }
            self.terminal_error = Some(result);
            return true;
        }
        if event["type"] == "content" && truthy(&event["text"]) {
            let chunk_text = text(&event["text"]);
            if self.texts.len() < MAX_TEST_TEXT_BYTES {
                let rem = MAX_TEST_TEXT_BYTES - self.texts.len();
                if chunk_text.len() <= rem {
                    self.texts.push_str(&chunk_text);
                } else {
                    let mut end = rem;
                    while !chunk_text.is_char_boundary(end) {
                        end -= 1;
                    }
                    self.texts.push_str(&chunk_text[..end]);
                }
            }
        }
        if event["type"] == "test_complete" && event["success"] == true {
            self.success = true;
        }
        false
    }

    fn finish(self) -> Value {
        if let Some(error) = self.terminal_error {
            error
        } else if !self.success {
            json!({"success":false,"error":"账号测试未返回成功事件","error_code":"missing_success_event","uncertain":true,"executed_model":self.executed})
        } else {
            json!({"success":true,"response_text":self.texts.trim(),"executed_model":self.executed})
        }
    }
}

pub fn parse_test_sse(payload: &str) -> Value {
    let mut parser = TestSseParser::default();
    for line in payload.lines() {
        if parser.feed_line(line) {
            break;
        }
    }
    parser.finish()
}

async fn parse_test_stream<S, E>(mut stream: S) -> Result<Value, ()>
where
    S: futures_util::Stream<Item = Result<axum::body::Bytes, E>> + Unpin,
{
    let mut parser = TestSseParser::default();
    let mut line_buf: Vec<u8> = Vec::with_capacity(1024);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ())?;
        let mut rest = chunk.as_ref();
        while let Some(pos) = rest.iter().position(|b| *b == b'\n') {
            let piece = &rest[..pos];
            rest = &rest[pos + 1..];
            let total_without_cr = if piece.ends_with(b"\r") {
                line_buf.len() + piece.len() - 1
            } else if piece.is_empty() && line_buf.ends_with(b"\r") {
                line_buf.len() - 1
            } else {
                line_buf.len() + piece.len()
            };
            if total_without_cr > MAX_TEST_LINE_BYTES {
                parser.abort_overflow();
                return Ok(parser.finish());
            }
            line_buf.extend_from_slice(piece);
            if line_buf.ends_with(b"\r") {
                line_buf.pop();
            }
            if parser.feed_line(&String::from_utf8_lossy(&line_buf)) {
                return Ok(parser.finish());
            }
            line_buf.clear();
        }
        if !rest.is_empty() {
            let max_buffered = if rest.ends_with(b"\r") {
                MAX_TEST_LINE_BYTES + 1
            } else {
                MAX_TEST_LINE_BYTES
            };
            if line_buf.len() + rest.len() > max_buffered {
                parser.abort_overflow();
                return Ok(parser.finish());
            }
            line_buf.extend_from_slice(rest);
        }
    }
    if !line_buf.is_empty() {
        if line_buf.ends_with(b"\r") {
            line_buf.pop();
        }
        if line_buf.len() > MAX_TEST_LINE_BYTES {
            parser.abort_overflow();
        } else {
            let _ = parser.feed_line(&String::from_utf8_lossy(&line_buf));
        }
    }
    Ok(parser.finish())
}

pub async fn test(actions: &Actions, id: i64, model: &str, key: &str, seconds: u64) -> Value {
    let failure = |code: &str, error: &str, uncertain: bool| json!({"success":false,"error":error,"error_code":code,"model_id":model,"uncertain":uncertain});
    if key.trim().is_empty() || actions.upstream.active_url().is_empty() {
        return failure(
            "missing_sub2api_admin_credentials",
            "缺少 Sub2API 地址或 Admin API Key",
            false,
        );
    }
    let started = Instant::now();
    let deadline = Duration::from_secs(seconds.clamp(1, 60));
    let response = match actions
        .request(
            reqwest::Method::POST,
            &format!("accounts/{id}/test"),
            key,
            Some(&json!({"model_id":model,"prompt":"","mode":""})),
            deadline,
            true,
        )
        .await
    {
        Ok(value) => value,
        Err(error) => {
            return failure(
                if error.status.as_u16() == 504 {
                    "timeout"
                } else {
                    "network_error"
                },
                if error.status.as_u16() == 504 {
                    "请求超时"
                } else {
                    "测活连接中断"
                },
                true,
            )
        }
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return failure(
            &format!("http_{status}"),
            &format!("HTTP {status}"),
            status >= 500,
        );
    }
    let Ok(Ok(mut parsed)) =
        tokio::time::timeout(deadline, parse_test_stream(response.bytes_stream())).await
    else {
        return failure("account_test_error", "测活响应中断，结果待核对", true);
    };
    parsed["duration_ms"] = json!(started.elapsed().as_millis() as u64);
    parsed["model_id"] = model.into();
    parsed
}
pub(crate) async fn admin(
    actions: &Actions,
    id: i64,
    path: &str,
    key: &str,
    method: reqwest::Method,
    seconds: u64,
) -> Value {
    let response = match actions
        .request(
            method,
            &format!("accounts/{id}/{path}"),
            key,
            None,
            Duration::from_secs(seconds.clamp(2, 60)),
            false,
        )
        .await
    {
        Ok(v) => v,
        Err(error) => {
            return json!({"success":false,"error_code":if error.status.as_u16()==504 {"timeout"} else {"network_error"},"error":"恢复请求未确认"})
        }
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return json!({"success":false,"status_code":status,"error_code":format!("http_{status}"),"error":format!("HTTP {status}")});
    }
    let body = crate::upstream::read_limited(response, 2_000_000)
        .await
        .ok()
        .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        .unwrap_or(json!({}));
    json!({"success":true,"status_code":status,"data":body})
}
pub async fn recover(actions: &Actions, id: i64, key: &str, seconds: u64) -> Value {
    if key.trim().is_empty() || actions.upstream.active_url().is_empty() {
        return json!({"success":false,"error_code":"missing_sub2api_admin_credentials","error":"缺少 Sub2API 地址或 Admin API Key"});
    }
    let mut primary = admin(
        actions,
        id,
        "recover-state",
        key,
        reqwest::Method::POST,
        seconds,
    )
    .await;
    primary["method"] = "recover-state".into();
    primary["fallback"] = false.into();
    if truthy(&primary["success"]) || integer(&primary["status_code"]) != Some(404) {
        return primary;
    }
    let mut rate = admin(
        actions,
        id,
        "clear-rate-limit",
        key,
        reqwest::Method::POST,
        seconds,
    )
    .await;
    rate["method"] = "clear-rate-limit".into();
    rate["fallback"] = true.into();
    if !truthy(&rate["success"]) {
        return rate;
    }
    let mut temp = admin(
        actions,
        id,
        "temp-unschedulable",
        key,
        reqwest::Method::DELETE,
        seconds,
    )
    .await;
    temp["method"] = "temp-unschedulable".into();
    temp["fallback"] = true.into();
    if !truthy(&temp["success"]) {
        return temp;
    }
    json!({"success":true,"status_code":temp["status_code"],"method":"clear-rate-limit+temp-unschedulable","fallback":true})
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn oversized_test_complete_aborts_with_uncertain_failure() {
        let padding = "x".repeat(MAX_TEST_LINE_BYTES + 64);
        let payload = format!(
            "data: {{\"type\":\"test_start\",\"model_id\":\"gpt-5.4\"}}\n\
             data: {{\"type\":\"test_complete\",\"success\":true,\"detail\":\"{padding}\"}}\n"
        );
        let sync_parsed = parse_test_sse(&payload);
        assert_eq!(sync_parsed["success"], false);
        assert_eq!(sync_parsed["uncertain"], true);
        assert_eq!(sync_parsed["error_code"], "account_test_error");
        assert_eq!(sync_parsed["executed_model"], "gpt-5.4");

        let chunks: Vec<Result<Bytes, std::io::Error>> = payload
            .as_bytes()
            .chunks(128 * 1024)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let stream_parsed = parse_test_stream(futures_util::stream::iter(chunks))
            .await
            .unwrap();
        assert_eq!(stream_parsed["success"], false);
        assert_eq!(stream_parsed["uncertain"], true);
        assert_eq!(stream_parsed["error_code"], "account_test_error");
        assert_eq!(stream_parsed["executed_model"], "gpt-5.4");
    }

    #[tokio::test]
    async fn oversized_error_frame_aborts_immediately_before_later_test_complete() {
        let padding = "e".repeat(MAX_TEST_LINE_BYTES + 64);
        let oversized_error = format!(
            "data: {{\"type\":\"test_start\",\"model\":\"gpt-5.4\"}}\n\
             data: {{\"type\":\"error\",\"code\":\"upstream_500\",\"message\":\"{padding}\"}}\n"
        );
        let trailing_complete = "data: {\"type\":\"test_complete\",\"success\":true}\n";
        let combined = format!("{oversized_error}{trailing_complete}");

        let sync_parsed = parse_test_sse(&combined);
        assert_eq!(sync_parsed["success"], false);
        assert_eq!(sync_parsed["uncertain"], true);
        assert_eq!(sync_parsed["error_code"], "account_test_error");

        let pulled = Arc::new(AtomicUsize::new(0));
        let pulled_ref = pulled.clone();
        let raw_chunks = vec![
            Bytes::from(oversized_error.into_bytes()),
            Bytes::from_static(trailing_complete.as_bytes()),
        ];
        let tracked_stream = futures_util::stream::iter(raw_chunks).map(move |bytes| {
            pulled_ref.fetch_add(1, Ordering::SeqCst);
            Ok::<Bytes, std::io::Error>(bytes)
        });
        let stream_parsed = parse_test_stream(tracked_stream).await.unwrap();
        assert_eq!(stream_parsed["success"], false);
        assert_eq!(stream_parsed["uncertain"], true);
        assert_eq!(stream_parsed["error_code"], "account_test_error");
        assert_eq!(
            pulled.load(Ordering::SeqCst),
            1,
            "stream must abort on the oversized error chunk without reading trailing test_complete"
        );
    }
}
