use serde_json::Value;
use sub2ops_server::validation;

#[test]
fn operation_payload_types_defaults_and_unknown_fields_match_pinned_python() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../tests/contracts/request-fixtures.json")).unwrap();
    assert_eq!(
        fixture["reference_sha"],
        "6be7c291ed38dcc337cb88f693639040a3463899"
    );
    let mut mismatches = vec![];
    for case in fixture["cases"].as_array().unwrap() {
        let actual =
            validation::operation_payload(case["action"].as_str().unwrap(), &case["payload"]);
        let expected = &case["expected"];
        match actual {
            Ok(payload) => {
                if expected["status"] != 200 || payload != expected["payload"] {
                    mismatches.push(format!("{}: accepted payload differs", case["id"]));
                }
            }
            Err(error) => {
                if expected["status"] != error.status.as_u16() {
                    mismatches.push(format!("{}: status {}", case["id"], error.status.as_u16()));
                }
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn template_account_integer_normalization_matches_pinned_python() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../tests/contracts/request-fixtures.json")).unwrap();
    for case in fixture["template_integer_cases"].as_array().unwrap() {
        let result = validation::coercible_integer(&case["input"], 1, i64::MAX);
        match result {
            Ok(value) => {
                assert_eq!(case["expected"]["status"], 200, "{}", case["id"]);
                assert_eq!(
                    value,
                    case["expected"]["value"].as_i64().unwrap(),
                    "{}",
                    case["id"]
                );
            }
            Err(error) => assert_eq!(
                error.status.as_u16(),
                case["expected"]["status"],
                "{}",
                case["id"]
            ),
        }
    }
}

#[test]
fn key_detection_overrides_validate_the_new_contract() {
    use serde_json::json;
    use sub2ops_server::model_detection::detection_request;
    let base = json!({"expected_version":"a".repeat(64), "enabled":false, "interval_minutes":15});
    for (model, normalized) in [
        (json!(null), json!(null)),
        (json!("  fixture-model  "), json!("fixture-model")),
    ] {
        let mut payload = base.clone();
        payload["model_override"] = model;
        assert_eq!(
            detection_request(&payload).unwrap()["model_override"],
            normalized
        );
    }
    for model in [
        json!(false),
        json!({}),
        json!([]),
        json!(""),
        json!("bad model"),
        json!("*"),
    ] {
        let mut payload = base.clone();
        payload["model_override"] = model;
        assert!(detection_request(&payload).is_err());
    }
}

#[test]
fn smart_detection_fields_are_optional_strict_and_preserved_by_request_validation() {
    use serde_json::json;
    use sub2ops_server::model_detection::detection_request;
    let base = json!({"expected_version":"a".repeat(64),"enabled":true,"interval_minutes":37});
    let legacy = detection_request(&base).unwrap();
    assert!(legacy.get("schedule_mode").is_none());
    assert!(legacy.get("min_interval_minutes").is_none());
    for (mode, floor) in [
        (json!("smart"), json!(5)),
        (json!("fixed"), json!(30)),
        (json!("smart"), json!(17)),
    ] {
        let mut request = base.clone();
        request["schedule_mode"] = mode.clone();
        request["min_interval_minutes"] = floor.clone();
        let request = detection_request(&request).unwrap();
        assert_eq!(request["schedule_mode"], mode);
        assert_eq!(request["min_interval_minutes"], floor);
    }
    for (field, value) in [
        ("schedule_mode", json!(null)),
        ("schedule_mode", json!("auto")),
        ("min_interval_minutes", json!(4)),
        ("min_interval_minutes", json!(31)),
        ("min_interval_minutes", json!(5.5)),
        ("min_interval_minutes", json!("5")),
    ] {
        let mut request = base.clone();
        request[field] = value;
        assert!(detection_request(&request).is_err());
    }
}

#[test]
fn verdict_policy_accepts_partial_saves_without_defaulting_schedule_fields() {
    use serde_json::json;
    use sub2ops_server::model_detection::detection_request;
    let base = json!({"expected_version":"a".repeat(64),"verdict_mode":"relaxed","degraded_model_id":" OPENAI/GPT-5.6-LUNA "});
    let parsed = detection_request(&base).unwrap();
    assert_eq!(parsed["degraded_model_id"], "gpt-5.6-luna");
    assert!(parsed.get("enabled").is_none());
    assert!(parsed.get("interval_minutes").is_none());
    for (field, value) in [
        ("verdict_mode", json!("unknown")),
        ("verdict_mode", json!(null)),
        ("degraded_model_id", json!(true)),
        ("degraded_model_id", json!(" ")),
        ("degraded_model_id", json!("bad model")),
        ("interval_minutes", json!(30)),
        ("schedule_mode", json!("smart")),
        ("enabled", json!(null)),
    ] {
        let mut invalid = base.clone();
        invalid[field] = value;
        assert!(detection_request(&invalid).is_err(), "{invalid}");
    }
    assert!(detection_request(&json!({"expected_version":"a".repeat(64)})).is_err());
    let legacy =
        detection_request(&json!({"expected_version":"a".repeat(64),"enabled":false})).unwrap();
    assert!(legacy.get("verdict_mode").is_none());
    assert!(legacy.get("degraded_model_id").is_none());
}
