//! Raw, read-only provider discovery. The admin picker projects aliases and can
//! silently fall back to defaults; neither is evidence of upstream support.
use crate::{
    actions::{text_model, Actions},
    compat::{integer, string, truthy},
    db::Database,
    error::{Error, Result},
    model_candidates::Failure,
    model_test_stream::Target,
    security::clean,
    upstream::read_limited,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

pub const CLIENT_VERSION: &str = "0.146.0";
const MAX_BODY: usize = 8 * 1024 * 1024;
type Headers = BTreeMap<String, String>;

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

pub fn models(body: &Value) -> Result<(Vec<Value>, usize)> {
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
    let entries = arrays.iter().map(|v| v.len()).sum();
    if arrays.is_empty() || entries > 10_000 {
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
    Ok((values, entries))
}

pub(crate) async fn fetch(
    actions: &Actions,
    id: i64,
    target: &Target,
) -> std::result::Result<Vec<Value>, Failure> {
    #[cfg(not(any(test, feature = "test-fixtures")))]
    let _ = (actions, id);
    let url = endpoint(target).map_err(Failure::permanent)?;
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
    let mut headers = target.headers.clone();
    headers.insert(
        "accept",
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.remove("content-type");
    headers.remove("openai-beta");
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut page_url = url.clone();
        let mut cursors = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut values = Vec::new();
        let mut entries = 0;
        loop {
            let response = client
                .get(page_url)
                .headers(headers.clone())
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
                let error = directory_error(status, 502);
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
            let body: Value = serde_json::from_slice(&bytes)
                .map_err(|_| Failure::permanent(Error::new(502, "上游模型列表格式无效")))?;
            let (page, count) = models(&body).map_err(Failure::permanent)?;
            entries += count;
            if entries > 10_000 {
                return Err(Failure::permanent(Error::new(
                    502,
                    "上游模型列表格式无效或超过限制",
                )));
            }
            values.extend(
                page.into_iter()
                    .filter(|model| seen.insert(model["id"].as_str().unwrap().to_owned())),
            );
            if target.platform != "anthropic" || body["has_more"] != true {
                return Ok(values);
            }
            let cursor = body["last_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| Failure::permanent(Error::new(502, "上游模型分页标识缺失")))?;
            if !cursors.insert(cursor.to_owned()) || cursors.len() > 100 {
                return Err(Failure::permanent(Error::new(502, "上游模型分页无效")));
            }
            page_url = url.clone();
            page_url.query_pairs_mut().append_pair("after_id", cursor);
        }
    })
    .await
    .map_err(|_| Failure::temporary(Error::new(504, "上游模型目录读取超时，请稍后重试")))?
}

async fn read_http_timeout(
    client: &reqwest::Client,
    url: &str,
    headers: &Headers,
    timeout: Duration,
) -> Result<(u16, Vec<u8>)> {
    let mut request = client.get(url).timeout(timeout);
    for (key, value) in headers {
        request = request.header(key, value);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    Ok((status, read_limited(response, MAX_BODY).await?))
}

async fn source_catalog_read(db: &Database, account: &Value) -> Result<Value> {
    let timeout = Duration::from_secs(8);
    let (url, headers) = catalog_request(db, account).await?;
    let client = catalog_client(db, account, timeout).await?;
    let mut data = catalog_pages(
        &client,
        &url,
        &headers,
        timeout,
        string(&account["platform"]),
    )
    .await?;
    redact_catalog(&mut data, &account["credentials"]);
    Ok(data)
}

async fn catalog_request(db: &Database, account: &Value) -> Result<(reqwest::Url, Headers)> {
    let credentials = &account["credentials"];
    let platform = string(&account["platform"]);
    let kind = string(&account["type"]);
    if !matches!(
        platform,
        "openai"
            | "grok"
            | "anthropic"
            | "gemini"
            | "kimi"
            | "zhipu"
            | "deepseek"
            | "minimax"
            | "opencode_go"
    ) {
        return Err(Error::new(422, "该平台没有只读目录协议"));
    }
    let mut base = string(&credentials["base_url"])
        .trim_end_matches('/')
        .to_owned();
    let token = string(
        &credentials[if kind == "oauth" {
            "access_token"
        } else {
            "api_key"
        }],
    );
    if token.is_empty() {
        return Err(Error::new(422, "当前凭据不可用"));
    }
    let mut headers: Headers = [("Accept".into(), "application/json".into())].into();
    let mut url;
    if platform == "openai" && kind == "oauth" {
        url =
            format!("https://chatgpt.com/backend-api/codex/models?client_version={CLIENT_VERSION}");
        headers.extend([
            ("Authorization".into(), format!("Bearer {token}")),
            ("Originator".into(), "codex_cli_rs".into()),
            ("Version".into(), CLIENT_VERSION.into()),
            (
                "User-Agent".into(),
                format!("codex_cli_rs/{CLIENT_VERSION} (Ubuntu 22.4.0; x86_64) xterm-256color"),
            ),
        ]);
        if truthy(&credentials["chatgpt_account_id"]) {
            headers.insert(
                "ChatGPT-Account-ID".into(),
                string(&credentials["chatgpt_account_id"]).into(),
            );
        }
    } else if platform == "gemini" {
        if kind != "apikey" {
            return Err(Error::new(422, "该凭据没有只读目录接口"));
        }
        if base.is_empty() {
            base = "https://generativelanguage.googleapis.com/v1beta".into();
        }
        url = if base.ends_with("/models") {
            base
        } else {
            format!(
                "{base}{}",
                if base.ends_with("/v1beta") {
                    "/models"
                } else {
                    "/v1beta/models"
                }
            )
        };
        headers.insert("x-goog-api-key".into(), token.into());
    } else {
        if platform == "grok" && kind == "oauth" && base.is_empty() {
            let setting = db
                .one(
                    "SELECT value FROM settings WHERE key='grok_default_base_url_mode'",
                    &[],
                )
                .await?
                .unwrap_or(Value::Null);
            base = match string(&setting["value"]) {
                "api" => "https://api.x.ai/v1",
                "us-east-1" => "https://us-east-1.api.x.ai/v1",
                "us-west-2" => "https://us-west-2.api.x.ai/v1",
                "eu-west-1" => "https://eu-west-1.api.x.ai/v1",
                _ => "https://cli-chat-proxy.grok.com/v1",
            }
            .into();
        }
        if base.is_empty() {
            base = match platform {
                "openai" => "https://api.openai.com/v1",
                "grok" => "https://api.x.ai/v1",
                "anthropic" => "https://api.anthropic.com/v1",
                _ => "",
            }
            .into();
        }
        if base.is_empty() {
            return Err(Error::new(422, "未配置上游目录地址"));
        }
        let explicit = base.ends_with('#');
        if explicit {
            base.pop();
            base = base.trim_end_matches('/').into();
        }
        url = if base.ends_with("/models") {
            base
        } else {
            format!(
                "{base}{}",
                if explicit || base.ends_with("/v1") {
                    "/models"
                } else {
                    "/v1/models"
                }
            )
        };
        if platform == "openai" {
            url.push_str(&format!("?client_version={CLIENT_VERSION}"));
        }
        headers.insert("Authorization".into(), format!("Bearer {token}"));
        if platform == "anthropic" {
            headers.insert("anthropic-version".into(), "2023-06-01".into());
            if kind == "oauth" {
                headers.insert("anthropic-beta".into(), "oauth-2025-04-20".into());
            }
            if kind == "apikey" {
                headers.remove("Authorization");
                headers.insert("x-api-key".into(), token.into());
            }
        }
        if platform == "grok"
            && kind == "oauth"
            && reqwest::Url::parse(&url)
                .ok()
                .is_some_and(|v| v.host_str() == Some("cli-chat-proxy.grok.com"))
        {
            headers.extend([
                ("X-XAI-Token-Auth".into(), "xai-grok-cli".into()),
                ("x-grok-client-version".into(), "0.2.120".into()),
                ("x-grok-client-identifier".into(), "grok-shell".into()),
                ("User-Agent".into(), "xai-grok-workspace/0.2.120".into()),
            ]);
            for (field, header) in [("sub", "X-UserID"), ("email", "X-Email")] {
                if truthy(&credentials[field]) {
                    headers.insert(header.into(), string(&credentials[field]).into());
                }
            }
        }
    }
    let parsed = reqwest::Url::parse(&url).map_err(|_| Error::new(422, "上游目录地址无效"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.host_str().is_none()
    {
        return Err(Error::new(422, "上游目录地址无效"));
    }
    Ok((parsed, headers))
}

async fn catalog_client(
    db: &Database,
    account: &Value,
    timeout: Duration,
) -> Result<reqwest::Client> {
    let client = if let Some(id) = integer(&account["proxy_id"]).filter(|v| *v != 0) {
        let proxy = db
            .one(
                "SELECT protocol,host,port,username,password FROM proxies WHERE id=$1 AND deleted_at IS NULL",
                &[id.into()],
            )
            .await?
            .ok_or_else(|| Error::new(422, "账号代理不可用"))?;
        let host = string(&proxy["host"]);
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.into()
        };
        let mut url = reqwest::Url::parse(&format!(
            "{}://{}:{}",
            string(&proxy["protocol"]),
            host,
            integer(&proxy["port"]).unwrap_or(0)
        ))
        .map_err(|_| Error::new(422, "账号代理不可用"))?;
        if truthy(&proxy["username"]) {
            url.set_username(string(&proxy["username"]))
                .map_err(|_| Error::new(422, "账号代理不可用"))?;
            url.set_password(Some(string(&proxy["password"])))
                .map_err(|_| Error::new(422, "账号代理不可用"))?;
        }
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .proxy(
                reqwest::Proxy::all(url.as_str()).map_err(|_| Error::new(422, "账号代理不可用"))?,
            )
            .build()?
    } else {
        static NO_PROXY_CLIENT: std::sync::LazyLock<Option<reqwest::Client>> =
            std::sync::LazyLock::new(|| {
                reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .ok()
            });
        match NO_PROXY_CLIENT.as_ref() {
            Some(shared) => shared.clone(),
            None => reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(timeout)
                .build()?,
        }
    };
    Ok(client)
}

pub(crate) const DIRECTORY_REVOKED: &str = "model_directory_revoked";

pub(crate) fn directory_error(upstream: u16, status: u16) -> Error {
    let message = format!("上游模型目录请求失败 [http_{upstream}]");
    if matches!(upstream, 401 | 403) {
        Error::new(status, json!({"code":DIRECTORY_REVOKED,"message":message}))
    } else {
        Error::new(status, message)
    }
}

async fn catalog_pages(
    client: &reqwest::Client,
    parsed: &reqwest::Url,
    headers: &Headers,
    timeout: Duration,
    platform: &str,
) -> Result<Value> {
    let (status, body) = read_http_timeout(client, parsed.as_str(), headers, timeout).await?;
    if status != 200 {
        return Err(directory_error(
            status,
            if matches!(status, 401 | 403) {
                status
            } else {
                502
            },
        ));
    }
    let mut data: Value = serde_json::from_slice(&body)?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut cursors = BTreeSet::new();
    loop {
        let next = if platform == "gemini" {
            data["nextPageToken"]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(|v| ("pageToken", v.to_owned(), "models"))
        } else if data["has_more"] == true {
            Some((
                "after_id",
                data["last_id"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| Error::new(502, "上游模型分页标识缺失"))?
                    .to_owned(),
                "data",
            ))
        } else {
            None
        };
        let Some((key, cursor, field)) = next else {
            break;
        };
        if !cursors.insert(cursor.clone()) || cursors.len() > 100 {
            return Err(Error::new(502, "上游模型分页无效"));
        }
        let mut page_url = parsed.clone();
        page_url.query_pairs_mut().append_pair(key, &cursor);
        let (reply_status, reply_body) = tokio::time::timeout_at(
            deadline,
            read_http_timeout(client, page_url.as_str(), headers, timeout),
        )
        .await
        .map_err(|_| Error::new(504, "上游模型目录读取超时"))??;
        if reply_status != 200 {
            return Err(directory_error(reply_status, 502));
        }
        let page: Value = serde_json::from_slice(&reply_body)?;
        let entries = page[field]
            .as_array()
            .ok_or_else(|| Error::new(502, "上游模型分页格式无效"))?;
        let all = data[field]
            .as_array_mut()
            .ok_or_else(|| Error::new(502, "上游模型列表格式无效"))?;
        if all.len() + entries.len() > 10_000 {
            return Err(Error::new(502, "上游模型列表超过限制"));
        }
        all.extend(entries.iter().cloned());
        for field in ["has_more", "last_id", "nextPageToken"] {
            data[field] = page[field].clone();
        }
    }

    Ok(data)
}
fn redact_catalog(data: &mut Value, credentials: &Value) {
    let secrets: Vec<&str> = ["api_key", "access_token", "refresh_token", "id_token"]
        .into_iter()
        .filter_map(|k| credentials[k].as_str())
        .filter(|v| v.chars().count() >= 8)
        .collect();
    fn scrub(value: &mut Value, secrets: &[&str]) {
        match value {
            Value::String(value) => {
                for secret in secrets {
                    *value = value.replace(secret, "[redacted]");
                }
            }
            Value::Array(values) => {
                for value in values {
                    scrub(value, secrets);
                }
            }
            Value::Object(values) => {
                for value in values.values_mut() {
                    scrub(value, secrets);
                }
            }
            _ => (),
        }
    }
    scrub(data, &secrets);
}

pub async fn account_models_read(db: &Database, id: i64) -> Result<Vec<Value>> {
    let account = db
        .one(
            "SELECT a.id,a.platform,a.type,coalesce(p.credentials,a.credentials) AS credentials,a.extra,coalesce(a.proxy_id,p.proxy_id) AS proxy_id FROM accounts a LEFT JOIN accounts p ON p.id=coalesce(nullif(to_jsonb(a)->>'parent_account_id','')::bigint,nullif(a.extra->>'parent_account_id','')::bigint) AND p.deleted_at IS NULL WHERE a.id=$1 AND a.deleted_at IS NULL",
            &[id.into()],
        )
        .await?
        .ok_or_else(|| Error::new(404, "账号不存在"))?;
    let data = source_catalog_read(db, &account).await?;
    let entries = data
        .as_array()
        .or_else(|| data.get("data").and_then(Value::as_array))
        .or_else(|| data.get("models").and_then(Value::as_array))
        .ok_or_else(|| Error::new(502, "上游模型列表格式无效"))?;
    if entries.len() > 10_000 {
        return Err(Error::new(502, "上游模型列表超过限制"));
    }
    let mut seen = BTreeSet::new();
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let id = entry
                .get("id")
                .or_else(|| entry.get("slug"))
                .or_else(|| entry.get("modelId"))
                .or_else(|| entry.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .map(|id| {
                    if account["platform"] == "gemini" {
                        id.strip_prefix("models/").unwrap_or(id)
                    } else {
                        id
                    }
                })
                .filter(|id| !id.is_empty() && seen.insert((*id).to_owned()))?;
            let display_name = clean(
                entry
                    .get("display_name")
                    .or_else(|| entry.get("displayName"))
                    .unwrap_or(&Value::Null),
                200,
            );
            let model_id = clean(&Value::String(id.to_owned()), 200);
            if model_id.is_empty() {
                return None;
            }
            let label = if display_name.is_empty() {
                model_id.clone()
            } else {
                display_name
            };
            Some(json!({"id": model_id, "display_name": label, "type": "model"}))
        })
        .collect())
}
