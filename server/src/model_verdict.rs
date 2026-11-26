//! Shared classification, policy and downgrade comparison for model detection, recovery and rechecks.
use crate::{
    compat::{integer, string},
    error::{Error, Result},
    lm_detector::ENGINE,
};
use regex::Regex;
use serde_json::{json, Value};
use std::{cmp::Ordering, sync::LazyLock};

pub use sub2ops_protocol::dto::LEGACY_DEGRADED_MODEL as LEGACY_TARGET;
pub const DEFAULT_TEST_MODEL: &str = LEGACY_TARGET;

pub(crate) fn valid_report(report: &Value) -> bool {
    integer(&report["used_outputs"]).is_some_and(|used| used >= 1)
        && !string(&report["prediction"]).trim().is_empty()
}

pub(crate) fn parse_gpt(model: &str) -> Option<(&str, u8)> {
    let (version, tier) = model.strip_prefix("gpt-")?.rsplit_once('-')?;
    let tier = match tier {
        "astra" => 3,
        "sol" => 2,
        "luna" => 0,
        _ => return None,
    };
    version
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some((version, tier))
}

pub(crate) fn compare_versions(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        let (a, b) = (left.next(), right.next());
        if a.is_none() && b.is_none() {
            return Ordering::Equal;
        }
        let a = a.unwrap_or("0").trim_start_matches('0');
        let b = b.unwrap_or("0").trim_start_matches('0');
        let order = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
        if order != Ordering::Equal {
            return order;
        }
    }
}

pub fn is_downgrade(requested: &str, predicted: &str) -> bool {
    let requested = requested.trim().to_ascii_lowercase();
    let predicted = predicted.trim().to_ascii_lowercase();
    if predicted.is_empty() {
        return false;
    }
    if predicted == LEGACY_TARGET {
        return true;
    }
    if requested == predicted {
        return false;
    }
    let (Some((requested_version, requested_tier)), Some((predicted_version, predicted_tier))) =
        (parse_gpt(&requested), parse_gpt(&predicted))
    else {
        return false;
    };
    match predicted_tier.cmp(&requested_tier) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal if requested_tier == 2 => false,
        Ordering::Equal => compare_versions(predicted_version, requested_version) == Ordering::Less,
    }
}

pub(crate) fn diagnose_prediction(requested: &str, predicted: &str) {
    let requested = requested.trim().to_ascii_lowercase();
    let predicted = predicted.trim().to_ascii_lowercase();
    if !predicted.is_empty()
        && predicted != LEGACY_TARGET
        && predicted != requested
        && (parse_gpt(&requested).is_none() || parse_gpt(&predicted).is_none())
    {
        eprintln!("model_detection_diagnostic level=debug reason=unsupported_model");
    }
}

pub(crate) fn is_lm(job: &Value) -> bool {
    job["engine"] == ENGINE
}

// Model identity and comparison rules match lich13-switch 6cdef7a.
pub(crate) fn normalize(model: &str) -> String {
    let value = model.trim().to_ascii_lowercase();
    let value = value
        .rsplit('/')
        .next()
        .unwrap_or(&value)
        .split(':')
        .next()
        .unwrap_or("");
    static CLAUDE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^claude-(opus|sonnet|haiku|fable)-(\d+)-(\d+)(?:-\d{8})?$").unwrap()
    });
    if let Some(c) = CLAUDE.captures(value) {
        return format!("claude-{}-{}.{}", &c[1], &c[2], &c[3]);
    }
    static OLDER_CLAUDE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^claude-(\d+)-(\d+)-(opus|sonnet|haiku)(?:-\d{8})?$").unwrap()
    });
    if let Some(c) = OLDER_CLAUDE.captures(value) {
        return format!("claude-{}-{}.{}", &c[3], &c[1], &c[2]);
    }
    value.into()
}

