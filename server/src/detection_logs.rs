//! Read-only projections of automatic test results. These summaries never drive automation.
use crate::{
    compat::{datetime, integer, parse_datetime, string},
    error::{Error, Result},
    model_detection::Detection,
    model_downgrade::{is_downgrade, valid_report},
    security::safe_error,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

const TERMINAL: &[&str] = &[
    "completed",
    "failed",
    "cancelled",
    "interrupted",
    "needs_confirmation",
];

pub(crate) fn valid_summary(value: &Value) -> bool {
    if value.is_null() {
        return true;
    }
    value.is_object()
        && crate::verdict_policy::validate(value).is_ok()
        && ["outcome", "first_prediction", "prediction"]
            .iter()
            .all(|key| value.get(*key).is_none_or(|v| v.is_null() || v.is_string()))
        && [
            "analysis_available",
            "first_downgrade",
            "aggregate_downgrade",
        ]
        .iter()
        .all(|key| {
            value
                .get(*key)
                .is_none_or(|v| v.is_null() || v.is_boolean())
        })
        && ["probability", "first_probability"].iter().all(|key| {
            value.get(*key).is_none_or(|v| {
                v.is_null()
                    || v.as_f64()
                        .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
            })
        })
        && value
            .get("used_outputs")
            .is_none_or(|v| v.is_null() || v.is_u64())
        && [
            "disposition",
            "recovery",
            "verification",
            "restoration",
            "notification",
        ]
        .iter()
        .all(|key| {
            value.get(*key).is_none_or(|v| {
                v.is_null()
                    || v.is_object()
                        && ["status", "reason"]
                            .iter()
                            .all(|field| v.get(*field).is_none_or(|f| f.is_string()))
                        && v.get("basis").is_none_or(|f| {
                            f.is_null()
                                || matches!(
                                    f.as_str(),
                                    Some("first_group" | "aggregate" | "sample_family")
                                )
                        })
                        && ["marked", "schedule_verified", "mark_cleared"]
                            .iter()
                            .all(|field| {
                                v.get(*field).is_none_or(|f| f.is_null() || f.is_boolean())
                            })
                        && ["completed_at", "delivered_at"].iter().all(|field| {
                            v.get(*field)
                                .is_none_or(|f| f.is_null() || parse_datetime(f).is_some())
                        })
            })
        })
}

fn text(value: &Value, limit: usize) -> Value {
    safe_error(value, limit).into()
}
fn probability(value: &Value) -> Value {
    value
        .as_f64()
        .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
        .map_or(Value::Null, |n| json!(n))
}
pub fn receipt(value: &Value) -> Value {
    if !value.is_object() {
        return Value::Null;
    }
    json!({"status":text(&value["status"],40),"marked":value["marked"].as_bool(),
        "group_indexes":value["group_indexes"],
        "basis":value.get("basis").filter(|v|matches!(v.as_str(),Some("first_group"|"aggregate"|"sample_family"))),
        "schedule_verified":value["schedule_verified"].as_bool(),"mark_cleared":value["mark_cleared"].as_bool(),
        "reason":text(&value["reason"],200),"completed_at":time(&value["completed_at"])})
}
fn time(value: &Value) -> Value {
    parse_datetime(value).map_or(Value::Null, |at| datetime(at).into())
}

pub fn summary(job: &Value) -> Value {
    let saved = &job["result_summary"];
    let report = &job["report"];
    let has_report = valid_report(report);
    let first =
        job["first_group_valid"] == true && !string(&job["first_group_prediction"]).is_empty();
    let mut out = if saved.is_object() {
        saved.clone()
    } else {
        json!({})
    };
    crate::verdict_policy::copy(job, &mut out);
    if job["engine"].is_string() {
        out["engine"] = job["engine"].clone();
        out["sample_count"] = job["planned_groups"].clone();
        out["bank_version"] = job["bank_version"].clone();
    }
    if has_report {
        out["prediction"] = text(&report["prediction"], 200);
        out["probability"] = probability(&report["probability"]);
        out["used_outputs"] = report["used_outputs"].as_u64().into();
        if !out["aggregate_downgrade"].is_boolean() {
            out["aggregate_downgrade"] = is_downgrade(
                string(&job["requested_model"]),
                string(&report["prediction"]),
            )
            .into();
        }
        out["analysis_available"] = true.into();
    } else if !out.is_object() || out.get("analysis_available").is_none() {
        out["analysis_available"] = false.into();
    }
    if first {
        out["first_prediction"] = text(&job["first_group_prediction"], 200);
        if !job["first_group_probability"].is_null() {
            out["first_probability"] = probability(&job["first_group_probability"]);
        }
        if !out["first_downgrade"].is_boolean() {
            out["first_downgrade"] = ((job["completion_reason"] == "automatic_degradation"
                && job["automatic_disposition"]["basis"] != "aggregate")
                || is_downgrade(
                    string(&job["requested_model"]),
                    string(&job["first_group_prediction"]),
                ))
            .into();
        }
    }
    if job["automatic_disposition"].is_object() && !out["disposition"].is_object() {
        let kind = if job["automatic_disposition"]["kind"] == "recheck" {
            "recovery"
        } else {
            "disposition"
        };
        out[kind] = receipt(&job["automatic_disposition"]);
    }
    if !out["outcome"].is_string() {
        out["outcome"] = if job["status"] != "completed" {
            job["status"].clone()
        } else if out["first_downgrade"] == true {
            "degraded".into()
        } else if out["analysis_available"] != true {
            "unavailable".into()
        } else if out["aggregate_downgrade"] == true {
            "aggregate_degraded".into()
        } else {
            "normal".into()
        };
    }
    if crate::lm_verdict::is_lm(job) || crate::verdict_policy::relaxed(job) {
        let classification = if job["classification"].is_object() {
            job["classification"].clone()
        } else {
            crate::verdict_policy::classify(job)
        };
        out["engine"] = job["engine"].clone();
        out["sample_count"] = job["planned_groups"].clone();
        if report.is_object() {
            out["decision"] = report["decision"].clone();
            out["prediction_name"] = report["prediction_name"].clone();
        }
        out["reason"] = classification["reason"].clone();
        out["classification"] = classification.clone();
        out["bank_version"] = job["bank_version"].clone();
        if job["groups"]
            .as_array()
            .is_some_and(|groups| !groups.is_empty())
        {
            out["group_predictions"] = json!(job["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|g| json!({"index":g["index"],"prediction":g["report"]["prediction_name"]}))
                .collect::<Vec<_>>());
        }
        if job["status"] == "completed" {
            out["outcome"] = classification["outcome"].clone();
        }
        out["first_downgrade"] = Value::Null;
        out["aggregate_downgrade"] = Value::Null;
    }
    out
}

pub fn preserve_summary(job: &mut Value) {
    if job["automatic"] == true && TERMINAL.contains(&string(&job["status"])) {
        job["result_summary"] = summary(job);
    }
}

fn summary_view(value: &Value) -> Value {
    json!({"verdict_mode":value["verdict_mode"],"degraded_model_id":value["degraded_model_id"],"engine":value["engine"],"sample_count":value["sample_count"],"decision":value["decision"],"reason":value["reason"],"prediction_name":value["prediction_name"],"classification":value["classification"],"bank_version":value["bank_version"],"group_predictions":value["group_predictions"],"outcome":text(&value["outcome"],40),"analysis_available":value["analysis_available"]==true,
        "first_prediction":text(&value["first_prediction"],200),"prediction":text(&value["prediction"],200),
        "probability":probability(&value["probability"]),"first_probability":probability(&value["first_probability"]),"used_outputs":value["used_outputs"].as_u64(),
        "first_downgrade":value["first_downgrade"].as_bool(),"aggregate_downgrade":value["aggregate_downgrade"].as_bool(),
        "disposition":receipt(&value["disposition"]),"recovery":receipt(&value["recovery"]),
        "verification":receipt(&value["verification"]),"restoration":receipt(&value["restoration"])})
}
fn job_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn decode(value: &str) -> Result<(DateTime<Utc>, String)> {
    if value.len() > 512 {
        return Err(Error::invalid());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| Error::invalid())?;
    let (at, id): (String, String) =
        serde_json::from_slice(&bytes).map_err(|_| Error::invalid())?;
    let at = parse_datetime(&json!(at)).ok_or_else(Error::invalid)?;
    if !job_id(&id) {
        return Err(Error::invalid());
    }
    Ok((at, id))
}
fn stamp(job: &Value) -> Result<DateTime<Utc>> {
    if ["completed_at", "started_at", "queued_at"]
        .iter()
        .any(|key| {
            let value = &job[*key];
            !value.is_null() && value != "" && parse_datetime(value).is_none()
        })
    {
        return Err(Error::new(503, "检测日志时间无效"));
    }
    ["completed_at", "started_at", "queued_at"]
        .iter()
        .find_map(|key| parse_datetime(&job[*key]))
        .ok_or_else(|| Error::new(503, "检测日志时间无效"))
}
fn notification(job: &Value, detection: &Value, oauth: &Value) -> Value {
    let saved = &job["result_summary"]["notification"];
    if saved.is_object() {
        return json!({"status":text(&saved["status"],40),"merged":false,"delivered_at":time(&saved["delivered_at"]),"reason":text(&saved["reason"],200)});
    }
    if oauth["pending_events"]
        .as_object()
        .into_iter()
        .flatten()
        .any(|(_, event)| event["model_test_job_id"] == job["id"])
    {
        return json!({"status":"queued","merged":false,"delivered_at":null,"reason":""});
    }
    let direct = &detection["disposition_notifications"][string(&job["id"])];
    let merged = job["recovery_context"]["kind"] == "credit" && !direct.is_object();
    let value = if merged {
        &oauth["scheduler"][integer(&job["account_id"]).unwrap_or(0).to_string()]
            ["credit_result_notifications"][string(&job["recovery_context"]["fingerprint"])]
    } else {
        direct
    };
    if !value.is_object()
        || merged
            && value["job_id"] != job["id"]
            && oauth["scheduler"][integer(&job["account_id"]).unwrap_or(0).to_string()]
                ["auto_reset_credit"]["model_test_job_id"]
                != job["id"]
    {
        return Value::Null;
    }
    json!({"status":text(&value["status"],40),"merged":merged,
        "delivered_at":time(&value["delivered_at"]),"reason":text(&value["reason"],200)})
}

impl Detection {
    pub fn log_notification(&self, id: &str, suppressed: bool) -> Result<()> {
        self.tests.transaction(|state| {
            let Some(job) = state["jobs"].get_mut(id) else { return Ok(()); };
            if job["automatic"] != true { return Ok(()); }
            let mut view = summary(job);
            view["notification"] = json!({"status":if suppressed {"suppressed"} else {"delivered"},
                "delivered_at":if suppressed {Value::Null} else {datetime(self.now()).into()},"reason":""});
            job["result_summary"] = view;
            Ok(())
        })
    }
    /// Record only display receipts; automation continues to use its original stores.
    pub fn log_receipt(&self, id: &str, kind: &str, value: &Value) {
        if self
            .tests
            .transaction(|state| {
                let Some(job) = state["jobs"].get_mut(id) else {
                    return Ok(());
                };
                if job["automatic"] != true {
                    return Ok(());
                }
                let mut view = summary(job);
                view[kind] = receipt(value);
                job["result_summary"] = view;
                Ok(())
            })
            .is_err()
        {
            eprintln!("detection_log_diagnostic reason=receipt_write_failed");
        }
    }
    pub fn logs(&self, before: Option<&str>, limit: usize) -> Result<Value> {
        if !(1..=100).contains(&limit) {
            return Err(Error::invalid());
        }
        let before = before.map(decode).transpose()?;
        let jobs = self.tests.shared_data()?;
        let detection = self.store.read_shared()?;
        let raw_oauth = self.monitor.store.0.read_shared()?;
        if ["pending_events", "recovery_history"].iter().any(|key| {
            raw_oauth.get(*key).is_some_and(|v| {
                v.as_object()
                    .is_none_or(|rows| rows.values().any(|v| !v.is_object()))
            })
        }) {
            return Err(Error::new(503, "检测恢复状态无效"));
        }
        let oauth = self.monitor.store.normalized_snapshot(raw_oauth);
        if detection.get("disposition_notifications").is_some_and(|v| {
            v.as_object().is_none_or(|notices| {
                notices
                    .values()
                    .any(|v| !v.is_object() || !v["status"].is_string())
            })
        }) {
            return Err(Error::new(503, "检测通知状态无效"));
        }
        let mut selected = Vec::new();
        for (stored_id, job) in jobs["jobs"]
            .as_object()
            .ok_or_else(|| Error::new(503, "检测任务状态无效"))?
        {
            if job.get("automatic").is_some_and(|v| !v.is_boolean()) {
                return Err(Error::new(503, "检测任务来源无效"));
            }
            if job["automatic"] != true {
                continue;
            }
            if !TERMINAL.contains(&string(&job["status"]))
                && !crate::model_tests::ACTIVE.contains(&string(&job["status"]))
            {
                return Err(Error::new(503, "检测任务状态无效"));
            }
            if !TERMINAL.contains(&string(&job["status"])) {
                continue;
            }
            let id = string(&job["id"]);
            if !job_id(id)
                || stored_id != id
                || integer(&job["account_id"]).is_none_or(|id| id <= 0)
                || !job["requested_model"].is_string()
            {
                return Err(Error::new(503, "检测日志状态无效"));
            }
            let key = (stamp(job)?, id.to_owned());
            if before.as_ref().is_some_and(|before| &key >= before) {
                continue;
            }
            // Keep only one response page, independent of history size.
            let index = selected.partition_point(|(existing, _)| existing > &key);
            if index <= limit {
                selected.insert(index, (key, job));
                selected.truncate(limit + 1);
            }
        }
        let more = selected.len() > limit;
        selected.truncate(limit);
        let cursor = if more {
            selected.last().map(|(key, _)| {
                URL_SAFE_NO_PAD.encode(
                    serde_json::to_vec(&(key.0.to_rfc3339(), &key.1))
                        .expect("cursor serialization"),
                )
            })
        } else {
            None
        };
        let items = selected.into_iter().map(|((at,_),job)| {
            let id = integer(&job["account_id"]).unwrap();
            let mut view = summary(job);
            let control = &detection["accounts"][id.to_string()];
            for (field,kind) in [("disposition","disposition"),("recheck_recovery","recovery")] {
                if control[field]["job_id"]==job["id"] { view[kind]=receipt(&control[field]); }
            }
            if let Some(history) = oauth["recovery_history"].as_object().into_iter().flatten().map(|(_,v)|v)
                .find(|v| v["model_test_job_id"]==job["id"] && v["account_id"]==id && parse_datetime(&v["recovered_at"]).is_some()) {
                view["restoration"]=json!({"status":"restored","completed_at":history["recovered_at"]});
            }
            let recovered = &detection["disposition_notifications"][string(&job["id"])];
            if recovered["kind"]=="recovered" && !view["recovery"].is_object() {
                view["recovery"]=json!({"status":"completed","mark_cleared":true,"completed_at":recovered["created_at"]});
            }
            // These notifications are created only after both writes are confirmed.
            if recovered.is_object() && recovered["kind"]!="recovered" && recovered["job_id"]==job["id"] && !view["disposition"].is_object() {
                view["disposition"]=json!({"status":"completed","marked":true,"schedule_verified":true,"completed_at":recovered["created_at"],"basis":recovered["basis"]});
            }
            let mut triggers = job["triggers"].as_array().into_iter().flatten().filter_map(|v|v.as_str())
                .filter_map(|v| if v=="scheduled" {Some("scheduled")} else if v.starts_with("error:") {Some("error")}
                    else if v.starts_with("slow:") {Some("slow")} else if v.starts_with("recovery") {Some("recovery")}
                    else if v.starts_with("credit") {Some("credit")} else {None})
                .collect::<std::collections::BTreeSet<_>>();
            if job["degradation_recheck"]==true {triggers.insert("recheck");}
            json!({"job_id":job["id"],"account_id":id,"account_name":text(&job["account_name"],120),
                "model_id":text(&job["requested_model"],200),"status":job["status"],"created_at":at.to_rfc3339(),
                "started_at":time(&job["started_at"]),"completed_at":time(&job["completed_at"]),
                "triggers":triggers,"summary":summary_view(&view),"error":text(&job["error"],200),
                "notification":notification(job,&detection,&oauth),"details_available":job["report"].is_object()})
        }).collect::<Vec<_>>();
        Ok(json!({"items":items,"next_cursor":cursor}))
    }
}
