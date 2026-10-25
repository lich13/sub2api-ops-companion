use crate::{
    compat::{datetime, parse_datetime, string, truthy},
    error::Result,
    oauth_state::OAuthState,
    settings::daily_time,
};
use chrono::{DateTime, Duration, FixedOffset, Timelike, Utc};
use serde_json::{json, Value};
pub fn next_time(at: DateTime<Utc>, time: &str) -> Result<DateTime<Utc>> {
    let value = daily_time(&json!(time))?;
    let hour = value[..2].parse::<u32>().unwrap();
    let minute = value[3..].parse::<u32>().unwrap();
    let local = at.with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap());
    let mut target = local
        .with_hour(hour)
        .unwrap()
        .with_minute(minute)
        .unwrap()
        .with_second(0)
        .unwrap()
        .with_nanosecond(0)
        .unwrap();
    if target <= local {
        target += Duration::days(1);
    }
    Ok(target.with_timezone(&Utc))
}
pub fn eligible(row: &Value, at: DateTime<Utc>) -> bool {
    if !row.is_object()
        || (!row["deleted_at"].is_null() && row["deleted_at"] != "")
        || !string(&row["platform"]).eq_ignore_ascii_case("openai")
        || !string(&row["type"]).eq_ignore_ascii_case("oauth")
        || row["status"] != "active"
        || row["schedulable"] != true
    {
        return false;
    }
    if [
        "temp_unschedulable_until",
        "rate_limit_reset_at",
        "overload_until",
    ]
    .iter()
    .any(|field| {
        !row[field].is_null()
            && row[field] != ""
            && parse_datetime(&row[field]).is_none_or(|t| t > at)
    }) {
        return false;
    }
    let reason = string(&row["temp_unschedulable_reason"]).to_lowercase();
    if ["manual", "telegram", "人工", "手动"]
        .iter()
        .any(|word| reason.contains(word))
    {
        return false;
    }
    if truthy(&row["rate_limited_at"]) && parse_datetime(&row["rate_limit_reset_at"]).is_none() {
        return false;
    }
    !(truthy(&row["auto_pause_on_expired"])
        && truthy(&row["expires_at"])
        && parse_datetime(&row["expires_at"]).is_none_or(|t| t <= at))
}
#[derive(Clone)]
pub struct Schedule {
    store: OAuthState,
}
fn daily(state: &mut Value) -> &mut Value {
    if !state["daily_test"].is_object() {
        state["daily_test"] = json!({});
    }
    if !state["daily_test"]["batches"].is_object() {
        state["daily_test"]["batches"] = json!({});
    }
    &mut state["daily_test"]
}
fn configuration(state: &mut Value, config: &Value, at: DateTime<Utc>) -> Result<()> {
    let enabled = config["oauth_daily_test_enabled"].as_bool().unwrap_or(true);
    let time = daily_time(
        config
            .get("oauth_daily_test_time")
            .unwrap_or(&json!("05:00")),
    )?;
    state["enabled"] = enabled.into();
    state["time"] = time.clone().into();
    state["next_run_at"] = if enabled {
        datetime(next_time(at, &time)?)
    } else {
        String::new()
    }
    .into();
    Ok(())
}
impl Schedule {
    pub fn new(store: OAuthState) -> Self {
        Self { store }
    }
    pub fn startup(&self, config: &Value, at: DateTime<Utc>) -> Result<()> {
        if self.store.0.is_read_only() {
            self.store.shared_snapshot()?;
            return Ok(());
        }
        self.store.transaction(|state| {
            let state = daily(state);
            for batch in state["batches"].as_object_mut().unwrap().values_mut() {
                if matches!(string(&batch["status"]), "queued" | "running") {
                    batch["status"] = "interrupted".into();
                    batch["completed_at"] = datetime(at).into();
                }
            }
            configuration(state, config, at)
        })
    }
    pub fn tick(&self, config: &Value, at: DateTime<Utc>) -> Result<()> {
        if self.store.0.is_read_only() {
            return Ok(());
        }
        let enabled = config["oauth_daily_test_enabled"].as_bool().unwrap_or(true);
        let time = daily_time(
            config
                .get("oauth_daily_test_time")
                .unwrap_or(&json!("05:00")),
        )?;
        let snapshot = self.store.shared_snapshot()?;
        let state = &snapshot["daily_test"];
        if state["enabled"] == enabled
            && state["time"] == time
            && (!enabled || parse_datetime(&state["next_run_at"]).is_none_or(|t| at < t))
        {
            return Ok(());
        }
        self.store.transaction(|state| {let state=daily(state);if state["enabled"]!=enabled || state["time"]!=time {return configuration(state,config,at);}let Some(target)=parse_datetime(&state["next_run_at"]) else{return Ok(());};if !enabled || at<target {return Ok(());}
            let date=target.with_timezone(&FixedOffset::east_opt(8*3600).unwrap()).date_naive().to_string();let batches=&mut state["batches"];let scheduled=datetime(target);
            if batches.get(&date).is_none() || batches[&date]["status"]=="missed" && batches[&date]["scheduled_at"]!=scheduled {batches[&date]=json!({"date":date,"scheduled_at":scheduled,"status":if at<target+Duration::minutes(1) {"queued"} else {"missed"},"accounts":{}});}
            while batches.as_object().unwrap().len()>60 {let key=batches.as_object().unwrap().keys().min().unwrap().clone();batches.as_object_mut().unwrap().shift_remove(&key);}
            state["next_run_at"]=datetime(next_time(at,&time)?).into();Ok(())})
    }
    pub fn claim(&self, config: &Value, at: DateTime<Utc>) -> Result<Option<Value>> {
        if self.store.0.is_read_only() {
            return Err(crate::error::Error::read_only());
        }
        if !self.store.shared_snapshot()?["daily_test"]["batches"]
            .as_object()
            .into_iter()
            .flat_map(|batches| batches.values())
            .any(|batch| batch["status"] == "queued")
        {
            return Ok(None);
        }
        self.store.transaction(|state| {
            let state = daily(state);
            if config["oauth_daily_test_enabled"] == false {
                for batch in state["batches"].as_object_mut().unwrap().values_mut() {
                    if batch["status"] == "queued" {
                        batch["status"] = "cancelled".into();
                        batch["completed_at"] = datetime(at).into();
                    }
                }
                return Ok(None);
            }
            for batch in state["batches"].as_object_mut().unwrap().values_mut() {
                if batch["status"] == "queued" {
                    batch["status"] = "running".into();
                    batch["started_at"] = datetime(at).into();
                    return Ok(Some(batch.clone()));
                }
            }
            Ok(None)
        })
    }
    pub fn save_batch(&self, batch: &Value, pending: &Value) -> Result<()> {
        self.store.transaction(|state| {
            daily(state)["batches"][string(&batch["date"])] = batch.clone();
            if let Some(events) = pending.as_object() {
                state["pending_events"]
                    .as_object_mut()
                    .unwrap()
                    .extend(events.clone());
            }
            Ok(())
        })
    }
}
