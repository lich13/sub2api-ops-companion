use serde_json::json;
use std::collections::BTreeMap;
use sub2ops_server::{compat::sha256, sub2api_config::*};
fn bytes() -> Vec<u8> {
    b"jwt:\n  secret: fixture-configuration-key-for-tests-only\nlog:\n  level: info\n".to_vec()
}
fn payload(path: &str, value: serde_json::Value) -> serde_json::Value {
    json!({"expected_revision":sha256(bytes()),"runtime_revision":"a".repeat(64),"changes":[{"path":path,"remove":false,"value":value}]})
}
#[test]
fn catalogue_matches_reviewed_version_and_fields_have_explanations() {
    let fields = CATALOG["fields"].as_array().unwrap();
    assert_eq!(fields.len(), 545);
    assert_eq!(fields.iter().filter(|f| f["internal"] == true).count(), 16);
    assert_eq!(CATALOG["groups"].as_array().unwrap().len(), 41);
    assert!(fields
        .iter()
        .all(|f| !f["description"].as_str().unwrap().is_empty()
            && !f["range"].as_str().unwrap().is_empty()
            && f["source_url"]
                .as_str()
                .unwrap()
                .contains(CATALOG["revision"].as_str().unwrap())));
}
#[test]
fn validates_types_ranges_conditions_and_cross_field_limits() {
    for (path, value) in [
        ("log.level", json!("trace")),
        ("jwt.expire_hour", json!(169)),
        ("server.h2c.enabled", json!("true")),
        ("database.max_idle_conns", json!(100000)),
        (
            "gateway.scheduling.sticky_session_wait_timeout",
            json!("-1s"),
        ),
    ] {
        assert!(
            prepare(&bytes(), &BTreeMap::new(), &payload(path, value)).is_err(),
            "{path}"
        );
    }
    let (out, _) = prepare(
        &bytes(),
        &BTreeMap::new(),
        &payload("gateway.stream_data_interval_timeout", json!(0)),
    )
    .unwrap();
    assert!(String::from_utf8(out)
        .unwrap()
        .contains("stream_data_interval_timeout: 0"));
}
#[test]
fn environment_priority_and_explicit_empty_lists_are_honored() {
    let mut env = BTreeMap::from([
        ("JWT_EXPIRE_HOUR".into(), "168".into()),
        ("TZ".into(), "UTC".into()),
        ("TIMEZONE".into(), "Asia/Shanghai".into()),
        ("SERVER_TRUSTED_PROXIES".into(), "".into()),
    ]);
    assert!(prepare(&bytes(), &env, &payload("jwt.expire_hour", json!(24))).is_err());
    assert_eq!(
        override_value(field("timezone").unwrap(), &env).unwrap(),
        ("TZ".into(), json!("UTC"))
    );
    assert_eq!(
        override_value(field("server.trusted_proxies").unwrap(), &env)
            .unwrap()
            .1,
        json!([])
    );
    env.insert("JWT_EXPIRE_HOUR".into(), "".into());
    assert!(override_value(field("jwt.expire_hour").unwrap(), &env).is_none());
}
#[test]
fn special_environment_aliases_and_legacy_precedence_match_upstream() {
    let env = BTreeMap::from([
        ("WECHAT_OAUTH_OPEN_APP_ID".into(), "fixture-id".into()),
        (
            "WECHAT_OAUTH_OPEN_APP_SECRET".into(),
            "fixture-secret".into(),
        ),
        (
            "WEBAUTHN_RP_ORIGINS".into(),
            "https://example.invalid,https://app.example.invalid".into(),
        ),
    ]);
    assert_eq!(
        environment_override(field("wechat_connect.enabled").unwrap(), &json!({}), &env)
            .unwrap()
            .1,
        true
    );
    assert!(environment_override(
        field("wechat_connect.open_app_id").unwrap(),
        &json!({"wechat_connect":{"app_id":"explicit"}}),
        &env
    )
    .is_none());
    assert_eq!(
        override_value(field("webauthn.rp_origins").unwrap(), &env)
            .unwrap()
            .1,
        json!(["https://example.invalid", "https://app.example.invalid"])
    );
}
#[test]
fn complex_structures_and_enabled_dependencies_are_validated() {
    for (path, value) in [
        (
            "gemini.quota.tiers",
            json!({"fixture":{"pro_rpd":"invalid"}}),
        ),
        (
            "security.proxy_probe.urls",
            json!([{"url":"https://example.invalid","parser":"unsupported"}]),
        ),
        ("linuxdo_connect.enabled", json!(true)),
        ("batch_image.vertex_enabled", json!(true)),
        ("webauthn.enabled", json!(true)),
    ] {
        assert!(
            prepare(&bytes(), &BTreeMap::new(), &payload(path, value)).is_err(),
            "{path}"
        );
    }
    let mut queue = payload("batch_image.queue_ready_key", json!(""));
    queue["changes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":"batch_image.queue_enabled","remove":false,"value":true}));
    assert!(prepare(&bytes(), &BTreeMap::new(), &queue).is_err());
    let value = json!({"fixture":{"pro_rpd":100,"flash_rpd":null,"cooldown_minutes":5}});
    assert!(prepare(
        &bytes(),
        &BTreeMap::new(),
        &payload("gemini.quota.tiers", value)
    )
    .is_ok());
    assert_eq!(duration_seconds("+.5s"), Some(0.5));
}
#[test]
fn invalid_yaml_and_duplicate_keys_do_not_reach_writes() {
    assert!(parse_yaml(b"server: 1\nserver: 2\n").is_err());
    assert!(parse_yaml(b"[1,2]").is_err());
    assert!(parse_yaml(b"a: [").is_err());
    assert!(parse_yaml(b"a: 1\n---\nb: 2\n").is_err());
    let error = parse_yaml(b"password: [fixture-private-value\n").unwrap_err();
    assert_eq!(error.status.as_u16(), 422);
    assert!(!error.detail.to_string().contains("fixture-private-value"));
}
#[test]
fn maintained_yaml_parser_preserves_existing_scalar_alias_and_tag_semantics() {
    for (raw, expected) in [
        (
            "a: on\nb: yes\nc: true\nd: 01\ne: 0x10\nf: 1.25\ng: null\nh: TRUE\n",
            json!({"a":"on","b":"yes","c":true,"d":"01","e":16,"f":1.25,"g":null,"h":true}),
        ),
        (
            "port: '8080'\nflag: 'true'\nempty: ''\nmulti: |\n  hello\n  world\n",
            json!({"port":"8080","flag":"true","empty":"","multi":"hello\nworld\n"}),
        ),
        (
            "first: &value {port: 8080}\nsecond: *value\n",
            json!({"first":{"port":8080},"second":{"port":8080}}),
        ),
        (
            "base: &base {port: 8080}\nserver: {<<: *base, host: '127.0.0.1'}\n",
            json!({"base":{"port":8080},"server":{"<<":{"port":8080},"host":"127.0.0.1"}}),
        ),
        (
            "value: !Fixture {port: 8080}\n",
            json!({"value":{"!Fixture":{"port":8080}}}),
        ),
        (
            "value: !Fixture hello\n",
            json!({"value":{"!Fixture":"hello"}}),
        ),
        ("value: !!str 123\n", json!({"value":"123"})),
        ("value: !str 123\n", json!({"value":{"!str":123}})),
        (
            "value: !<tag:example.invalid,2026:fixture> hello\n",
            json!({"value":"hello"}),
        ),
        ("value: !!binary SGVsbG8=\n", json!({"value":"SGVsbG8="})),
        ("true: value\n", json!({"true":"value"})),
        ("1.5: value\n", json!({"1.5":"value"})),
        ("value: .nan\n", json!({"value":null})),
        ("1: value\n", json!({"1":"value"})),
    ] {
        assert_eq!(parse_yaml(raw.as_bytes()).unwrap(), expected, "{raw}");
    }
}
#[test]
fn yaml_write_preserves_unknown_values_alias_contents_and_scalar_types() {
    let source = b"log:\n  level: info\nunknown:\n  base: &base {port: '8080', code: 01}\n  alias: *base\n  tagged: !Fixture {enabled: true}\n  literal: !!str 123\n  binary: !!binary SGVsbG8=\n  nonfinite: .nan\n";
    let before = parse_yaml(source).unwrap();
    let mut request = payload("log.level", json!("debug"));
    request["expected_revision"] = json!(sha256(source));
    let (output, value) = prepare(source, &BTreeMap::new(), &request).unwrap();
    assert_eq!(value["unknown"], before["unknown"]);
    assert_eq!(value["log"]["level"], "debug");
    assert_eq!(parse_yaml(&output).unwrap(), value);
}
#[test]
fn removes_only_requested_keys_preserves_unknowns_and_secret() {
    let mut request = payload("log.level", json!(null));
    request["changes"][0]["remove"] = json!(true);
    let (_, value) = prepare(&bytes(), &BTreeMap::new(), &request).unwrap();
    assert!(value.get("log").is_none());
    assert_eq!(
        value["jwt"]["secret"],
        "fixture-configuration-key-for-tests-only"
    );
    request["changes"][0]["path"] = json!("jwt.secret");
    let (_, value) = prepare(&bytes(), &BTreeMap::new(), &request).unwrap();
    assert!(value.get("jwt").is_none());
}
#[test]
fn diagnostic_text_is_complete_including_credentials_and_line_breaks() {
    let text = format!(
        "https://example.invalid/?token=fixture\n  password=fixture\n{}",
        "长错误".repeat(400)
    );
    let value = sub2ops_server::proxy_diagnostics::safe_result(
        "test",
        &json!({"success":false,"message":text}),
    )
    .unwrap();
    assert_eq!(value["message"], text);
    let quality=sub2ops_server::proxy_diagnostics::safe_result("quality-check",&json!({"score":0,"grade":"F","summary":text,"items":[{"target":"https://example.invalid","status":"fail","message":text}]})).unwrap();
    assert_eq!(quality["items"][0]["message"], text);
    assert_eq!(quality["summary"], text);
}
