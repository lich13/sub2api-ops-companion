use crate::{
    actions::Actions,
    compat::{future, integer, now, parse_datetime, string, text, truthy},
    credit_ownership,
    error::{Error, Result},
    queries::latest_completed_result,
    quota,
    settings::strict_bool,
    store::{valid_object, Store},
    upstream::read_limited,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub fn strict_id(value: &Value) -> Option<i64> {
    if let Some(id) = value.as_i64() {
        return (id > 0).then_some(id);
    }
    let value = value.as_str()?.trim();
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id = value.parse::<i64>().ok()?;
    (id > 0 && id.to_string() == value).then_some(id)
}
pub fn live_key(row: &Value, expected: Option<i64>) -> bool {
    row.is_object()
        && strict_id(&row["id"]).is_some_and(|id| expected.is_none_or(|v| id == v))
        && matches!(
            string(&row["platform"]).trim().to_lowercase().as_str(),
            "openai" | "grok"
        )
        && string(&row["type"]).trim().eq_ignore_ascii_case("apikey")
        && (row["deleted_at"].is_null() || row["deleted_at"] == "")
}
pub fn blocked(row: &Value, at: DateTime<Utc>) -> bool {
    string(&row["status"]).trim().to_lowercase() != "active"
        || row["schedulable"] != true
        || [
            "temp_unschedulable_until",
            "rate_limit_reset_at",
            "overload_until",
        ]
        .iter()
        .any(|field| future(&row[*field], at))
        || strict_bool(&row["auto_pause_on_expired"], false).0
            && parse_datetime(&row["expires_at"]).is_some_and(|v| v <= at)
}
pub fn fresh(result: &Value, at: DateTime<Utc>, seconds: i64) -> bool {
    result.is_object()
        && parse_datetime(&result["queried_at"]).is_some_and(|t| {
            t <= at
                && at.signed_duration_since(t).num_milliseconds()
                    <= seconds.max(0).saturating_mul(1000)
        })
}
pub fn grok_state(row: &Value, at: DateTime<Utc>) -> &'static str {
    if !row["status"].is_string()
        || string(&row["status"]).trim().is_empty()
        || !row["schedulable"].is_boolean()
    {
        return "unknown";
    }
    if string(&row["status"]).trim().to_lowercase() != "active" || row["schedulable"] == false {
        return "unavailable";
    }
    for field in [
        "temp_unschedulable_until",
        "rate_limit_reset_at",
        "overload_until",
    ] {
        if !row[field].is_null() && row[field] != "" {
            match parse_datetime(&row[field]) {
                None => return "unknown",
                Some(t) if t > at => return "unavailable",
                _ => {}
            }
        }
    }
    let auto = row
        .get("auto_pause_on_expired")
        .unwrap_or(&Value::Bool(false));
    if !auto.is_boolean() {
        return "unknown";
    }
    if *auto == true && !row["expires_at"].is_null() && row["expires_at"] != "" {
        match parse_datetime(&row["expires_at"]) {
            None => return "unknown",
            Some(t) if t <= at => return "unavailable",
            _ => {}
        }
    }
    if !row["extra"].is_null() && !row["extra"].is_object() {
        return "unknown";
    }
    match row["extra"]
        .get("grok_needs_reauth")
        .unwrap_or(&Value::Bool(false))
        .as_bool()
    {
        Some(true) => "unavailable",
        Some(false) => "available",
        None => "unknown",
    }
}
pub fn oauth_state(row: &Value, result: &Value, at: DateTime<Utc>, seconds: i64) -> &'static str {
    if blocked(row, at) {
        return "unavailable";
    }
    if !fresh(result, at, seconds) {
        return "unknown";
    }
    if matches!(
        string(&result["error_code"]).trim().to_lowercase().as_str(),
        "401" | "http_401" | "402" | "http_402"
    ) {
        return "unavailable";
    }
    if !truthy(&result["success"]) {
        return "unknown";
    }
    let summary =
        if result["data"]["five_hour"].is_object() || result["data"]["seven_day"].is_object() {
            quota::from_usage_data(
                &result["data"],
                row,
                parse_datetime(&result["queried_at"]).unwrap_or(at),
            )
        } else if result["oauth_quota"].is_object() {
            quota::sanitize_summary(row, &result["oauth_quota"])
        } else {
            return "unknown";
        };
    let mut available = true;
    let mut exhausted = false;
    for key in quota::required_windows(&summary["plan_type"]) {
        let used = summary["ui_windows"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|v| v["key"] == *key)
            .and_then(|v| quota::percent(&v["used_percent"]));
        match used {
            None => available = false,
            Some(v) if v >= 100.0 => {
                available = false;
                exhausted = true;
            }
            _ => {}
        }
    }
    if available {
        "available"
    } else if exhausted {
        "unavailable"
    } else {
        "unknown"
    }
}
pub fn desired(states: &[&str]) -> Option<bool> {
    if states.is_empty() {
        None
    } else if states.contains(&"available") {
        Some(false)
    } else if states.iter().all(|v| *v == "unavailable") {
        Some(true)
    } else {
        None
    }
}
pub fn managed_ids(value: &Value) -> Result<Vec<i64>> {
    let values = value
        .as_array()
        .ok_or_else(|| Error::new(422, "Key 回退保存失败：账号 ID 必须是唯一的正整数"))?;
    let mut seen = BTreeSet::new();
    let mut output = vec![];
    for value in values {
        let id = text(value)
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|i| *i > 0)
            .ok_or_else(|| Error::new(422, "Key 回退保存失败：账号 ID 必须是唯一的正整数"))?;
        if seen.insert(id) {
            output.push(id);
        }
    }
    Ok(output)
}
#[derive(Clone)]
pub struct Config {
    pub enabled: bool,
    pub openai: bool,
    pub grok: bool,
    pub ids: Vec<i64>,
    pub coexist: BTreeSet<i64>,
    pub version: i64,
    pub updated_at: String,
    pub updated_by: String,
    pub valid: bool,
}
impl Config {
    pub fn defaults(valid: bool) -> Self {
        Self {
            enabled: false,
            openai: false,
            grok: false,
            ids: vec![],
            coexist: BTreeSet::new(),
            version: 0,
            updated_at: String::new(),
            updated_by: String::new(),
            valid,
        }
    }
    pub fn platform_enabled(&self, platform: &str) -> bool {
        if platform == "openai" {
            self.openai
        } else {
            self.grok
        }
    }
    pub fn panel(&self) -> Value {
        json!({"enabled":self.enabled && self.valid,"openai_enabled":self.openai && self.valid,"grok_enabled":self.grok && self.valid,"managed_account_ids":self.ids,"coexist_account_ids":self.coexist,"config_valid":self.valid,"config_updated_at":if self.updated_at.is_empty() {Value::Null} else {json!(self.updated_at)}})
    }
}
pub fn parse_config(data: &Value) -> Config {
    let invalid = || Config::defaults(false);
    if !data.is_object() {
        return invalid();
    }
    let (legacy, ok) = strict_bool(&data["enabled"], false);
    if !ok && data.get("enabled").is_some() {
        return invalid();
    }
    let (openai, a) = strict_bool(&data["openai_enabled"], legacy);
    let (grok, b) = strict_bool(&data["grok_enabled"], legacy);
    if !a || !b {
        return invalid();
    }
    let empty = json!([]);
    let Some(raw) = data.get("managed_account_ids").unwrap_or(&empty).as_array() else {
        return invalid();
    };
    let mut ids = vec![];
    for value in raw {
        let Some(id) = strict_id(value) else {
            return invalid();
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let Some(raw) = data.get("coexist_account_ids").unwrap_or(&empty).as_array() else {
        return invalid();
    };
    if raw
        .iter()
        .any(|v| v.as_i64().is_none_or(|id| !ids.contains(&id)))
    {
        return invalid();
    }
    Config {
        enabled: openai || grok,
        openai,
        grok,
        ids,
        coexist: raw.iter().filter_map(Value::as_i64).collect(),
        version: integer(&data["config_version"])
            .unwrap_or(0)
            .clamp(0, 1_000_000_000),
        updated_at: if truthy(&data["updated_at"]) {
            text(&data["updated_at"])
        } else {
            String::new()
        },
        updated_by: if truthy(&data["updated_by"]) {
            text(&data["updated_by"])
        } else {
            String::new()
        },
        valid: true,
    }
}
pub fn envelope_matches(body: &Value, id: i64, schedulable: bool) -> bool {
    if !body.is_object()
        || !(body["code"] == 0 || body["code"] == "0")
        || !body["data"].is_object()
        || body["data"].get("schedulable").is_none()
    {
        return false;
    }
    let (observed, ok) = strict_bool(&body["data"]["schedulable"], false);
    ok && observed == schedulable
        && body["data"]
            .get("id")
            .is_none_or(|v| strict_id(v) == Some(id))
}
pub async fn set_schedulable(
    actions: &Actions,
    id: i64,
    enabled: bool,
    key: &str,
    seconds: u64,
) -> Value {
    if actions.settings.read_only {
        return json!({"success":false,"error_code":"read_only"});
    }
    if key.trim().is_empty() || actions.upstream.active_url().is_empty() {
        return json!({"success":false,"error_code":"missing_sub2api_admin_credentials","error":"缺少 Sub2API 地址或 Admin API Key"});
    }
    match actions
        .request(
            reqwest::Method::POST,
            &format!("accounts/{id}/schedulable"),
            key,
            Some(&json!({"schedulable":enabled})),
            Duration::from_secs(seconds.clamp(2, 60)),
            false,
        )
        .await
    {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = match read_limited(response, 2_000_000).await {
                Ok(raw) => serde_json::from_slice(&raw).unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            if (200..300).contains(&status) && envelope_matches(&body, id, enabled) {
                json!({"success":true,"status_code":status,"data":body["data"]})
            } else {
                json!({"success":false,"status_code":status,"error_code":if (200..300).contains(&status) {"invalid_schedulable_response".into()} else {format!("http_{status}")}})
            }
        }
        Err(error) => {
            json!({"success":false,"error_code":if error.status.as_u16()==504 {"timeout"} else {"network_error"}})
        }
    }
}
#[derive(Clone)]
pub struct KeyFallback {
    pub store: Store,
    pub lock: Arc<tokio::sync::Mutex<()>>,
    actions: Actions,
    resume: Arc<Mutex<Option<i64>>>,
}
impl KeyFallback {
    pub fn new(actions: Actions) -> Self {
        Self {
            store: Store::new(
                actions.settings.fallback_path.clone(),
                json!({}),
                valid_object,
                actions.settings.read_only,
            ),
            lock: Arc::new(tokio::sync::Mutex::new(())),
            actions,
            resume: Arc::new(Mutex::new(None)),
        }
    }
    pub fn load(&self) -> Config {
        self.store
            .read()
            .map(|v| parse_config(&v))
            .unwrap_or_else(|_| Config::defaults(false))
    }
    pub fn preflight(&self) -> Result<()> {
        if self.load().valid {
            Ok(())
        } else {
            Err(Error::new(503, "托管配置无法读取"))
        }
    }
    pub fn managed(&self) -> Result<BTreeSet<i64>> {
        let config = self.load();
        if !config.valid {
            return Err(Error::new(503, "托管配置无法读取"));
        }
        Ok(config.ids.into_iter().collect())
    }
    pub fn migrate(&self) -> Result<()> {
        let raw = self.store.read()?;
        if self.store.is_read_only()
            || raw.get("enabled").is_none()
            || raw.get("openai_enabled").is_some()
            || raw.get("grok_enabled").is_some()
        {
            return Ok(());
        }
        self.store.transaction(|data| {let config=parse_config(data);if !config.valid {return Err(Error::new(503,"托管配置无法读取"));}*data=json!({"enabled":config.enabled,"openai_enabled":config.openai,"grok_enabled":config.grok,"managed_account_ids":config.ids,"config_version":config.version,"updated_at":config.updated_at,"updated_by":config.updated_by});Ok(())})
    }
    pub fn write_locked(
        &self,
        openai: bool,
        grok: bool,
        ids: &[i64],
        coexist: Option<&BTreeSet<i64>>,
        version: i64,
        user: &str,
    ) -> Result<Config> {
        self.store.transaction(|data| {let current=parse_config(data);if !current.valid {return Err(Error::new(503,"托管配置无法读取"));}let coexist=coexist.cloned().unwrap_or_else(||current.coexist.into_iter().filter(|id|ids.contains(id)).collect());
            if coexist.iter().any(|id|!ids.contains(id)) {return Err(Error::new(422,"同步启用账号必须属于已托管 Key"));}
            *data=json!({"enabled":openai || grok,"openai_enabled":openai,"grok_enabled":grok,"managed_account_ids":ids,"coexist_account_ids":coexist,"config_version":version,"updated_at":now(),"updated_by":user});Ok(parse_config(data))
        })
    }
    pub async fn save(&self, values: &Value, user: &str) -> Result<Config> {
        self.save_expected(values, user, None).await
    }
    pub async fn save_expected(
        &self,
        values: &Value,
        user: &str,
        expected: Option<&str>,
    ) -> Result<Config> {
        let ids = managed_ids(&values["managed_account_ids"])?;
        let _guard = self.lock.lock().await;
        let rows=self.actions.queries.db.all("SELECT id,platform,type,deleted_at FROM accounts WHERE deleted_at IS NULL AND lower(platform) IN ('openai','grok') AND lower(type)='apikey' ORDER BY id",&[]).await?;
        let live: BTreeSet<_> = rows
            .iter()
            .filter(|r| live_key(r, None))
            .filter_map(|r| strict_id(&r["id"]))
            .collect();
        let stale: Vec<_> = ids.iter().filter(|id| !live.contains(id)).collect();
        if !stale.is_empty() {
            return Err(Error::new(
                422,
                format!(
                    "Key 回退保存失败：账号 {} 不是有效的 OpenAI 或 Grok apikey",
                    stale
                        .iter()
                        .map(|id| format!("#{id}"))
                        .collect::<Vec<_>>()
                        .join("、")
                ),
            ));
        }
        let current = self.load();
        if expected.is_some_and(|value| value != crate::compat::config_revision(&current.panel())) {
            return Err(Error::new(409, "设置已被其他窗口修改，请刷新后重试"));
        }
        let coexist = values
            .get("coexist_account_ids")
            .map(managed_ids)
            .transpose()?
            .map(|v| v.into_iter().collect::<BTreeSet<_>>())
            .unwrap_or_else(|| {
                current
                    .coexist
                    .iter()
                    .filter(|id| ids.contains(id))
                    .copied()
                    .collect()
            });
        let openai = values
            .get("openai_enabled")
            .or_else(|| values.get("enabled"))
            .map(crate::validation::boolean)
            .transpose()?
            .unwrap_or(current.openai);
        let grok = values
            .get("grok_enabled")
            .or_else(|| values.get("enabled"))
            .map(crate::validation::boolean)
            .transpose()?
            .unwrap_or(current.grok);
        let written = self.write_locked(
            openai,
            grok,
            &ids,
            Some(&coexist),
            current.version + 1,
            user,
        )?;
        crate::audit::write(
            &self.actions.settings.audit_path,
            "key_fallback_config_update",
            json!({"user":user,"enabled":written.enabled,"openai_enabled":written.openai,"grok_enabled":written.grok,"managed_account_ids":written.ids,"config_version":written.version}),
            false,
        );
        Ok(written)
    }
    pub async fn run(
        &self,
        at: DateTime<Utc>,
        snapshot: Option<&Value>,
        detection_gate: impl Fn(i64) -> Result<bool> + Send + Sync,
    ) -> Result<Value> {
        if self.actions.settings.read_only {
            return Err(Error::read_only());
        }
        let _guard = self.lock.lock().await;
        let config = self.load();
        let mut report = json!({"skipped":false,"desired":null,"changed_ids":[],"failed_ids":[],"reason":"","platform_reasons":{}});
        if !config.valid || !config.enabled || config.ids.is_empty() {
            report["skipped"] = true.into();
            report["reason"] = if !config.valid {
                "invalid_config"
            } else {
                "disabled_or_empty"
            }
            .into();
            return Ok(report);
        }
        if snapshot.is_none() {
            report["platform_reasons"]["openai"] = "refresh_in_progress".into();
        }
        let mut wants = json!({});
        for platform in ["openai", "grok"] {
            let mut states = json!({});
            if platform != "openai" || snapshot.is_some() {
                match self
                    .actions
                    .queries
                    .db
                    .all(
                        &format!(
                            "{} AND lower(platform)=$1 AND lower(type)='oauth' ORDER BY id",
                            credit_ownership::ACCOUNT_SELECT
                        ),
                        &[platform.into()],
                    )
                    .await
                {
                    Err(_) => report["platform_reasons"][platform] = "inventory_failed".into(),
                    Ok(rows) => {
                        for row in rows {
                            let Some(id) = strict_id(&row["id"]) else {
                                continue;
                            };
                            let state = if platform == "grok" {
                                grok_state(&row, at)
                            } else {
                                let snapshot = snapshot.unwrap();
                                let saved = latest_completed_result(
                                    &snapshot["oauth_results"][id.to_string()],
                                    &snapshot["scheduler"][id.to_string()],
                                );
                                let result = quota::latest_result(&row, &saved, at);
                                oauth_state(&row, &result, at, 3600)
                            };
                            states[id.to_string()] = state.into();
                        }
                    }
                }
            }
            wants[platform] = json!(desired(
                &states
                    .as_object()
                    .unwrap()
                    .values()
                    .map(string)
                    .collect::<Vec<_>>()
            ));
            report[if platform == "openai" {
                "oauth_states"
            } else {
                "grok_oauth_states"
            }] = states;
        }
        report["desired"] = wants["openai"].clone();
        report["desired_by_platform"] = wants.clone();
        if wants["openai"].is_null() && wants["grok"].is_null() {
            report["skipped"] = true.into();
            report["reason"] = report["platform_reasons"]
                .get("openai")
                .cloned()
                .unwrap_or(json!("keep_existing"));
            return Ok(report);
        }
        let mut order = config.ids.clone();
        if let Some(resume) = *self.resume.lock().unwrap_or_else(|e| e.into_inner()) {
            if let Some(index) = order.iter().position(|id| *id == resume) {
                order.rotate_left(index);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exhausted = false;
        let mut changed = vec![];
        let mut failed = vec![];
        for id in order {
            let latest = self.load();
            if !latest.valid || latest.version != config.version {
                report["skipped"] = true.into();
                report["reason"] = "config_changed".into();
                break;
            }
            if !latest.ids.contains(&id) {
                continue;
            }
            let Ok(row) = credit_ownership::account(&self.actions.queries.db, id).await else {
                continue;
            };
            if !live_key(&row, Some(id)) || string(&row["status"]).to_lowercase() != "active" {
                continue;
            }
            let platform = string(&row["platform"]).trim().to_lowercase();
            if !latest.platform_enabled(&platform) {
                continue;
            }
            let Some(mut enabled) = wants[&platform].as_bool() else {
                continue;
            };
            if !enabled && latest.coexist.contains(&id) {
                enabled = true;
            }
            if (row["schedulable"] == true) == enabled {
                continue;
            }
            if Instant::now() >= deadline {
                *self.resume.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
                exhausted = true;
                report["reason"] = "dispatch_budget_exhausted".into();
                break;
            }
            let _control = self.actions.locks.control(id).await;
            if enabled && detection_gate(id)? {
                continue;
            }
            let key = self.actions.queries.oauth.admin_token()?;
            let result = set_schedulable(&self.actions, id, enabled, &key, 3).await;
            if result["success"] == true {
                changed.push(id);
            } else {
                failed.push(id);
            }
            let mut event = json!({"account_id":id,"platform":platform,"schedulable":enabled,"success":result["success"]==true});
            if result["success"] != true {
                event["error_code"] = result
                    .get("error_code")
                    .cloned()
                    .unwrap_or(json!("schedulable_request_error"));
            }
            crate::audit::write(
                &self.actions.settings.audit_path,
                "key_fallback_set_schedulable",
                event,
                false,
            );
        }
        if !exhausted {
            *self.resume.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        report["changed_ids"] = json!(changed);
        report["failed_ids"] = json!(failed);
        Ok(report)
    }
    pub async fn detach_deleted_locked(&self, jobs: &mut [Value]) -> Result<()> {
        let config = self.load();
        if !config.valid {
            return Err(Error::new(503, "托管配置无法读取"));
        }
        let mut affected = BTreeSet::new();
        for job in jobs.iter() {
            affected.insert(integer(&job["account_id"]).unwrap());
            affected.extend(crate::operations::ids(&job["child_account_ids"]));
        }
        let detached: BTreeSet<_> = affected
            .iter()
            .filter(|id| config.ids.contains(id))
            .copied()
            .collect();
        if !detached.is_empty() {
            self.write_locked(
                config.openai,
                config.grok,
                &config
                    .ids
                    .iter()
                    .filter(|id| !detached.contains(id))
                    .copied()
                    .collect::<Vec<_>>(),
                None,
                config.version + 1,
                "desktop:admin",
            )?;
        }
        for id in affected {
            credit_ownership::cancel(&self.actions.queries.oauth, id)?;
        }
        for job in jobs {
            job["detached"] = detached
                .contains(&integer(&job["account_id"]).unwrap())
                .into();
        }
        Ok(())
    }
}
