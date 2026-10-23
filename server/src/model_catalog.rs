use crate::{
    compat::{integer, now, py_json, string, truthy},
    db::Database,
    error::{Error, Result},
    model_config::{group_version, ModelConfig},
    model_reasoning::{
        complete_descriptor, forwarding, model_id, patch_descriptor, reasoning_fields,
        reasoning_values, routing_binding, same_reasoning, FIELDS,
    },
    model_rules::{admitted, revision, transform, validate_overrides},
    security::clean,
    settings::Settings,
    upstream::{read_limited, Upstream},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub const CLIENT_VERSION: &str = "0.146.0";
pub const MAX_BODY: usize = 8 * 1024 * 1024;
const GROUP_FIELDS: &str = "id,name,platform,model_allowlist,codex_models_manifest_config,max_reasoning_effort,max_reasoning_effort_over_limit,reasoning_effort_mappings,updated_at";
pub type Headers = BTreeMap<String, String>;
pub struct HttpReply {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
}
struct Cache {
    baselines: BTreeMap<(i64, String, String), (Instant, Value)>,
    responses: BTreeMap<String, (Instant, Vec<u8>, String)>,
    sources: BTreeMap<String, (Instant, Value)>,
    version: Option<(Instant, String)>,
    status: Value,
    group_locks: BTreeMap<i64, Arc<tokio::sync::Mutex<()>>>,
}
#[derive(Clone)]
pub struct Catalog {
    db: Database,
    upstream: Upstream,
    pub store: ModelConfig,
    client: reqwest::Client,
    cache: Arc<Mutex<Cache>>,
    read_only: bool,
}
pub fn caller_key(headers: &Headers) -> &str {
    let auth = headers
        .get("authorization")
        .map(String::as_str)
        .unwrap_or("");
    if auth.to_lowercase().starts_with("bearer ") && !auth[7..].trim().is_empty() {
        return auth[7..].trim();
    }
    headers
        .get("x-api-key")
        .filter(|v| !v.is_empty())
        .or_else(|| headers.get("x-goog-api-key"))
        .map(String::as_str)
        .unwrap_or("")
}
pub fn import_fields(entry: &Value) -> Value {
    let mut out = json!({});
    for field in FIELDS {
        if let Some(value) = entry.get(*field) {
            out[*field] = value.clone();
        }
    }
    if let Some(levels) = out
        .get_mut("supported_reasoning_levels")
        .and_then(Value::as_array_mut)
    {
        for value in levels {
            if value.is_string() {
                *value = json!({"effort":value,"description":""});
            }
        }
    }
    if entry["reasoning"] == false && out.get("supported_reasoning_levels").is_none() {
        out["supported_reasoning_levels"] = json!([{"effort":"none","description":""}]);
        out["default_reasoning_level"] = "none".into();
    } else if out.get("supported_reasoning_levels").is_none() {
        let mut values = vec![];
        for option in entry["reasoning_options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v.is_object() && string(&v["type"]).to_lowercase() == "effort")
        {
            for value in option["values"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|v| v.is_string())
            {
                if !values.contains(value) {
                    values.push(value.clone());
                }
            }
        }
        if !values.is_empty() {
            out["supported_reasoning_levels"] = json!(values
                .iter()
                .map(|v| json!({"effort":v,"description":""}))
                .collect::<Vec<_>>());
        }
    }
    out
}
pub fn response_etag(body: &Value) -> String {
    format!("\"{}\"", revision(body))
}
pub fn source_identity(account: &Value) -> String {
    revision(account)
}
pub fn routes_revision(accounts: &[Value], routes: &[Value]) -> String {
    revision(&json!([accounts, routes]))
}
impl Catalog {
    pub fn new(settings: &Settings, db: Database, upstream: Upstream) -> Result<Self> {
        Ok(Self {
            db,
            upstream,
            read_only: settings.read_only,
            store: ModelConfig::new(settings.model_path.clone(), settings.read_only),
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(12))
                .build()?,
            cache: Arc::new(Mutex::new(Cache {
                baselines: BTreeMap::new(),
                responses: BTreeMap::new(),
                sources: BTreeMap::new(),
                version: None,
                status: json!({"state":"ready","message":"","updated_at":null}),
                group_locks: BTreeMap::new(),
            })),
        })
    }
    fn mark(&self, message: &str) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).status = json!({"state":if message.is_empty() {"ready"} else {"fallback"},"message":message,"updated_at":now()});
    }
    pub fn status(&self) -> Value {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    async fn read_http(
        &self,
        method: reqwest::Method,
        url: &str,
        headers: &Headers,
        body: Option<&Value>,
        client: Option<&reqwest::Client>,
    ) -> Result<HttpReply> {
        if self.read_only && method != reqwest::Method::GET {
            return Err(Error::read_only());
        }
        let mut request = client.unwrap_or(&self.client).request(method, url);
        for (key, value) in headers {
            request = request.header(key, value);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_owned())))
            .collect();
        Ok(HttpReply {
            status,
            headers,
            body: read_limited(response, MAX_BODY).await?,
        })
    }
    async fn admin(
        &self,
        key: &str,
        method: reqwest::Method,
        path: &str,
        payload: Option<&Value>,
    ) -> Result<Value> {
        let response = self
            .read_http(
                method,
                &format!("{}/api/v1/admin{path}", self.upstream.active_url()),
                &[("x-api-key".into(), key.into())].into(),
                payload,
                None,
            )
            .await
            .map_err(|_| Error::new(502, "Sub2API 分组操作未确认，未自动重放"))?;
        let body: Value = serde_json::from_slice(&response.body)
            .map_err(|_| Error::new(502, "Sub2API 分组操作未确认，未自动重放"))?;
        if response.status >= 400 || body["code"] != 0 {
            return Err(Error::new(
                if matches!(response.status, 401 | 403) {
                    401
                } else {
                    502
                },
                "Sub2API 分组操作未确认，请刷新核对",
            ));
        }
        Ok(body["data"].clone())
    }
    pub async fn group(&self, id: i64) -> Result<Value> {
        if id < 1 {
            return Err(Error::new(422, "分组 ID 无效"));
        }
        let mut row = self
            .db
            .one(
                &format!("SELECT {GROUP_FIELDS} FROM groups WHERE id=$1 AND deleted_at IS NULL"),
                &[id.into()],
            )
            .await?
            .ok_or_else(|| Error::new(404, "分组不存在"))?;
        if !truthy(&row["model_allowlist"]) {
            row["model_allowlist"] = json!({"enabled":false,"models":[]});
        }
        row["version"] = group_version(&row).into();
        Ok(row)
    }
    pub async fn accounts(&self, id: i64) -> Result<Vec<Value>> {
        self.db.all("SELECT a.id,a.platform,a.type,a.status,a.schedulable,a.credentials,a.extra,a.proxy_id FROM accounts a JOIN account_groups ag ON ag.account_id=a.id WHERE ag.group_id=$1 AND a.deleted_at IS NULL ORDER BY a.priority,a.id",&[id.into()]).await
    }
    pub async fn routes(&self, group: &Value) -> Result<Vec<Value>> {
        if group["platform"] != "composite" {
            return Ok(vec![]);
        }
        self.db.all("SELECT id,public_model,match_type,target_platform,upstream_model,endpoint,priority,enabled FROM composite_model_routes WHERE group_id=$1 AND enabled=true",&[integer(&group["id"]).unwrap_or(0).into()]).await
    }
    pub async fn list_groups(&self) -> Result<Value> {
        Ok(
            json!({"groups":self.db.all("SELECT id,name,platform FROM groups WHERE deleted_at IS NULL ORDER BY id",&[]).await?,"status":self.status()}),
        )
    }
    pub async fn native_version(&self, key: &str) -> String {
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((until, value)) = &cache.version {
                if *until > Instant::now() {
                    return value.clone();
                }
            }
        }
        let key = if key.is_empty() {
            self.db
                .one("SELECT value FROM settings WHERE key='admin_api_key'", &[])
                .await
                .ok()
                .flatten()
                .map(|v| string(&v["value"]).to_owned())
                .unwrap_or_default()
        } else {
            key.to_owned()
        };
        let value = if key.is_empty() {
            Value::Null
        } else {
            self.admin(&key, reqwest::Method::GET, "/system/version", None)
                .await
                .unwrap_or(Value::Null)
        };
        let version = string(&value["version"])
            .strip_prefix('v')
            .unwrap_or(string(&value["version"]));
        let parts: Vec<_> = version.split('.').collect();
        let version = if parts.len() == 3
            && parts
                .iter()
                .all(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
        {
            version.to_owned()
        } else {
            String::new()
        };
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).version = Some((
            Instant::now() + Duration::from_secs(if version.is_empty() { 5 } else { 30 }),
            version.clone(),
        ));
        version
    }
    pub async fn baseline(&self, group: &Value) -> Result<(Value, String)> {
        let id = integer(&group["id"]).ok_or_else(Error::invalid)?;
        let version = string(&group["version"]);
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, body)) = cache
                .baselines
                .iter()
                .filter(|((gid, v, _), (at, _))| {
                    *gid == id && v == version && at.elapsed() < Duration::from_secs(60)
                })
                .map(|(_, v)| v)
                .max_by_key(|(at, _)| *at)
            {
                return Ok((body.clone(), "native".into()));
            }
        }
        let keys = self.db.all("SELECT key FROM api_keys WHERE group_id=$1 AND deleted_at IS NULL AND status='active' ORDER BY id LIMIT 3",&[id.into()]).await?;
        for row in keys {
            if let Ok(response) = self
                .read_http(
                    reqwest::Method::GET,
                    &format!(
                        "{}/v1/models?client_version={CLIENT_VERSION}",
                        self.upstream.active_url()
                    ),
                    &[(
                        "Authorization".into(),
                        format!("Bearer {}", string(&row["key"])),
                    )]
                    .into(),
                    None,
                    None,
                )
                .await
            {
                if response.status == 200 {
                    if let Ok(body) = serde_json::from_slice::<Value>(&response.body) {
                        if body["models"].is_array() {
                            self.cache
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .baselines
                                .insert(
                                    (id, version.into(), "admin-preview".into()),
                                    (Instant::now(), body.clone()),
                                );
                            return Ok((body, "native".into()));
                        }
                    }
                }
            }
        }
        Ok((json!({"models":[]}), "unavailable".into()))
    }
    pub fn active_reasoning(
        body: &Value,
        group: &Value,
        saved: &Value,
        accounts: &[Value],
        routes: &[Value],
        version: &str,
    ) -> Result<(Value, Value)> {
        let mut overrides = json!({});
        let mut supplements = json!({});
        for (model, fields) in saved["overrides"].as_object().into_iter().flatten() {
            overrides[model] = Value::Object(
                fields
                    .as_object()
                    .ok_or_else(Error::invalid)?
                    .iter()
                    .filter(|(k, _)| !FIELDS.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
        }
        let native: BTreeMap<_, _> = body["models"]
            .as_array()
            .ok_or_else(Error::invalid)?
            .iter()
            .map(|v| (string(&v["slug"]), v))
            .collect();
        for (model, entry) in saved["reasoning"].as_object().into_iter().flatten() {
            if entry["mode"] != "active" || !admitted(&group["model_allowlist"], model) {
                continue;
            }
            let (binding, available) = routing_binding(group, model, accounts, routes);
            let efforts: Vec<String> = entry["efforts"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| string(v).into())
                .collect();
            if entry["binding"] != binding
                || forwarding(group, model, &efforts, &available, version)["state"] != "verified"
            {
                continue;
            }
            let fields = reasoning_fields(&entry["efforts"], &entry["default_effort"])?;
            let original = native.get(model.as_str()).copied().unwrap_or(&Value::Null);
            if same_reasoning(original, &fields) {
                continue;
            }
            let base = if truthy(original) {
                original
            } else {
                &entry["descriptor"]
            };
            if !complete_descriptor(base, model) {
                continue;
            }
            if !native.contains_key(model.as_str()) {
                supplements[model] = base.clone();
            }
            overrides[model] =
                crate::model_rules::merge(&overrides[model], &patch_descriptor(base, &fields));
        }
        Ok((overrides, supplements))
    }
    pub async fn proxy(&self, path: &str, query: &str, headers: &Headers) -> HttpReply {
        let forwarded = headers
            .iter()
            .filter(|(k, _)| {
                matches!(
                    k.to_lowercase().as_str(),
                    "authorization"
                        | "x-api-key"
                        | "x-goog-api-key"
                        | "user-agent"
                        | "originator"
                        | "version"
                        | "x-client-version"
                        | "x-forwarded-for"
                        | "x-real-ip"
                        | "x-forwarded-proto"
                        | "accept"
                        | "openai-beta"
                        | "chatgpt-account-id"
                )
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let url = format!(
            "{}{path}{}{}",
            self.upstream.active_url(),
            if query.is_empty() { "" } else { "?" },
            query
        );
        let response = self
            .read_http(reqwest::Method::GET, &url, &forwarded, None, None)
            .await;
        let Ok(mut response) = response else {
            self.mark("原始模型目录暂不可用");
            return HttpReply {
                status: 503,
                headers: [("content-type".into(), "application/json".into())].into(),
                body: b"{\"error\":\"catalog_unavailable\"}".to_vec(),
            };
        };
        response.headers.retain(|k, _| {
            matches!(
                k.as_str(),
                "content-type" | "etag" | "cache-control" | "retry-after" | "vary"
            )
        });
        if response.status != 200 {
            return response;
        }
        match self.proxy_transform(query, headers, &response.body).await {
            Ok(Some((body, etag))) => {
                self.mark("");
                response.headers.insert("etag".into(), etag.clone());
                response
                    .headers
                    .insert("cache-control".into(), "private, no-cache".into());
                response
                    .headers
                    .insert("x-sub2ops-catalog".into(), "active".into());
                let requested = headers
                    .get("if-none-match")
                    .map(String::as_str)
                    .unwrap_or("");
                if requested
                    .split(',')
                    .map(|v| v.trim().strip_prefix("W/").unwrap_or(v.trim()))
                    .any(|v| v == etag || v == "*")
                {
                    response.status = 304;
                    response.body.clear();
                } else {
                    response.body = body;
                }
            }
            Ok(None) => (),
            Err(_) => {
                self.mark("兼容层暂不可用，已返回 Sub2API 原始目录");
                response
                    .headers
                    .insert("x-sub2ops-catalog".into(), "fallback".into());
            }
        }
        response
    }
    async fn proxy_transform(
        &self,
        query: &str,
        headers: &Headers,
        raw: &[u8],
    ) -> Result<Option<(Vec<u8>, String)>> {
        let identity = self
            .db
            .one(
                "SELECT group_id FROM api_keys WHERE key=$1 AND deleted_at IS NULL",
                &[caller_key(headers).into()],
            )
            .await?;
        let Some(id) = identity
            .and_then(|v| integer(&v["group_id"]))
            .filter(|v| *v != 0)
        else {
            return Ok(None);
        };
        let group = self.group(id).await?;
        let saved = self.store.group(id)?;
        let original: Value = serde_json::from_slice(raw)?;
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.baselines.insert(
                (id, string(&group["version"]).into(), query.into()),
                (Instant::now(), original.clone()),
            );
            if cache.baselines.len() > 128 {
                if let Some(key) = cache
                    .baselines
                    .iter()
                    .min_by_key(|(_, v)| v.0)
                    .map(|(k, _)| k.clone())
                {
                    cache.baselines.remove(&key);
                }
            }
        }
        let accounts = if group["platform"] == "composite" || truthy(&saved["reasoning"]) {
            self.accounts(id).await?
        } else {
            vec![]
        };
        let routes = self.routes(&group).await?;
        let version = if truthy(&saved["reasoning"]) {
            self.native_version("").await
        } else {
            String::new()
        };
        let cache_key = revision(&json!([
            id,
            group["version"],
            saved["revision"],
            query,
            version,
            revision(&original),
            routes_revision(&accounts, &routes)
        ]));
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((until, body, etag)) = cache.responses.get(&cache_key) {
                if *until > Instant::now() {
                    return Ok(Some((body.clone(), etag.clone())));
                }
            }
        }
        let (overrides, supplements) =
            Self::active_reasoning(&original, &group, &saved, &accounts, &routes, &version)?;
        let final_body = transform(
            &original,
            &overrides,
            &group["model_allowlist"],
            &supplements,
        )?;
        let body = py_json(&final_body, false, true).into_bytes();
        let etag = response_etag(&final_body);
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.responses.retain(|_, v| v.0 > Instant::now());
            if cache.responses.len() >= 128 {
                cache.responses.clear();
            }
            cache.responses.insert(
                cache_key,
                (
                    Instant::now() + Duration::from_secs(15),
                    body.clone(),
                    etag.clone(),
                ),
            );
        }
        Ok(Some((body, etag)))
    }

    pub async fn source_catalog(&self, account: &Value, timeout: Duration) -> Result<Value> {
        let identity = source_identity(account);
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((until, value)) = cache.sources.get(&identity) {
                if *until > Instant::now() {
                    return Ok(value.clone());
                }
            }
        }
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
                | "opencodego"
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
            url = format!(
                "https://chatgpt.com/backend-api/codex/models?client_version={CLIENT_VERSION}"
            );
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
                let setting = self
                    .db
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
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout.max(Duration::from_millis(100)));
        if let Some(id) = integer(&account["proxy_id"]).filter(|v| *v != 0) {
            let proxy = self.db.one("SELECT protocol,host,port,username,password FROM proxies WHERE id=$1 AND deleted_at IS NULL",&[id.into()]).await?.ok_or_else(||Error::new(422,"账号代理不可用"))?;
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
            builder = builder.proxy(
                reqwest::Proxy::all(url.as_str()).map_err(|_| Error::new(422, "账号代理不可用"))?,
            );
        }
        let client = builder.build()?;
        let response = self
            .read_http(reqwest::Method::GET, &url, &headers, None, Some(&client))
            .await?;
        if response.status != 200 {
            return Err(Error::new(502, "上游目录暂不可用"));
        }
        let mut data: Value = serde_json::from_slice(&response.body)?;
        let deadline = tokio::time::Instant::now() + timeout;
        let mut cursors = std::collections::BTreeSet::new();
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
            let reply = tokio::time::timeout_at(
                deadline,
                self.read_http(
                    reqwest::Method::GET,
                    page_url.as_str(),
                    &headers,
                    None,
                    Some(&client),
                ),
            )
            .await
            .map_err(|_| Error::new(504, "上游模型目录读取超时"))??;
            if reply.status != 200 {
                return Err(Error::new(502, "上游目录暂不可用"));
            }
            let page: Value = serde_json::from_slice(&reply.body)?;
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
        scrub(&mut data, &secrets);
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.sources.retain(|_, v| v.0 > Instant::now());
            if cache.sources.len() >= 64 {
                cache.sources.clear();
            }
            cache.sources.insert(
                identity,
                (Instant::now() + Duration::from_secs(60), data.clone()),
            );
        }
        Ok(data)
    }

    pub async fn account_models(&self, id: i64) -> Result<Vec<Value>> {
        let account = self
            .db
            .one(
                "SELECT a.id,a.platform,a.type,coalesce(p.credentials,a.credentials) AS credentials,a.extra,coalesce(p.proxy_id,a.proxy_id) AS proxy_id FROM accounts a LEFT JOIN accounts p ON p.id=coalesce(nullif(to_jsonb(a)->>'parent_account_id','')::bigint,nullif(a.extra->>'parent_account_id','')::bigint) AND p.deleted_at IS NULL WHERE a.id=$1 AND a.deleted_at IS NULL",
                &[id.into()],
            )
            .await?
            .ok_or_else(|| Error::new(404, "账号不存在"))?;
        let data = self
            .source_catalog(&account, Duration::from_secs(8))
            .await?;
        let entries = data
            .as_array()
            .or_else(|| data.get("data").and_then(Value::as_array))
            .or_else(|| data.get("models").and_then(Value::as_array))
            .ok_or_else(|| Error::new(502, "上游模型列表格式无效"))?;
        if entries.len() > 10_000 {
            return Err(Error::new(502, "上游模型列表超过限制"));
        }
        let mut seen = std::collections::BTreeSet::new();
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

    fn summary(group: &Value) -> Value {
        crate::compat::pick(group, &["id", "name", "platform", "version"])
    }
    fn reasoning_item(
        model: &str,
        entry: &Value,
        group: &Value,
        native: &Value,
        accounts: &[Value],
        routes: &[Value],
        version: &str,
    ) -> Result<Value> {
        let (binding, available) = routing_binding(group, model, accounts, routes);
        let efforts: Vec<String> = entry["efforts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| string(v).into())
            .collect();
        let checked = forwarding(group, model, &efforts, &available, version);
        let fields = reasoning_fields(&entry["efforts"], &entry["default_effort"])?;
        let mut state = string(&checked["state"]);
        let mut reason = string(&checked["reason"]);
        if !admitted(&group["model_allowlist"], model) {
            state = "unverified";
            reason = "分组白名单尚未允许该模型";
        } else if entry["binding"] != binding {
            state = "unverified";
            reason = "路由或分组策略已变化，请重新核对并保存";
        } else if state == "verified" && same_reasoning(&native[model], &fields) {
            state = "native";
            reason = "原生目录已提供相同档位，可恢复原生";
        } else if state == "verified"
            && !complete_descriptor(
                if truthy(&native[model]) {
                    &native[model]
                } else {
                    &entry["descriptor"]
                },
                model,
            )
        {
            state = "unverified";
            reason = "缺少该模型自身的完整目录描述";
        } else if state == "verified" && entry["mode"] == "active" {
            state = "active";
        } else if state == "verified" {
            state = "draft";
            reason = "草稿已具备生效条件，请核对后保存";
        }
        Ok(
            json!({"model":model,"efforts":entry["efforts"],"default_effort":entry["default_effort"],"state":state,"reason":reason,"source":entry.get("source").cloned().unwrap_or(json!("manual")),"updated_at":entry["updated_at"],"version":version}),
        )
    }
    fn native_map(body: &Value) -> Value {
        Value::Object(
            body["models"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| (string(&m["slug"]).to_owned(), m.clone()))
                .collect(),
        )
    }
    pub async fn read_reasoning(&self, id: i64, key: &str) -> Result<Value> {
        let group = self.group(id).await?;
        let saved = self.store.group(id)?;
        let mut entries = saved["reasoning"]
            .as_object()
            .cloned()
            .map(Value::Object)
            .unwrap_or(json!({}));
        for (model, fields) in saved["overrides"].as_object().into_iter().flatten() {
            let (efforts, default) = reasoning_values(fields);
            if entries.get(model).is_none() && !efforts.is_empty() && !default.is_empty() {
                entries[model] = json!({"efforts":efforts,"default_effort":default,"mode":"draft","source":"legacy"});
            }
        }
        let mut items = vec![];
        if truthy(&entries) {
            let (body, _) = self.baseline(&group).await?;
            let native = Self::native_map(&body);
            let accounts = self.accounts(id).await?;
            let routes = self.routes(&group).await?;
            let version = self.native_version(key).await;
            for (model, entry) in entries.as_object().unwrap() {
                items.push(Self::reasoning_item(
                    model, entry, &group, &native, &accounts, &routes, &version,
                )?);
            }
        }
        Ok(
            json!({"group":Self::summary(&group),"revision":saved["revision"],"items":items,"status":self.status()}),
        )
    }
    pub async fn resolve_reasoning(&self, id: i64, key: &str, payload: &Value) -> Result<Value> {
        let model = model_id(&payload["model"])?;
        let group = self.group(id).await?;
        let saved = self.store.group(id)?;
        let (body, _) = self.baseline(&group).await?;
        let native = body["models"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|m| m["slug"] == model)
            .cloned()
            .unwrap_or(Value::Null);
        let accounts = self.accounts(id).await?;
        let routes = self.routes(&group).await?;
        let (binding, available) = routing_binding(&group, model, &accounts, &routes);
        let mut descriptor = native.clone();
        let mut imported = json!({});
        let mut source = "manual".to_owned();
        let destinations: std::collections::BTreeSet<_> = available
            .iter()
            .map(|(a, m)| (string(&a["platform"]), m.as_str()))
            .collect();
        if destinations.len() == 1 {
            let deadline = Instant::now() + Duration::from_secs(8);
            for (account, target) in available.iter().take(3) {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let Ok(data) = self
                    .source_catalog(account, remaining.min(Duration::from_secs(3)))
                    .await
                else {
                    continue;
                };
                let entries = if data.is_array() {
                    &data
                } else {
                    data.get("models").unwrap_or(&data["data"])
                };
                let Some(found) = entries.as_array().into_iter().flatten().find(|m| {
                    m.is_object()
                        && m.get("slug")
                            .or_else(|| m.get("id"))
                            .unwrap_or(&m["modelId"])
                            == target
                }) else {
                    continue;
                };
                imported = import_fields(found);
                if complete_descriptor(found, target) {
                    let mut actual = found.clone();
                    actual["slug"] = model.into();
                    let fields = Value::Object(
                        actual
                            .as_object()
                            .unwrap()
                            .iter()
                            .filter(|(k, _)| !matches!(k.as_str(), "id" | "slug"))
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                    );
                    if validate_overrides(&json!({model:fields}), &Value::Null).is_err() {
                        continue;
                    }
                    descriptor = if truthy(&native) {
                        native.clone()
                    } else {
                        actual
                    };
                }
                if !reasoning_values(&imported).0.is_empty() {
                    source = "upstream".into();
                    break;
                }
            }
        }
        let existing = &saved["reasoning"][model];
        let (mut efforts, mut default) = reasoning_values(&imported);
        if efforts.is_empty() {
            if truthy(existing) {
                efforts = existing["efforts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| string(v).into())
                    .collect();
                default = string(&existing["default_effort"]).into();
                source = existing
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("manual")
                    .into();
            } else if truthy(&native) {
                (efforts, default) = reasoning_values(&native);
                source = "native".into();
            }
        }
        if !payload["efforts"].is_null() {
            let suggested = json!({"supported_reasoning_levels":efforts.iter().map(|v|json!({"effort":v})).collect::<Vec<_>>(),"default_reasoning_level":default});
            let fields = reasoning_fields(&payload["efforts"], &payload["default_effort"])?;
            (efforts, default) = reasoning_values(&fields);
            if !same_reasoning(&suggested, &fields) {
                source = "manual".into();
            }
        }
        let checked = forwarding(
            &group,
            model,
            &efforts,
            &available,
            &self.native_version(key).await,
        );
        let (native_efforts, native_default) = reasoning_values(&native);
        Ok(
            json!({"group":Self::summary(&group),"revision":saved["revision"],"model":model,"binding":binding,"efforts":efforts,"default_effort":default,"source":source,"needs_allowlist":!admitted(&group["model_allowlist"],model),"native_efforts":native_efforts,"native_default":native_default,"descriptor_available":complete_descriptor(&descriptor,model),"forwarding":checked,"_descriptor":descriptor}),
        )
    }
    fn group_lock(&self, id: i64) -> Arc<tokio::sync::Mutex<()>> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .group_locks
            .entry(id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
    pub async fn save_reasoning(&self, id: i64, key: &str, payload: &Value) -> Result<Value> {
        if self.read_only {
            return Err(Error::read_only());
        }
        let model = model_id(&payload["model"])?;
        let fields = reasoning_fields(&payload["efforts"], &payload["default_effort"])?;
        let lock = self.group_lock(id);
        let _guard = lock.lock().await;
        let mut group = self.group(id).await?;
        let saved = self.store.group(id)?;
        if group["version"] != payload["expected_version"]
            || saved["revision"] != payload["expected_revision"]
        {
            return Err(Error::new(409, "配置已变更，请刷新后重新核对，草稿已保留"));
        }
        let resolved = self.resolve_reasoning(id, key, payload).await?;
        if resolved["binding"] != payload["expected_binding"] {
            return Err(Error::new(409, "账号路由已变化，请重新核对，草稿已保留"));
        }
        let native_fields = if truthy(&resolved["native_default"]) {
            reasoning_fields(&resolved["native_efforts"], &resolved["native_default"])?
        } else {
            json!({})
        };
        if same_reasoning(&native_fields, &fields)
            && !truthy(&resolved["needs_allowlist"])
            && resolved["forwarding"]["state"] == "verified"
        {
            if saved["reasoning"].get(model).is_some()
                || FIELDS
                    .iter()
                    .any(|k| saved["overrides"][model].get(*k).is_some())
            {
                self.store
                    .save_reasoning(id, model, None, string(&saved["revision"]))?;
            }
            return Ok(
                json!({"outcome":"native","message":"原生已支持，无需补全","state":self.read_reasoning(id,key).await?}),
            );
        }
        let active = resolved["forwarding"]["state"] == "verified"
            && truthy(&resolved["descriptor_available"]);
        if active && truthy(&resolved["needs_allowlist"]) && !truthy(&payload["confirm_allowlist"])
        {
            return Err(Error::new(409, "请确认仅将该模型追加到分组白名单"));
        }
        if self.group(id).await?["version"] != group["version"] {
            return Err(Error::new(409, "分组已变更，请刷新后重新核对"));
        }
        let mut appended = false;
        if active && truthy(&resolved["needs_allowlist"]) {
            let mut whitelist = group["model_allowlist"].clone();
            whitelist["models"]
                .as_array_mut()
                .ok_or_else(Error::invalid)?
                .push(model.into());
            self.admin(
                key,
                reqwest::Method::PUT,
                &format!("/groups/{id}"),
                Some(&json!({"model_allowlist":whitelist})),
            )
            .await?;
            group = self.group(id).await?;
            if group["model_allowlist"] != whitelist {
                return Err(Error::new(
                    409,
                    "白名单结果未确认；补全未保存，请刷新核对，不要自动重试",
                ));
            }
            appended = true;
        }
        let commit: Result<()> = async {
            let (binding,_) = routing_binding(&group,model,&self.accounts(id).await?,&self.routes(&group).await?);
            if resolved["binding"] != binding || self.group(id).await?["version"] != group["version"] {return Err(Error::new(409,"路由或分组已变更"));}
            let entry = json!({"efforts":resolved["efforts"],"default_effort":resolved["default_effort"],"binding":binding,"source":resolved["source"],"mode":if active {"active"} else {"draft"},"descriptor":resolved["_descriptor"],"updated_at":now()});
            self.store.save_reasoning(id,model,Some(entry),string(&saved["revision"]))?; Ok(())
        }.await;
        if let Err(error) = commit {
            if !appended {
                return Err(error);
            }
            return Ok(
                json!({"outcome":"partial","message":"白名单已添加，补全未保存；请重新核对后保存，未自动重试","state":self.read_reasoning(id,key).await?}),
            );
        }
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.responses.clear();
            cache.baselines.retain(|(gid, _, _), _| *gid != id);
        }
        Ok(
            json!({"outcome":if active {"saved"} else {"draft"},"message":if active {"目录已补全"} else {"已保存草稿，未对外发布"},"state":self.read_reasoning(id,key).await?}),
        )
    }
    pub async fn remove_reasoning(&self, id: i64, key: &str, payload: &Value) -> Result<Value> {
        if self.read_only {
            return Err(Error::read_only());
        }
        let model = model_id(&payload["model"])?;
        let lock = self.group_lock(id);
        let _guard = lock.lock().await;
        if self.group(id).await?["version"] != payload["expected_version"] {
            return Err(Error::new(409, "分组已变更，请刷新后重试"));
        }
        self.store
            .save_reasoning(id, model, None, string(&payload["expected_revision"]))?;
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .responses
            .clear();
        self.read_reasoning(id, key).await
    }
}
