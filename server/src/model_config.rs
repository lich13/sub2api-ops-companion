use crate::{
    compat::{now, py_json, string},
    error::{Error, Result},
    model_reasoning::{model_id, reasoning_fields},
    model_rules::{revision, validate_overrides},
    store::Store,
};
use serde_json::{json, Value};
use std::path::PathBuf;

fn validate(value: &Value) -> Result<()> {
    if !value.is_object() || value["schema"].as_i64() != Some(1) || !value["groups"].is_object() {
        return Err(Error::new(503, "模型配置文件无效"));
    }
    if py_json(value, true, false).len() > 16 * 1024 * 1024 {
        return Err(Error::new(503, "模型配置文件过大"));
    }
    for group in value["groups"].as_object().unwrap().values() {
        if !group.is_object() {
            return Err(Error::new(503, "模型配置文件无效"));
        }
        validate_overrides(group.get("overrides").unwrap_or(&json!({})), &Value::Null)?;
        if let Some(entries) = group.get("reasoning") {
            let entries = entries
                .as_object()
                .filter(|v| v.len() <= 500)
                .ok_or_else(|| Error::new(503, "思考档位配置无效"))?;
            for (model, entry) in entries {
                model_id(&json!(model))?;
                if !entry.is_object() {
                    return Err(Error::new(503, "思考档位配置无效"));
                }
                reasoning_fields(&entry["efforts"], &entry["default_effort"])?;
            }
        }
    }
    Ok(())
}
#[derive(Clone)]
pub struct ModelConfig(pub Store);
impl ModelConfig {
    pub fn new(path: PathBuf, read_only: bool) -> Self {
        let lock = PathBuf::from(format!("{}.lock", path.display()));
        Self(Store::with_lock(
            path,
            lock,
            json!({"schema":1,"groups":{}}),
            validate,
            read_only,
        ))
    }
    pub fn read(&self) -> Result<Value> {
        self.0.read()
    }
    pub fn group(&self, id: i64) -> Result<Value> {
        let data = self.0.read_shared()?;
        let mut saved = data["groups"][id.to_string()]
            .as_object()
            .cloned()
            .map(Value::Object)
            .unwrap_or(json!({}));
        let version = revision(&saved);
        if saved.get("overrides").is_none() {
            saved["overrides"] = json!({});
        }
        saved["revision"] = version.into();
        Ok(saved)
    }
    pub fn save_reasoning(
        &self,
        id: i64,
        model: &str,
        entry: Option<Value>,
        expected: &str,
    ) -> Result<Value> {
        model_id(&json!(model))?;
        if let Some(entry) = &entry {
            reasoning_fields(&entry["efforts"], &entry["default_effort"])?;
        }
        self.0.transaction(|data| {
            let old = data["groups"][id.to_string()]
                .as_object()
                .cloned()
                .map(Value::Object)
                .unwrap_or(json!({}));
            if revision(&old) != expected {
                return Err(Error::new(409, "模型配置已变更，请刷新后重试"));
            }
            let mut updated = old.clone();
            if !updated["reasoning"].is_object() {
                updated["reasoning"] = json!({});
            }
            if let Some(entry) = &entry {
                updated["reasoning"][model] = entry.clone();
            } else {
                updated["reasoning"]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove(model);
            }
            let mut overrides = old["overrides"]
                .as_object()
                .cloned()
                .map(Value::Object)
                .unwrap_or(json!({}));
            if let Some(fields) = overrides.get_mut(model) {
                fields
                    .as_object_mut()
                    .unwrap()
                    .shift_remove("supported_reasoning_levels");
                fields
                    .as_object_mut()
                    .unwrap()
                    .shift_remove("default_reasoning_level");
                if fields.as_object().unwrap().is_empty() {
                    overrides.as_object_mut().unwrap().shift_remove(model);
                }
            }
            updated["overrides"] = overrides;
            updated["updated_at"] = now().into();
            data["groups"][id.to_string()] = updated;
            Ok(())
        })?;
        self.group(id)
    }
    pub fn save(&self, id: i64, overrides: &Value, expected: &str) -> Result<Value> {
        validate_overrides(overrides, &Value::Null)?;
        self.0.transaction(|data| {
            let old = data["groups"][id.to_string()]
                .as_object()
                .cloned()
                .map(Value::Object)
                .unwrap_or(json!({}));
            if revision(&old) != expected {
                return Err(Error::new(409, "模型配置已变更，请刷新后重试"));
            }
            data["groups"][id.to_string()] = json!({"overrides":overrides,"updated_at":now()});
            Ok(())
        })?;
        self.group(id)
    }
}

pub fn group_version(row: &Value) -> String {
    let allowlist = if crate::compat::truthy(&row["model_allowlist"]) {
        row["model_allowlist"].clone()
    } else {
        json!({"enabled":false,"models":[]})
    };
    revision(
        &json!({"id":row["id"],"platform":row["platform"],"updated_at":string(&row["updated_at"]),"model_allowlist":allowlist,"manifest":row["codex_models_manifest_config"]}),
    )
}
