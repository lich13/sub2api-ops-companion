//! Explicit, offline migrations for retired deployment configuration. These are
//! never run by the HTTP service or by its read-only preflight.
use crate::{
    compat::{string, text},
    error::{Error, Result},
    settings::{bool_value, daily_time, normalize_url, OAUTH_FIELDS},
};
use fs2::FileExt;
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
};

fn invalid() -> Error {
    Error::new(422, "迁移输入无效，未删除原配置")
}
fn path(env: &BTreeMap<String, String>, key: &str, default: &str) -> PathBuf {
    env.get(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}
fn checked_path(path: &Path) -> Result<PathBuf> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(invalid());
    }
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in full.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let mut ancestor = normalized.as_path();
    let mut missing = Vec::new();
    while !ancestor.exists() {
        missing.push(ancestor.file_name().ok_or_else(invalid)?.to_owned());
        ancestor = ancestor.parent().ok_or_else(invalid)?;
    }
    let mut resolved = fs::canonicalize(ancestor)?;
    for part in missing.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

fn read_object(path: &Path) -> Result<Value> {
    checked_path(path)?;
    match fs::read(path) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            if !value.is_object() {
                return Err(invalid());
            }
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(_) => Err(invalid()),
    }
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    checked_path(path)?;
    let parent = path.parent().ok_or_else(invalid)?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::Builder::new()
        .prefix(".sub2ops-migration-")
        .tempfile_in(parent)?;
    #[cfg(unix)]
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|_| Error::internal())?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn retire(paths: &[PathBuf]) -> Result<u64> {
    let mut size = 0;
    for path in paths.iter().collect::<BTreeSet<_>>() {
        if path.exists() {
            size += fs::metadata(path)?.len();
            fs::remove_file(path)?;
            File::open(path.parent().ok_or_else(invalid)?)?.sync_all()?;
        }
    }
    Ok(size)
}
fn guard(paths: &[PathBuf]) -> Result<Option<File>> {
    for path in paths {
        checked_path(path)?;
        if path.exists() {
            if !path.is_file() {
                return Err(invalid());
            }
            let file = File::open(path)?;
            file.try_lock_exclusive()
                .map_err(|_| Error::conflict("迁移源正被使用"))?;
            return Ok(Some(file));
        }
    }
    Ok(None)
}
fn distinct(retired: &[PathBuf], outputs: &[PathBuf]) -> Result<()> {
    let output_count = outputs.len();
    let retired = retired
        .iter()
        .map(|p| checked_path(p))
        .collect::<Result<BTreeSet<_>>>()?;
    let outputs = outputs
        .iter()
        .map(|p| checked_path(p))
        .collect::<Result<BTreeSet<_>>>()?;
    if outputs.len() != output_count || !retired.is_disjoint(&outputs) {
        return Err(invalid());
    }
    for p in retired {
        if p.exists() && !p.is_file() {
            return Err(invalid());
        }
    }
    Ok(())
}
fn strip_derived(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("telegram_windows");
            for value in map.values_mut() {
                strip_derived(value);
            }
        }
        Value::Array(array) => {
            for value in array {
                strip_derived(value);
            }
        }
        _ => {}
    }
}
fn env_key(line: &str) -> &str {
    line.trim_start()
        .strip_prefix("export ")
        .unwrap_or(line.trim_start())
        .split_once('=')
        .map(|(name, _)| name.trim())
        .unwrap_or("")
}

