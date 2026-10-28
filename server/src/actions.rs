use crate::{
    account_locks::{AccountLocks, Lease},
    account_templates::{self, Templates},
    audit,
    compat::{account_version, integer, now, operation_versions, string, truthy},
    credit_ownership,
    db::Param,
    error::{Error, Result},
    model_candidates::{Candidates, Failure},
    model_rules::Allowlist,
    queries::{settings_capabilities, Queries},
    security::{clean, safe_error},
    settings::Settings,
    upstream::{read_limited, Upstream},
};
use base64::Engine;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinHandle};

fn strict_id(value: &Value) -> Option<i64> {
    if let Some(id) = value.as_i64() {
        return (id > 0).then_some(id);
    }
    let value = value.as_str()?.trim();
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id = value.parse::<i64>().ok()?;
    (id > 0 && id.to_string() == value).then_some(id)
}

pub fn schedulable_response_matches(body: &Value, id: i64, schedulable: bool) -> bool {
    if !body.is_object()
        || !(body["code"] == 0 || body["code"] == "0")
        || !body["data"].is_object()
        || body["data"].get("schedulable").is_none()
    {
        return false;
    }
    let (observed, ok) = crate::settings::strict_bool(&body["data"]["schedulable"], false);
    ok && observed == schedulable
        && body["data"]
            .get("id")
            .is_none_or(|v| strict_id(v) == Some(id))
}

pub async fn set_schedulable(
    actions: &Actions,
    id: i64,
    enabled: bool,
    key: &str,
    seconds: u64,
) -> Value {
    if actions.settings.read_only {
        return json!({"success":false,"error_code":"read_only"});
    }
    if key.trim().is_empty() || actions.upstream.active_url().is_empty() {
        return json!({"success":false,"error_code":"missing_sub2api_admin_credentials","error":"缺少 Sub2API 地址或 Admin API Key"});
    }
    match actions
        .request(
            reqwest::Method::POST,
            &format!("accounts/{id}/schedulable"),
            key,
            Some(&json!({"schedulable":enabled})),
            Duration::from_secs(seconds.clamp(2, 60)),
            false,
        )
        .await
    {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = match read_limited(response, 2_000_000).await {
                Ok(raw) => serde_json::from_slice(&raw).unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            if (200..300).contains(&status) && schedulable_response_matches(&body, id, enabled) {
                json!({"success":true,"status_code":status,"data":body["data"]})
            } else {
                json!({"success":false,"status_code":status,"error_code":if (200..300).contains(&status) {"invalid_schedulable_response".into()} else {format!("http_{status}")}})
            }
        }
        Err(error) => {
            json!({"success":false,"error_code":if error.status.as_u16()==504 {"timeout"} else {"network_error"}})
        }
    }
}

