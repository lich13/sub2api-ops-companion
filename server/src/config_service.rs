use crate::{
    audit,
    bark::{self, Bark},
    compat::{config_revision, integer, now, string, text},
    error::{Error, Result},
    key_fallback::KeyFallback,
    oauth_state::OAuthState,
    queries::Queries,
    recovery_policy::{self, CONNECTION_IDS, MODEL_IDS, SEEN_IDS},
    settings::{daily_time, Settings, OAUTH_FIELDS},
    store::{valid_object, Store},
};
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Clone)]
pub struct ConfigService {
    pub settings: Arc<Settings>,
    pub oauth: Store,
    pub bark_store: Store,
    pub bark: Bark,
    pub fallback: KeyFallback,
    pub state: OAuthState,
    pub queries: Queries,
    pub lock: Arc<tokio::sync::Mutex<()>>,
}
fn conflict() -> Error {
    Error::new(409, "设置已被其他窗口修改，请刷新后重试")
}
impl ConfigService {
    pub fn new(
        settings: Arc<Settings>,
        bark: Bark,
        fallback: KeyFallback,
        queries: Queries,
    ) -> Self {
        Self {
            oauth: Store::new(
                settings.oauth_path.clone(),
                json!({}),
                valid_object,
                settings.read_only,
            ),
            bark_store: Store::new(
                settings.bark_path.clone(),
                json!({}),
                valid_object,
                settings.read_only,
            ),
            state: queries.oauth.clone(),
            settings,
            bark,
            fallback,
            queries,
            lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
    pub fn oauth_values(&self, raw: &Value) -> Value {
        OAUTH_FIELDS
            .iter()
            .map(|key| {
                (
                    (*key).to_owned(),
                    raw.get(*key).unwrap_or(&self.settings.oauth[*key]).clone(),
                )
            })
            .collect()
    }
    pub fn current(&self) -> Result<Value> {
        Ok(self.oauth_values(&self.oauth.read()?))
    }
    pub async fn migrate_legacy(&self) -> Result<()> {
        if self.settings.read_only {
            return Err(Error::read_only());
        }
        let _guard = self.lock.lock().await;
        if self
            .oauth
            .read()?
            .get("oauth_7d_probe_interval_seconds")
            .is_none()
        {
            return Ok(());
        }
        self.oauth.transaction(|raw| {
            raw.as_object_mut()
                .ok_or_else(Error::internal)?
                .retain(|key, _| {
                    OAUTH_FIELDS.contains(&key.as_str())
                        || matches!(key.as_str(), "updated_at" | "updated_by")
                        || key == SEEN_IDS
                });
            Ok(())
        })
    }
    pub fn snapshot(&self, section: Option<&str>) -> Result<Value> {
        let raw = if section.is_none_or(|s| s == "oauth") {
            self.oauth.read()?
        } else {
            json!({})
        };
        let oauth = if section.is_none_or(|s| s == "oauth") {
            self.oauth_values(&raw)
        } else {
            json!({})
        };
        let bark = if section.is_none_or(|s| s == "bark") {
            self.bark.panel()?
        } else {
            json!({})
        };
        let fallback = if section.is_none_or(|s| s == "key_fallback") {
            self.fallback.load().panel()
        } else {
            json!({})
        };
        let bark_raw = if section.is_none_or(|s| s == "bark") {
            // Optional Bark config can be invalid while the service keeps
            // displaying its disabled/invalid status. Transactions still use
            // the strict store and cannot overwrite unreadable state.
            self.bark_store.read().unwrap_or(json!({}))
        } else {
            json!({})
        };
        let mut result = json!({"oauth":oauth,"bark":bark,"key_fallback":fallback});
        result["oauth"]["revision"] = config_revision(&json!([result["oauth"], raw])).into();
        result["bark"]["revision"] = config_revision(&json!([result["bark"], bark_raw])).into();
        result["key_fallback"]["revision"] = config_revision(&result["key_fallback"]).into();
        Ok(result)
    }
    pub async fn reconcile(&self, rows: &[Value]) -> Result<bool> {
        if self.settings.read_only {
            return Ok(false);
        }
        let _guard = self.lock.lock().await;
        let eligible = recovery_policy::eligible_ids(rows);
        let before = self.oauth.read()?;
        let (connection, models) =
            if before.get(CONNECTION_IDS).is_some() || before.get(MODEL_IDS).is_some() {
                (
                    recovery_policy::account_ids(before.get(CONNECTION_IDS).unwrap_or(&json!([])))?,
                    recovery_policy::account_ids(before.get(MODEL_IDS).unwrap_or(&json!([])))?,
                )
            } else {
                (eligible.clone(), BTreeSet::new())
            };
        if !connection.is_disjoint(&models) {
            return Err(Error::new(503, "两类恢复账号不能重复"));
        }
        let seen = if let Some(v) = before.get(SEEN_IDS) {
            recovery_policy::account_ids(v)?
        } else {
            eligible.clone()
        };
        let mut next = before.clone();
        next[CONNECTION_IDS] = json!(connection
            .union(&eligible.difference(&seen).copied().collect())
            .copied()
            .filter(|id| !models.contains(id))
            .collect::<BTreeSet<_>>());
        next[MODEL_IDS] = json!(models);
        next[SEEN_IDS] = json!(seen
            .union(&eligible)
            .copied()
            .chain(connection.iter().copied())
            .chain(models.iter().copied())
            .collect::<BTreeSet<_>>());
        if next == before {
            return Ok(false);
        }
        self.oauth.transaction(|raw| {
            if *raw != before {
                return Err(conflict());
            }
            *raw = next;
            Ok(())
        })?;
        Ok(true)
    }
    pub async fn save(
        &self,
        section: &str,
        changes: &Value,
        user: &str,
        expected: Option<&str>,
    ) -> Result<Value> {
        if self.settings.read_only {
            return Err(Error::read_only());
        }
        let _guard = self.lock.lock().await;
        if !matches!(section, "oauth" | "bark" | "key_fallback") {
            return Err(Error::new(422, "未知设置分区"));
        }
        let current = self.snapshot(Some(section))?;
        if expected.is_some_and(|v| v != string(&current[section]["revision"])) {
            return Err(conflict());
        }
        let changes = changes.as_object().ok_or_else(Error::invalid)?;
        let stamp = now();
        match section {
            "oauth" => {
                if changes.keys().any(|key| {
                    key != "oauth_7d_probe_interval_seconds"
                        && !OAUTH_FIELDS.contains(&key.as_str())
                }) {
                    return Err(Error::new(422, "未知 OAuth 设置"));
                }
                let raw = self.oauth.read()?;
                let mut values = self.oauth_values(&raw);
                for (key, value) in changes {
                    if key != "oauth_7d_probe_interval_seconds" {
                        values[key] = value.clone();
                    }
                }
                for key in [
                    "oauth_recovery_monitor_enabled",
                    "oauth_daily_test_enabled",
                    "oauth_auto_reset_credit_enabled",
                ] {
                    if !values[key].is_boolean() {
                        return Err(Error::new(422, "开关必须为布尔值"));
                    }
                }
                values["oauth_daily_test_time"] =
                    daily_time(&values["oauth_daily_test_time"])?.into();
                for (key, low, high) in [
                    ("oauth_usage_refresh_concurrency", 1, 16),
                    ("oauth_recovery_test_concurrency", 1, 8),
                    ("oauth_early_probe_batch_size", 1, 50),
                ] {
                    if !values[key].is_i64()
                        || !integer(&values[key]).is_some_and(|v| (low..=high).contains(&v))
                    {
                        return Err(Error::new(422, format!("{key} 必须在 {low}–{high} 之间")));
                    }
                }
                let model = text(&values["oauth_recovery_test_model_id"])
                    .trim()
                    .to_owned();
                if model.is_empty() || model.chars().count() > 160 {
                    return Err(Error::new(422, "测活模型无效"));
                }
                values["oauth_recovery_test_model_id"] = model.into();
                for field in [CONNECTION_IDS, MODEL_IDS] {
                    let value = if values[field].is_null() {
                        json!([])
                    } else {
                        values[field].clone()
                    };
                    values[field] = json!(recovery_policy::account_ids(&value)?);
                }
                let connection = recovery_policy::account_ids(&values[CONNECTION_IDS])?;
                let models = recovery_policy::account_ids(&values[MODEL_IDS])?;
                if !connection.is_disjoint(&models) {
                    return Err(Error::new(422, "账号只能选择一种恢复验证方式"));
                }
                let selected = connection.union(&models).copied().collect::<BTreeSet<_>>();
                let selection_changed =
                    changes.contains_key(CONNECTION_IDS) || changes.contains_key(MODEL_IDS);
                let previous = current[section][CONNECTION_IDS]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(current[section][MODEL_IDS].as_array().into_iter().flatten())
                    .filter_map(integer)
                    .collect::<BTreeSet<_>>();
                if selection_changed && !selected.is_empty() {
                    let eligible = recovery_policy::eligible_ids(&self.queries.accounts().await?);
                    if selected
                        .difference(&previous)
                        .any(|id| !eligible.contains(id))
                    {
                        return Err(Error::new(422, "所选账号不属于可管理的 OpenAI OAuth 账号"));
                    }
                }
                let mut payload = raw.clone();
                payload
                    .as_object_mut()
                    .unwrap()
                    .extend(values.as_object().unwrap().clone());
                payload["updated_at"] = stamp.clone().into();
                payload["updated_by"] = user.into();
                if raw.get(SEEN_IDS).is_some() {
                    if expected.is_none()
                        && [CONNECTION_IDS, MODEL_IDS].iter().any(|field| {
                            values[field]
                                != current[section]
                                    .get(*field)
                                    .filter(|v| !v.is_null())
                                    .cloned()
                                    .unwrap_or(json!([]))
                        })
                    {
                        return Err(Error::new(409, "修改恢复账号前请刷新设置并提交配置版本"));
                    }
                    payload[SEEN_IDS] = json!(recovery_policy::account_ids(&raw[SEEN_IDS])?
                        .union(&selected)
                        .copied()
                        .collect::<BTreeSet<_>>());
                }
                for field in [
                    "oauth_early_probe_interval_seconds",
                    "oauth_recovery_push_enabled",
                    "oauth_night_recovery_cooldown_enabled",
                    "oauth_usage_refresh_enabled",
                    "oauth_regular_refresh_interval_seconds",
                ] {
                    payload.as_object_mut().unwrap().shift_remove(field);
                }
                let affected = previous
                    .union(&selected)
                    .copied()
                    .filter(|id| {
                        recovery_policy::method(&current[section], *id)
                            != recovery_policy::method(&values, *id)
                    })
                    .collect::<Vec<_>>();
                self.oauth.transaction(|on_disk| {
                    if *on_disk != raw {
                        return Err(conflict());
                    }
                    if selection_changed && !affected.is_empty() {
                        self.state.transaction(|state| {
                            for id in &affected {
                                let meta = &mut state["scheduler"][id.to_string()];
                                if !meta.is_object() {
                                    *meta = json!({});
                                }
                                meta["recovery_method_generation"] = json!(
                                    integer(&meta["recovery_method_generation"]).unwrap_or(0) + 1
                                );
                                if meta["recovery_intent"].is_object() {
                                    meta["recovery_intent"]["status"] = "cancelled".into();
                                    meta["recovery_intent"]["next_retry_at"] = "".into();
                                }
                            }
                            Ok(())
                        })?;
                    }
                    *on_disk = payload;
                    Ok(())
                })?;
            }
            "bark" => {
                if changes
                    .keys()
                    .any(|key| !matches!(key.as_str(), "enabled" | "device_key" | "server_url"))
                {
                    return Err(Error::new(422, "未知 Bark 设置"));
                }
                let runtime = self.bark.runtime();
                let enabled = changes
                    .get("enabled")
                    .cloned()
                    .unwrap_or(json!(runtime.enabled));
                if !enabled.is_boolean() {
                    return Err(Error::new(422, "开关必须为布尔值"));
                }
                let key = changes
                    .get("device_key")
                    .filter(|v| crate::compat::truthy(v))
                    .map(text)
                    .unwrap_or_default();
                let key = if key.trim().is_empty() {
                    runtime.key
                } else {
                    key.trim().to_owned()
                };
                let url = changes
                    .get("server_url")
                    .filter(|v| crate::compat::truthy(v))
                    .map(text)
                    .unwrap_or_default();
                let url = if url.trim().is_empty() {
                    if runtime.url.is_empty() {
                        bark::DEFAULT_URL.to_owned()
                    } else {
                        runtime.url
                    }
                } else {
                    bark::normalize_url(url.trim())?
                };
                if enabled == true && key.is_empty() {
                    return Err(Error::new(422, "启用 Bark 前需要填写 Device Key"));
                }
                let payload = self.bark_store.transaction(|raw| {
                    let panel = self.bark.panel()?;
                    if expected.is_some_and(|v| v != config_revision(&json!([panel, raw]))) {
                        return Err(conflict());
                    }
                    raw["enabled"] = enabled;
                    raw["device_key"] = key.into();
                    raw["server_url"] = url.into();
                    raw["updated_at"] = stamp.into();
                    raw["updated_by"] = user.into();
                    Ok(raw.clone())
                })?;
                self.bark.configure(&payload, true);
            }
            _ => {
                if changes.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "openai_enabled"
                            | "grok_enabled"
                            | "managed_account_ids"
                            | "coexist_account_ids"
                    )
                }) {
                    return Err(Error::new(422, "未知 Key 回退设置"));
                }
                let mut values = json!({});
                for key in ["openai_enabled", "grok_enabled", "managed_account_ids"] {
                    values[key] = current[section][key].clone();
                }
                values.as_object_mut().unwrap().extend(changes.clone());
                if !changes.contains_key("coexist_account_ids") {
                    let selected = values["managed_account_ids"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    values["coexist_account_ids"] = json!(current[section]["coexist_account_ids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|id| selected.contains(id))
                        .cloned()
                        .collect::<Vec<_>>());
                }
                for key in ["openai_enabled", "grok_enabled"] {
                    if !values[key].is_boolean() {
                        return Err(Error::new(422, "开关必须为布尔值"));
                    }
                }
                self.fallback.save_expected(&values, user, expected).await?;
            }
        }
        audit::write(
            &self.settings.audit_path,
            &format!("{section}_config_update"),
            json!({"user":user,"fields":changes.keys().collect::<BTreeSet<_>>()}),
            false,
        );
        Ok(self.snapshot(Some(section))?[section].clone())
    }
}
