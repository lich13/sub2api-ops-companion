use crate::{
    compat::{integer, string},
    lm_detector::ENGINE,
    model_downgrade,
};
use serde_json::{json, Value};

pub fn is_lm(job: &Value) -> bool {
    job["engine"] == ENGINE
}
fn family(model: &str) -> Option<u8> {
    let model = model.trim().to_ascii_lowercase();
    if model.starts_with("claude-") {
        Some(4)
    } else if model.starts_with("gpt-") {
        Some(3)
    } else if model.starts_with("grok-") {
        Some(2)
    } else if model.starts_with("deepseek-") {
        Some(1)
    } else {
        None
    }
}
fn supported_gpt(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    let Some(rest) = model.strip_prefix("gpt-") else {
        return false;
    };
    let Some((version, tier)) = rest.rsplit_once('-') else {
        return false;
    };
    matches!(tier, "astra" | "sol" | "luna")
        && version
            .split('.')
            .all(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
}
fn compare(requested: &str, report: &Value) -> Option<bool> {
    if !matches!(string(&report["decision"]), "partial" | "scored")
        || report["merged_class"] == true
        || !model_downgrade::valid_report(report)
    {
        return None;
    }
    let predicted = string(&report["prediction"]);
    let (a, b) = (family(requested)?, family(predicted)?);
    if a != b {
        return Some(b < a);
    }
    if a == 3 {
        if predicted.eq_ignore_ascii_case(model_downgrade::LEGACY_TARGET) {
            return Some(true);
        }
        if !supported_gpt(requested) || !supported_gpt(predicted) {
            return None;
        }
        return Some(model_downgrade::is_downgrade(requested, predicted));
    }
    requested.eq_ignore_ascii_case(predicted).then_some(false)
}
pub fn classify(job: &Value) -> Value {
    let unknown = |reason: &str| json!({"outcome":"unavailable","reason":reason,"basis":null,"group_indexes":[]});
    let planned = integer(&job["planned_groups"]).unwrap_or(1) as usize;
    let Some(groups) = job["groups"].as_array() else {
        return unknown("采样尚未完成");
    };
    if groups.len() != planned
        || job["status"] != "completed"
        || job["can_retry"] == true
        || job["uncertain"] == true
        || job["error_code"].is_string()
        || groups.iter().any(|g| {
            g["status"] != "completed" || g["uncertain"] == true || g["transport_complete"] != true
        })
        || integer(&job["report"]["used_outputs"]) != Some(planned as i64)
    {
        return unknown("所选采样未完整有效完成");
    }
    let requested = string(&job["requested_model"]);
    let Some(aggregate) = compare(requested, &job["report"]) else {
        return unknown("汇总结果无法明确判断，已保留原状态");
    };
    for group in groups {
        if compare(requested, &group["report"]).is_none() {
            return unknown("存在合并类别、未知型号或无法判断的样本，已保留原状态");
        }
    }
    let lower = groups
        .iter()
        .filter(|g| family(string(&g["report"]["prediction"])) < family(requested))
        .map(|g| g["index"].clone())
        .collect::<Vec<_>>();
    let degraded = aggregate || !lower.is_empty();
    json!({"outcome":if degraded{"degraded"}else{"normal"},"reason":"","basis":if !lower.is_empty(){"sample_family"}else if planned==1{"first_group"}else{"aggregate"},"group_indexes":lower})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report(model: &str) -> Value {
        json!({"prediction":model,"decision":"partial","used_outputs":1,"merged_class":false})
    }
    fn job(models: &[&str], aggregate: &str) -> Value {
        let mut r = report(aggregate);
        r["used_outputs"] = json!(models.len());
        if models.len() == 3 {
            r["decision"] = "scored".into();
        }
        json!({"engine":ENGINE,"status":"completed","planned_groups":models.len(),"requested_model":"gpt-6-sol","report":r,"groups":models.iter().enumerate().map(|(i,m)|json!({"index":i+1,"status":"completed","transport_complete":true,"report":report(m)})).collect::<Vec<_>>()})
    }
    #[test]
    fn all_samples_must_be_known_before_lower_family_disposition() {
        let mut j = job(&["gpt-6-sol", "grok-4", "claude-sonnet-5"], "gpt-6-sol");
        assert_eq!(classify(&j)["group_indexes"], json!([2]));
        assert_eq!(classify(&j)["outcome"], "degraded");
        j["groups"][0]["report"]["merged_class"] = true.into();
        assert_eq!(classify(&j)["outcome"], "unavailable");
        j["groups"][0]["report"] = report("qwen3");
        assert_eq!(classify(&j)["outcome"], "unavailable");
    }
    #[test]
    fn three_groups_wait_and_same_family_uses_aggregate() {
        let mut j = job(&["gpt-6-luna", "gpt-6-sol", "gpt-6-sol"], "gpt-6-sol");
        assert_eq!(classify(&j)["outcome"], "normal");
        j["status"] = "running".into();
        assert_eq!(classify(&j)["outcome"], "unavailable");
        j["status"] = "completed".into();
        j["groups"][2]["status"] = "failed".into();
        assert_eq!(classify(&j)["outcome"], "unavailable");
    }
    #[test]
    fn single_group_preserves_business_order_and_legacy_target() {
        for (model, outcome) in [
            ("claude-opus-5", "normal"),
            ("grok-4", "degraded"),
            ("deepseek-v4", "degraded"),
            ("gpt-5.6-luna", "degraded"),
            ("gpt-5.6-sol", "normal"),
            ("gpt-4o", "unavailable"),
            ("gemini-3", "unavailable"),
        ] {
            assert_eq!(
                classify(&job(&[model], model))["outcome"],
                outcome,
                "{model}"
            );
        }
    }
}
