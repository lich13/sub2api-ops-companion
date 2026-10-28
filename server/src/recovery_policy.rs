use crate::{
    compat::{integer, truthy},
    error::{Error, Result},
};
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};

pub const CONNECTION_IDS: &str = "oauth_recovery_connection_account_ids";
pub const MODEL_IDS: &str = "oauth_recovery_model_account_ids";
pub const SEEN_IDS: &str = "oauth_recovery_seen_account_ids";

pub struct MethodIndex {
    default_connection: bool,
    models: HashSet<i64>,
    connections: HashSet<i64>,
}

impl MethodIndex {
    pub fn from_config(config: &Value) -> Self {
        let default_connection = config[CONNECTION_IDS].is_null() && config[MODEL_IDS].is_null();
        let models = config[MODEL_IDS]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(integer)
            .collect();
        let connections = config[CONNECTION_IDS]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(integer)
            .collect();
        Self {
            default_connection,
            models,
            connections,
        }
    }

    pub fn method(&self, id: i64) -> Option<&'static str> {
        if self.default_connection {
            return Some("connection");
        }
        if self.models.contains(&id) {
            Some("model")
        } else if self.connections.contains(&id) {
            Some("connection")
        } else {
            None
        }
    }
}

pub fn account_ids(value: &Value) -> Result<BTreeSet<i64>> {
    let items = value
        .as_array()
        .ok_or_else(|| Error::new(422, "恢复账号必须为正整数 ID 列表"))?;
    items
        .iter()
        .map(|v| {
            integer(v)
                .filter(|id| v.is_i64() && *id > 0)
                .ok_or_else(|| Error::new(422, "恢复账号必须为正整数 ID 列表"))
        })
        .collect()
}
pub fn method(config: &Value, id: i64) -> Option<&'static str> {
    if config[CONNECTION_IDS].is_null() && config[MODEL_IDS].is_null() {
        return Some("connection");
    }
    if config[MODEL_IDS]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| integer(v) == Some(id)))
    {
        Some("model")
    } else if config[CONNECTION_IDS]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| integer(v) == Some(id)))
    {
        Some("connection")
    } else {
        None
    }
}
pub fn eligible_ids(rows: &[Value]) -> BTreeSet<i64> {
    rows.iter()
        .filter(|row| {
            row["platform"] == "openai"
                && row["type"] == "oauth"
                && !truthy(&row["deleted_at"])
                && !truthy(&row["parent_account_id"])
                && !truthy(&row["extra"]["parent_account_id"])
        })
        .filter_map(|row| integer(&row["id"]))
        .collect()
}