pub fn migrate_oauth(env: &BTreeMap<String, String>, env_file: &Path) -> Result<Value> {
    let legacy_path = path(env, "TELEGRAM_CONFIG_PATH", "/data/telegram-config.json");
    let target = path(env, "OAUTH_CONFIG_PATH", "/data/oauth-config.json");
    let state_path = path(
        env,
        "USAGE_QUERY_STATE_PATH",
        "/data/usage-query-state.json",
    );
    let _guard = guard(&[env_file.to_path_buf(), legacy_path.clone(), target.clone()])?;
    let legacy = read_object(&legacy_path)?;
    let current = read_object(&target)?;
    let bot = if let Some(value) = legacy["state_path"].as_str().filter(|s| !s.is_empty()) {
        PathBuf::from(value)
    } else {
        path(env, "TELEGRAM_STATE_PATH", "/data/telegram-state.json")
    };
    let retired = [legacy_path, bot];
    distinct(
        &retired,
        &[target.clone(), state_path.clone(), env_file.to_path_buf()],
    )?;
    let defaults = json!({"oauth_auto_reset_credit_enabled":false,"oauth_daily_test_enabled":true,"oauth_daily_test_time":"05:00","oauth_early_probe_batch_size":8,"oauth_recovery_connection_account_ids":null,"oauth_recovery_model_account_ids":null,"oauth_recovery_monitor_enabled":true,"oauth_recovery_test_concurrency":2,"oauth_recovery_test_model_id":crate::model_downgrade::DEFAULT_TEST_MODEL,"oauth_usage_refresh_concurrency":4});
    let mut result = json!({});
    for key in OAUTH_FIELDS {
        let default = &defaults[*key];
        let upper = key.to_uppercase();
        let value = current
            .get(*key)
            .or_else(|| legacy.get(*key))
            .cloned()
            .or_else(|| {
                env.get(&upper)
                    .or_else(|| env.get(&format!("TELEGRAM_{upper}")))
                    .map(|v| json!(v))
            })
            .unwrap_or_else(|| default.clone());
        result[*key] = if let Some(default) = default.as_bool() {
            json!(bool_value(&value, default))
        } else if let Some(default) = default.as_i64() {
            let max = match *key {
                "oauth_usage_refresh_concurrency" => 16,
                "oauth_recovery_test_concurrency" => 8,
                _ => 50,
            };
            json!(text(&value)
                .trim()
                .parse::<i64>()
                .unwrap_or(default)
                .clamp(1, max))
        } else if *key == "oauth_daily_test_time" {
            json!(daily_time(&value)?)
        } else if key.ends_with("account_ids") {
            if !value.is_null()
                && !value
                    .as_array()
                    .is_some_and(|a| a.iter().all(|v| v.as_i64().is_some_and(|n| n > 0)))
            {
                return Err(invalid());
            }
            value
        } else {
            let v = string(&value).trim();
            if v.is_empty() {
                default.clone()
            } else {
                json!(v)
            }
        };
    }
    let raw_env = fs::read_to_string(env_file)?;
    let mut neutral: String = raw_env
        .split_inclusive('\n')
        .filter(|line| !env_key(line).starts_with("TELEGRAM_"))
        .collect();
    if !neutral
        .lines()
        .any(|line| env_key(line) == "OAUTH_CONFIG_PATH")
    {
        neutral = format!(
            "{}\nOAUTH_CONFIG_PATH={}\n",
            neutral.trim_end_matches('\n'),
            target.display()
        );
    }
    let state = read_object(&state_path)?;
    let mut cleaned = state.clone();
    strip_derived(&mut cleaned);
    let result_bytes = json_bytes(&result)?;
    let state_bytes = json_bytes(&cleaned)?;
    atomic(&target, &result_bytes)?;
    if read_object(&target)? != result {
        return Err(Error::internal());
    }
    if cleaned != state {
        atomic(&state_path, &state_bytes)?;
    }
    atomic(env_file, neutral.as_bytes())?;
    retire(&retired)?;
    Ok(
        json!({"migrated":true,"fields":result.as_object().unwrap().len(),"mode":"0o600","legacy_removed":true}),
    )
}

