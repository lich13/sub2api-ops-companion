use crate::dto::{Integer, ScheduleMode};
use crate::snapshot::{AccountRecord, DegradationMarkRecord, TestEventRecord, UsageSummaryRecord};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use typeshare::typeshare;

type Json = serde_json::Value;

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GlobalQuotaReply {
    pub seconds: Integer,
    pub revision: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountQuotaReply {
    pub account_id: Integer,
    pub has_access_token: bool,
    pub revision: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountIdentity {
    pub id: Integer,
    pub name: String,
    pub r#type: String,
    pub platform: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreatedAccountReply {
    pub account: AccountIdentity,
    pub warnings: Vec<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChoiceRecord {
    pub id: Integer,
    pub name: String,
    pub platform: Option<String>,
    pub status: Option<String>,
    pub long_context_pricing_enabled: Option<bool>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreationOptionsReply {
    pub groups: Vec<ChoiceRecord>,
    pub proxies: Vec<ChoiceRecord>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreationCheckReply {
    pub has_risk: bool,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreationModelsReply {
    pub models: Vec<String>,
    pub warnings: Option<Vec<String>>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenAuthorization {
    pub auth_url: String,
    pub session_id: String,
    pub state: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GrokImportReply {
    pub created: Vec<IndexedAccount>,
    pub failed: Vec<IndexedFailure>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexedAccount {
    pub index: Integer,
    pub account: AccountIdentity,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexedFailure {
    pub index: Integer,
    pub error: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionImportReply {
    pub created: Integer,
    pub updated: Integer,
    pub skipped: Integer,
    pub failed: Integer,
    pub errors: Vec<IndexedMessage>,
    pub warnings: Vec<IndexedMessage>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexedMessage {
    pub index: Integer,
    pub message: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyRowReply {
    pub id: Integer,
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub port: Integer,
    pub status: String,
    pub username_set: bool,
    pub password_set: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub expires_at: Option<String>,
    pub fallback_mode: Option<String>,
    pub backup_proxy_id: Option<Integer>,
    pub expiry_warn_days: Option<Integer>,
    pub account_count: Option<Integer>,
    pub latency_ms: Option<Integer>,
    pub latency_status: Option<String>,
    pub ip_address: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub quality_status: Option<String>,
    pub quality_score: Option<Integer>,
    pub quality_grade: Option<String>,
    pub quality_checked: Option<Integer>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyPageReply {
    pub items: Vec<ProxyRowReply>,
    pub page: Integer,
    pub page_size: Integer,
    pub total: Integer,
    pub pages: Integer,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyChoiceReply {
    pub id: Integer,
    pub name: String,
    pub protocol: String,
    pub status: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyTestReply {
    pub success: bool,
    pub message: String,
    pub latency_ms: Option<f64>,
    pub ip_address: String,
    pub city: String,
    pub region: String,
    pub country: String,
    pub country_code: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyQualityReply {
    pub score: f64,
    pub grade: String,
    pub summary: String,
    pub exit_ip: String,
    pub country: String,
    pub country_code: String,
    pub base_latency_ms: Option<f64>,
    pub passed_count: Option<Integer>,
    pub warn_count: Option<Integer>,
    pub failed_count: Option<Integer>,
    pub challenge_count: Option<Integer>,
    pub checked_at: Option<Integer>,
    pub items: Vec<ProxyQualityCheck>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyQualityCheck {
    pub target: String,
    pub status: String,
    pub message: String,
    pub latency_ms: Option<f64>,
    pub http_status: Option<Integer>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeletedProxyReply {
    pub deleted: bool,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkippedProxy {
    pub id: Integer,
    pub reason: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeleteProxyBatchReply {
    pub deleted_ids: Vec<Integer>,
    pub skipped: Vec<SkippedProxy>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateProxyBatchReply {
    pub created: Integer,
    pub skipped: Integer,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyImportReply {
    pub proxy_created: Integer,
    pub proxy_reused: Integer,
    pub proxy_failed: Integer,
    pub errors: Vec<NamedMessage>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NamedMessage {
    pub name: String,
    pub message: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyExportReply {
    pub exported_at: String,
    pub proxies: Vec<ExportedProxy>,
    pub accounts: Vec<AccountIdentity>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportedProxy {
    pub name: String,
    pub host: String,
    pub port: Integer,
    pub proxy_key: Option<String>,
    pub protocol: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub status: Option<String>,
    pub expires_at: Option<String>,
    pub fallback_mode: Option<String>,
    pub backup_proxy_name: Option<String>,
    pub expiry_warn_days: Option<Integer>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilityField {
    pub editable: bool,
    pub reason: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelMappingRule {
    pub source: String,
    pub target: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelProfileReply {
    pub whitelist: Vec<String>,
    pub mappings: Vec<ModelMappingRule>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountSettingsReply {
    pub account_id: Integer,
    pub priority: Integer,
    pub concurrency: Option<Integer>,
    pub load_factor: Option<Integer>,
    pub effective_load_factor: Integer,
    pub proxy_id: Option<Integer>,
    pub model_config: ModelProfileReply,
    pub capabilities: HashMap<String, CapabilityField>,
    pub versions: HashMap<String, String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PriorityOverviewReply {
    pub platforms: Vec<PriorityPlatformReply>,
    pub active_task: Option<PriorityTaskReply>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PriorityPlatformReply {
    pub platform: String,
    pub parallelism: Option<Integer>,
    pub version: String,
    pub items: Vec<PriorityAccountReply>,
    pub last_task: Option<PriorityTaskReply>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PriorityAccountReply {
    pub id: Integer,
    pub name: String,
    pub platform: String,
    pub r#type: String,
    pub priority: Integer,
    pub version: String,
    pub editable: bool,
    pub reason: String,
    pub status: String,
    pub schedulable: bool,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PriorityTaskReply {
    pub id: String,
    pub status: String,
    pub cancel_requested: bool,
    pub reason: String,
    pub total: Integer,
    pub change_count: Integer,
    pub verified_count: Integer,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub platform: String,
    pub base: Integer,
    pub parallelism: Option<Integer>,
    pub waiting_reason: String,
    pub waiting_account_ids: Vec<Integer>,
    pub items: Option<Vec<PriorityItemReply>>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PriorityItemReply {
    pub account_id: Integer,
    pub account_name: String,
    pub original_priority: Integer,
    pub target_priority: Integer,
    pub observed_priority: Option<Integer>,
    pub status: String,
    pub platform: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostValidationReply {
    pub valid: bool,
    pub errors: Vec<HostFieldIssue>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostFieldIssue {
    pub path: String,
    pub message: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostOperationReply {
    pub status: String,
    pub message: Option<String>,
    pub updated_at: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BankVersionReply {
    pub revision: String,
    pub sha256: String,
    pub analyzer_version: Integer,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BankStatusReply {
    pub version: BankVersionReply,
    pub source: String,
    pub status: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemUpdateReply {
    pub message: String,
    pub need_restart: Option<bool>,
    pub already_up_to_date: Option<bool>,
    pub current_version: Option<String>,
    pub latest_version: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationRequestFields {
    pub priority: Option<Integer>,
    pub concurrency: Option<Integer>,
    pub load_factor: Option<Integer>,
    pub proxy_id: Option<Integer>,
    pub schedulable: Option<bool>,
    pub marked: Option<bool>,
    pub confirmed: Option<bool>,
    pub model_id: Option<String>,
    pub mode: Option<String>,
    pub action: Option<String>,
    pub template_id: Option<String>,
    pub template_version: Option<String>,
    pub group_ids: Option<Vec<Integer>>,
    pub scope_group_ids: Option<Vec<Integer>>,
    pub whitelist: Option<Vec<String>>,
    pub mappings: Option<Vec<ModelMappingRule>>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationCurrentFields {
    pub id: Option<Integer>,
    pub priority: Option<Integer>,
    pub concurrency: Option<Integer>,
    pub load_factor: Option<Integer>,
    pub proxy_id: Option<Integer>,
    pub name: Option<String>,
    pub platform: Option<String>,
    pub r#type: Option<String>,
    pub status: Option<String>,
    pub schedulable: Option<bool>,
    pub marked: Option<bool>,
    pub group_ids: Option<Vec<Integer>>,
    pub whitelist: Option<Vec<String>>,
    pub mappings: Option<Vec<ModelMappingRule>>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationReceipt {
    pub id: Option<Integer>,
    pub priority: Option<Integer>,
    pub concurrency: Option<Integer>,
    pub load_factor: Option<Integer>,
    pub proxy_id: Option<Integer>,
    pub name: Option<String>,
    pub platform: Option<String>,
    pub r#type: Option<String>,
    pub status: Option<String>,
    pub schedulable: Option<bool>,
    pub marked: Option<bool>,
    pub group_ids: Option<Vec<Integer>>,
    pub whitelist: Option<Vec<String>>,
    pub mappings: Option<Vec<ModelMappingRule>>,
    pub account_id: Option<Integer>,
    pub duration_ms: Option<Integer>,
    pub verified: Option<bool>,
    pub unchanged: Option<bool>,
    pub success: Option<bool>,
    pub uncertain: Option<bool>,
    pub applied: Option<bool>,
    pub deleted: Option<bool>,
    pub template_id: Option<String>,
    pub message: Option<String>,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub version: Option<String>,
    pub field_version: Option<String>,
    pub synced_at: Option<String>,
    pub completed_at: Option<String>,
    pub requested_model: Option<String>,
    pub actual_model: Option<String>,
    pub models: Option<Vec<String>>,
    pub warnings: Option<Vec<String>>,
    pub settings_versions: Option<HashMap<String, String>>,
    pub account: Option<AccountRecord>,
    pub degradation_mark: Option<DegradationMarkRecord>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationHeader {
    pub id: String,
    pub account_id: Integer,
    pub action: String,
    pub status: String,
    pub account_name: Option<String>,
    pub batch_id: Option<String>,
    pub reason: Option<String>,
    pub started_at: Option<String>,
    pub scheduled_at: Option<String>,
    pub updated_at: Option<String>,
    pub created_at: Option<String>,
    pub completed_at: Option<String>,
    pub next_retry_at: Option<String>,
    pub repeat: Option<String>,
    pub repeat_series_id: Option<String>,
    pub repeat_state: Option<String>,
    pub repeat_reason: Option<String>,
    pub next_operation_id: Option<String>,
    pub previous_operation_id: Option<String>,
    pub execution_mode: Option<String>,
    pub dispatch_id: Option<String>,
    pub attempt_count: Option<Integer>,
    pub next_event: Option<Integer>,
    pub parent_account_id: Option<Integer>,
    pub child_account_ids: Option<Vec<Integer>>,
    pub native_target: Option<OperationRequestFields>,
    pub requested: OperationRequestFields,
    pub current: Option<OperationCurrentFields>,
    pub result: Option<OperationReceipt>,
    pub events: Option<Vec<TestEventRecord>>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchHeader {
    pub batch_id: String,
    pub pending: Integer,
    pub items: Vec<OperationHeader>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationListReply {
    pub pending: Integer,
    pub items: Vec<OperationHeader>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelTestHeader {
    pub id: String,
    pub account_id: Integer,
    pub status: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageDeltaReply {
    pub latest_id: Integer,
    pub new_count: Integer,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostConfigHeader {
    pub available: bool,
    pub editable: bool,
    pub reason: String,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ListReply {
    pub items: Vec<Json>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordDetailHeader {
    pub id: Integer,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfigHeader {
    pub oauth: ConfigOAuthHeader,
    pub bark: BarkSettingsReply,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfigOAuthHeader {
    pub revision: String,
    pub model_detection_model_id: String,
    pub model_detection_platforms: HashMap<String, DetectionPlatformSetting>,
    pub oauth_early_reset_enabled: bool,
    pub oauth_early_reset_interval_seconds: Integer,
    pub connection_test_retry_interval_seconds: Option<Integer>,
    pub connection_test_max_retries: Option<Integer>,
    pub oauth_auto_reset_credit_enabled: Option<bool>,
    pub oauth_daily_test_enabled: Option<bool>,
    pub oauth_daily_test_time: Option<String>,
    pub oauth_recovery_connection_account_ids: Option<Vec<Integer>>,
    pub oauth_recovery_model_account_ids: Option<Vec<Integer>>,
    pub oauth_recovery_monitor_enabled: Option<bool>,
    pub oauth_recovery_test_concurrency: Option<Integer>,
    pub oauth_recovery_test_model_id: Option<String>,
    pub oauth_usage_refresh_concurrency: Option<Integer>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectionPlatformSetting {
    pub engine: String,
    pub sample_count: Integer,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BarkSettingsReply {
    pub revision: String,
    pub configured: Option<bool>,
    pub config_valid: Option<bool>,
    pub enabled: Option<bool>,
    pub device_key_set: Option<bool>,
    pub device_key_status: Option<String>,
    pub server_url_valid: Option<bool>,
    pub config_updated_at: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemplatesHeader {
    pub version: String,
    pub configured: bool,
    pub templates: HashMap<String, ModelProfileReply>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuotaRefreshHeader {
    pub status: String,
    pub items: Vec<QuotaRefreshEntry>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuotaRefreshEntry {
    pub usage: Option<UsageSummaryRecord>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectionUpdateBody {
    pub expected_version: String,
    pub enabled: Option<bool>,
    pub schedule_mode: Option<ScheduleMode>,
    pub interval_minutes: Option<Integer>,
    pub min_interval_minutes: Option<Integer>,
    pub model_id: Option<Json>,
    pub model_override: Option<String>,
    pub verdict_mode: Option<String>,
    pub degraded_model_id: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigUpdateBody {
    pub expected_revision: String,
    pub changes: HashMap<String, Json>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountQuotaBody {
    pub expected_revision: String,
    pub access_token: Option<String>,
}

#[typeshare]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalQuotaBody {
    pub expected_revision: String,
    pub seconds: Integer,
}
