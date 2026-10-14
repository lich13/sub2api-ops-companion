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
