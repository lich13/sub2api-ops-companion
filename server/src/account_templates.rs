use crate::{
    compat::{digest, integer, string, truthy},
    error::{Error, Result},
    store::{valid_policy, Store},
};
use regex::Regex;
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::PathBuf, sync::LazyLock};

pub static MODEL_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._:/-]{1,200}$").unwrap());
static SOURCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[A-Za-z0-9._:/-]{1,199}\*?|\*)$").unwrap());
static CUSTOM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^custom-[a-f0-9]{24}$").unwrap());
const BUILTINS: [(&str, &str); 3] = [
    ("full", "满血"),
    ("degraded", "降智"),
    ("takeover", "降智接管"),
];

pub fn profile(value: &Value) -> Result<Value> {
    let map = value
        .as_object()
        .ok_or_else(|| Error::new(422, "模型配置格式无效"))?;
    if map
        .keys()
        .any(|key| !matches!(key.as_str(), "whitelist" | "mappings"))
    {
        return Err(Error::new(422, "模型配置包含未知字段"));
    }
    let mut result = value.clone();
    for key in ["whitelist", "mappings"] {
        if !map.contains_key(key) {
            result[key] = json!([]);
        }
        if result[key].as_array().is_none_or(|v| v.len() > 300) {
            return Err(Error::new(422, "模型配置超过大小限制"));
        }
    }
    combine(&result)?;
    Ok(result)
}
pub fn combine(value: &Value) -> Result<Value> {
    let mut result = json!({});
    if let Some(values) = value.get("whitelist") {
        for value in values
            .as_array()
            .ok_or_else(|| Error::new(422, "白名单格式无效"))?
        {
            let name = value
                .as_str()
                .filter(|v| MODEL_ID.is_match(v))
                .ok_or_else(|| Error::new(422, "白名单必须是精确模型 ID"))?;
            if result.get(name).is_some() {
                return Err(Error::new(422, format!("重复的模型：{name}")));
            }
            result[name] = name.into();
        }
    }
    if let Some(values) = value.get("mappings") {
        for rule in values
            .as_array()
            .ok_or_else(|| Error::new(422, "映射格式无效"))?
        {
            let source = string(&rule["source"]);
            let target = string(&rule["target"]);
            if rule.as_object().is_none_or(|v| v.len() != 2)
                || !SOURCE.is_match(source)
                || !MODEL_ID.is_match(target)
            {
                return Err(Error::new(
                    422,
                    "映射源只允许末尾通配符，目标必须是精确模型 ID",
                ));
            }
            if result.get(source).is_some() || source == target {
                return Err(Error::new(422, format!("模型条目冲突：{source}")));
            }
            result[source] = target.into();
        }
    }
    Ok(result)
}
pub fn split(mapping: &Value) -> Result<Value> {
    if mapping.is_null() {
        return Ok(json!({"whitelist":[],"mappings":[]}));
    }
    let map = mapping
        .as_object()
        .filter(|v| v.values().all(Value::is_string))
        .ok_or_else(|| Error::new(409, "现有模型配置格式无效，请先在 Sub2API 核对"))?;
    Ok(
        json!({"whitelist":map.iter().filter(|(k,v)|v.as_str()==Some(k.as_str())).map(|(k,_)|k).collect::<Vec<_>>(),"mappings":map.iter().filter(|(k,v)|v.as_str()!=Some(k.as_str())).map(|(k,v)|json!({"source":k,"target":v})).collect::<Vec<_>>()}),
    )
}
pub fn eligible(row: &Value) -> bool {
    row.is_object()
        && !truthy(&row["deleted_at"])
        && row["platform"] == "openai"
        && matches!(string(&row["type"]), "oauth" | "apikey")
        && !truthy(&row["parent_account_id"])
}
pub fn passthrough(row: &Value) -> bool {
    truthy(&row["passthrough"])
        || ["openai_passthrough", "openai_oauth_passthrough"]
            .iter()
            .any(|key| row["extra"][*key] == true || row["extra"][*key] == "true")
}
pub fn account_row(row: &Value) -> Value {
    let mut value = crate::compat::pick(
        row,
        &[
            "id",
            "name",
            "platform",
            "type",
            "deleted_at",
            "parent_account_id",
            "model_mapping",
            "credential_version",
            "model_mapping_version",
        ],
    );
    value["passthrough"] = passthrough(row).into();
    value
}
pub fn account_version(row: &Value) -> Value {
    crate::compat::operation_versions(&account_row(row))["account_template"].clone()
}
fn sync_custom(data: &mut Value) {
    data["custom_templates"] = data["template_order"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|key| {
            let key = key.as_str()?;
            if BUILTINS.iter().any(|(id, _)| *id == key) || !data["templates"][key].is_object() {
                return None;
            }
            let mut value = data["templates"][key].clone();
            value["id"] = key.into();
            value["name"] = data["template_meta"][key]
                .get("name")
                .cloned()
                .unwrap_or_else(|| key.into());
            Some(value)
        })
        .collect::<Vec<_>>()
        .into();
}
pub fn normalize(mut data: Value) -> Result<Value> {
    valid_policy(&data)?;
    if !data["configured"].is_boolean()
        || !data["revision"].is_i64()
        || data["templates"].as_object().is_none_or(|v| v.len() > 32)
        || !matches!(
            data.get("schema_version")
                .and_then(Value::as_i64)
                .unwrap_or(1),
            1 | 2
        )
    {
        return Err(Error::new(503, "账号模板状态无效"));
    }
    if data.get("template_meta").is_none() {
        data["template_meta"] = json!({});
    }
    if !data["template_meta"].is_object() {
        return Err(Error::new(503, "账号模板状态无效"));
    }
    let mut meta = json!({});
    for (key, value) in data["templates"].as_object().unwrap() {
        profile(value).map_err(|_| Error::new(503, "账号模板状态无效"))?;
        let default = BUILTINS
            .iter()
            .find(|(id, _)| *id == key)
            .map(|(_, name)| *name);
        let name = data["template_meta"][key]["name"]
            .as_str()
            .or(default)
            .unwrap_or("");
        if default.is_none() && !CUSTOM.is_match(key)
            || name.trim().is_empty()
            || name.chars().count() > 40
        {
            return Err(Error::new(503, "账号模板状态无效"));
        }
        meta[key] = json!({"name":name.trim(),"builtin":false});
    }
    data["template_meta"] = meta;
    data["schema_version"] = 2.into();
    if data.get("template_order").is_none() {
        data["template_order"] = json!(["full", "degraded", "takeover"]);
    }
    let order = data["template_order"]
        .as_array()
        .filter(|v| v.iter().all(Value::is_string))
        .ok_or_else(|| Error::new(503, "账号模板顺序无效"))?;
    let mut seen = BTreeSet::new();
    let mut output = vec![];
    for key in order.iter().map(string).chain(
        data["templates"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str),
    ) {
        if data["templates"].get(key).is_some() && seen.insert(key.to_owned()) {
            output.push(json!(key));
        }
    }
    data["template_order"] = output.into();
    sync_custom(&mut data);
    Ok(data)
}
#[derive(Clone)]
pub struct Templates {
    pub store: Store,
}
impl Templates {
    pub fn new(path: PathBuf, read_only: bool) -> Self {
        Self {
            store: Store::policy(
                path,
                json!({"schema_version":2,"revision":0,"configured":false,"templates":{"full":{"whitelist":[],"mappings":[]},"degraded":{"whitelist":[],"mappings":[]},"takeover":{"whitelist":[],"mappings":[]}},"template_meta":{},"template_order":["full","degraded","takeover"],"custom_templates":[]}),
                read_only,
            ),
        }
    }
    pub fn data(&self) -> Result<Value> {
        let original = self.store.read()?;
        let normalized = normalize(original.clone())?;
        if normalized == original || self.store.is_read_only() {
            return Ok(normalized);
        }
        self.store.transaction(|current| {
            let mut migrated = normalize(current.clone())?;
            if migrated != *current {
                migrated["revision"] = (integer(&current["revision"]).unwrap_or(0) + 1).into();
                *current = migrated;
            }
            Ok(current.clone())
        })
    }
    pub fn view(&self, row: Option<&Value>) -> Result<Value> {
        let data = self.data()?;
        let mut versions = json!({});
        for (id, value) in data["templates"].as_object().unwrap() {
            versions[id] = digest(value).into();
        }
        let mut result = json!({"configured":data["configured"],"version":digest(&data),"templates":data["templates"],"template_meta":data["template_meta"],"template_order":data["template_order"],"template_versions":versions,"custom_templates":data["custom_templates"]});
        if let Some(row) = row {
            result["account"] = json!({"id":row["id"],"eligible":eligible(row),"passthrough":passthrough(row),"reason":if !eligible(row) {"仅支持独立 OpenAI OAuth 或 Key 账号"} else if passthrough(row) {"透传模式会绕过模型限制，请先处理透传设置"} else {""},"version":account_version(row),"config":split(&row["model_mapping"])?});
        }
        Ok(result)
    }
    /// Copy explicitly selected deployment sources once. Startup and HTTP
    /// requests never choose source accounts or initialize templates implicitly.
    pub async fn initialize_sources(
        &self,
        db: &crate::db::Database,
        full_id: Option<i64>,
        degraded_id: Option<i64>,
    ) -> Result<Value> {
        let data = self.data()?;
        if data["configured"] == true {
            return self.view(None);
        }
        let (Some(full_id), Some(degraded_id)) = (full_id, degraded_id) else {
            return self.view(None);
        };
        if full_id < 1 || degraded_id < 1 || full_id == degraded_id {
            return self.view(None);
        }
        let Ok(rows) = db.all(
            "SELECT id,platform,type,deleted_at, nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id, credentials->'model_mapping' AS model_mapping FROM accounts WHERE id=ANY($1) AND deleted_at IS NULL",
            &[crate::db::Param::Ints(vec![full_id, degraded_id])],
        ).await else {
            return self.view(None);
        };
        let full = rows.iter().find(|row| integer(&row["id"]) == Some(full_id));
        let degraded = rows
            .iter()
            .find(|row| integer(&row["id"]) == Some(degraded_id));
        let (Some(full), Some(degraded)) = (
            full.filter(|row| eligible(row)),
            degraded.filter(|row| eligible(row)),
        ) else {
            return self.view(None);
        };
        let full = split(&full["model_mapping"])?;
        let degraded = split(&degraded["model_mapping"])?;
        combine(&full)?;
        combine(&degraded)?;
        self.store.transaction(|current| {
            if current["configured"] != true {
                current["templates"] = json!({"full":full,"degraded":degraded,"takeover":degraded});
                current["configured"] = true.into();
                current["revision"] = (integer(&current["revision"]).unwrap_or(0) + 1).into();
            }
            Ok(())
        })?;
        self.view(None)
    }
    pub fn desired(&self, payload: &Value) -> Result<Value> {
        let data = self.data()?;
        if data["configured"] != true {
            return Err(Error::new(409, "请先保存账号模板"));
        }
        let template = data["templates"]
            .get(string(&payload["template_id"]))
            .ok_or_else(|| Error::new(409, "模板已删除，请重新选择"))?;
        let version = string(&payload["template_version"]);
        if version != digest(&data) && version != digest(template) {
            return Err(Error::new(409, "模板内容已变化，请重新选择"));
        }
        combine(template)
    }
    pub fn save(&self, payload: &Value) -> Result<Value> {
        let values = BUILTINS
            .into_iter()
            .map(|(id, _)| Ok((id, profile(&payload[id])?)))
            .collect::<Result<Vec<_>>>()?;
        self.store.transaction(|data| {
            if BUILTINS
                .iter()
                .any(|(id, _)| data["templates"].get(id).is_none())
            {
                return Err(Error::new(409, "模板已删除，请升级客户端后逐个编辑模板"));
            }
            if digest(data) != string(&payload["expected_version"]) {
                return Err(Error::new(409, "模板已变化，请重新读取"));
            }
            for (id, value) in values {
                data["templates"][id] = value;
            }
            sync_custom(data);
            data["configured"] = true.into();
            data["revision"] = (integer(&data["revision"]).unwrap_or(0) + 1).into();
            Ok(())
        })?;
        self.view(None)
    }
    pub fn edit(&self, id: Option<&str>, payload: &Value, delete: bool) -> Result<Value> {
        let data = self.data()?;
        if id.is_some_and(|id| data["templates"].get(id).is_none()) {
            return Err(Error::new(404, "账号模板不存在"));
        }
        if digest(&data) != string(&payload["expected_version"]) {
            return Err(Error::new(409, "模板已变化，请重新读取"));
        }
        if id.is_none() && data["templates"].as_object().unwrap().len() >= 32 {
            return Err(Error::new(409, "最多保存 32 套模板"));
        }
        let name = string(&payload["name"]).trim();
        if !delete && (name.is_empty() || name.chars().count() > 40) {
            return Err(Error::new(422, "模板名称不能为空或超过 40 个字符"));
        }
        let value = if delete {
            Value::Null
        } else {
            profile(&json!({"whitelist":payload["whitelist"],"mappings":payload["mappings"]}))?
        };
        let id = id.map(str::to_owned).unwrap_or_else(|| {
            format!(
                "custom-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..24]
            )
        });
        self.store.transaction(|current| {
            if digest(current) != string(&payload["expected_version"]) {
                return Err(Error::new(409, "模板已变化，请重新读取"));
            }
            if delete {
                current["templates"]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove(&id);
                current["template_meta"]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove(&id);
                current["template_order"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|v| v != &id);
            } else {
                let new = current["templates"].get(&id).is_none();
                current["templates"][&id] = value;
                current["template_meta"][&id] = json!({"name":name,"builtin":false});
                if new {
                    current["template_order"]
                        .as_array_mut()
                        .unwrap()
                        .push(id.clone().into());
                }
                current["configured"] = true.into();
            }
            sync_custom(current);
            current["revision"] = (integer(&current["revision"]).unwrap_or(0) + 1).into();
            Ok(())
        })?;
        self.view(None)
    }
}