pub const EDIT_ACTIONS: &[&str] = &[
    "priority",
    "concurrency",
    "load_factor",
    "model_config",
    "proxy",
    "sync_models",
];
pub const TEST_MODES: &[&str] = &[
    "default", "compact", "text", "image", "video", "search", "tts", "stt", "realtime",
];
pub struct OperationContext {
    pub action: String,
    pub expected: String,
}
tokio::task_local! {pub static OPERATION: OperationContext;}
pub fn version_matches(row: &Value, expected: &str) -> bool {
    OPERATION
        .try_with(|context| operation_versions(row)[&context.action] == context.expected)
        .unwrap_or_else(|_| account_version(row) == expected)
}
#[derive(Clone)]
pub struct Actions {
    pub queries: Queries,
    pub upstream: Upstream,
    pub locks: AccountLocks,
    pub priorities: crate::priority_orders::PriorityGate,
    pub templates: Templates,
    pub settings: Arc<Settings>,
    pub supervisor: crate::supervision::Supervisor,
    pub(crate) candidates: Candidates,
    client: reqwest::Client,
    #[cfg(any(test, feature = "test-fixtures"))]
    pub(crate) directory_endpoint: Arc<std::sync::Mutex<Option<String>>>,
}
pub struct TestRun {
    receiver: mpsc::Receiver<Value>,
    task: JoinHandle<()>,
}
impl TestRun {
    pub async fn recv(&mut self) -> Option<Value> {
        self.receiver.recv().await
    }
}
impl Drop for TestRun {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn current_value(row: &Value, action: &str) -> Result<Value> {
    match action {
        "model_config" => account_templates::split(&row["model_mapping"]),
        "proxy" => Ok(json!({"proxy_id":row["proxy_id"]})),
        "sync_models" => Ok(json!({})),
        _ => Ok(json!({action:row[action]})),
    }
}
pub fn target_reached(row: &Value, action: &str, payload: &Value) -> bool {
    if action == "sync_models" || settings_capabilities(row)[action]["editable"] != true {
        return false;
    }
    if action == "model_config" {
        return account_templates::combine(payload).is_ok_and(|v| {
            v == if row["model_mapping"].is_null() {
                json!({})
            } else {
                row["model_mapping"].clone()
            }
        });
    }
    let field = if action == "proxy" {
        "proxy_id"
    } else {
        action
    };
    row[field] == payload[field]
}
pub fn text_model(id: &str) -> bool {
    account_templates::MODEL_ID.is_match(id)
        && ![
            "image",
            "audio",
            "video",
            "tts",
            "whisper",
            "realtime",
            "search",
            "embedding",
        ]
        .iter()
        .any(|s| id.to_lowercase().contains(s))
}
pub fn exact_models(allowlists: &[Value]) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    allowlists
        .iter()
        .flat_map(|v| v["models"].as_array().into_iter().flatten())
        .filter_map(|v| {
            let id = v.as_str()?.trim();
            if !text_model(id) || !seen.insert(id.to_owned()) {
                return None;
            }
            Some(json!({"id":id,"display_name":id,"type":"model"}))
        })
        .collect()
}
pub fn filter_models(items: &[Value], allowlists: &[Value]) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    let unrestricted = allowlists.is_empty() || allowlists.iter().any(|v| !truthy(&v["enabled"]));
    let allowlists: Vec<_> = if unrestricted {
        vec![]
    } else {
        allowlists.iter().map(Allowlist::new).collect()
    };
    items.iter().filter_map(|item| {
        item["id"].as_str()?;let id=clean(&item["id"],200).trim().to_owned();if id.is_empty() || !seen.insert(id.clone()) {return None;}
        if !unrestricted && !allowlists.iter().any(|v|v.admits(&id)) {return None;}
        Some(json!({"id":clean(&item["id"],200),"display_name":clean(&item["display_name"],200),"type":clean(&item["type"],200)}))
    }).collect()
}
pub fn validate_media(value: &str, kind: &str) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    let (header, encoded) = value
        .split_once(',')
        .ok_or_else(|| Error::new(422, "素材必须是有效的图片或音频文件"))?;
    if !header.starts_with(&format!("data:{kind}/")) || !header.contains(";base64") {
        return Err(Error::new(422, "素材必须是有效的图片或音频文件"));
    }
    let mime = header[5..].split(';').next().unwrap_or("").to_lowercase();
    if kind == "image"
        && !["image/png", "image/jpeg", "image/webp", "image/gif"].contains(&mime.as_str())
    {
        return Err(Error::new(422, "图片格式仅支持 PNG、JPEG、WebP 或 GIF"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| Error::new(422, "素材编码无效"))?;
    if bytes.len()
        > if kind == "image" {
            4 * 1024 * 1024
        } else {
            8 * 1024 * 1024
        }
    {
        return Err(Error::new(422, "素材超过大小限制"));
    }
    Ok(())
}
impl Actions {
    pub fn new(
        queries: Queries,
        upstream: Upstream,
        locks: AccountLocks,
        templates: Templates,
        settings: Arc<Settings>,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        let supervisor = crate::supervision::Supervisor::default();
        Ok(Self {
            queries,
            upstream,
            locks,
            templates,
            priorities: crate::priority_orders::PriorityGate::new(&settings),
            candidates: Candidates::new(supervisor.clone()).persisted(&settings)?,
            settings,
            supervisor,
            client,
            #[cfg(any(test, feature = "test-fixtures"))]
            directory_endpoint: Default::default(),
        })
    }
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn set_model_directory_endpoint(&self, endpoint: &str) -> Result<()> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| Error::invalid())?;
        if url.scheme() != "http"
            || !matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::invalid());
        }
        *self
            .directory_endpoint
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(endpoint.into());
        Ok(())
    }
    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        key: &str,
        body: Option<&Value>,
        timeout: Duration,
        stream: bool,
    ) -> Result<reqwest::Response> {
        if self.supervisor.is_stopping() {
            return Err(Error::new(503, "服务正在停止"));
        }
        if self.settings.read_only && method != reqwest::Method::GET {
            return Err(Error::read_only());
        }
        let base = self.upstream.active_url();
        if base.is_empty() {
            return Err(Error::new(502, "未配置 Sub2API 服务地址"));
        }
        let mut request = self
            .client
            .request(
                method,
                format!("{base}/api/v1/admin/{}", path.trim_start_matches('/')),
            )
            .header("x-api-key", key)
            .header("X-Admin-UI-Request", "1")
            .header(
                "Accept",
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .timeout(timeout);
        if let Some(value) = body {
            request = request.json(value);
        }
        request.send().await.map_err(|error| {
            if path.starts_with("proxies/")
                && (path.ends_with("/test") || path.ends_with("/quality-check"))
            {
                return if error.is_timeout() {
                    Error::new(504, "代理检测超时，未重放请求")
                } else {
                    Error::new(502, "代理检测请求失败")
                };
            }
            if error.is_timeout() {
                Error::new(504, "请求超时，未重放操作")
            } else {
                Error::new(502, "无法读取 Sub2API 响应")
            }
        })
    }
    pub async fn json(
        &self,
        method: reqwest::Method,
        path: &str,
        key: &str,
        body: Option<&Value>,
        seconds: u64,
    ) -> Result<Value> {
        let response = self
            .request(method, path, key, body, Duration::from_secs(seconds), false)
            .await?;
        if !response.status().is_success() {
            return Err(Error::new(
                502,
                format!("Sub2API 请求失败 [http_{}]", response.status().as_u16()),
            ));
        }
        let bytes = read_limited(response, 8_000_000).await?;
        let body: Value =
            serde_json::from_slice(&bytes).map_err(|_| Error::new(502, "无法读取 Sub2API 响应"))?;
        if !body.is_object() || body["code"] != 0 {
            return Err(Error::new(502, "Sub2API 未确认操作成功"));
        }
        Ok(body["data"].clone())
    }
    pub async fn account(&self, id: i64, version: Option<&str>) -> Result<Value> {
        let row = self.queries.account(id).await?;
        if version.is_some_and(|v| !version_matches(&row, v)) {
            return Err(Error::new(409, "账号已变化，请刷新后重试"));
        }
        Ok(row)
    }
    pub async fn accounts(&self, ids: &[i64]) -> Result<Vec<Value>> {
        static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            crate::sql::DESKTOP_ACCOUNT_SQL.replace("{filter}", "AND a.id=ANY($1)")
        });
        self.queries
            .db
            .all(&SQL, &[Param::Ints(ids.to_vec())])
            .await
    }
    pub async fn settings_view(&self, id: i64) -> Result<Value> {
        if id < 1 {
            return Err(Error::new(422, "账号编号无效"));
        }
        let row = self.account(id, None).await?;
        Ok(
            json!({"account_id":id,"priority":integer(&row["priority"]).unwrap_or(0),"concurrency":row["concurrency"],"load_factor":row["load_factor"],"effective_load_factor":if truthy(&row["load_factor"]) {row["load_factor"].clone()} else if truthy(&row["concurrency"]) {row["concurrency"].clone()} else {json!(1)},"proxy_id":row["proxy_id"],"model_config":account_templates::split(&row["model_mapping"])? ,"capabilities":settings_capabilities(&row),"versions":crate::compat::pick(&operation_versions(&row),EDIT_ACTIONS)}),
        )
    }
    pub async fn proxies(&self, key: &str) -> Result<Vec<Value>> {
        let data = self
            .json(reqwest::Method::GET, "proxies/all", key, None, 10)
            .await?;
        let items = data
            .as_array()
            .filter(|v| v.len() <= 10000)
            .ok_or_else(|| Error::new(502, "代理列表格式无效"))?;
        items.iter().map(|item| {
            if !item.is_object() || item["id"].as_i64().is_none_or(|v| v <= 0) {return Err(Error::new(502,"代理列表格式无效"));}
            let protocol=string(&item["protocol"]);let status=string(&item["status"]);
            Ok(json!({"id":item["id"],"name":clean(&item["name"],160),"protocol":if ["http","https","socks5","socks5h"].contains(&protocol) {protocol} else {"unknown"},"status":if ["active","inactive","error"].contains(&status) {status} else {"unknown"}}))
        }).collect()
    }
    pub async fn apply_setting(
        &self,
        id: i64,
        action: &str,
        payload: &Value,
        expected: &str,
        key: &str,
    ) -> Result<Value> {
        if self.settings.read_only {
            return Err(Error::read_only());
        }
        let _priority = if action == "priority" {
            Some(self.priorities.enter().await?)
        } else {
            None
        };
        let mut row = self.account(id, None).await?;
        let original_parent = row["parent_account_id"].clone();
        if !truthy(&row["parent_account_id"]) {
            row["parent_account_id"] = row["extra"]["parent_account_id"].clone();
        }
        let _lease = self.locks.try_account(&row)?;
        let live = self.account(id, None).await?;
        if live["parent_account_id"] != original_parent
            || operation_versions(&live)[action] != expected
        {
            return Err(Error::new(
                409,
                "相关字段或账号资格已变化，请核对后重新提交",
            ));
        }
        let capability = settings_capabilities(&live)[action].clone();
        if capability["editable"] != true {
            return Err(Error::new(409, capability["reason"].clone()));
        }
        if action == "proxy"
            && !payload["proxy_id"].is_null()
            && !self
                .proxies(key)
                .await?
                .iter()
                .any(|v| v["id"] == payload["proxy_id"] && v["status"] == "active")
        {
            return Err(Error::new(409, "所选代理已不可用，请重新选择"));
        }
        if operation_versions(&self.account(id, None).await?)[action] != expected {
            return Err(Error::new(409, "相关字段已变化，请核对后重新提交"));
        }
        let scalar = matches!(action, "priority" | "concurrency" | "load_factor");
        if scalar {
            credit_ownership::prepare(
                &self.queries.oauth,
                &credit_ownership::account(&self.queries.db, id).await?,
                action,
                &payload[action],
            )?;
        }
        if action == "priority" {
            if let Err(error) = self
                .priorities
                .before_write(&self.queries.db, id, expected, &payload[action])
                .await
            {
                credit_ownership::reconcile(
                    &self.queries.oauth,
                    &credit_ownership::account(&self.queries.db, id).await?,
                )?;
                return Err(error);
            }
        }
        let result = if action == "sync_models" {
            let data = self
                .json(
                    reqwest::Method::POST,
                    &format!("accounts/{id}/models/sync-upstream"),
                    key,
                    None,
                    60,
                )
                .await?;
            let list = data["models"]
                .as_array()
                .filter(|v| v.len() <= 10000)
                .ok_or_else(|| Error::new(502, "上游模型同步响应格式无效"))?;
            let mut seen = BTreeSet::new();
            let models: Vec<_> = list
                .iter()
                .filter_map(Value::as_str)
                .filter(|id| {
                    account_templates::MODEL_ID.is_match(id) && seen.insert((*id).to_owned())
                })
                .collect();
            if models.is_empty() {
                return Err(Error::new(422, "上游未返回可用模型，已有草稿已保留"));
            }
            json!({"models":models,"warnings":if truthy(&data["warnings"]) {vec!["模型能力信息不完整"]} else {vec![]},"synced_at":now()})
        } else {
            let (method, path, body) = if action == "model_config" {
                (
                    reqwest::Method::POST,
                    "accounts/bulk-update".to_owned(),
                    json!({"account_ids":[id],"credentials":{"model_mapping":account_templates::combine(payload)?}}),
                )
            } else {
                let field = if action == "proxy" {
                    "proxy_id"
                } else {
                    action
                };
                (
                    reqwest::Method::PUT,
                    format!("accounts/{id}"),
                    json!({field:if action == "proxy" && payload[field].is_null() {json!(0)} else {payload[field].clone()}}),
                )
            };
            if let Err(error) = self.json(method, &path, key, Some(&body), 10).await {
                if !matches!(error.status.as_u16(), 502 | 504)
                    || !target_reached(&self.account(id, None).await?, action, payload)
                {
                    return Err(error);
                }
            }
            let actual = self.account(id, None).await?;
            if !target_reached(&actual, action, payload) {
                return Err(Error::new(502, "字段写入尚未确认，正在核对实际值"));
            }
            if scalar {
                credit_ownership::reconcile(
                    &self.queries.oauth,
                    &credit_ownership::account(&self.queries.db, id).await?,
                )?;
            }
            if action == "priority" {
                self.priorities.after_write(id, &actual)?;
            }
            let mut result = current_value(&actual, action)?;
            result["verified"] = true.into();
            let versions = operation_versions(&actual);
            result["field_version"] = versions[action].clone();
            result["settings_versions"] = crate::compat::pick(&versions, EDIT_ACTIONS);
            result
        };
        audit::write(
            &self.settings.audit_path,
            "desktop_account_settings",
            json!({"account_id":id,"action":action,"result":"completed"}),
            false,
        );
        Ok(result)
    }
    pub async fn allowlists(&self, row: &Value, model_test: bool) -> Result<Vec<Value>> {
        let ids: Vec<_> = row["group_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(integer)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let query = if model_test {
            "SELECT model_allowlist FROM groups WHERE deleted_at IS NULL AND id=ANY($1) AND platform=$2 ORDER BY sort_order,id"
        } else {
            "SELECT model_allowlist FROM groups WHERE deleted_at IS NULL AND id=ANY($1) ORDER BY id"
        };
        let mut params = vec![Param::Ints(ids)];
        if model_test {
            params.push(string(&row["platform"]).into());
        }
        Ok(self
            .queries
            .db
            .all(query, &params)
            .await?
            .into_iter()
            .map(|v| {
                if truthy(&v["model_allowlist"]) {
                    v["model_allowlist"].clone()
                } else {
                    json!({"enabled":false,"models":[]})
                }
            })
            .collect())
    }
    pub async fn models(&self, id: i64, key: &str, model_test: bool) -> Result<Vec<Value>> {
        if model_test {
            return self.model_test_candidates(id, key, false).await;
        }
        let row = self.account(id, None).await?;
        let allowlists = self.allowlists(&row, false).await?;
        let data = self
            .json(
                reqwest::Method::GET,
                &format!("accounts/{id}/models"),
                key,
                None,
                30,
            )
            .await?;
        let list = data
            .as_array()
            .ok_or_else(|| Error::new(502, "模型列表格式无效"))?;
        Ok(filter_models(&list[..list.len().min(1000)], &allowlists))
    }
    pub(crate) async fn candidate_source(&self, id: i64, key: &str) -> Result<(String, String)> {
        let row = self.account(id, None).await?;
        let parent = integer(&row["parent_account_id"])
            .or_else(|| integer(&row["extra"]["parent_account_id"]))
            .filter(|id| *id > 0);
        // Parent metadata invalidates inherited credentials; its directory and
        // group allowlist are never used to add candidate models.
        let mut parent_version = if let Some(parent) = parent {
            self.queries.db.one("SELECT id,platform,type,proxy_id,nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id,md5(credentials::text) AS credential_version,extra FROM accounts WHERE id=$1 AND deleted_at IS NULL", &[parent.into()]).await?
        } else {
            None
        };
        let protocol_fields = [
            "parent_account_id",
            "openai_responses_mode",
            "openai_responses_supported",
            "anthropic_apikey_auth_scheme",
            "openai_passthrough",
            "openai_oauth_passthrough",
        ];
        if let Some(parent) = &mut parent_version {
            parent["extra"] = crate::compat::pick(&parent["extra"], &protocol_fields);
        }
        let proxy_id = integer(&row["proxy_id"]).filter(|id| *id > 0).or_else(|| {
            parent_version
                .as_ref()
                .and_then(|v| integer(&v["proxy_id"]).filter(|id| *id > 0))
        });
        let proxy_version = if let Some(proxy) = proxy_id {
            self.queries.db.one("SELECT md5(row(protocol,host,port,username,password)::text) AS version FROM proxies WHERE id=$1 AND deleted_at IS NULL", &[proxy.into()]).await?
        } else {
            None
        };
        let client_identity = if row["type"] == "oauth" {
            self.queries.db.all("SELECT key,value FROM settings WHERE key IN ('openai_codex_user_agent','openai_codex_client_version','openai_codex_client_version_synced') ORDER BY key", &[]).await?
        } else {
            vec![]
        };
        let base = self.upstream.active_url();
        if base.is_empty() {
            return Err(Error::new(502, "未配置 Sub2API 服务地址"));
        }
        let identity = crate::compat::digest(&json!([
            base,
            crate::compat::sha256(key),
            crate::compat::pick(
                &row,
                &[
                    "id",
                    "platform",
                    "type",
                    "parent_account_id",
                    "credential_version",
                    "proxy_id",
                    "model_mapping_version",
                    "model_catalog_version",
                    "group_ids"
                ]
            ),
            crate::compat::pick(&row["extra"], &protocol_fields),
            parent_version,
            proxy_version,
            client_identity
        ]));
        Ok((identity, base))
    }
    pub(crate) async fn model_test_candidates(
        &self,
        id: i64,
        key: &str,
        refresh: bool,
    ) -> Result<Vec<Value>> {
        if key.is_empty() {
            return Err(Error::new(503, "管理员授权不可用，无法核对模型目录"));
        }
        let (identity, _) = self.candidate_source(id, key).await?;
        let actions = self.clone();
        let expected = identity.clone();
        let key = key.to_owned();
        let fetch = async move {
            let values = crate::model_directory::fetch(&actions, id).await?;
            let (current, _) = actions
                .candidate_source(id, &key)
                .await
                .map_err(Failure::permanent)?;
            if current != expected {
                return Err(Failure::permanent(Error::new(
                    503,
                    "账号或上游连接已变化，请重新读取模型目录",
                )));
            }
            Ok(values)
        };
        self.candidates.get(identity, refresh, fetch).await
    }
    pub async fn model_allowed(&self, row: &Value, model: &str) -> Result<bool> {
        if !text_model(model) {
            return Ok(false);
        }
        let key = self.queries.oauth.admin_token()?;
        if key.is_empty() {
            return Err(Error::new(503, "管理员授权不可用，无法核对模型目录"));
        }
        Ok(self
            .model_test_candidates(integer(&row["id"]).ok_or_else(Error::invalid)?, &key, false)
            .await?
            .iter()
            .any(|v| v["id"] == model))
    }
    pub async fn apply_template(&self, id: i64, payload: &Value, key: &str) -> Result<Value> {
        if self.settings.read_only {
            return Err(Error::read_only());
        }
        let _lease = self.locks.try_account(&self.account(id, None).await?)?;
        let row = self.account(id, None).await?;
        if !account_templates::eligible(&row) {
            return Err(Error::new(409, "仅可应用到独立 OpenAI OAuth 或 Key 账号"));
        }
        if account_templates::passthrough(&row) {
            return Err(Error::new(409, "透传模式会绕过模型限制，请先处理透传设置"));
        }
        let mapping = self.templates.desired(payload)?;
        if row["model_mapping"] == mapping {
            return Ok(json!({"verified":true,"account_id":id,"unchanged":true}));
        }
        if account_templates::account_version(&row) != payload["expected_version"] {
            return Err(Error::new(409, "账号模型配置或凭据已变化，请重新选择模板"));
        }
        let result = self
            .json(
                reqwest::Method::POST,
                "accounts/bulk-update",
                key,
                Some(&json!({"account_ids":[id],"credentials":{"model_mapping":mapping}})),
                5,
            )
            .await;
        let live = self.account(id, None).await?;
        let verified = account_templates::eligible(&live)
            && !account_templates::passthrough(&live)
            && live["model_mapping"] == mapping;
        audit::write(
            &self.settings.audit_path,
            "account_template_applied",
            json!({"account_id":id,"template_id":payload["template_id"],"verified":verified,"code":if result.is_ok() {"ok"} else {"uncertain"}}),
            false,
        );
        if !verified {
            return Err(Error::new(
                502,
                json!({"code":"template_unconfirmed","message":"模板应用未确认，请核对实际配置"}),
            ));
        }
        Ok(json!({"verified":true,"account_id":id,"template_id":payload["template_id"]}))
    }
    pub async fn set_groups(&self, id: i64, payload: &Value, key: &str) -> Result<Value> {
        if self.settings.read_only {
            return Err(Error::read_only());
        }
        let scope: BTreeSet<_> = payload["scope_group_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(integer)
            .collect();
        let target: BTreeSet<_> = payload["group_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(integer)
            .collect();
        if id < 1 || scope.iter().chain(&target).any(|v| *v < 1) || !target.is_subset(&scope) {
            return Err(Error::new(422, "分组范围无效"));
        }
        let _lease = self.locks.try_account(&self.account(id, None).await?)?;
        let row = self
            .account(id, Some(string(&payload["expected_version"])))
            .await?;
        let groups = self
            .queries
            .db
            .all(
                "SELECT id,platform FROM groups WHERE deleted_at IS NULL AND id=ANY($1)",
                &[Param::Ints(scope.iter().copied().collect())],
            )
            .await?;
        if groups
            .iter()
            .filter_map(|v| integer(&v["id"]))
            .collect::<BTreeSet<_>>()
            != scope
            || groups.iter().any(|v| v["platform"] != row["platform"])
        {
            return Err(Error::new(409, "分组已变化或平台不匹配，请刷新"));
        }
        let original: BTreeSet<_> = row["group_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(integer)
            .collect();
        let desired: Vec<_> = original
            .difference(&scope)
            .copied()
            .chain(target)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let failure = self
            .json(
                reqwest::Method::PUT,
                &format!("accounts/{id}"),
                key,
                Some(&json!({"group_ids":desired})),
                10,
            )
            .await
            .err();
        let live = self.account(id, None).await?;
        let mut actual = live["group_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(integer)
            .collect::<Vec<_>>();
        actual.sort_unstable();
        if actual != desired {
            return Err(
                failure.unwrap_or_else(|| Error::new(502, "分组写入未确认，请刷新查看实际归属"))
            );
        }
        audit::write(
            &self.settings.audit_path,
            "desktop_groups",
            json!({"account_id":id,"before":original,"after":desired}),
            false,
        );
        Ok(
            json!({"verified":true,"account_id":id,"group_ids":desired,"version":account_version(&live)}),
        )
    }
    pub async fn prepare_test(&self, id: i64, payload: &Value) -> Result<Lease> {
        if self.settings.read_only {
            return Err(Error::read_only());
        }
        let row = self
            .account(id, Some(string(&payload["expected_version"])))
            .await?;
        let lease = self.locks.try_account(&row)?;
        let row = self
            .account(id, Some(string(&payload["expected_version"])))
            .await?;
        let platform = string(&row["platform"]);
        let mode = string(&payload["mode"]);
        if platform != "openai" && platform != "grok" && mode != "default" {
            return Err(Error::new(422, "此平台仅支持普通测试连接"));
        }
        if platform == "openai" && !matches!(mode, "default" | "compact") {
            return Err(Error::new(422, "OpenAI 测试模式无效"));
        }
        if platform == "grok"
            && !matches!(
                mode,
                "text" | "image" | "video" | "search" | "tts" | "stt" | "realtime"
            )
        {
            return Err(Error::new(422, "Grok 测试模式无效"));
        }
        let image = string(&payload["image_data_url"]);
        let audio = string(&payload["audio_data_url"]);
        validate_media(image, "image")?;
        validate_media(audio, "audio")?;
        if !(image.is_empty()
            || platform == "openai"
                && mode == "default"
                && string(&payload["model_id"]).starts_with("gpt-image-")
            || platform == "grok" && matches!(mode, "image" | "video"))
        {
            return Err(Error::new(422, "当前模式不接受图片素材"));
        }
        if !(audio.is_empty() || platform == "grok" && mode == "stt") {
            return Err(Error::new(422, "当前模式不接受音频素材"));
        }
        Ok(lease)
    }
    pub fn test_stream(
        &self,
        id: i64,
        payload: Value,
        key: String,
        lease: Lease,
        summary: bool,
    ) -> TestRun {
        let (sender, receiver) = mpsc::channel(16);
        let actions = self.clone();
        let task = self.supervisor.spawn("connection_test", async move {
            let _lease = lease;
            let started = Instant::now();
            let run = actions.consume_test(id, &payload, &key, &sender, summary, started);
            let result = tokio::time::timeout(Duration::from_secs(300), run).await;
            let message = match result {
                Ok(Ok(())) => return,
                Ok(Err(error)) if error.status.as_u16() == 504 => "测试超时，未重放请求",
                Ok(Err(_)) => "测试连接中断，未重放请求",
                Err(_) => "测试超时，未重放请求",
            };
            let mut event = json!({"type":"error","error":message});
            if summary {
                event["uncertain"] = true.into();
            }
            let _ = sender.send(event).await;
        });
        TestRun { receiver, task }
    }
    async fn consume_test(
        &self,
        id: i64,
        payload: &Value,
        key: &str,
        sender: &mpsc::Sender<Value>,
        summary: bool,
        started: Instant,
    ) -> Result<()> {
        let mut body = crate::compat::pick(
            payload,
            &[
                "model_id",
                "mode",
                "prompt",
                "image_data_url",
                "audio_data_url",
            ],
        );
        for field in ["model_id", "prompt", "image_data_url", "audio_data_url"] {
            if body[field].is_null() {
                body[field] = "".into();
            }
        }
        if matches!(string(&body["mode"]), "search" | "tts" | "stt" | "realtime") {
            body["model_id"] = "".into();
        }
        let mut response = self
            .request(
                reqwest::Method::POST,
                &format!("accounts/{id}/test"),
                key,
                Some(&body),
                Duration::from_secs(90),
                true,
            )
            .await?;
        if response.status().as_u16() != 200 {
            let mut event = json!({"type":"error","error":format!("测试请求失败 [http_{}]",response.status().as_u16())});
            if summary {
                event["error_code"] = format!("http_{}", response.status().as_u16()).into();
            }
            let _ = sender.send(event).await;
            return Ok(());
        }
        let mut pending = Vec::new();
        let mut total = 0usize;
        let mut completed = false;
        loop {
            let chunk = tokio::select! {result=response.chunk()=>result.map_err(|e|if e.is_timeout() {Error::new(504,"测试超时")} else {Error::new(502,"测试连接中断")})?,_ = sender.closed()=>return Ok(())};
            if let Some(bytes) = chunk.as_ref() {
                total = total.saturating_add(bytes.len());
                pending.extend_from_slice(bytes);
            }
            if total > 134_217_728 {
                let mut event = json!({"type":"error","error":"测试输出超过大小限制"});
                if summary {
                    event["uncertain"] = true.into();
                }
                let _ = sender.send(event).await;
                return Ok(());
            }
            let mut offset = 0;
            while let Some(end) = pending[offset..]
                .iter()
                .position(|v| *v == b'\n')
                .map(|v| v + offset)
                .or_else(|| {
                    if chunk.is_none() && offset < pending.len() {
                        Some(pending.len())
                    } else {
                        None
                    }
                })
            {
                let line = String::from_utf8_lossy(&pending[offset..end]);
                offset = (end + 1).min(pending.len());
                let Some(raw) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(raw.trim()) else {
                    continue;
                };
                let kind = string(&event["type"]);
                if summary && !matches!(kind, "test_start" | "test_complete" | "error")
                    || !matches!(
                        kind,
                        "test_start"
                            | "status"
                            | "content"
                            | "image"
                            | "audio"
                            | "video"
                            | "test_complete"
                            | "error"
                    )
                {
                    continue;
                }
                let mut value = json!({"type":kind});
                if summary && kind == "error" {
                    let parsed = crate::oauth_native::parse_test_sse(&format!("data: {event}"));
                    value["error_code"] = safe_error(&parsed["error_code"], 80).into();
                    value["error"] = parsed["error"].clone();
                    if parsed["uncertain"] == true {
                        value["uncertain"] = true.into();
                    }
                }
                for field in ["text", "model", "error"] {
                    if let Some(entry) = event.get(field) {
                        if kind == "content" && field == "text" {
                            if entry.is_string() {
                                value[field] = entry.clone();
                            }
                        } else {
                            value[field] = safe_error(entry, 16000).into();
                        }
                    }
                }
                if matches!(kind, "image" | "audio" | "video") {
                    let field = format!("{kind}_url");
                    let media = string(&event[&field]);
                    let valid = media.starts_with(&format!("data:{kind}/"))
                        || reqwest::Url::parse(media).is_ok_and(|v| {
                            v.scheme() == "https"
                                && v.host_str().is_some()
                                && v.username().is_empty()
                                && v.password().is_none()
                        });
                    if !valid || media.len() > 90_000_000 {
                        let _ = sender
                            .send(json!({"type":"error","error":"上游返回了不支持的媒体格式"}))
                            .await;
                        return Ok(());
                    }
                    value[&field] = media.into();
                    value["mime_type"] = clean(&event["mime_type"], 80).into();
                }
                if kind == "test_complete" {
                    value["success"] = (event["success"] == true).into();
                }
                if matches!(kind, "test_complete" | "error") {
                    completed = true;
                    value["completed_at"] = now().into();
                    value["duration_ms"] =
                        json!((started.elapsed().as_secs_f64() * 1000.0).round_ties_even() as u64);
                }
                if sender.send(value).await.is_err() {
                    return Ok(());
                }
            }
            pending.drain(..offset);
            if chunk.is_none() {
                break;
            }
        }
        if !completed {
            let mut value = json!({"type":"error","error":"连接结束，未收到测试完成结果"});
            if summary {
                value["uncertain"] = true.into();
            }
            let _ = sender.send(value).await;
        }
        Ok(())
    }
}
