use crate::{
    compat::{datetime, integer, parse_datetime, truthy},
    db::Param,
    error::{Error, Result},
    model_detection::Detection,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn smart(value: &Value) -> bool {
    value["schedule_mode"] == "smart"
}

pub fn periodic(job: &Value) -> bool {
    job["automatic"] == true
        && job["triggers"]
            .as_array()
            .is_some_and(|triggers| triggers.len() == 1 && triggers[0] == "scheduled")
}

pub fn interval(requests: u64, minimum: i64) -> Option<i64> {
    let minutes = match requests {
        0 => return None,
        1..=2 => 30,
        3..=9 => 25,
        10..=29 => 20,
        30..=59 => 15,
        60..=119 => 10,
        _ => 5,
    };
    Some(minutes.max(minimum.clamp(5, 30)))
}

pub fn effective_interval(value: &Value) -> Option<i64> {
    if smart(value) {
        value["recent_request_count"]
            .as_u64()
            .and_then(|count| interval(count, integer(&value["min_interval_minutes"]).unwrap_or(5)))
    } else {
        integer(&value["interval_minutes"])
    }
}

pub fn pause_reason(value: &Value) -> &'static str {
    if value["recent_request_count"].as_u64() == Some(0) {
        "最近一小时无业务调用，已暂停定时检测"
    } else {
        "调用统计暂不可用，等待重新读取"
    }
}

impl Detection {
    pub(crate) async fn refresh_call_frequency(&self) -> Result<()> {
        let _guard = self.frequency_lock.lock().await;
        let state = self.store.read()?;
        let accounts: Vec<_> = state["accounts"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, value)| value["enabled"] == true && smart(value))
            .collect();
        let at = self.now();
        if accounts.is_empty()
            || accounts.iter().all(|(_, value)| {
                parse_datetime(&value["frequency_sampled_at"])
                    .is_some_and(|sample| sample <= at && at - sample < Duration::minutes(1))
            })
        {
            return Ok(());
        }
        let ids = accounts
            .iter()
            .map(|(id, _)| id.parse::<i64>().map_err(|_| Error::internal()))
            .collect::<Result<Vec<_>>>()?;
        // Only gateway business usage is recorded in usage_logs. Companion model
        // tests call the provider directly and do not contribute to this count.
        let rows = self.actions.queries.db.all(
            "SELECT account_id, count(*) AS requests FROM usage_logs WHERE account_id = ANY($1) AND created_at > $2 AND created_at <= $3 GROUP BY account_id",
            &[Param::Ints(ids.clone()), Param::Time(Some(at - Duration::hours(1))), Param::Time(Some(at))],
        ).await;
        let counts = rows.map(|rows| {
            rows.into_iter()
                .filter_map(|row| Some((integer(&row["account_id"])?, row["requests"].as_u64()?)))
                .collect::<BTreeMap<_, _>>()
        });
        self.store.transaction(|state| {
            for id in &ids {
                let value = &mut state["accounts"][id.to_string()];
                if value["enabled"] == true && smart(value) {
                    value["frequency_sampled_at"] = datetime(at).into();
                    value["recent_request_count"] = counts
                        .as_ref()
                        .ok()
                        .map(|counts| json!(counts.get(id).copied().unwrap_or(0)))
                        .unwrap_or(Value::Null);
                }
            }
            Ok(())
        })
    }

    pub(crate) fn smart_due(&self, id: i64, value: &Value) -> Result<Option<DateTime<Utc>>> {
        let Some(minutes) = effective_interval(value) else {
            return Ok(None);
        };
        let anchor = parse_datetime(&value["schedule_anchor_at"])
            .or_else(|| parse_datetime(&value["last_result"]["completed_at"]))
            .unwrap_or_else(|| self.now());
        let due = anchor + Duration::minutes(minutes);
        Ok(Some(due.max(
            parse_datetime(&self.next_allowed_at(id)?).unwrap_or(due),
        )))
    }

    pub(crate) fn update_smart_schedule(
        &self,
        id: i64,
        value: &Value,
    ) -> Result<Option<DateTime<Utc>>> {
        let interval = effective_interval(value);
        let resuming = value["schedule_idle_paused"] == true && interval.is_some();
        let due = if resuming {
            Some(
                (self.now() + Duration::minutes(interval.unwrap()))
                    .max(parse_datetime(&self.next_allowed_at(id)?).unwrap_or_else(|| self.now())),
            )
        } else {
            self.smart_due(id, value)?
        };
        self.store.transaction(|state| {
            let current = &mut state["accounts"][id.to_string()];
            if current["generation"] != value["generation"]
                || !smart(current)
                || current["enabled"] != true
            {
                return Ok(());
            }
            if resuming || current["schedule_anchor_at"].is_null() {
                current["schedule_anchor_at"] = datetime(self.now()).into();
            }
            if interval.is_none() && value["recent_request_count"].as_u64() == Some(0) {
                current["schedule_idle_paused"] = true.into();
            } else if resuming {
                current["schedule_idle_paused"] = false.into();
            }
            current["next_at"] = due.map(datetime).map_or(Value::Null, Value::from);
            current["status"] = if due.is_some() { "waiting" } else { "paused" }.into();
            current["reason"] = if due.is_some() {
                ""
            } else {
                pause_reason(value)
            }
            .into();
            Ok(())
        })?;
        Ok(due)
    }

    pub(crate) async fn check_smart_dispatch(&self, job: &Value) -> Result<()> {
        if !periodic(job) || truthy(&job["started_at"]) {
            return Ok(());
        }
        let id = integer(&job["account_id"]).ok_or_else(Error::invalid)?;
        if !smart(&self.control(id)?) {
            return Ok(());
        }
        self.refresh_call_frequency().await?;
        let value = self.control(id)?;
        let due = self.smart_due(id, &value)?;
        if due.is_none_or(|due| due > self.now()) {
            let message = if due.is_none() {
                pause_reason(&value)
            } else {
                "检测周期已调整，等待下次执行"
            };
            return Err(Error::new(
                409,
                json!({"code":"smart_schedule_deferred","message":message}),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn six_frequency_tiers_respect_the_account_floor() {
        for (count, expected) in [
            (0, None),
            (1, Some(30)),
            (2, Some(30)),
            (3, Some(25)),
            (9, Some(25)),
            (10, Some(20)),
            (29, Some(20)),
            (30, Some(15)),
            (59, Some(15)),
            (60, Some(10)),
            (119, Some(10)),
            (120, Some(5)),
            (10000, Some(5)),
        ] {
            assert_eq!(interval(count, 5), expected);
            assert_eq!(interval(count, 17), expected.map(|n| n.max(17)));
            assert_eq!(interval(count, 30), expected.map(|_| 30));
        }
    }
    #[test]
    fn error_clues_and_manual_tests_are_not_periodic() {
        assert!(periodic(
            &json!({"automatic":true,"triggers":["scheduled"]})
        ));
        assert!(!periodic(
            &json!({"automatic":false,"triggers":["scheduled"]})
        ));
        assert!(!periodic(&json!({"automatic":true,"triggers":["error:7"]})));
        assert!(!periodic(
            &json!({"automatic":true,"triggers":["scheduled","error:7"]})
        ));
    }
}
