use crate::{
    compat::{integer, string},
    lm_detector::ENGINE,
    model_downgrade,
};
use regex::Regex;
use serde_json::{json, Value};
use std::{cmp::Ordering, sync::LazyLock};

pub fn is_lm(job: &Value) -> bool {
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
    // A provider-defined alias sharing a prefix is not a known model identity.
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
    static MODEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:gpt-([0-9]+(?:\.[0-9]+)*)-(astra|sol|luna)|claude-(opus|sonnet|haiku)-([0-9]+(?:\.[0-9]+)*)(?:-[0-9]{8})?|grok-([0-9]+(?:\.[0-9]+)*)|deepseek-v([0-9]+(?:\.[0-9]+)*)-(pro|flash)(?:-[0-9]{4,8})?)$").unwrap()
    });
    let c = MODEL.captures(model)?;
    if let Some(v) = c.get(1) {
        return Some((
            v.as_str().into(),
            match &c[2] {
                "astra" => 3,
                "sol" => 2,
                _ => 0,
            },
        ));
    }
    if let Some(v) = c.get(4) {
        return Some((
            v.as_str().into(),
            match &c[3] {
                "opus" => 3,
                "sonnet" => 2,
                _ => 1,
            },
        ));
    }
    if let Some(v) = c.get(5) {
        return Some((v.as_str().into(), 0));
    }
    Some((
        c[6].split('.').take(2).collect::<Vec<_>>().join("."),
        if &c[7] == "pro" { 2 } else { 1 },
    ))
}
fn compare_versions(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        let (a, b) = (left.next(), right.next());
        if a.is_none() && b.is_none() {
            return Ordering::Equal;
        }
        // Compare unsigned decimal segments without a machine-integer limit.
        // Missing trailing segments and all-zero segments are equivalent.
        let a = a.unwrap_or("0").trim_start_matches('0');
        let b = b.unwrap_or("0").trim_start_matches('0');
        let order = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
        if order != Ordering::Equal {
            return order;
        }
    }
}

/// None is unclassified, not a successful recovery check.
fn compare_models(requested: &str, predicted: &str) -> Option<bool> {
    let requested = normalize(requested);
    let predicted = normalize(predicted);
    let (a, b) = (family(&requested)?, family(&predicted)?);
    if a != b {
        return Some(b < a);
    }
    if a == 3 && predicted == model_downgrade::LEGACY_TARGET {
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
fn compare(requested: &str, report: &Value) -> Option<bool> {
    if !matches!(string(&report["decision"]), "partial" | "scored")
        || report["merged_class"] == true
        || !model_downgrade::valid_report(report)
    {
        return None;
    }
    compare_models(requested, string(&report["prediction"]))
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
    let requested = job["forwarded_model"]
        .as_str()
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| string(&job["requested_model"]));
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
        .filter(|g| {
            family(&normalize(string(&g["report"]["prediction"]))) < family(&normalize(requested))
        })
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
            ("", " GPT-5.6-LUNA ", false),
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
                compare_models(requested, predicted) == Some(true),
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
        let mut value = job(&["gpt-6-sol"], "gpt-6-sol");
        value["requested_model"] = "fixture-alias".into();
        value["forwarded_model"] = "claude-opus-5".into();
        assert_eq!(classify(&value)["outcome"], "degraded");
        value["forwarded_model"] = "fixture-alias".into();
        assert_eq!(classify(&value)["outcome"], "unavailable");
    }
}
