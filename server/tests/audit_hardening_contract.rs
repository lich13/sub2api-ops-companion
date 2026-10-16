use serde_json::{json, Value};
use std::{fs, panic::AssertUnwindSafe};
use sub2ops_server::{audit, error_evidence, operations, proxy_diagnostics};

#[test]
fn audit_write_redacts_nested_sensitive_values_and_preserves_public_structure() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("audit.jsonl");
    let payload = json!({
        "kind":"fixture-audit",
        "attempt":17,
        "enabled":true,
        "empty":"",
        "normal_message":"fixture operation completed",
        "nested":[
            {
                "label":"fixture-child",
                "api_key":"fixture-audit-api-key",
                "access_token":"fixture-audit-access-token",
                "count":3,
                "blank":""
            },
            {"authorization":"Bearer fixture-audit-authorization","ok":false}
        ],
        "credentials":{"username":"fixture-user","password":"fixture-audit-password"},
        "prompt":"fixture-audit-prompt",
        "request_body":{"messages":[{"content":"fixture-audit-content"}]}
    });

    audit::write(&path, "fixture.operation", payload, false);

    let records = audit::read(&path, 10, Some("fixture."));
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record["event"], "fixture.operation");
    let saved = &record["payload"];
    assert_eq!(saved["kind"], "fixture-audit");
    assert_eq!(saved["attempt"], 17);
    assert_eq!(saved["enabled"], true);
    assert_eq!(saved["empty"], "");
    assert_eq!(saved["normal_message"], "fixture operation completed");
    assert_eq!(saved["nested"][0]["label"], "fixture-child");
    assert_eq!(saved["nested"][0]["api_key"], "[REDACTED]");
    assert_eq!(saved["nested"][0]["access_token"], "[REDACTED]");
    assert_eq!(saved["nested"][0]["count"], 3);
    assert_eq!(saved["nested"][0]["blank"], "");
    assert_eq!(saved["nested"][1]["authorization"], "[REDACTED]");
    assert_eq!(saved["nested"][1]["ok"], false);
    assert_eq!(saved["credentials"], "[REDACTED]");
    assert_eq!(saved["prompt"], "[REDACTED]");
    assert_eq!(saved["request_body"], "[REDACTED]");

    let serialized = record.to_string();
    for marker in [
        "fixture-audit-api-key",
        "fixture-audit-access-token",
        "fixture-audit-authorization",
        "fixture-audit-password",
        "fixture-audit-prompt",
        "fixture-audit-content",
    ] {
        assert!(
            !serialized.contains(marker),
            "audit record leaked a fixture marker"
        );
    }
}

#[test]
fn audit_new_file_is_private_and_read_only_mode_does_not_write() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("nested").join("audit.jsonl");
    audit::write(&path, "fixture.private", json!({"value":1}), false);
    assert!(path.is_file());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let read_only_path = root.path().join("read-only-audit.jsonl");
    audit::write(
        &read_only_path,
        "fixture.read-only",
        json!({"value":1}),
        true,
    );
    assert!(!read_only_path.exists());
    fs::write(&read_only_path, b"existing fixture record\n").unwrap();
    let before = fs::read(&read_only_path).unwrap();
    audit::write(
        &read_only_path,
        "fixture.read-only",
        json!({"value":2}),
        true,
    );
    assert_eq!(fs::read(&read_only_path).unwrap(), before);
}

#[test]
fn audit_write_failure_is_ignored_without_panicking() {
    let root = tempfile::tempdir().unwrap();
    let parent_file = root.path().join("file-used-as-parent");
    fs::write(&parent_file, b"fixture").unwrap();
    let path = parent_file.join("audit.jsonl");

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        audit::write(&path, "fixture.failure", json!({"value":1}), false);
    }));

    assert!(result.is_ok());
    assert!(parent_file.is_file());
}

