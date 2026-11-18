use crate::compat::truthy;
use serde_json::{json, Value};

pub fn merge(base: &Value, patch: &Value) -> Value {
    let mut base = base
        .as_object()
        .cloned()
        .map(Value::Object)
        .unwrap_or(json!({}));
    for (key, value) in patch.as_object().into_iter().flatten() {
        base[key] = if value.is_object() && base[key].is_object() {
            merge(&base[key], value)
        } else {
            value.clone()
        };
    }
    base
}
enum ModelPattern<'a> {
    Exact(&'a str),
    Wildcard(Option<regex::Regex>),
}
impl<'a> ModelPattern<'a> {
    fn new(pattern: &'a str) -> Self {
        // Keep the regex engine's size-limit behavior for oversized literals.
        if !pattern.contains('*') && pattern.len() <= 4096 {
            return Self::Exact(pattern);
        }
        let regex = format!(
            "^(?:{})$",
            pattern
                .split('*')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(".*")
        );
        Self::Wildcard(regex::Regex::new(&regex).ok())
    }
    fn matches(&self, model: &str) -> bool {
        match self {
            Self::Exact(pattern) => *pattern == model,
            Self::Wildcard(regex) => regex.as_ref().is_some_and(|regex| regex.is_match(model)),
        }
    }
}
pub(crate) struct Allowlist<'a>(Option<Vec<ModelPattern<'a>>>);
impl<'a> Allowlist<'a> {
    pub(crate) fn new(value: &'a Value) -> Self {
        Self(truthy(&value["enabled"]).then(|| {
            value["models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(ModelPattern::new)
                .collect()
        }))
    }
    pub(crate) fn admits(&self, model: &str) -> bool {
        self.0
            .as_ref()
            .is_none_or(|patterns| patterns.iter().any(|pattern| pattern.matches(model)))
    }
}

#[cfg(test)]
mod matching_tests {
    use super::*;

    fn legacy(pattern: &str) -> Option<regex::Regex> {
        regex::Regex::new(&format!(
            "^(?:{})$",
            pattern
                .split('*')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(".*")
        ))
        .ok()
    }

    #[test]
    fn exact_and_batch_matching_preserve_unicode_escaping_and_newline_semantics() {
        let patterns = [
            "",
            "*",
            "gpt-*",
            "中文*模型",
            "a+b.[x]$",
            "a\\b",
            "ab**cd",
            "a\nb",
            "*\nb",
            "模型🦀",
        ];
        let models = [
            "",
            "gpt-fixture",
            "GPT-fixture",
            "中文测试模型",
            "a+b.[x]$",
            "axb",
            "a\\b",
            "abcd",
            "ab\ncd",
            "a\nb",
            "gpt-fixture\n",
            "模型🦀",
        ];
        for pattern in patterns {
            let old = legacy(pattern).unwrap();
            let value = json!({"enabled":true,"models":[pattern,null,4]});
            let batch = Allowlist::new(&value);
            for model in models {
                let expected = old.is_match(model);
                assert_eq!(
                    ModelPattern::new(pattern).matches(model),
                    expected,
                    "{pattern:?} / {model:?}"
                );
                assert_eq!(
                    batch.admits(model),
                    expected,
                    "batch: {pattern:?} / {model:?}"
                );
            }
        }
        for (value, expected) in [
            (Value::Null, true),
            (json!({"enabled":false}), true),
            (json!({"enabled":true}), false),
            (json!({"enabled":true,"models":[null,4]}), false),
        ] {
            let batch = Allowlist::new(&value);
            for model in models {
                assert_eq!(batch.admits(model), expected);
            }
        }
    }

    #[test]
    fn oversized_patterns_keep_regex_resource_limits() {
        for pattern in ["a".repeat(5000), "*".repeat(100_000)] {
            let old = legacy(&pattern);
            assert_eq!(
                ModelPattern::new(&pattern).matches(&pattern),
                old.as_ref().is_some_and(|v| v.is_match(&pattern))
            );
            if pattern.starts_with('*') {
                assert!(old.is_none());
            }
        }
    }

    #[test]
    fn recursive_merge_replaces_arrays_and_preserves_explicit_null_and_false() {
        let base = json!({
            "nested": {"keep": 1, "replace": true, "clear": "old"},
            "items": ["a", "b"],
            "flag": true,
        });
        let patch = json!({
            "nested": {"replace": false, "clear": null, "added": "new"},
            "items": ["c"],
            "flag": false,
        });
        assert_eq!(
            merge(&base, &patch),
            json!({
                "nested": {"keep": 1, "replace": false, "clear": null, "added": "new"},
                "items": ["c"],
                "flag": false,
            })
        );
        assert_eq!(merge(&Value::Null, &json!({"k": 1})), json!({"k": 1}));
    }
}
