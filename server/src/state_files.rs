//! Durable fields used by the scheduler and by restart reconciliation. Flattened
//! fields belong to individual actions and survive a read/modify/write cycle.
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

type Fields = Map<String, Value>;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationFile {
    pub version: u32,
    pub jobs: IndexMap<String, SavedOperation>,
    pub requests: IndexMap<String, String>,
    #[serde(default)]
    pub batches: IndexMap<String, SavedBatch>,
    #[serde(default)]
    pub batch_requests: IndexMap<String, String>,
    #[serde(flatten)]
    extra: Fields,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedOperation {
    pub account_id: i64,
    pub status: String,
    pub payload: Fields,
    #[serde(flatten)]
    pub fields: Fields,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedBatch {
    pub job_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunks: Option<Vec<SavedChunk>>,
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedChunk {
    pub status: String,
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct PriorityFile {
    pub version: u32,
    pub jobs: IndexMap<String, SavedOrder>,
    pub requests: IndexMap<String, String>,
    pub active_id: Option<String>,
    pub last_id: Option<String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub parallelism: IndexMap<String, usize>,
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedOrder {
    pub id: String,
    pub status: String,
    pub platform: String,
    pub request_id: String,
    pub reason: String,
    pub updated_at: String,
    pub cancel_requested: bool,
    pub waiting_account_ids: Vec<i64>,
    pub items: Vec<PriorityItem>,
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct PriorityItem {
    pub account_id: i64,
    pub status: String,
    pub target_priority: i64,
    pub original_priority: i64,
    pub observed_priority: Option<i64>,
    pub identity: String,
    #[serde(flatten)]
    pub fields: Fields,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthFile {
    pub version: u32,
    pub settings: OAuthSettings,
    pub scheduler: IndexMap<String, SchedulerEntry>,
    pub oauth_results: IndexMap<String, Fields>,
    pub pending_events: IndexMap<String, Fields>,
    pub recovery_history: IndexMap<String, Fields>,
    pub daily_test: Fields,
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub2api_admin_token: Option<String>,
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SchedulerEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_intent: Option<Fields>,
    #[serde(flatten)]
    pub fields: Fields,
}