#[test]
fn capacity_message_is_classified_before_audit_redacts_attached_credentials() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("capacity-audit.jsonl");
    let error_body = json!({
        "message":error_evidence::CAPACITY_MESSAGES[1],
        "credentials":{"username":"fixture-user","refresh_token":"fixture-audit-refresh-token"},
        "api_key":"fixture-audit-api-key",
        "bark_device_key":"fixture-audit-bark-device-key",
        "diagnostics":{"device_key":"fixture-audit-device-key"}
    });
    let row = json!({
        "account_platform":"openai",
        "account_type":"oauth",
        "error_owner":"provider",
        "error_phase":"upstream",
        "error_body":error_body.clone()
    });

    let matched = error_evidence::match_message(&row);
    assert_eq!(matched, Some(error_evidence::CAPACITY_MESSAGES[1]));

    audit::write(
        &path,
        "fixture.capacity_evidence",
        json!({"classification":matched,"evidence":{"error_body":error_body}}),
        false,
    );

    let records = audit::read(&path, 10, Some("fixture.capacity"));
    assert_eq!(records.len(), 1);
    let evidence = &records[0]["payload"]["evidence"]["error_body"];
    assert_eq!(evidence["message"], error_evidence::CAPACITY_MESSAGES[1]);
    assert_eq!(evidence["credentials"], "[REDACTED]");
    assert_eq!(evidence["api_key"], "[REDACTED]");
    assert_eq!(evidence["bark_device_key"], "[REDACTED]");
    assert_eq!(evidence["diagnostics"]["device_key"], "[REDACTED]");

    let serialized = records[0].to_string();
    for marker in [
        "fixture-audit-refresh-token",
        "fixture-audit-api-key",
        "fixture-audit-bark-device-key",
        "fixture-audit-device-key",
    ] {
        assert!(
            !serialized.contains(marker),
            "audit evidence leaked a fixture marker"
        );
    }
}

fn quality_diagnostic(items: Value) -> Value {
    json!({
        "score": 90,
        "grade": "A",
        "summary": "fixture quality result",
        "items": items
    })
}

#[test]
fn proxy_quality_result_rejects_malformed_items() {
    let valid = quality_diagnostic(json!([{"status":"pass"}]));
    let safe = proxy_diagnostics::safe_result("quality-check", &valid)
        .expect("well-shaped quality items remain accepted");
    assert_eq!(safe["items"][0]["status"], "pass");

    let mut missing_items = valid.clone();
    missing_items.as_object_mut().unwrap().remove("items");
    let too_many_items = quality_diagnostic(json!(vec![json!({"status":"pass"}); 33]));
    let cases = [
        ("missing items", missing_items),
        (
            "items is not an array",
            quality_diagnostic(json!({"status":"pass"})),
        ),
        ("too many items", too_many_items),
        ("item is not an object", quality_diagnostic(json!([null]))),
        (
            "unsupported item status",
            quality_diagnostic(json!([{"status":"unknown"}])),
        ),
    ];

    for (case, data) in cases {
        let error = proxy_diagnostics::safe_result("quality-check", &data).expect_err(case);
        assert_eq!(error.status.as_u16(), 502, "{case}");
        assert_eq!(error.detail, "代理质量响应格式不兼容", "{case}");
    }
}

fn valid_operation_batch_state() -> Value {
    json!({
        "version": 1,
        "jobs": {
            "fixture-job": {
                "account_id": 7,
                "payload": {},
                "status": "queued",
                "batch_id": "fixture-batch"
            }
        },
        "requests": {},
        "batches": {"fixture-batch": {"job_ids": ["fixture-job"]}},
        "batch_requests": {}
    })
}

#[test]
fn operation_batch_job_ids_reject_malformed_references() {
    let valid = valid_operation_batch_state();
    operations::validate_store(&valid).expect("well-formed batch job reference");

    let mut missing_ids = valid.clone();
    missing_ids["batches"]["fixture-batch"]
        .as_object_mut()
        .unwrap()
        .remove("job_ids");
    let mut mismatched_batch = valid.clone();
    mismatched_batch["jobs"]["fixture-job"]["batch_id"] = "other-batch".into();
    let cases = [
        ("missing job_ids", missing_ids),
        ("job_ids is not an array", {
            let mut data = valid.clone();
            data["batches"]["fixture-batch"]["job_ids"] = json!("fixture-job");
            data
        }),
        ("job id is not a string", {
            let mut data = valid.clone();
            data["batches"]["fixture-batch"]["job_ids"] = json!([null]);
            data
        }),
        ("job id is missing", {
            let mut data = valid.clone();
            data["batches"]["fixture-batch"]["job_ids"] = json!(["missing-job"]);
            data
        }),
        ("job belongs to another batch", mismatched_batch),
    ];

    for (case, data) in cases {
        let error = operations::validate_store(&data).expect_err(case);
        assert_eq!(error.status.as_u16(), 503, "{case}");
        assert_eq!(error.detail, "操作状态无法读取", "{case}");
    }
}