pub async fn migrate_desktop(
    env: &BTreeMap<String, String>,
    env_file: &Path,
    remove_menu: bool,
) -> Result<Value> {
    let old = path(env, "OPS_SSO_CONFIG_PATH", "/data/sso-config.json");
    let target = path(env, "SUB2API_CONFIG_PATH", "/data/sub2api-config.json");
    let session = path(env, "OPS_SESSION_STORE_PATH", "/data/sessions.json");
    let models = env
        .get("GROUP_MODEL_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| target.with_file_name("group-model-config.json"));
    let _guard = guard(&[env_file.to_path_buf(), old.clone(), target.clone()])?;
    let retired = [old.clone(), session];
    distinct(
        &retired,
        &[target.clone(), models.clone(), env_file.to_path_buf()],
    )?;
    let source = read_object(if old.exists() { &old } else { &target })?;
    let url = |field: &str, key: &str| {
        normalize_url(
            source
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or_else(|| env.get(key).map(String::as_str).unwrap_or("")),
        )
    };
    let base = url("base_url", "SUB2API_BASE_URL")?;
    let verify = url("verify_base_url", "SUB2API_VERIFY_BASE_URL")?;
    if base.is_empty() && verify.is_empty() {
        return Err(invalid());
    }
    let effective = json!({"base_url":base,"verify_base_url":verify});
    if models.exists() {
        read_object(&models)?;
    }
    checked_path(env_file)?;
    let neutral = if env_file.exists() {
        Some(
            fs::read_to_string(env_file)?
                .split_inclusive('\n')
                .filter(|line| {
                    ![
                        "OPS_BASIC_",
                        "OPS_SESSION_",
                        "OPS_SSO_",
                        "SUB2API_SSO_",
                        "OPS_UPDATE_",
                    ]
                    .iter()
                    .any(|prefix| env_key(line).starts_with(prefix))
                })
                .collect::<String>(),
        )
    } else {
        None
    };
    let mut db = if remove_menu {
        Some(sqlx::PgConnection::connect(env.get("DATABASE_URL").ok_or_else(invalid)?).await?)
    } else {
        None
    };
    use sqlx::Connection;
    let mut transaction = match db.as_mut() {
        Some(db) => Some(db.begin().await?),
        None => None,
    };
    let mut menu = None;
    let mut removed = 0;
    if let Some(transaction) = transaction.as_mut() {
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut **transaction)
            .await?;
        let raw = sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key='custom_menu_items' FOR UPDATE",
        )
        .fetch_optional(&mut **transaction)
        .await?;
        if let Some(raw) = raw {
            let original: Value = serde_json::from_str(&raw).map_err(|_| invalid())?;
            let items = original.as_array().ok_or_else(invalid)?;
            let base_host = reqwest::Url::parse(&base)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned));
            let kept: Vec<Value> = items
                .iter()
                .filter(|item| {
                    let Some(raw) = item.get("url").and_then(Value::as_str) else {
                        return true;
                    };
                    let parsed = reqwest::Url::parse(raw).ok();
                    let path = parsed
                        .as_ref()
                        .map(|u| u.path())
                        .unwrap_or_else(|| raw.split(['?', '#']).next().unwrap_or(""));
                    let host = parsed.as_ref().and_then(|u| u.host_str());
                    !(path.trim_end_matches('/') == "/sub2ops/sso/start"
                        && (host.is_none() || host == base_host.as_deref()))
                })
                .cloned()
                .collect();
            removed = items.len() - kept.len();
            if removed > 0 {
                menu = Some(serde_json::to_string(&kept)?);
            }
        }
    }
    atomic(&target, &json_bytes(&effective)?)?;
    if read_object(&target)? != effective {
        return Err(Error::internal());
    }
    if !models.exists() {
        atomic(&models, &json_bytes(&json!({"schema":1,"groups":{}}))?)?;
    }
    if let (Some(transaction), Some(menu)) = (transaction.as_mut(), menu) {
        sqlx::query("UPDATE settings SET value=$1,updated_at=now() WHERE key='custom_menu_items'")
            .bind(menu)
            .execute(&mut **transaction)
            .await?;
    }
    if let Some(transaction) = transaction {
        transaction.commit().await?;
    }
    if let Some(neutral) = neutral {
        atomic(env_file, neutral.as_bytes())?;
    }
    let released = retire(&retired)?;
    Ok(json!({"connection_migrated":true,"menu_items_removed":removed,"retired_bytes":released}))
}