fn family(model: &str) -> Option<u8> {
    static FAMILIES: LazyLock<Vec<(Regex, u8)>> = LazyLock::new(|| {
        [
            (
                r"^claude-(?:opus|sonnet|haiku|fable)-[0-9]+(?:\.[0-9]+)*(?:-[0-9]{8})?$",
                4,
            ),
            (
                r"^gpt-(?:[0-9]+(?:\.[0-9]+)*(?:-(?:astra|sol|luna|terra))?|4o(?:-mini)?)$",
                3,
            ),
            (r"^grok-[0-9]+(?:\.[0-9]+)*(?:-(?:fast|code))?$", 2),
            (
                r"^deepseek-(?:v[0-9]+(?:\.[0-9]+)*(?:-(?:pro|flash))?|r1)(?:-[0-9]{4,8})?$",
                1,
            ),
        ]
        .into_iter()
        .map(|(pattern, rank)| (Regex::new(pattern).unwrap(), rank))
        .collect()
    });
    FAMILIES
        .iter()
        .find_map(|(pattern, rank)| pattern.is_match(model).then_some(*rank))
}

fn tier(model: &str) -> Option<(String, u8)> {
    if let Some((version, rank)) = parse_gpt(model) {
        return Some((version.into(), rank));
    }
    static MODEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:claude-(opus|sonnet|haiku)-([0-9]+(?:\.[0-9]+)*)(?:-[0-9]{8})?|grok-([0-9]+(?:\.[0-9]+)*)|deepseek-v([0-9]+(?:\.[0-9]+)*)-(pro|flash)(?:-[0-9]{4,8})?)$").unwrap()
    });
    let c = MODEL.captures(model)?;
    if let Some(v) = c.get(2) {
        return Some((
            v.as_str().into(),
            match &c[1] {
                "opus" => 3,
                "sonnet" => 2,
                _ => 1,
            },
        ));
    }
    if let Some(v) = c.get(3) {
        return Some((v.as_str().into(), 0));
    }
    Some((
        c[4].split('.').take(2).collect::<Vec<_>>().join("."),
        if &c[5] == "pro" { 2 } else { 1 },
    ))
}

/// None is unclassified, not a successful recovery check.
fn compare_models(requested: &str, predicted: &str) -> Option<bool> {
    let requested = normalize(requested);
    let predicted = normalize(predicted);
    let (a, b) = (family(&requested)?, family(&predicted)?);
    if a != b {
        return Some(b < a);
    }
    if a == 3 && predicted == LEGACY_TARGET {
        return Some(true);
    }
    if a == 2 {
        static GROK: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^grok-([0-9]+(?:\.[0-9]+)*)(-(?:fast|code))?$").unwrap());
        let (r, p) = (GROK.captures(&requested)?, GROK.captures(&predicted)?);
        if r.get(2).map(|m| m.as_str()) != p.get(2).map(|m| m.as_str()) {
            return None;
        }
        return Some(compare_versions(&p[1], &r[1]) == Ordering::Less);
    }
    let ((rv, rt), (pv, pt)) = (tier(&requested)?, tier(&predicted)?);
    Some(match pt.cmp(&rt) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal if a == 3 && rt == 2 => false,
        Ordering::Equal => compare_versions(&pv, &rv) == Ordering::Less,
    })
}

fn compare_lm(requested: &str, report: &Value) -> Option<bool> {
    if !matches!(string(&report["decision"]), "partial" | "scored")
        || report["merged_class"] == true
        || !valid_report(report)
    {
        return None;
    }
    compare_models(requested, string(&report["prediction"]))
}

