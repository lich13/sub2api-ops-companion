use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
pub fn id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
pub fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn py_json(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(py_json).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", json!(k), py_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => v.to_string(),
    }
}
pub fn digest(v: &Value) -> String {
    format!("{:x}", Sha256::digest(py_json(v).as_bytes()))
}
pub fn version(row: &Value) -> String {
    let identity: serde_json::Map<String, Value> = [
        "id",
        "platform",
        "type",
        "parent_account_id",
        "credential_version",
        "proxy_id",
    ]
    .into_iter()
    .map(|k| (k.into(), row[k].clone()))
    .collect();
    let mut groups = row["group_ids"].as_array().cloned().unwrap_or_default();
    groups.sort_by_key(|v| v.as_i64().unwrap_or_default());
    digest(&json!([identity, [groups, row["model_catalog_version"]]]))
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountInput {
    pub account_id: i64,
    pub expected_version: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub confirmed: bool,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub action: String,
    pub payload: Payload,
    pub accounts: Vec<AccountInput>,
    pub request_id: String,
    pub client_id: String,
}
impl Submission {
    pub fn validate(&self) -> Result<(), Failure> {
        let key = |s: &str| {
            (16..=64).contains(&s.len())
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        };
        let mut ids = std::collections::HashSet::new();
        if self.action != "test"
            || !self.payload.confirmed
            || self.accounts.is_empty()
            || self.accounts.len() > 1000
            || !key(&self.request_id)
            || !key(&self.client_id)
            || self.accounts.iter().any(|a| {
                a.account_id < 1 || !hex(&a.expected_version, 64) || !ids.insert(a.account_id)
            })
        {
            return Err(Failure(422, "invalid_request", "批量测试请求无效"));
        }
        Ok(())
    }
    pub fn signature(&self) -> String {
        let mut accounts = self.accounts.clone();
        accounts.sort_by_key(|a| a.account_id);
        digest(
            &json!({"action":self.action,"payload":self.payload,"accounts":accounts,"client_id":self.client_id}),
        )
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Job {
    pub id: String,
    pub batch_id: String,
    pub account_id: i64,
    pub account_name: String,
    pub action: String,
    pub parent_account_id: Option<i64>,
    pub expected_version: String,
    pub requested: Value,
    pub status: String,
    pub reason: String,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub result: Option<Value>,
}
impl Job {
    pub fn public(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("serializable job");
        value.as_object_mut().unwrap().remove("expected_version");
        value
    }
    pub fn pending(&self) -> bool {
        matches!(
            self.status.as_str(),
            "queued" | "running" | "checking" | "needs_confirmation"
        )
    }
    pub fn change(&mut self, status: &str, reason: &str) {
        self.status = status.into();
        self.reason = reason.into();
        self.updated_at = now();
    }
    pub fn lock_ids(&self) -> Vec<i64> {
        let mut ids = vec![self.account_id];
        if let Some(parent) = self.parent_account_id {
            ids.push(parent);
        }
        ids.sort();
        ids.dedup();
        ids
    }
}

#[derive(Debug)]
pub struct Failure(pub u16, pub &'static str, pub &'static str);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.0).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"detail":{"code":self.1,"message":self.2}})),
        )
            .into_response()
    }
}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self(503, "state_unavailable", "操作状态无法保存或读取")
    }
}
impl From<serde_json::Error> for Failure {
    fn from(_: serde_json::Error) -> Self {
        Self(503, "state_invalid", "操作状态无法读取")
    }
}
impl From<sqlx::Error> for Failure {
    fn from(_: sqlx::Error) -> Self {
        Self(503, "database_unavailable", "账号状态无法读取")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_digest_spacing() {
        assert_eq!(
            py_json(&json!({"z":[1,null],"a":"中文"})),
            "{\"a\": \"中文\", \"z\": [1, null]}"
        );
    }
}
