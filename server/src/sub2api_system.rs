use crate::{
    actions::Actions,
    error::{Error, Result},
    security::safe_error,
    upstream::read_limited,
};
use reqwest::Method;
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};

pub async fn request(
    actions: &Actions,
    key: &str,
    action: &str,
    query: &BTreeMap<String, String>,
) -> Result<Value> {
    if !query.is_empty()
        && !(action == "check-updates"
            && query.len() == 1
            && query
                .get("force")
                .is_some_and(|value| matches!(value.as_str(), "true" | "false")))
    {
        return Err(Error::invalid());
    }
    let write = matches!(action, "update" | "restart");
    let path = format!(
        "system/{action}{}",
        if query.get("force").is_some_and(|value| value == "true") {
            "?force=true"
        } else {
            ""
        }
    );
    // Native updates can take 15 minutes. Only this operation gets a longer deadline.
    // Send each write once; native operations continue even after a disconnected client.
    let response = actions
        .request(
            if write { Method::POST } else { Method::GET },
            &path,
            key,
            None,
            Duration::from_secs(if action == "update" { 915 } else { 30 }),
            false,
        )
        .await?;
    let status = response.status();
    let body: Value = serde_json::from_slice(&read_limited(response, 8_000_000).await?)
        .map_err(|_| Error::new(502, "无法读取 Sub2API 响应"))?;
    if !status.is_success() || body["code"] != 0 {
        let message = safe_error(&body["message"], 1500);
        return Err(Error::new(
            if status.is_success() {
                502
            } else {
                status.as_u16()
            },
            if message.is_empty() {
                "Sub2API 未确认操作成功".into()
            } else {
                message
            },
        ));
    }
    if !body["data"].is_object() {
        return Err(Error::new(502, "无法读取 Sub2API 响应"));
    }
    Ok(body["data"].clone())
}
