//! Raw, read-only provider discovery. The admin picker projects aliases and can
//! silently fall back to defaults; neither is evidence of upstream support.
use crate::{
    actions::{text_model, Actions},
    error::{Error, Result},
    model_candidates::Failure,
    model_target,
    model_test_stream::Target,
    security::clean,
    upstream::read_limited,
};
use serde_json::{json, Value};
use std::{collections::BTreeSet, time::Duration};

pub fn endpoint(target: &Target) -> Result<reqwest::Url> {
    let base = target
        .url
        .strip_suffix("/responses")
        .or_else(|| target.url.strip_suffix("/messages"))
        .or_else(|| target.url.strip_suffix("/chat/completions"))
        .ok_or_else(|| Error::new(422, "模型目录地址无效"))?;
    let mut url = reqwest::Url::parse(&format!("{base}/models"))
        .map_err(|_| Error::new(422, "模型目录地址无效"))?;
    if target.oauth {
        let version = target
            .headers
            .get("version")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::new(409, "模型目录客户端版本不可用"))?;
        url.query_pairs_mut().append_pair("client_version", version);
    }
    Ok(url)
}

pub fn models(body: &Value) -> Result<Vec<Value>> {
    if body.get("error").is_some_and(|v| !v.is_null()) || body.get("code").is_some_and(|v| v != 0) {
        return Err(Error::new(502, "上游未确认模型目录读取成功"));
    }
    let arrays: Vec<&Vec<Value>> = if let Some(items) = body.as_array() {
        vec![items]
    } else {
        ["data", "models"]
            .iter()
            .filter_map(|key| body.get(*key))
            .map(|v| {
                v.as_array()
                    .ok_or_else(|| Error::new(502, "上游模型列表格式无效"))
            })
            .collect::<Result<_>>()?
    };
    if arrays.is_empty() || arrays.iter().map(|v| v.len()).sum::<usize>() > 10_000 {
        return Err(Error::new(502, "上游模型列表格式无效或超过限制"));
    }
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    for item in arrays.into_iter().flatten() {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| item.get("slug").and_then(Value::as_str))
            .or_else(|| item.as_str())
            .map(str::trim)
            .ok_or_else(|| Error::new(502, "上游模型条目格式无效"))?;
        if !text_model(id) || !seen.insert(id.to_owned()) {
            continue;
        }
        let label = clean(&item["display_name"], 200);
        values.push(
            json!({"id":id,"display_name":if label.is_empty() {id} else {&label},"type":"model"}),
        );
    }
    Ok(values)
}

pub(crate) async fn fetch(actions: &Actions, id: i64) -> std::result::Result<Vec<Value>, Failure> {
    let row = model_target::account(actions, id)
        .await
        .map_err(Failure::permanent)?;
    let (target, _) = model_target::connection(actions, &row, "")
        .await
        .map_err(Failure::permanent)?;
    let url = endpoint(&target).map_err(Failure::permanent)?;
    #[cfg(any(test, feature = "test-fixtures"))]
    let url = {
        let replacement = actions
            .directory_endpoint
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        replacement.map_or(Ok(url), |base| {
            reqwest::Url::parse(&format!("{}/{id}", base.trim_end_matches('/')))
                .map_err(|_| Failure::permanent(Error::invalid()))
        })?
    };
    let client = target
        .client()
        .map_err(|_| Failure::permanent(Error::new(409, "账号代理配置不可用")))?;
    let mut headers = target.headers;
    headers.insert(
        "accept",
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.remove("content-type");
    headers.remove("openai-beta");
    let response = client
        .get(url)
        .headers(headers)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .map_err(|error| {
            Failure::temporary(if error.is_timeout() {
                Error::new(504, "上游模型目录读取超时，请稍后重试")
            } else {
                Error::new(502, "无法读取上游模型目录")
            })
        })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let error = Error::new(502, format!("上游模型目录请求失败 [http_{status}]"));
        return Err(if status == 429 || status >= 500 {
            Failure::temporary(error)
        } else {
            Failure::permanent(error)
        });
    }
    let bytes = read_limited(response, 8_000_000).await.map_err(|error| {
        if error.detail == "上游响应超过限制" {
            Failure::permanent(error)
        } else {
            Failure::temporary(error)
        }
    })?;
    let body = serde_json::from_slice(&bytes)
        .map_err(|_| Failure::permanent(Error::new(502, "上游模型列表格式无效")))?;
    models(&body).map_err(Failure::permanent)
}
