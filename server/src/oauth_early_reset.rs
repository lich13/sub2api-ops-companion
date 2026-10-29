//! One quota observer per known OAuth plan, using the existing recovery workers.
use crate::{
    compat::{datetime, integer, parse_datetime, string, truthy},
    error::Result,
    oauth_admission,
    oauth_monitor::Monitor,
    oauth_queries::{QueryOptions, Source},
    quota, recovery_events, recovery_policy, recovery_rules,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use std::{borrow::Borrow, collections::BTreeMap, time::Instant};

#[derive(Default)]
pub(crate) struct State {
    representatives: BTreeMap<String, Representative>,
}
struct Representative {
    id: i64,
    credentials: String,
    baseline: Value,
    last_attempt: Option<DateTime<Utc>>,
}

pub fn pending(intent: &Value) -> bool {
    intent["kind"] == "early_reset"
        && !matches!(
            string(&intent["status"]),
            "recovered" | "cancelled" | "blocked" | "auth_failed" | "retry_exhausted"
        )
}

pub fn eligible(row: &Value, meta: &Value, at: DateTime<Utc>) -> bool {
    let card = &meta["auto_reset_credit"];
    oauth_admission::automatic_eligible(row, at)
        && parse_datetime(&row["expires_at"]).is_none_or(|expiry| expiry > at)
        && !truthy(&row["parent_account_id"])
        && !truthy(&row["extra"]["parent_account_id"])
        && !(truthy(&card["owns_pause"])
            || truthy(&card["attempt_at"])
                && !matches!(string(&card["stage"]), "finished" | "closed" | "recovered"))
        && meta["quota_query"]["auth_fingerprint"] != oauth_admission::credential_fingerprint(row)
        && (truthy(&meta["quota_query"]["auth_fingerprint"])
            || !oauth_admission::AUTH_ERRORS.contains(&string(&meta["last_error_code"])))
}

fn known_plan(row: &Value) -> Option<String> {
    let plan = quota::account_plan(row);
    matches!(
        plan.as_str(),
        "free" | "plus" | "pro" | "team" | "enterprise"
    )
    .then_some(plan)
}

fn complete_evidence(row: &Value, result: &Value) -> bool {
    if result["success"] != true
        || result["account_id"] != row["id"]
        || result["oauth_quota"]["plan_type"] != quota::account_plan(row)
    {
        return false;
    }
    let summary = quota::summary_from_result(row, result);
    let windows = quota::windows_by_key(&summary["ui_windows"]);
    quota::required_windows(&summary["plan_type"])
        .iter()
        .all(|key| {
            let window = windows.get(*key).unwrap_or(&Value::Null);
            let data = &result["data"][if *key == "codex_5h" {
                "five_hour"
            } else {
                "seven_day"
            }];
            let used = data
                .get("utilization")
                .or_else(|| data.get("used_percent"))
                .or_else(|| data.get("used"))
                .unwrap_or(&window["used_percent"]);
            quota::percent(used).is_some_and(|used| (0.0..=100.0).contains(&used))
                && parse_datetime(&window["reset_at"]).is_some()
        })
}

fn decrease(row: &Value, before: &Value, after: &Value, at: DateTime<Utc>) -> bool {
    let (Some(old_at), Some(new_at)) = (
        parse_datetime(&before["queried_at"]),
        parse_datetime(&after["queried_at"]),
    ) else {
        return false;
    };
    if old_at >= new_at
        || new_at > at
        || before["oauth_quota"]["plan_type"] != after["oauth_quota"]["plan_type"]
        || !complete_evidence(row, before)
        || !complete_evidence(row, after)
    {
        return false;
    }
    let old = quota::summary_from_result(row, before);
    let new = quota::summary_from_result(row, after);
    let previous = quota::windows_by_key(&old["ui_windows"]);
    let current = quota::windows_by_key(&new["ui_windows"]);
    quota::required_windows(&new["plan_type"])
        .iter()
        .any(|key| {
            let a = previous.get(*key).unwrap_or(&Value::Null);
            let b = current.get(*key).unwrap_or(&Value::Null);
            parse_datetime(&a["reset_at"]).is_some_and(|reset| new_at < reset)
                && quota::percent(&a["used_percent"])
                    .zip(quota::percent(&b["used_percent"]))
                    .is_some_and(|(old, new)| {
                        (0.0..=100.0).contains(&old) && (0.0..=100.0).contains(&new) && new < old
                    })
        })
}

pub fn quota_ready(row: &Value, result: &Value, meta: &Value, at: DateTime<Utc>) -> bool {
    let intent = &meta["recovery_intent"];
    intent["kind"] == "early_reset"
        && intent["plan_type"] == quota::account_plan(row)
        && intent["credential_fingerprint"] == oauth_admission::credential_fingerprint(row)
        && eligible(row, meta, at)
        && recovery_rules::change_is_safe(&intent["block_state"], row)
        && complete_evidence(row, result)
        && oauth_admission::fresh_quota(row, result, at)
        && quota::summary_from_result(row, result)
            .as_object()
            .is_some_and(|summary| {
                let summary = Value::Object(summary.clone());
                let threshold = recovery_rules::threshold_descriptor(row, &summary, at);
                recovery_rules::all_available(&summary)
                    && (!truthy(&threshold)
                        || recovery_rules::threshold_available(&summary, &threshold))
            })
        && parse_datetime(&result["queried_at"])
            .zip(parse_datetime(&intent["observed_at"]))
            .is_some_and(|(queried, detected)| queried >= detected)
        && parse_datetime(&row["overload_until"]).is_none_or(|until| until <= at)
}

impl Monitor {
    pub(crate) async fn tick_early_reset(
        &self,
        rows: &[impl Borrow<Value>],
        config: &Value,
        at: DateTime<Utc>,
    ) -> Result<()> {
        let state = self.shared_state()?;
        let enabled = config["oauth_early_reset_enabled"] == true;
        if !enabled {
            self.early_reset.lock().await.representatives.clear();
            let cancellable = |intent: &Value| {
                pending(intent)
                    && intent["waiting_reason"] != "model_verification_pending"
                    && !truthy(&intent["verification_dispatched"])
                    && !truthy(&intent["verification_receipt"])
                    && !truthy(&intent["recovery_write_dispatched"])
            };
            if state["scheduler"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .any(|m| cancellable(&m["recovery_intent"]))
            {
                self.store.transaction(|data| {
                    for meta in data["scheduler"]
                        .as_object_mut()
                        .into_iter()
                        .flat_map(|m| m.values_mut())
                    {
                        if cancellable(&meta["recovery_intent"]) {
                            meta["recovery_intent"]["status"] = "cancelled".into();
                            meta["recovery_intent"]["last_error_code"] =
                                "early_reset_disabled".into();
                        }
                    }
                    Ok(())
                })?;
            }
        }
        let limit = (integer(&config["oauth_usage_refresh_concurrency"]).unwrap_or(4)
            + integer(&config["oauth_recovery_test_concurrency"]).unwrap_or(2))
        .clamp(1, 24) as usize;
        let mut workers = self.workers.lock().await;
        for (id, meta) in state["scheduler"].as_object().into_iter().flatten() {
            let intent = &meta["recovery_intent"];
            let Ok(id) = id.parse::<i64>() else { continue };
            if !pending(intent)
                || !enabled
                    && intent["waiting_reason"] != "model_verification_pending"
                    && !truthy(&intent["verification_receipt"])
                    && !truthy(&intent["recovery_write_dispatched"])
                || workers.contains_key(&id)
                || workers.len() >= limit
                || parse_datetime(&intent["next_retry_at"]).is_some_and(|due| at < due)
            {
                continue;
            }
            let this = self.clone();
            workers.insert(
                id,
                self.actions
                    .supervisor
                    .spawn_result("oauth_early_recovery", async move {
                        this.run_early_recovery(id).await
                    }),
            );
        }
        if !enabled {
            return Ok(());
        }
        let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
        for row in rows {
            let row = row.borrow();
            let id = integer(&row["id"]).unwrap_or(0);
            if let Some(plan) =
                known_plan(row).filter(|_| eligible(row, &state["scheduler"][id.to_string()], at))
            {
                if !self.hooks.detection_held(id)? {
                    groups.entry(plan).or_default().push(row);
                }
            }
        }
        let interval = Duration::seconds(
            integer(&config["oauth_early_reset_interval_seconds"])
                .unwrap_or(60)
                .clamp(2, i64::MAX / 1000),
        );
        let mut observers = self.early_reset.lock().await;
        observers
            .representatives
            .retain(|plan, _| groups.contains_key(plan));
        for (plan, members) in groups {
            let existing = observers.representatives.get(&plan);
            if !existing.is_some_and(|old| {
                members.iter().any(|row| {
                    integer(&row["id"]) == Some(old.id)
                        && oauth_admission::credential_fingerprint(row) == old.credentials
                })
            }) {
                let row = members
                    .iter()
                    .min_by_key(|row| {
                        let id = integer(&row["id"]).unwrap_or(0);
                        let result =
                            quota::latest_result(row, &state["oauth_results"][id.to_string()], at);
                        let descriptor = recovery_rules::quota_descriptor(
                            &quota::summary_from_result(row, &result),
                            at,
                        );
                        (
                            !parse_datetime(&descriptor["due_at"]).is_some_and(|due| due > at),
                            id,
                        )
                    })
                    .expect("nonempty plan group");
                observers.representatives.insert(
                    plan.clone(),
                    Representative {
                        id: integer(&row["id"]).unwrap(),
                        credentials: oauth_admission::credential_fingerprint(row),
                        baseline: Value::Null,
                        last_attempt: None,
                    },
                );
            }
            if state["scheduler"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .any(|meta| {
                    pending(&meta["recovery_intent"])
                        && meta["recovery_intent"]["plan_type"] == plan
                })
            {
                continue;
            }
            let observer = observers.representatives.get_mut(&plan).unwrap();
            if workers.contains_key(&observer.id)
                || workers.len() >= limit
                || observer
                    .last_attempt
                    .is_some_and(|last| at >= last && at.signed_duration_since(last) < interval)
            {
                continue;
            }
            observer.last_attempt = Some(at);
            let id = observer.id;
            let this = self.clone();
            workers.insert(
                id,
                self.actions
                    .supervisor
                    .spawn_result("oauth_early_probe", async move {
                        this.run_early_probe(id, plan).await
                    }),
            );
        }
        Ok(())
    }

    async fn run_early_probe(&self, id: i64, plan: String) -> Result<Value> {
        let row = self.row(id).await?;
        let state = self.shared_state()?;
        if self.config.current()?["oauth_early_reset_enabled"] != true
            || known_plan(&row).as_ref() != Some(&plan)
            || !eligible(&row, &state["scheduler"][id.to_string()], self.now())
            || self.hooks.detection_held(id)?
        {
            return Ok(json!({"skipped":true}));
        }
        let Ok(_permit) = self.budget.clone().try_acquire_owned() else {
            return Ok(json!({"deferred":true}));
        };
        let Ok(_lease) = self.lease(&row) else {
            return Ok(json!({"deferred":true}));
        };
        let generation = self.generation(id)?;
        let result = self
            .queries
            .query(
                id,
                &self.store.admin_token()?,
                QueryOptions {
                    source: Source::EarlyReset,
                    reason: "early_reset_probe".into(),
                    at: self.now(),
                    requested_at: Some(Instant::now()),
                    generation: Some(generation),
                    ..QueryOptions::default()
                },
            )
            .await;
        if result["success"] != true
            || self.generation(id)? != generation
            || self.config.current()?["oauth_early_reset_enabled"] != true
        {
            return Ok(result);
        }
        let fresh = self.row(id).await?;
        if !eligible(
            &fresh,
            &self.shared_state()?["scheduler"][id.to_string()],
            self.now(),
        ) || self.hooks.detection_held(id)?
            || known_plan(&fresh).as_ref() != Some(&plan)
            || oauth_admission::credential_fingerprint(&fresh)
                != oauth_admission::credential_fingerprint(&row)
        {
            return Ok(result);
        }
        let triggered = {
            let mut observers = self.early_reset.lock().await;
            let Some(observer) = observers.representatives.get_mut(&plan).filter(|entry| {
                entry.id == id
                    && entry.credentials == oauth_admission::credential_fingerprint(&fresh)
            }) else {
                return Ok(result);
            };
            let changed = decrease(&fresh, &observer.baseline, &result, self.now());
            observer.baseline = result.clone();
            changed
        };
        if triggered {
            let rows = self.inventory(true).await?;
            let fingerprint = format!("early_reset:{plan}:{id}:{}", string(&result["queried_at"]));
            let current = self.shared_state()?;
            let selected = rows
                .iter()
                .filter(|row| known_plan(row).as_ref() == Some(&plan))
                .filter_map(|row| {
                    let id = integer(&row["id"])?;
                    let meta = &current["scheduler"][id.to_string()];
                    (eligible(row, meta, self.now())
                        && !self.hooks.detection_held(id).unwrap_or(true))
                    .then_some((id, row, self.generation(id).ok()?))
                })
                .collect::<Vec<_>>();
            self.store.transaction(|data| {
                for (id, row, generation) in &selected {
                    let meta = &mut data["scheduler"][id.to_string()];
                    let previous = &meta["recovery_intent"];
                    if integer(&meta["manual_control"]["generation"]).unwrap_or(0) != *generation
                        || !eligible(row, meta, self.now())
                        || pending(previous)
                        || (truthy(&previous["verification_dispatched"])
                            || truthy(&previous["verification_receipt"])
                            || truthy(&previous["recovery_write_dispatched"]))
                            && !matches!(
                                string(&previous["status"]),
                                "recovered" | "cancelled" | "retry_exhausted"
                            )
                    {
                        continue;
                    }
                    meta["recovery_intent"] = recovery_events::new_intent(
                        &json!({
                            "kind":"early_reset", "plan_type":plan, "fingerprint":fingerprint,
                            "observed_at":result["queried_at"], "due_at":result["queried_at"],
                            "early_reset_detected":true, "detected_at":datetime(self.now()),
                            "credential_fingerprint":oauth_admission::credential_fingerprint(row),
                            "block_state":crate::recovery_verification::state_block(row),
                            "manual_generation":generation,
                            "window_keys":quota::required_windows(&json!(plan)),
                        }),
                        "waiting_quota",
                        Some(self.now()),
                    );
                }
                Ok(())
            })?;
        }
        Ok(result)
    }

    async fn run_early_recovery(&self, id: i64) -> Result<Value> {
        let state = self.shared_state()?;
        let meta = &state["scheduler"][id.to_string()];
        let intent = meta["recovery_intent"].clone();
        let generation = self.generation(id)?;
        if !pending(&intent) {
            return Ok(json!({"skipped":true}));
        }
        let method_generation = integer(&meta["recovery_method_generation"]).unwrap_or(0);
        let row = match self.row(id).await {
            Ok(row) => row,
            Err(error) if error.status.as_u16() == 404 => {
                return self.blocked_intent(
                    id,
                    &intent,
                    "account_deleted",
                    generation,
                    method_generation,
                )
            }
            Err(error) => return Err(error),
        };
        let config = self.config.current()?;
        let Some(method) = recovery_policy::intent_method(&config, id, &intent) else {
            return Ok(json!({"skipped":true}));
        };
        if !eligible(&row, meta, self.now())
            || self.hooks.detection_held(id)?
            || intent["manual_generation"] != generation
            || intent["plan_type"] != quota::account_plan(&row)
            || intent["credential_fingerprint"] != oauth_admission::credential_fingerprint(&row)
        {
            return self.blocked_intent(
                id,
                &intent,
                "recovery_state_changed",
                generation,
                method_generation,
            );
        }
        let Ok(_permit) = self.budget.clone().try_acquire_owned() else {
            return Ok(json!({"deferred":true}));
        };
        let Ok(lease) = self.lease(&row) else {
            return Ok(json!({"deferred":true}));
        };
        let saved = &state["oauth_results"][id.to_string()];
        let result = if quota_ready(&row, saved, meta, self.now()) {
            saved.clone()
        } else if config["oauth_early_reset_enabled"] != true {
            return self.blocked_intent(
                id,
                &intent,
                "early_reset_disabled",
                generation,
                method_generation,
            );
        } else {
            self.queries
                .query(
                    id,
                    &self.store.admin_token()?,
                    QueryOptions {
                        source: Source::EarlyReset,
                        reason: "early_reset_recovery".into(),
                        at: self.now(),
                        requested_at: Some(Instant::now()),
                        generation: Some(generation),
                        ..QueryOptions::default()
                    },
                )
                .await
        };
        if result["skipped"] == true {
            return Ok(result);
        }
        let current = self.row(id).await?;
        let state = self.shared_state()?;
        if !quota_ready(
            &current,
            &result,
            &state["scheduler"][id.to_string()],
            self.now(),
        ) {
            return self.blocked_intent(
                id,
                &intent,
                if result["success"] == true {
                    "quota_not_released"
                } else {
                    "quota_query_failed"
                },
                generation,
                method_generation,
            );
        }
        self.verify_recovery(
            id,
            &intent,
            &result,
            generation,
            method_generation,
            method,
            lease,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn early_decrease_requires_matching_complete_evidence_before_the_old_deadline() {
        let at = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let row = json!({"id":1,"credentials":{"plan_type":"plus"}});
        let evidence = |when, used| {
            let data = json!({"five_hour":{"utilization":used,"resets_at":datetime(at+Duration::hours(5))},"seven_day":{"utilization":25,"resets_at":datetime(at+Duration::days(7))}});
            json!({"account_id":1,"success":true,"queried_at":datetime(when),"oauth_quota":quota::from_usage_data(&data,&row,when)})
        };
        let baseline = evidence(at, 100);
        let now = at + Duration::seconds(60);
        let restored = evidence(now, 15);
        assert!(decrease(&row, &baseline, &restored, now));
        for (before, after, check_at) in [
            (Value::Null, restored.clone(), now),
            (baseline.clone(), evidence(now, 100), now),
            (baseline.clone(), evidence(at, 15), now),
            (baseline.clone(), restored.clone(), at),
            (
                baseline.clone(),
                evidence(at + Duration::hours(5), 15),
                at + Duration::hours(5),
            ),
            (
                baseline.clone(),
                {
                    let mut v = restored.clone();
                    v["success"] = false.into();
                    v
                },
                now,
            ),
            (
                baseline.clone(),
                {
                    let mut v = restored.clone();
                    v["oauth_quota"]["plan_type"] = "pro".into();
                    v
                },
                now,
            ),
            (
                baseline.clone(),
                {
                    let mut v = restored.clone();
                    v["oauth_quota"]["ui_windows"] = json!([]);
                    v
                },
                now,
            ),
        ] {
            assert!(!decrease(&row, &before, &after, check_at));
        }
        for plan in ["", "oauth", "unknown", "unrecognized-plan"] {
            assert!(known_plan(&json!({"credentials":{"plan_type":plan}})).is_none());
        }
    }
}

#[cfg(test)]
mod eligibility_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn early_recovery_preserves_manual_expiry_auth_and_credit_holds() {
        let at = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let row = json!({"id":1,"platform":"openai","type":"oauth","status":"active","schedulable":true,"credentials":{"plan_type":"plus","access_token":"fixture"}});
        assert!(eligible(&row, &json!({}), at));
        for (key, value) in [
            ("schedulable", json!(false)),
            ("status", json!("disabled")),
            ("expires_at", json!(datetime(at))),
            ("parent_account_id", json!(2)),
            ("deleted_at", json!(datetime(at))),
        ] {
            let mut changed = row.clone();
            changed[key] = value;
            assert!(!eligible(&changed, &json!({}), at), "{key}");
        }
        for meta in [
            json!({"auto_reset_credit":{"owns_pause":true}}),
            json!({"auto_reset_credit":{"attempt_at":datetime(at),"stage":"testing"}}),
            json!({"quota_query":{"auth_fingerprint":oauth_admission::credential_fingerprint(&row)}}),
        ] {
            assert!(!eligible(&row, &meta, at));
        }
        let config = json!({"oauth_early_reset_enabled":true,"oauth_recovery_monitor_enabled":false,"oauth_recovery_connection_account_ids":[],"oauth_recovery_model_account_ids":[1]});
        let intent = json!({"kind":"early_reset"});
        assert_eq!(
            recovery_policy::intent_method(&config, 1, &intent),
            Some("model")
        );
        assert_eq!(
            recovery_policy::intent_method(&config, 2, &intent),
            Some("connection")
        );
        let mut off = config;
        off["oauth_early_reset_enabled"] = false.into();
        assert_eq!(recovery_policy::intent_method(&off, 1, &intent), None);
        assert_eq!(
            recovery_policy::intent_method(
                &off,
                1,
                &json!({"kind":"early_reset","waiting_reason":"model_verification_pending"})
            ),
            Some("model")
        );
    }
}
