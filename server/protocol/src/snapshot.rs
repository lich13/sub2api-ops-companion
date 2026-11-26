use crate::dto::{DetectionResult, Integer, ScheduleMode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use typeshare::typeshare;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageActionWire {
    QueryUsage,
    QueryResetCredits,
    ResetQuota,
    ProbeQuota,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageBranchWire {
    None,
    Apikey,
    OpenaiOauth,
    GrokFree,
    GrokPaid,
    Native,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatusWire {
    Known,
    Unavailable,
    Error,
    AuthorizationRequired,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowStatusWire {
    Known,
    Unknown,
    Stale,
    Error,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowColorWire {
    Indigo,
    Emerald,
    Purple,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeyQuotaStateWire {
    Idle,
    Loading,
    Ok,
    Error,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeyQuotaSourceWire {
    Sub2api,
    Newapi,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatsStatusWire {
    Ok,
    Error,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CreditEvidenceSourceWire {
    UpstreamError,
    AccountRateLimit,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryKindWire {
    QuotaRecovery,
    ResetCredit,
    ConnectionTest,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub observed_at: String,
    pub accounts: Vec<AccountRecord>,
    pub groups: Vec<GroupRecord>,
    pub errors: Vec<OpsErrorRecord>,
    pub recoveries: Vec<RecoveryRecord>,
    pub schema_version: Option<Integer>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestEventRecord {
    pub r#type: String,
    pub text: Option<String>,
    pub model: Option<String>,
    pub error: Option<String>,
    pub success: Option<bool>,
    pub image_url: Option<String>,
    pub audio_url: Option<String>,
    pub video_url: Option<String>,
    pub duration_ms: Option<f64>,
    pub completed_at: Option<String>,
    pub mime_type: Option<String>,
    pub audio_format: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountRecord {
    pub degradation_mark: Option<DegradationMarkRecord>,
    pub recovery_selectable: Option<bool>,
    pub model_detection: Option<ModelDetectionSummaryRecord>,
    pub id: Integer,
    pub parent_account_id: Option<Integer>,
    pub name: String,
    pub priority: Integer,
    pub concurrency: Option<Integer>,
    pub load_factor: Option<Integer>,
    pub settings_capabilities: Option<HashMap<String, SettingCapabilityRecord>>,
    pub platform: String,
    pub r#type: String,
    pub status: Option<String>,
    pub schedulable: bool,
    pub available: bool,
    pub group_ids: Vec<Integer>,
    pub blockers: Vec<AccountBlockerRecord>,
    pub recoverable: Option<bool>,
    pub version: String,
    pub operation_versions: HashMap<String, String>,
    pub last_success_at: Option<String>,
    pub last_called_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error_id: Option<Integer>,
    pub last_error_code: Option<String>,
    pub last_error_status: Option<Integer>,
    pub error_message: Option<String>,
    pub success_after_error: Option<bool>,
    pub usage_windows: Vec<UsageWindowRecord>,
    pub usage: Option<UsageSummaryRecord>,
    pub recovery_monitor: Option<RecoveryMonitorStateRecord>,
    pub auto_reset_credit: Option<AutoResetCreditStateRecord>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupRecord {
    pub id: Integer,
    pub sort_order: Option<Integer>,
    pub name: String,
    pub platform: String,
    pub account_id: Option<Integer>,
    pub account_name: String,
    pub model: String,
    pub upstream_model: String,
    pub called_at: Option<String>,
    pub recent_accounts: Vec<RecentAccountRecord>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpsErrorRecord {
    pub id: Option<Integer>,
    pub account_id: Option<Integer>,
    pub account_name: Option<String>,
    pub group_id: Option<Integer>,
    pub group_name: Option<String>,
    pub created_at: Option<String>,
    pub model: Option<String>,
    pub requested_model: Option<String>,
    pub upstream_model: Option<String>,
    pub status_code: Option<Integer>,
    pub upstream_status_code: Option<Integer>,
    pub provider_error_code: Option<String>,
    pub message: Option<String>,
    pub request_id: Option<String>,
    pub resolved: Option<bool>,
    pub error_owner: Option<String>,
    pub error_phase: Option<String>,
    pub error_source: Option<String>,
    pub disposition_notification: Option<ErrorDispositionNotificationRecord>,
    pub content: Option<String>,
    pub content_limited: Option<bool>,
    pub notification: Option<ErrorClueNotificationRecord>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryRecord {
    pub id: Option<Integer>,
    pub account_id: Option<Integer>,
    pub account_name: Option<String>,
    pub model_id: Option<String>,
    pub test_completed_at: Option<String>,
    pub recovered_at: Option<String>,
    pub legacy: Option<bool>,
    pub kind: Option<RecoveryKindWire>,
    pub result_status: Option<String>,
    pub attempt_count: Option<Integer>,
    pub reset_credit: Option<RecoveryResetCreditRecord>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DegradationMarkRecord {
    pub marked: Option<bool>,
    pub marked_at: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDetectionSummaryRecord {
    pub schedule_mode: Option<ScheduleMode>,
    pub min_interval_minutes: Option<Integer>,
    pub effective_interval_minutes: Option<Integer>,
    pub recent_request_count: Option<Integer>,
    pub last_automatic_started_at: Option<String>,
    pub enabled: Option<bool>,
    pub interval_minutes: Option<Integer>,
    pub model_id: Option<String>,
    pub version: Option<String>,
    pub next_at: Option<String>,
    pub status: String,
    pub reason: Option<String>,
    pub last_result: Option<DetectionResult>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingCapabilityRecord {
    pub editable: bool,
    pub reason: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountBlockerRecord {
    pub code: String,
    pub label: String,
    pub until: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageWindowRecord {
    pub window_minutes: Option<f64>,
    pub estimated: Option<bool>,
    pub key: String,
    pub label: String,
    pub used_percent: Option<f64>,
    pub reset_at: Option<String>,
    pub observed_at: Option<String>,
    pub status: WindowStatusWire,
    pub source: String,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub color: Option<WindowColorWire>,
    pub stats: Option<UsageStatsRecord>,
    pub estimated_total_cost: Option<f64>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageSummaryRecord {
    pub stats_status: Option<StatsStatusWire>,
    pub key_quota: Option<KeyQuotaRecord>,
    pub branch: UsageBranchWire,
    pub native_read: Option<bool>,
    pub status: Option<UsageStatusWire>,
    pub message: Option<String>,
    pub balances: Option<Vec<UsageBalanceRecord>>,
    pub windows: Vec<UsageWindowRecord>,
    pub today: Option<UsageStatsRecord>,
    pub actions: Vec<UsageActionWire>,
    pub reset_credits: Option<ResetCreditsInfoRecord>,
    pub prepaid_balance: Option<f64>,
    pub monthly_limit: Option<f64>,
    pub monthly_used: Option<f64>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryMonitorStateRecord {
    pub status: String,
    pub label: String,
    pub reason: String,
    pub next_at: Option<String>,
    pub attempt_count: Option<Integer>,
    pub next_retry_at: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AutoResetCreditStateRecord {
    pub trigger_at: Option<String>,
    pub consume_deadline_at: Option<String>,
    pub verification_deadline_at: Option<String>,
    pub completed_at: Option<String>,
    pub outcome: Option<String>,
    pub notification_status: Option<String>,
    pub notification_attempts: Option<Integer>,
    pub evidence_source: Option<CreditEvidenceSourceWire>,
    pub evidence_at: Option<String>,
    pub stage: String,
    pub label: String,
    pub error: String,
    pub next_at: Option<String>,
    pub attempt_at: Option<String>,
    pub test_completed_at: Option<String>,
    pub recovered_at: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentAccountRecord {
    pub log_id: Integer,
    pub account_id: Integer,
    pub account_name: String,
    pub model: String,
    pub upstream_model: String,
    pub called_at: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorDispositionNotificationRecord {
    pub status: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorClueNotificationRecord {
    pub status: String,
    pub reason: Option<String>,
    pub at: Option<String>,
    pub attempts: Option<Integer>,
    pub next_at: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryResetCreditRecord {
    pub consumed: Option<bool>,
    pub completed_at: String,
    pub verification_method: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageStatsRecord {
    pub requests: Integer,
    pub tokens: Integer,
    pub cost: f64,
    pub standard_cost: f64,
    pub user_cost: f64,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyQuotaRecord {
    pub state: KeyQuotaStateWire,
    pub source: Option<KeyQuotaSourceWire>,
    pub checked_at: Option<f64>,
    pub success_at: Option<f64>,
    pub retry_at: Option<f64>,
    pub next_refresh_at: Option<f64>,
    pub stale: bool,
    pub error: Option<String>,
    pub key_status: Option<String>,
    pub plans: Vec<QuotaPlanRecord>,
    pub subscriptions: Vec<QuotaSubscriptionRecord>,
    pub expires_at: Option<String>,
    pub expires_at_unix: Option<f64>,
    pub today: Option<KeyQuotaUsageRecord>,
    pub total_usage: Option<KeyQuotaUsageRecord>,
    pub version: Option<String>,
    pub error_code: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageBalanceRecord {
    pub currency: String,
    pub balance: f64,
    pub observed_at: Option<String>,
    pub status: Option<WindowStatusWire>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResetCreditsInfoRecord {
    pub available: Option<Integer>,
    pub expires_at: Vec<String>,
    pub observed_at: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuotaPlanRecord {
    pub name: String,
    pub remaining: Option<f64>,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub unit: String,
    pub unlimited: bool,
    pub reset_at: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuotaSubscriptionRecord {
    pub name: Option<String>,
    pub status: Option<String>,
    pub active: Option<bool>,
    pub expires_at: Option<String>,
    pub plans: Vec<QuotaPlanRecord>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyQuotaUsageRecord {
    pub requests: Option<f64>,
    pub tokens: Option<f64>,
    pub cost: Option<f64>,
}
