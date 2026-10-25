use super::*;
use crate::{compat::parse_datetime, credit_rules, quota};
use chrono::{DateTime, Duration as TimeDelta, Timelike, Utc};

pub struct CycleObservation {
    pub windows: Vec<Value>,
    pub stopped: bool,
}

/// Use the same current-window quota and upstream-limit evidence as credit recovery,
/// independently of whether automatic credit consumption is enabled.
pub fn observe_cycle(
    row: &Value,
    saved: &Value,
    evidence: &Value,
    at: DateTime<Utc>,
) -> Result<CycleObservation> {
    if row["platform"] != "openai" || row["type"] != "oauth" {
        return Err(Error::new(422, "持续预约仅支持 OpenAI OAuth 账号"));
    }
    let summary = quota::summary_from_result(row, saved);
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    let seven = windows.get("codex_7d").unwrap_or(&Value::Null);
    if saved["success"] != true
        || parse_datetime(&saved["queried_at"]).is_none_or(|stamp| stamp > at)
        || quota::percent(&seven["used_percent"]).is_none()
        || parse_datetime(&seven["reset_at"]).is_none()
    {
        return Err(Error::new(
            422,
            "无法确认 7d 额度周期，请查询用量后重新预约",
        ));
    }
    Ok(CycleObservation {
        windows: windows.into_iter().map(|(_, window)| window).collect(),
        stopped: credit_rules::depletion_evidence(row, saved, evidence, at).is_object(),
    })
}

pub(super) fn next_cycle(
    observation: &CycleObservation,
    at: DateTime<Utc>,
    previous: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    observation
        .windows
        .iter()
        .filter_map(|window| {
            quota::percent(&window["used_percent"])?;
            let reset = parse_datetime(&window["reset_at"])?;
            let rounded =
                reset.with_nanosecond(0)? + TimeDelta::seconds(i64::from(reset.nanosecond() != 0));
            let due = rounded.checked_add_signed(TimeDelta::seconds(3))?;
            (due > at && previous.is_none_or(|previous| due > previous)).then_some(due)
        })
        .min()
}

pub fn repeat_open(job: &Value) -> bool {
    job["repeat"] == "quota_cycle"
        && matches!(
            string(&job["repeat_state"]),
            "active" | "needs_confirmation"
        )
}

pub(super) fn pending(job: &Value) -> bool {
    PENDING.contains(&string(&job["status"])) || repeat_open(job)
}

const DEPLETED: &str = "7d 额度已满 100% 且存在有效上游 429，连续预约已停止";

fn leaf(data: &Value, jid: &str) -> Result<String> {
    let mut current = jid;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return Err(Error::new(503, "连续预约关联无法读取"));
        }
        let job = data["jobs"]
            .get(current)
            .ok_or_else(|| Error::new(404, "操作不存在"))?;
        match job["next_operation_id"].as_str() {
            Some(next) => current = next,
            None => return Ok(current.to_owned()),
        }
    }
}

impl Operations {
    pub(super) fn stop_repeat(&self, jid: &str, reason: &str) -> Result<Value> {
        let job = self.store.transaction(|data| {
            let current = leaf(data, jid)?;
            let job = &mut data["jobs"][&current];
            if repeat_open(job) {
                job["repeat_state"] = "stopped".into();
                job["repeat_reason"] = reason.into();
                if matches!(string(&job["status"]), "queued" | "needs_confirmation") {
                    job["status"] = "cancelled".into();
                    job["reason"] = reason.into();
                }
                job["updated_at"] = now().into();
            }
            Ok(job.clone())
        })?;
        Ok(self.public(&job, false, 0))
    }

    pub(super) fn pause_repeat(&self, jid: &str, reason: &str) -> Result<()> {
        self.store.transaction(|data| {
            let job = &mut data["jobs"][jid];
            if job["repeat_state"] == "active" {
                job["repeat_state"] = "needs_confirmation".into();
                job["repeat_reason"] = reason.into();
                if job["status"] == "queued" {
                    job["status"] = "needs_confirmation".into();
                    job["reason"] = reason.into();
                }
                job["updated_at"] = now().into();
            }
            Ok(())
        })
    }

    pub(super) async fn cycle_allowed(&self, job: &Value, refresh: bool) -> Result<bool> {
        if job["repeat"] != "quota_cycle" {
            return Ok(true);
        }
        let jid = string(&job["id"]);
        if job["repeat_state"] != "active" {
            return Ok(false);
        }
        match self
            .backend
            .cycle_observation(
                integer(&job["account_id"]).unwrap(),
                &self.key(jid)?,
                refresh,
            )
            .await
        {
            Ok(observation) if observation.stopped => {
                self.stop_repeat(jid, DEPLETED)?;
                Ok(false)
            }
            Ok(_) => Ok(true),
            Err(error) if error.detail["code"] == "account_busy" => Ok(false),
            Err(error) => {
                self.pause_repeat(jid, &crate::security::safe_error(&error.detail, 240))?;
                Ok(false)
            }
        }
    }

