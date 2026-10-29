//! Account policy is copied into each new test; retries keep that original policy.
use crate::{
    compat::{integer, string},
    error::{Error, Result},
    model_downgrade,
};
use serde_json::{json, Value};

pub fn relaxed(value: &Value) -> bool {
    value["verdict_mode"] == "relaxed"
}

pub fn target(value: &Value) -> &str {
    value["degraded_model_id"]
        .as_str()
        .unwrap_or(model_downgrade::LEGACY_TARGET)
}

pub fn validate(value: &Value) -> Result<()> {
    if value
        .get("verdict_mode")
        .is_some_and(|v| v != "strict" && v != "relaxed")
        || value
            .get("degraded_model_id")
            .is_some_and(|v| !crate::account_templates::MODEL_ID.is_match(string(v)))
    {
        return Err(Error::invalid());
    }
    Ok(())
}

pub fn copy(source: &Value, destination: &mut Value) {
    destination["verdict_mode"] = if relaxed(source) { "relaxed" } else { "strict" }.into();
    destination["degraded_model_id"] = target(source).into();
}

pub fn matches(left: &str, right: &str) -> bool {
    crate::lm_verdict::normalize(left) == crate::lm_verdict::normalize(right)
}

pub fn bank_contains(bank: &Value, value: &Value) -> bool {
    bank["models"].as_array().is_some_and(|models| {
        models
            .iter()
            .any(|m| matches(string(&m["id"]), target(value)))
    })
}

pub fn lm_contains(bank: &crate::lm_detector::Snapshot, value: &Value) -> bool {
    bank.detector
        .models()
        .iter()
        .any(|model| matches(&model.id, target(value)) && !model.display_name.contains('/'))
}

pub fn classify(job: &Value) -> Value {
    if !relaxed(job) && crate::lm_verdict::is_lm(job) {
        return crate::lm_verdict::classify(job);
    }
    let unknown = || json!({"outcome":"unavailable","reason":"采样未完整有效完成或模型无法明确识别","basis":null,"group_indexes":[]});
    let report = &job["report"];
    if job["status"] != "completed" || !model_downgrade::valid_report(report) {
        return unknown();
    }
    let degraded = if relaxed(job) {
        let planned = integer(&job["planned_groups"]).unwrap_or(3);
        let Some(groups) = job["groups"].as_array() else {
            return unknown();
        };
        let identifiable = |report: &Value| {
            model_downgrade::valid_report(report)
                && report["merged_class"] != true
                && (!crate::lm_verdict::is_lm(job)
                    || matches!(string(&report["decision"]), "partial" | "scored"))
        };
        if !matches!(planned, 1 | 3)
            || groups.len() != planned as usize
            || integer(&report["used_outputs"]) != Some(planned)
            || job["can_retry"] == true
            || job["uncertain"] == true
            || job["error_code"].is_string()
            || !identifiable(report)
            || groups.iter().any(|g| {
                g["status"] != "completed"
                    || g["uncertain"] == true
                    || g["transport_complete"] != true
                    || crate::lm_verdict::is_lm(job) && !identifiable(&g["report"])
            })
        {
            return unknown();
        }
        matches(string(&report["prediction"]), target(job))
    } else {
        model_downgrade::is_downgrade(
            string(&job["requested_model"]),
            string(&report["prediction"]),
        ) || job["first_group_valid"] == true
            && model_downgrade::is_downgrade(
                string(&job["requested_model"]),
                string(&job["first_group_prediction"]),
            )
    };
    json!({"outcome":if degraded {"degraded"} else {"normal"},"reason":"","basis":"aggregate","group_indexes":[]})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job(engine: &str) -> Value {
        let report = json!({"prediction":"gpt-6-luna","decision":"scored","used_outputs":3});
        json!({"engine":engine,"verdict_mode":"relaxed","degraded_model_id":"gpt-5.6-luna",
            "requested_model":"gpt-6-sol","status":"completed","planned_groups":3,"report":report,
            "first_group_valid":true,"first_group_prediction":"gpt-5.6-luna",
            "groups":(0..3).map(|_|json!({"status":"completed","transport_complete":true,"report":{"prediction":"gpt-5.6-luna","decision":"partial","used_outputs":1}})).collect::<Vec<_>>()})
    }
    #[test]
    fn relaxed_uses_only_complete_aggregate_for_both_engines() {
        for engine in ["modeltrace", "lm_detector"] {
            let mut value = job(engine);
            assert_eq!(classify(&value)["outcome"], "normal");
            value["report"]["prediction"] = " GPT-5.6-LUNA ".into();
            assert_eq!(classify(&value)["outcome"], "degraded");
            for (field, bad) in [
                ("status", json!("running")),
                ("uncertain", json!(true)),
                ("can_retry", json!(true)),
                ("error_code", json!("fixture")),
            ] {
                let mut invalid = value.clone();
                invalid[field] = bad;
                assert_eq!(classify(&invalid)["outcome"], "unavailable");
            }
            value["report"]["merged_class"] = true.into();
            assert_eq!(classify(&value)["outcome"], "unavailable");
            value["report"]["merged_class"] = false.into();
            value["groups"][2]["status"] = "skipped".into();
            assert_eq!(classify(&value)["outcome"], "unavailable");
            value["groups"][2]["status"] = "completed".into();
            value["groups"][2]["transport_complete"] = false.into();
            assert_eq!(classify(&value)["outcome"], "unavailable");
            value["groups"][2]["transport_complete"] = true.into();
            value["report"]["used_outputs"] = 2.into();
            assert_eq!(classify(&value)["outcome"], "unavailable");
            let mut normal = job(engine);
            normal["automatic"] = true.into();
            normal["degradation_recheck"] = true.into();
            assert!(crate::degradation_rechecks::normal_result(&normal));
            normal["groups"][1]["status"] = "failed".into();
            assert!(!crate::degradation_rechecks::normal_result(&normal));
        }
    }
}
