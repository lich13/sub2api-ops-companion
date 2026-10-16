use crate::{
    actions::Actions,
    compat::{integer, string, text, truthy},
    security::{clean, safe_error},
    upstream::read_limited,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub fn parse_test_sse(payload: &str) -> Value {
    let mut texts = vec![];
    let mut success = false;
    let mut executed = String::new();
    for line in payload.lines().map(str::trim) {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<Value>(data.trim()) else {
            continue;
        };
        if !event.is_object() {
            continue;
        }
        if matches!(string(&event["type"]), "test_start" | "test_complete") {
            let model = ["model_id", "model"]
                .iter()
                .find_map(|key| event[*key].as_str().filter(|v| !v.is_empty()));
            if let Some(model) = model {
                executed = clean(&json!(model), 200);
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
            let mut result =
                json!({"success":false,"error":error,"error_code":code,"executed_model":executed});
            // A transport failure reported inside SSE is just as ambiguous as a
            // dropped HTTP stream: the upstream may already have spent tokens.
            if matches!(
                code.trim().to_ascii_lowercase().as_str(),
                "network_error" | "timeout" | "account_test_error" | "missing_success_event"
            ) {
                result["uncertain"] = true.into();
            }
            return result;
        }
        if event["type"] == "content" && truthy(&event["text"]) {
            texts.push(text(&event["text"]));
        }
        if event["type"] == "test_complete" && event["success"] == true {
            success = true;
        }
    }
    if !success {
        json!({"success":false,"error":"账号测试未返回成功事件","error_code":"missing_success_event","uncertain":true,"executed_model":executed})
    } else {
        json!({"success":true,"response_text":texts.join("").trim(),"executed_model":executed})
    }
}
pub async fn test(actions: &Actions, id: i64, model: &str, key: &str, seconds: u64) -> Value {
    let failure = |code: &str, error: &str, uncertain: bool| json!({"success":false,"error":error,"error_code":code,"model_id":model,"uncertain":uncertain});
    if actions.settings.read_only {
        return failure("read_only", "服务处于只读验收模式", false);
    }
    if key.trim().is_empty() || actions.upstream.active_url().is_empty() {
        return failure(
            "missing_sub2api_admin_credentials",
            "缺少 Sub2API 地址或 Admin API Key",
            false,
        );
    }
    let started = Instant::now();
    let response = match actions
        .request(
            reqwest::Method::POST,
            &format!("accounts/{id}/test"),
            key,
            Some(&json!({"model_id":model,"prompt":"","mode":""})),
            Duration::from_secs(seconds.clamp(1, 60)),
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
    let bytes = match read_limited(response, 128 * 1024 * 1024).await {
        Ok(value) => value,
        Err(_) => return failure("account_test_error", "测活响应中断，结果待核对", true),
    };
    let mut parsed = parse_test_sse(&String::from_utf8_lossy(&bytes));
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
    if actions.settings.read_only {
        return json!({"success":false,"error_code":"read_only","error":"服务处于只读验收模式"});
    }
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
    let body = read_limited(response, 2_000_000)
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