    pub(super) async fn prepare_cycles(&self, data: &Value) -> Result<bool> {
        let mut prepared = false;
        for job in data["jobs"]
            .as_object()
            .unwrap()
            .values()
            .filter(|job| job["repeat_state"] == "active")
        {
            if job["status"] == "queued" {
                self.cycle_allowed(job, false).await?;
                prepared = true;
            } else if job["status"] == "completed" {
                self.advance_cycle(job).await?;
                prepared = true;
            }
        }
        Ok(prepared)
    }

    async fn advance_cycle(&self, previous: &Value) -> Result<()> {
        let _submit = self.submit_lock.lock().await;
        let jid = string(&previous["id"]);
        if self.store.read_shared()?["jobs"][jid]["repeat_state"] != "active" {
            return Ok(());
        }
        if !self.cycle_allowed(previous, false).await? {
            return Ok(());
        }
        let aid = integer(&previous["account_id"]).unwrap();
        let key = self.key(jid)?;
        let observation = match self.backend.cycle_observation(aid, &key, true).await {
            Ok(observation) => observation,
            Err(error) if error.detail["code"] == "account_busy" => return Ok(()),
            Err(error) => {
                return self.pause_repeat(jid, &crate::security::safe_error(&error.detail, 240))
            }
        };
        if observation.stopped {
            self.stop_repeat(jid, DEPLETED)?;
            return Ok(());
        }
        let Some(due) = next_cycle(
            &observation,
            self.backend.now(),
            parse_datetime(&previous["scheduled_at"]),
        ) else {
            return self.pause_repeat(jid, "暂无可用的新周期重置时间，请查询用量后重新预约");
        };
        let next_id = id();
        let mut next = json!({"id":next_id,"account_id":aid,"account_name":previous["account_name"],
            "action":"test","payload":previous["payload"],"expected_version":previous["expected_version"],
            "client_id":previous["client_id"],"parent_account_id":previous["parent_account_id"],
            "created_at":now(),"updated_at":now(),"status":"queued","reason":"等待预约时间","result":null,
            "scheduled_at":crate::compat::datetime(due),"repeat":"quota_cycle",
            "repeat_series_id":previous["repeat_series_id"],"repeat_state":"active"});
        next["payload"]["prompt"] = "".into();
        let inserted = self.store.transaction(|data| {
            let old = &mut data["jobs"][jid];
            if old["repeat_state"] != "active" || old["status"] != "completed" {
                return Ok(false);
            }
            old["repeat_state"] = "continued".into();
            old["next_operation_id"] = next_id.clone().into();
            data["jobs"][&next_id] = next;
            Ok(true)
        })?;
        if inserted {
            self.keys
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(next_id, key);
        }
        Ok(())
    }

    pub(super) async fn initialize_cycle(&self, job: &mut Value, key: &str) -> Result<()> {
        job["repeat"] = "quota_cycle".into();
        job["repeat_series_id"] = job["id"].clone();
        job["repeat_state"] = "active".into();
        if job["status"] == "needs_confirmation" {
            job["repeat_state"] = "needs_confirmation".into();
            job["repeat_reason"] = job["reason"].clone();
            return Ok(());
        }
        let observation = self
            .backend
            .cycle_observation(integer(&job["account_id"]).unwrap(), key, true)
            .await;
        let reason = match observation {
            Ok(observation) if observation.stopped => {
                job["repeat_state"] = "stopped".into();
                job["status"] = "cancelled".into();
                DEPLETED.to_owned()
            }
            Ok(observation) => match next_cycle(&observation, self.backend.now(), None) {
                Some(due) => {
                    job["scheduled_at"] = crate::compat::datetime(due).into();
                    job["reason"] = "等待预约时间".into();
                    return Ok(());
                }
                None => "暂无可用的周期重置时间，请查询用量后重新预约".into(),
            },
            Err(error) => crate::security::safe_error(&error.detail, 240),
        };
        if job["repeat_state"] != "stopped" {
            job["repeat_state"] = "needs_confirmation".into();
            job["status"] = "needs_confirmation".into();
        }
        job["reason"] = reason.clone().into();
        job["repeat_reason"] = reason.into();
        Ok(())
    }
}
