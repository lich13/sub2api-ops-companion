use crate::{
    compat::{string, text, truthy},
    error::{Error, Result},
    store::read_config,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

pub const OAUTH_FIELDS: &[&str] = &[
    "connection_test_retry_interval_seconds",
    "connection_test_max_retries",
    "model_detection_model_id",
    "model_detection_platforms",
    "oauth_auto_reset_credit_enabled",
    "oauth_daily_test_enabled",
    "oauth_daily_test_time",
    "oauth_early_probe_batch_size",
    "oauth_recovery_connection_account_ids",
    "oauth_recovery_model_account_ids",
    "oauth_recovery_monitor_enabled",
    "oauth_recovery_test_concurrency",
    "oauth_recovery_test_model_id",
    "oauth_usage_refresh_concurrency",
];

#[derive(Clone)]
pub struct Settings {
    pub database_url: String,
    pub bind: String,
    pub base_path: String,
    pub audit_path: PathBuf,
    pub usage_path: PathBuf,
    pub oauth_path: PathBuf,
    pub bark_path: PathBuf,
    pub connection_path: PathBuf,
    pub model_path: PathBuf,
    pub keepalive_path: PathBuf,
    pub oauth: Value,
    pub bark: Value,
    pub bark_valid: bool,
    pub base_url: String,
    pub verify_url: String,
    pub read_only: bool,
}

pub const DEFAULT_DETECTION_MODEL: &str = "gpt-6-luna";

pub fn detection_model(value: &Value) -> Result<String> {
    let model = if value.is_null() {
        ""
    } else {
        value
            .as_str()
            .ok_or_else(|| Error::new(422, "自动检测模型无效"))?
    }
    .trim();
    let model = if model.is_empty() {
        DEFAULT_DETECTION_MODEL
    } else {
        model
    };
    if !crate::account_templates::MODEL_ID.is_match(model) {
        return Err(Error::new(422, "自动检测模型无效"));
    }
    Ok(model.to_owned())
}

pub fn bool_value(value: &Value, default: bool) -> bool {
    if value.is_null() {
        return default;
    }
    if let Some(value) = value.as_bool() {
        return value;
    }
    matches!(
        text(value).trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub fn strict_bool(value: &Value, default: bool) -> (bool, bool) {
    if value.is_null() {
        return (default, true);
    }
    if let Some(value) = value.as_bool() {
        return (value, true);
    }
    match text(value).trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => (true, true),
        "0" | "false" | "no" | "off" => (false, true),
        _ => (default, false),
    }
}

pub fn daily_time(value: &Value) -> Result<String> {
    let value = string(value).trim();
    let bytes = value.as_bytes();
    if bytes.len() == 5
        && bytes[2] == b':'
        && [0, 1, 3, 4].into_iter().all(|i| bytes[i].is_ascii_digit())
        && value[..2].parse::<u8>().is_ok_and(|v| v < 24)
        && value[3..].parse::<u8>().is_ok_and(|v| v < 60)
    {
        Ok(value.to_owned())
    } else {
        Err(Error::new(422, "测活时间必须为 00:00–23:59（北京时间）"))
    }
}

pub fn normalize_url(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() {
        return Ok(String::new());
    }
    let url = reqwest::Url::parse(value).map_err(|_| Error::new(422, "Sub2API 服务地址无效"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::new(422, "Sub2API 服务地址无效"));
    }
    Ok(value.to_owned())
}

pub fn env_map() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

impl Settings {
    pub fn load(read_only: bool) -> Result<Self> {
        Self::from_env(&env_map(), read_only)
    }

    pub fn from_env(env: &BTreeMap<String, String>, read_only: bool) -> Result<Self> {
        let get =
            |key: &str, default: &str| env.get(key).cloned().unwrap_or_else(|| default.to_owned());
        let oauth_path = PathBuf::from(get("OAUTH_CONFIG_PATH", "/data/oauth-config.json"));
        let bark_path = PathBuf::from(get("BARK_CONFIG_PATH", "/data/bark-config.json"));
        let config = read_config(&oauth_path).inspect_err(|_| {
            eprintln!("startup_diagnostic resource=oauth_config error_code=read_failed");
        })?;
        let mut oauth = json!({});
        let configured = |key: &str| {
            config.get(key).cloned().unwrap_or_else(|| {
                env.get(&key.to_uppercase())
                    .map(|v| json!(v))
                    .unwrap_or(Value::Null)
            })
        };
        for (key, default) in [
            ("oauth_auto_reset_credit_enabled", false),
            ("oauth_recovery_monitor_enabled", true),
            ("oauth_daily_test_enabled", true),
        ] {
            oauth[key] = bool_value(&configured(key), default).into();
        }
        for (key, default, min, max) in [
            ("connection_test_retry_interval_seconds", 10, 1, 3600),
            ("connection_test_max_retries", 3, 0, 10),
            ("oauth_usage_refresh_concurrency", 4, 1, 16),
            ("oauth_recovery_test_concurrency", 2, 1, 8),
            // Historical early-probe setting: retain storage, validation and
            // default for compatibility. Current scheduling does not probe
            // before reset or use this value as a worker dispatch limit.
            ("oauth_early_probe_batch_size", 8, 1, 50),
        ] {
            let value = text(&configured(key))
                .trim()
                .parse::<i64>()
                .map(|v| v.clamp(min, max))
                .unwrap_or(default);
            oauth[key] = value.into();
        }
        oauth["oauth_daily_test_time"] = daily_time(&configured("oauth_daily_test_time"))
            .unwrap_or_else(|_| "05:00".into())
            .into();
        oauth["model_detection_model_id"] =
            detection_model(&configured("model_detection_model_id"))?.into();
        oauth["model_detection_platforms"] =
            detection_platforms(&configured("model_detection_platforms"))?;
        let model = configured("oauth_recovery_test_model_id");
        let model = string(&model).trim();
        oauth["oauth_recovery_test_model_id"] = if model.is_empty() {
            crate::model_downgrade::DEFAULT_TEST_MODEL
        } else {
            model
        }
        .into();
        for key in [
            "oauth_recovery_connection_account_ids",
            "oauth_recovery_model_account_ids",
        ] {
            oauth[key] = config[key].clone();
        }
        let bark_result = read_config(&bark_path);
        let mut bark_valid = bark_result.is_ok();
        let bark_config = bark_result.unwrap_or(json!({}));
        let bark_field = |key: &str, var: &str, default: &str| {
            bark_config
                .get(key)
                .cloned()
                .unwrap_or_else(|| json!(get(var, default)))
        };
        let (enabled, valid_bool) =
            strict_bool(&bark_field("enabled", "BARK_ENABLED", "false"), false);
        bark_valid &= valid_bool;
        for (key, is_bool) in [
            ("enabled", true),
            ("device_key", false),
            ("server_url", false),
        ] {
            if let Some(value) = bark_config.get(key) {
                bark_valid &= if is_bool {
                    value.is_boolean()
                } else {
                    value.is_string()
                };
            }
        }
        let key = bark_field("device_key", "BARK_DEVICE_KEY", "");
        let key = if truthy(&key) {
            text(&key)
        } else {
            String::new()
        };
        let url = bark_field("server_url", "BARK_SERVER_URL", crate::bark::DEFAULT_URL);
        let url = if truthy(&url) {
            text(&url)
        } else {
            String::new()
        };
        let url = if url.trim().is_empty() {
            crate::bark::DEFAULT_URL
        } else {
            url.trim()
        };
        bark_valid &= crate::bark::normalize_url(url).is_ok();
        if !bark_valid {
            eprintln!("startup_diagnostic resource=bark_config error_code=optional_config_invalid");
        }
        let bark = json!({"enabled":enabled,"device_key":key,"server_url":url});
        Ok(Self {
            database_url: env
                .get("DATABASE_URL")
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or_else(|| Error::new(503, "未配置数据库连接"))?,
            bind: env.get("SUB2OPS_BIND").cloned().unwrap_or_else(|| {
                format!(
                    "{}:{}",
                    get("HOST", "0.0.0.0"),
                    get("PORT", &get("APP_PORT", "18081"))
                )
            }),
            base_path: get("BASE_PATH", "/sub2ops")
                .trim_end_matches('/')
                .to_owned(),
            audit_path: get("AUDIT_PATH", "/data/audit.jsonl").into(),
            usage_path: get("USAGE_QUERY_STATE_PATH", "/data/usage-query-state.json").into(),
            oauth_path,
            bark_path,
            connection_path: get("SUB2API_CONFIG_PATH", "/data/sub2api-config.json").into(),
            model_path: get("GROUP_MODEL_CONFIG_PATH", "/data/group-model-config.json").into(),
            keepalive_path: get("KEEPALIVE_CONFIG_PATH", "/data/keepalive-state.json").into(),
            oauth,
            bark,
            bark_valid,
            base_url: get("SUB2API_BASE_URL", "").trim_end_matches('/').to_owned(),
            verify_url: get("SUB2API_VERIFY_BASE_URL", "")
                .trim_end_matches('/')
                .to_owned(),
            read_only,
        })
    }

    pub fn data_dir(&self) -> PathBuf {
        self.usage_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_owned()
    }

    pub fn connection_config(&self) -> Result<Value> {
        let value = read_config(&self.connection_path)?;
        Ok(json!({
            "base_url":normalize_url(value["base_url"].as_str().unwrap_or(&self.base_url))?,
            "verify_base_url":normalize_url(value["verify_base_url"].as_str().unwrap_or(&self.verify_url))?,
        }))
    }
}

/// Per-account-platform defaults. Other platform transports are not enabled by this setting.
pub fn detection_platforms(value: &Value) -> Result<Value> {
    let mut result = serde_json::json!({
        "openai":{"engine":"modeltrace","sample_count":1},
        "anthropic":{"engine":"lm_detector","sample_count":1},
        "grok":{"engine":"lm_detector","sample_count":1},
        "deepseek":{"engine":"lm_detector","sample_count":1}
    });
    if value.is_null() {
        return Ok(result);
    }
    let invalid = || Error::new(422, "检测源平台配置无效");
    for (platform, settings) in value.as_object().ok_or_else(invalid)? {
        if result.get(platform).is_none() {
            return Err(invalid());
        }
        let row = settings.as_object().ok_or_else(invalid)?;
        if row
            .keys()
            .any(|key| !matches!(key.as_str(), "engine" | "sample_count"))
            || !(settings["engine"] == "lm_detector"
                || platform == "openai" && settings["engine"] == "modeltrace")
            || !matches!(settings["sample_count"].as_i64(), Some(1 | 3))
        {
            return Err(invalid());
        }
        result[platform] = settings.clone();
    }
    Ok(result)
}
