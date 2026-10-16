//! Shared classification for automatic detection, recovery and rechecks.
use crate::compat::{integer, string};
use serde_json::Value;
use std::cmp::Ordering;

pub const LEGACY_TARGET: &str = "gpt-5.6-luna";
pub const DEFAULT_TEST_MODEL: &str = LEGACY_TARGET;

pub fn valid_report(report: &Value) -> bool {
    integer(&report["used_outputs"]).is_some_and(|used| used >= 1)
        && !string(&report["prediction"]).trim().is_empty()
}

pub fn bank_contains(bank: &Value, model: &str) -> bool {
    bank["models"]
        .as_array()
        .is_some_and(|models| models.iter().any(|entry| entry["id"] == model))
}

fn parse(model: &str) -> Option<(&str, u8)> {
    let (version, tier) = model.strip_prefix("gpt-")?.rsplit_once('-')?;
    let tier = match tier {
        "astra" => 3,
        "sol" => 2,
        "luna" => 0,
        // Neither terra nor models without an explicit supported tier have
        // an agreed ranking. Preserve the old unclassified behavior.
        _ => return None,
    };
    version
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some((version, tier))
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

pub fn is_downgrade(requested: &str, predicted: &str) -> bool {
    let requested = requested.trim().to_ascii_lowercase();
    let predicted = predicted.trim().to_ascii_lowercase();
    if predicted.is_empty() {
        return false;
    }
    // This exception intentionally precedes equality, including requests for
    // the legacy target itself and requests outside the supported model set.
    if predicted == LEGACY_TARGET {
        return true;
    }
    if requested == predicted {
        return false;
    }
    let (Some((requested_version, requested_tier)), Some((predicted_version, predicted_tier))) =
        (parse(&requested), parse(&predicted))
    else {
        return false;
    };
    match predicted_tier.cmp(&requested_tier) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => compare_versions(predicted_version, requested_version) == Ordering::Less,
    }
}

/// Called when an analysis is produced, never from a polling classification.
/// Only static diagnostics are emitted: upstream model text is untrusted.
pub(crate) fn diagnose_prediction(requested: &str, predicted: &str) {
    let requested = requested.trim().to_ascii_lowercase();
    let predicted = predicted.trim().to_ascii_lowercase();
    if !predicted.is_empty()
        && predicted != LEGACY_TARGET
        && predicted != requested
        && (parse(&requested).is_none() || parse(&predicted).is_none())
    {
        eprintln!("model_detection_diagnostic level=debug reason=unsupported_model");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            ("gpt-6-sol", "gpt-5.6-sol", true),
            ("gpt-5.6-sol", "gpt-6-sol", false),
            ("gpt-6-luna", "gpt-5.9-sol", false),
            ("gpt-6.1-astra", "gpt-6-astra", true),
            ("gpt-6-astra", "gpt-6.0-astra", false),
            ("gpt-5.10-sol", "gpt-5.9-sol", true),
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
                true,
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
        for bank in [
            Value::Null,
            json!({}),
            json!({"models":{}}),
            json!({"models":[null,1,{}]}),
        ] {
            assert!(!bank_contains(&bank, LEGACY_TARGET));
        }
        assert!(bank_contains(
            &json!({"models":[{"id":LEGACY_TARGET}]}),
            LEGACY_TARGET
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