pub(crate) fn classify_lm(job: &Value) -> Value {
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
    let requested = job["forwarded_model"]
        .as_str()
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| string(&job["requested_model"]));
    let Some(aggregate) = compare_lm(requested, &job["report"]) else {
        return unknown("汇总结果无法明确判断，已保留原状态");
    };
    for group in groups {
        if compare_lm(requested, &group["report"]).is_none() {
            return unknown("存在合并类别、未知型号或无法判断的样本，已保留原状态");
        }
    }
    let lower = groups
        .iter()
        .filter(|g| {
            family(&normalize(string(&g["report"]["prediction"]))) < family(&normalize(requested))
        })
        .map(|g| g["index"].clone())
        .collect::<Vec<_>>();
    let degraded = aggregate || !lower.is_empty();
    json!({"outcome":if degraded{"degraded"}else{"normal"},"reason":"","basis":if !lower.is_empty(){"sample_family"}else if planned==1{"first_group"}else{"aggregate"},"group_indexes":lower})
}
pub(crate) fn relaxed(value: &Value) -> bool {
    value["verdict_mode"] == "relaxed"
}

pub(crate) fn target(value: &Value) -> &str {
    value["degraded_model_id"].as_str().unwrap_or(LEGACY_TARGET)
}

pub(crate) fn validate(value: &Value) -> Result<()> {
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

pub(crate) fn copy(source: &Value, destination: &mut Value) {
    destination["verdict_mode"] = if relaxed(source) { "relaxed" } else { "strict" }.into();
    destination["degraded_model_id"] = target(source).into();
}

pub(crate) fn matches(left: &str, right: &str) -> bool {
    normalize(left) == normalize(right)
}

pub(crate) fn lm_contains(bank: &crate::lm_detector::Snapshot, value: &Value) -> bool {
    bank.detector
        .models()
        .iter()
        .any(|model| matches(&model.id, target(value)) && !model.display_name.contains('/'))
}

pub fn classify(job: &Value) -> Value {
    if !relaxed(job) && is_lm(job) {
        return classify_lm(job);
    }
    let unknown = || json!({"outcome":"unavailable","reason":"采样未完整有效完成或模型无法明确识别","basis":null,"group_indexes":[]});
    let report = &job["report"];
    if job["status"] != "completed" || !valid_report(report) {
        return unknown();
    }
    let degraded = if relaxed(job) {
        let planned = integer(&job["planned_groups"]).unwrap_or(3);
        let Some(groups) = job["groups"].as_array() else {
            return unknown();
        };
        let identifiable = |report: &Value| {
            valid_report(report)
                && report["merged_class"] != true
                && (!is_lm(job) || matches!(string(&report["decision"]), "partial" | "scored"))
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
                    || is_lm(job) && !identifiable(&g["report"])
            })
        {
            return unknown();
        }
        matches(string(&report["prediction"]), target(job))
    } else {
        is_downgrade(
            string(&job["requested_model"]),
            string(&report["prediction"]),
        ) || job["first_group_valid"] == true
            && is_downgrade(
                string(&job["requested_model"]),
                string(&job["first_group_prediction"]),
            )
    };
    json!({"outcome":if degraded {"degraded"} else {"normal"},"reason":"","basis":"aggregate","group_indexes":[]})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lm_report(model: &str) -> Value {
        json!({"prediction":model,"decision":"partial","used_outputs":1,"merged_class":false})
    }

    fn lm_job(models: &[&str], aggregate: &str) -> Value {
        let mut r = lm_report(aggregate);
        r["used_outputs"] = json!(models.len());
        if models.len() == 3 {
            r["decision"] = "scored".into();
        }
        json!({"engine":ENGINE,"status":"completed","planned_groups":models.len(),"requested_model":"gpt-6-sol","report":r,"groups":models.iter().enumerate().map(|(i,m)|json!({"index":i+1,"status":"completed","transport_complete":true,"report":lm_report(m)})).collect::<Vec<_>>()})
    }

    fn relaxed_job(engine: &str) -> Value {
        let report = json!({"prediction":"gpt-6-luna","decision":"scored","used_outputs":3});
        json!({"engine":engine,"verdict_mode":"relaxed","degraded_model_id":"gpt-5.6-luna",
            "requested_model":"gpt-6-sol","status":"completed","planned_groups":3,"report":report,
            "first_group_valid":true,"first_group_prediction":"gpt-5.6-luna",
            "groups":(0..3).map(|_|json!({"status":"completed","transport_complete":true,"report":{"prediction":"gpt-5.6-luna","decision":"partial","used_outputs":1}})).collect::<Vec<_>>()})
    }

    #[test]
    fn all_samples_must_be_known_before_lower_family_disposition() {
        let mut j = lm_job(&["gpt-6-sol", "grok-4", "claude-sonnet-5"], "gpt-6-sol");
        assert_eq!(classify(&j)["group_indexes"], json!([2]));
        assert_eq!(classify(&j)["outcome"], "degraded");
        j["groups"][0]["report"]["merged_class"] = true.into();
        assert_eq!(classify(&j)["outcome"], "unavailable");
        j["groups"][0]["report"] = lm_report("qwen3");
        assert_eq!(classify(&j)["outcome"], "unavailable");
    }

    #[test]
    fn three_groups_wait_and_same_family_uses_aggregate() {
        let mut j = lm_job(&["gpt-6-luna", "gpt-6-sol", "gpt-6-sol"], "gpt-6-sol");
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
                classify(&lm_job(&[model], model))["outcome"],
                outcome,
                "{model}"
            );
        }
    }

    #[test]
    fn gpt_and_cross_family_boundaries_delegate_to_shared_rules() {
        for (requested, predicted, expected) in [
            ("gpt-6-astra", "gpt-5.6-luna", Some(true)),
            ("gpt-6-astra", "gpt-6-sol", Some(true)),
            ("gpt-6.1-sol", "gpt-6-sol", Some(false)),
            ("gpt-6-sol", "gpt-5.6-terra", None),
            ("", " GPT-5.6-LUNA ", None),
            ("claude-sonnet-5", "gpt-5.6-luna", Some(true)),
            ("gpt-6-sol", "claude-sonnet-5", Some(false)),
        ] {
            assert_eq!(
                compare_models(requested, predicted),
                expected,
                "{requested} / {predicted}"
            );
        }
    }

    #[test]
    fn strict_platform_comparison_and_forwarded_model() {
        for (requested, predicted, expected) in [
            ("claude-opus-4-6", "claude-sonnet-5", Some(true)),
            ("claude-sonnet-5", "claude-opus-4.6", Some(false)),
            ("claude-opus-4.6", "claude-opus-4.5", Some(true)),
            (
                "claude-3-5-sonnet-20241022",
                "claude-sonnet-3.5",
                Some(false),
            ),
            ("grok-4.20", "grok-4.3", Some(true)),
            ("grok-4-fast", "grok-3-fast", Some(true)),
            ("grok-4-fast", "grok-4-code", None),
            ("deepseek-v4-pro", "deepseek-v4-flash", Some(true)),
            ("deepseek-v4-flash", "deepseek-v4-pro", Some(false)),
            ("deepseek-v4", "deepseek-v3", None),
            ("claude-custom", "gpt-6-sol", None),
            ("gpt-6-sol", "grok-alias", None),
            ("grok-4", "deepseek-v4", Some(true)),
            ("deepseek-v4", "grok-4", Some(false)),
        ] {
            assert_eq!(
                compare_models(requested, predicted),
                expected,
                "{requested} / {predicted}"
            );
        }
        let mut value = lm_job(&["gpt-6-sol"], "gpt-6-sol");
        value["requested_model"] = "fixture-alias".into();
        value["forwarded_model"] = "claude-opus-5".into();
        assert_eq!(classify(&value)["outcome"], "degraded");
        value["forwarded_model"] = "fixture-alias".into();
        assert_eq!(classify(&value)["outcome"], "unavailable");
    }

    #[test]
    fn relaxed_uses_only_complete_aggregate_for_both_engines() {
        for engine in ["modeltrace", "lm_detector"] {
            let mut value = relaxed_job(engine);
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
            let mut normal = relaxed_job(engine);
            normal["automatic"] = true.into();
            normal["degradation_recheck"] = true.into();
            assert!(crate::degradation_rechecks::normal_result(&normal));
            normal["groups"][1]["status"] = "failed".into();
            assert!(!crate::degradation_rechecks::normal_result(&normal));
        }
    }

    #[test]
    fn downgrade_vectors_preserve_legacy_exception_and_rank_tiers_first() {
        let vectors = [
            ("gpt-6.1-sol", "gpt-6-astra", false),
            ("gpt-6-luna", "gpt-5.6-luna", true),
            ("gpt-6-astra", "gpt-5.6-luna", true),
            ("gpt-5.6-luna", "gpt-5.6-luna", true),
            ("gpt-6-astra", "gpt-6-sol", true),
            ("gpt-6-sol", "gpt-6-luna", true),
            ("gpt-6-sol", "gpt-6-astra", false),
            ("gpt-6-sol", "gpt-5.6-sol", false),
            ("gpt-6.1-sol", "gpt-6-sol", false),
            ("gpt-6-sol", "gpt-6.1-sol", false),
            ("gpt-6.1-sol", "gpt-5.6-sol", false),
            ("gpt-5.6-sol", "gpt-6-sol", false),
            ("gpt-6-luna", "gpt-5.9-sol", false),
            ("gpt-6.1-astra", "gpt-6-astra", true),
            ("gpt-6-astra", "gpt-6.0-astra", false),
            ("gpt-5.10-sol", "gpt-5.9-sol", false),
            ("  GPT-6-Sol \t", "gpt-6-sol", false),
            ("gpt-6-sol", "claude-sonnet-5", false),
            ("gpt-6-sol", "", false),
            ("gpt-6-sol", "gpt-5.6-terra", false),
            ("gpt-6-terra", "gpt-6-luna", false),
            ("gpt-6-sol", "gpt-5.5", false),
            ("gpt-6", "gpt-6-luna", false),
            ("gpt-6-unknown", "gpt-6-luna", false),
            ("gpt-6-sol", "gpt-6-unknown", false),
            ("gpt-6-sol", " \n\t", false),
            ("", " GPT-5.6-LUNA ", true),
            ("claude-sonnet-5", "gpt-5.6-luna", true),
            ("gpt-6.0.0-sol", "gpt-06-sol", false),
            ("gpt-6-sol", "gpt-6.0.1-sol", false),
            ("gpt-6.-sol", "gpt-5-sol", false),
            ("gpt-6-sol", "gpt--5-sol", false),
            ("gpt-6-sol", "gpt-５-sol", false),
            ("gpt-6-sol", "prefix-gpt-5-sol", false),
            ("gpt-6-sol", "gpt-5-sol-extra", false),
            (
                "gpt-184467440737095516160-sol",
                "gpt-184467440737095516159-sol",
                false,
            ),
        ];
        for (requested, predicted, expected) in vectors {
            assert_eq!(
                is_downgrade(requested, predicted),
                expected,
                "{requested} / {predicted}"
            );
        }
    }

    #[test]
    fn invalid_reports_and_banks_are_rejected_without_panicking() {
        for report in [
            Value::Null,
            json!({}),
            json!({"used_outputs":0,"prediction":LEGACY_TARGET}),
            json!({"used_outputs":1,"prediction":" \t"}),
            json!({"used_outputs":1,"prediction":42}),
        ] {
            assert!(!valid_report(&report));
        }
        assert!(valid_report(
            &json!({"used_outputs":1,"prediction":"gpt-6-sol"})
        ));
        assert!(valid_report(
            &json!({"used_outputs":"1","prediction":"gpt-6-sol"})
        ));
    }

    #[test]
    fn example_connection_default_matches_shared_constant() {
        let example = include_str!("../../.env.example");
        assert_eq!(
            example
                .lines()
                .find_map(|line| line.strip_prefix("OAUTH_RECOVERY_TEST_MODEL_ID=")),
            Some(DEFAULT_TEST_MODEL)
        );
        assert_eq!(crate::model_detection::defaults()["model_id"], "gpt-6-luna");
    }
}
