//! Decode a response for its declared route. Credential-bearing replies are
//! re-serialized from their wire DTO before crossing into a client process.
use crate::{
    creation,
    dto::*,
    messages::*,
    routes::{Reply, Route},
    semantics,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

fn checked<T: DeserializeOwned>(data: Value) -> Option<Value> {
    T::deserialize(&data).ok()?;
    Some(data)
}

fn known_fields(result: &mut Value, original: &Value) {
    match (result, original) {
        (Value::Object(result), Value::Object(original)) => {
            result.retain(|key, value| {
                original.get(key).is_some_and(|source| {
                    known_fields(value, source);
                    true
                })
            });
        }
        (Value::Array(result), Value::Array(original)) => {
            for (value, source) in result.iter_mut().zip(original) {
                known_fields(value, source);
            }
        }
        (result @ Value::Number(_), source @ Value::Number(_)) => *result = source.clone(),
        _ => {}
    }
}

pub fn projected<T: DeserializeOwned + Serialize>(data: &Value) -> Option<Value> {
    wire_value(&T::deserialize(data).ok()?, data)
}

fn wire_value<T: Serialize>(value: &T, data: &Value) -> Option<Value> {
    let mut projected = serde_json::to_value(value).ok()?;
    known_fields(&mut projected, data);
    Some(projected)
}

pub fn client_response(method: &str, path: &str, data: Value) -> Option<Value> {
    let plain = path.split('?').next()?;
    let parts: Vec<_> = plain.strip_prefix('/')?.split('/').collect();
    let route = Route::resolve(&parts)?
        .parse_numbers(|s| s.parse::<i64>())
        .ok()?;
    let kind = route.endpoint(method)?.reply;
    let matches_account = |expected| data["account_id"].as_i64() == Some(expected);
    match route {
        Route::ApplyCredentials(id)
        | Route::Detection(id)
        | Route::Keepalive(id)
        | Route::AccountQuota(id)
        | Route::AccountToken(id)
        | Route::AccountSettings(id)
        | Route::SubmitOperation(id)
        | Route::StartModelTest(id)
            if !matches_account(id) =>
        {
            return None
        }
        Route::LatestModelTest(id) if !data.is_null() && !matches_account(id) => return None,
        Route::ModelTest(id)
        | Route::RetryModelTest(id)
        | Route::CancelModelTest(id)
        | Route::Operation(id)
        | Route::CancelOperation(id)
        | Route::PriorityOrder(id)
        | Route::CancelOrder(id)
            if data["id"].as_str() != Some(id) =>
        {
            return None
        }
        Route::ManageProxy(id)
            if kind == Reply::Proxy && data["id"].as_i64() != id.parse().ok() =>
        {
            return None
        }
        _ => {}
    }
    match kind {
        Reply::Connection => connection(&data),
        Reply::Capabilities => {
            if data["api_version"] != 1 {
                return None;
            }
            projected::<CapabilitiesReply>(&data)
        }
        Reply::AccountToken if !data["access_token"].as_str()?.is_empty() => {
            projected::<AccountTokenReply>(&data)
        }
        Reply::AccountToken => None,
        Reply::VerifiedAccount if data["verified"] == true => {
            projected::<VerifiedAccountReply>(&data)
        }
        Reply::VerifiedAccount => None,
        Reply::QuotaSettings if route == Route::GlobalQuota => projected::<GlobalQuotaReply>(&data),
        Reply::QuotaSettings => projected::<AccountQuotaReply>(&data),
        Reply::Detection => checked::<ModelDetectionReply>(data),
        Reply::Keepalive if semantics::keepalive_valid(&data) => checked::<KeepaliveReply>(data),
        Reply::Keepalive => None,
        Reply::SystemVersion => checked::<SystemVersionReply>(data),
        Reply::SystemUpdates => checked::<UpdatesReply>(data),
        Reply::SystemUpdate
            if data["need_restart"] == true
                || data["already_up_to_date"] == true
                    && data["current_version"].is_string()
                    && data["latest_version"].is_string() =>
        {
            checked::<SystemUpdateReply>(data)
        }
        Reply::SystemUpdate => None,
        Reply::Message => checked::<MessageReply>(data),
        Reply::Models => checked::<Vec<ModelOption>>(data),
        Reply::Snapshot if semantics::recoveries_valid(&data["recoveries"]) => {
            projected::<crate::snapshot::SnapshotRecord>(&data)
        }
        Reply::Snapshot => None,
        Reply::Usage => projected::<crate::snapshot::UsageSummaryRecord>(&data),
        Reply::Recoveries if !semantics::recoveries_valid(&data["items"]) => None,
        Reply::DetectionLogs => {
            data.get("next_cursor")?;
            let page = DetectionLogPageReply::deserialize(&data).ok()?;
            semantics::detection_logs_valid(&page)
                .then(|| wire_value(&page, &data))
                .flatten()
        }
        Reply::Errors | Reply::Recoveries | Reply::UsageOptions => checked::<ListReply>(data),
        Reply::UsageRecords if data.get("new_count").is_some() => checked::<UsageDeltaReply>(data),
        Reply::UsageRecords => checked::<ListReply>(data),
        Reply::UsageRecord => checked::<RecordDetailHeader>(data),
        Reply::ErrorDetail => projected::<crate::snapshot::OpsErrorRecord>(&data),
        Reply::AccountSettings => projected::<AccountSettingsReply>(&data),
        Reply::Operation if semantics::operation_valid(&data) => {
            projected::<OperationHeader>(&data)
        }
        Reply::Operation => None,
        Reply::Operations if data.get("batch_id").is_some() => {
            let batch = projected::<BatchHeader>(&data)?;
            semantics::batch_results(&batch).then_some(batch)
        }
        Reply::Operations => projected::<OperationListReply>(&data),
        Reply::Batch => {
            let batch = projected::<BatchHeader>(&data)?;
            let valid = if route == Route::ApplyTemplate {
                semantics::batch_valid(&batch)
            } else {
                semantics::proxy_batch_valid(&batch)
            };
            valid.then_some(batch)
        }
        Reply::ModelTest | Reply::LatestModelTest if !data.is_null() => {
            let job = ModelTestJob::deserialize(&data).ok()?;
            job.account_id
                .is_some_and(|id| id > 0)
                .then(|| wire_value(&job, &data))
                .flatten()
        }
        Reply::LatestModelTest => Some(Value::Null),
        Reply::ModelTest => None,
        Reply::Priorities => {
            let overview = PriorityOverviewReply::deserialize(&data).ok()?;
            let mut platforms = std::collections::HashSet::new();
            let mut accounts = std::collections::HashSet::new();
            for group in &overview.platforms {
                if !platforms.insert(&group.platform)
                    || group
                        .items
                        .iter()
                        .any(|item| item.platform != group.platform || !accounts.insert(item.id))
                    || group
                        .last_task
                        .as_ref()
                        .is_some_and(|task| task.platform != group.platform)
                {
                    return None;
                }
            }
            wire_value(&overview, &data)
        }
        Reply::PriorityOrder => {
            let task = PriorityTaskReply::deserialize(&data).ok()?;
            let summary = path.split_once('?').is_some_and(|(_, query)| {
                url::form_urlencoded::parse(query.as_bytes())
                    .any(|(k, v)| k == "details" && v == "0")
            });
            if task.items.is_none() && !summary {
                return None;
            }
            let mut ids = std::collections::HashSet::new();
            if task
                .items
                .iter()
                .flatten()
                .any(|item| item.platform != task.platform || !ids.insert(item.account_id))
            {
                return None;
            }
            wire_value(&task, &data)
        }
        Reply::Templates if semantics::templates_valid(&data) => checked::<TemplatesHeader>(data),
        Reply::Templates => None,
        Reply::Config
            if semantics::detection_platforms_valid(
                &data["oauth"]["model_detection_platforms"],
            ) =>
        {
            projected::<ConfigHeader>(&data)
        }
        Reply::Config => None,
        Reply::QuotaRefresh => checked::<QuotaRefreshHeader>(data),
        Reply::Bank => match route {
            Route::Bank("lm-detector") | Route::SyncBank("lm-detector") => {
                checked::<BankStatusReply<String>>(data)
            }
            _ => checked::<BankStatusReply<Integer>>(data),
        },
        Reply::HostConfig => checked::<HostConfigHeader>(data),
        Reply::HostOperation => checked::<HostOperationReply>(data),
        Reply::HostValidation => checked::<HostValidationReply>(data),
        Reply::Proxies => projected::<Vec<ProxyChoiceReply>>(&data),
        Reply::ProxyDiagnostic if matches!(route, Route::ProbeProxy(_, "test")) => {
            projected::<ProxyTestReply>(&data)
        }
        Reply::ProxyDiagnostic => projected::<ProxyQualityReply>(&data),
        Reply::ProxyList => projected::<ProxyPageReply>(&data),
        Reply::Proxy => projected::<ProxyRowReply>(&data),
        Reply::DeletedProxy if data["deleted"] == true => projected::<DeletedProxyReply>(&data),
        Reply::DeletedProxy => None,
        Reply::ProxyData if data["accounts"].as_array()?.is_empty() => {
            projected::<ProxyExportReply>(&data)
        }
        Reply::ProxyData => None,
        Reply::ProxyImport => projected::<ProxyImportReply>(&data),
        Reply::ProxyBatch if plain.ends_with("/batch-delete") => {
            projected::<DeleteProxyBatchReply>(&data)
        }
        Reply::ProxyBatch => projected::<CreateProxyBatchReply>(&data),
        Reply::ProxyCredentials => projected::<ProxyLogin>(&data),
        Reply::ProxyAccounts => projected::<Vec<AccountIdentity>>(&data),
        Reply::CreatedAccount => created(&data),
        Reply::CreationOptions => projected::<CreationOptionsReply>(&data),
        Reply::CreationSource => creation::reusable(&data),
        Reply::CreationAuth => {
            creation::auth_reply(&data, matches!(route, Route::CreationAuth(_, "authorize")))
        }
        Reply::CreationImport => match route {
            Route::CreationImport("codex-pat") => created(&data),
            Route::CreationImport("codex-session") => projected::<SessionImportReply>(&data),
            Route::CreationImport("grok-sso") => projected::<GrokImportReply>(&data),
            _ => None,
        },
        Reply::CreationCheck => projected::<CreationCheckReply>(&data),
        Reply::CreationModels => projected::<CreationModelsReply>(&data),
        Reply::CreationGroupPreview => projected::<CreationGroupPreviewReply>(&data),
        Reply::CreationCatalog => match route {
            Route::CreationCatalog("tls") => projected::<Vec<ChoiceRecord>>(&data),
            Route::CreationCatalog("antigravity-models") => {
                checked::<std::collections::HashMap<String, String>>(data)
            }
            Route::CreationCatalog(kind) => {
                let key = match kind {
                    "gemini" => "ai_studio_oauth_enabled",
                    "grok" => "password_auth_enabled",
                    "web-search" => "enabled",
                    "settings" => "account_quota_notify_enabled",
                    _ => return None,
                };
                Some(serde_json::json!({key: data[key].as_bool()?}))
            }
            _ => None,
        },
    }
}
pub fn connection(data: &Value) -> Option<Value> {
    let reply = ConnectionReply::deserialize(data).ok()?;
    if !matches!(reply.endpoint.as_str(), "verify" | "public" | "none")
        || !matches!(
            reply.state.as_str(),
            "ok" | "network_unreachable"
                | "auth_rejected"
                | "route_not_found"
                | "protocol_mismatch"
                | "upstream_error"
        )
        || reply
            .http_status
            .is_some_and(|status| !(100..=599).contains(&status))
    {
        return None;
    }
    projected::<ConnectionReply>(data)
}

fn created(data: &Value) -> Option<Value> {
    if data["account"]["id"].as_i64()? <= 0 {
        return None;
    }
    projected::<CreatedAccountReply>(data)
}
