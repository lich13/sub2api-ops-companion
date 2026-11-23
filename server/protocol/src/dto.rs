use serde::{Deserialize, Serialize};
use typeshare::typeshare;

pub type Integer = i64;

#[typeshare]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleMode {
    Fixed,
    Smart,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeFailure {
    pub code: String,
    pub status: Option<u16>,
    pub message: String,
    pub request_id: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionReply {
    pub state: String,
    pub endpoint: String,
    pub http_status: Option<u16>,
    pub message: String,
    pub retryable: bool,
    pub checked_at: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilitiesReply {
    pub api_version: u32,
    pub platforms: Vec<String>,
    pub service: String,
    pub version: String,
    pub implementation: String,
    pub build_revision: String,
    pub upstream_connection: ConnectionReply,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageReply {
    pub message: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemVersionReply {
    pub version: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReleaseInfoReply {
    pub name: Option<String>,
    pub html_url: Option<String>,
    pub published_at: Option<String>,
    pub body: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdatesReply {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub build_type: String,
    pub cached: bool,
    pub warning: Option<String>,
    pub release_info: Option<ReleaseInfoReply>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountTokenReply {
    pub account_id: Integer,
    pub access_token: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedAccountReply {
    pub account_id: Integer,
    pub verified: bool,
    pub status: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyLogin {
    pub username: String,
    pub password: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: String,
    pub display_name: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDetectionReply {
    pub account_id: Integer,
    pub enabled: bool,
    pub schedule_mode: ScheduleMode,
    pub interval_minutes: Integer,
    pub min_interval_minutes: Integer,
    pub effective_interval_minutes: Option<Integer>,
    pub recent_request_count: Option<Integer>,
    pub last_automatic_started_at: Option<String>,
    pub next_allowed_at: Option<String>,
    pub model_id: String,
    pub model_override: Option<String>,
    pub global_model_id: String,
    pub model_source: String,
    pub version: String,
    pub status: String,
    pub engine: Option<String>,
    pub sample_count: Option<Integer>,
    pub verdict_mode: Option<String>,
    pub degraded_model_id: Option<String>,
    pub generation: Option<Integer>,
    pub disposition: Option<Disposition>,
    pub disposition_notification: Option<NotificationResult>,
    pub recheck_recovery: Option<DetectionRecovery>,
    pub recovery_notification: Option<NotificationResult>,
    pub next_at: Option<String>,
    pub reason: Option<String>,
    pub job_id: Option<String>,
    pub last_result: Option<DetectionResult>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Disposition {
    pub marked: bool,
    pub schedule_verified: bool,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotificationResult {
    pub status: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectionRecovery {
    pub status: String,
    pub generation: Integer,
    pub mark_cleared: bool,
    pub reason: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectionResult {
    pub status: Option<String>,
    pub report: Option<DetectionReport>,
    pub completed_at: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectionReport {
    pub prediction_name: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeepaliveReply {
    pub account_id: Integer,
    pub enabled: bool,
    pub generation: Integer,
    pub version: String,
    pub status: String,
    pub reason: String,
    pub paused_reason: String,
    pub last_restore_at: Option<String>,
    pub restore_count: Integer,
    pub budget_used: Integer,
    pub next_allowed_at: Option<String>,
}

impl BridgeFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            status: None,
            request_id: None,
        }
    }
    pub fn background() -> Self {
        Self::new("background", "应用已转入后台，已停止读取")
    }
}
impl From<String> for BridgeFailure {
    fn from(msg: String) -> Self {
        Self::new("client_error", msg)
    }
}
impl From<&str> for BridgeFailure {
    fn from(msg: &str) -> Self {
        Self::new("client_error", msg)
    }
}
