use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::Path, sync::Arc};
use sub2ops_server::{compat::config_revision, db::Database, runtime::Runtime, settings::Settings};

const C: &str = "oauth_recovery_connection_account_ids";
const M: &str = "oauth_recovery_model_account_ids";
const S: &str = "oauth_recovery_seen_account_ids";
const D: &str = "model_detection_model_id";
const DEFAULT_DETECTION_MODEL: &str = "gpt-6-luna";

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/contracts/recovery-selection/fixtures.json"
    ))
    .unwrap()
}

fn environment(root: &Path, url: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("DATABASE_URL".into(), url.into()),
        ("SUB2API_BASE_URL".into(), "http://127.0.0.1:1".into()),
        ("BASE_PATH".into(), String::new()),
    ]);
    for (key, file) in [
        ("AUDIT_PATH", "audit.jsonl"),
        ("OAUTH_CONFIG_PATH", "oauth.json"),
        ("BARK_CONFIG_PATH", "bark.json"),
        ("KEY_FALLBACK_CONFIG_PATH", "fallback.json"),
        ("USAGE_QUERY_STATE_PATH", "state.json"),
        ("SUB2API_CONFIG_PATH", "connection.json"),
        ("GROUP_MODEL_CONFIG_PATH", "models.json"),
        ("KEEPALIVE_CONFIG_PATH", "keepalive-state.json"),
    ] {
        env.insert(key.into(), root.join(file).to_string_lossy().into());
    }
    env
}

struct Harness {
    root: tempfile::TempDir,
    runtime: Arc<Runtime>,
    owner: Database,
    schema: String,
}

impl Harness {
    async fn new(raw: &Value) -> Self {
        let root = tempfile::tempdir().unwrap();
        let owner = Database::open(&std::env::var("DATABASE_URL").unwrap(), false)
            .await
            .unwrap();
        let schema = format!("selection_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&owner.pool)
            .await
            .unwrap();
        let mut url = reqwest::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let scoped = Database::open(url.as_str(), false).await.unwrap();
        let ddl = include_str!("../../tests/contracts/http/schema.sql")
            .split("-- Every row below")
            .next()
            .unwrap();
        sqlx::raw_sql(ddl).execute(&scoped.pool).await.unwrap();
        sqlx::query("INSERT INTO accounts (id,name,platform,type,schedulable) VALUES (31,'fixture-31','openai','oauth',true),(32,'fixture-32','openai','oauth',true)")
            .execute(&scoped.pool).await.unwrap();
        scoped.close().await;
        fs::write(
            root.path().join("oauth.json"),
            serde_json::to_vec(raw).unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                root.path().join("oauth.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let mut settings =
            Settings::from_env(&environment(root.path(), url.as_str()), false).unwrap();
        settings.oauth = fixture()["defaults"].clone();
        let runtime = Runtime::new(settings).await.unwrap();
        Self {
            root,
            runtime,
            owner,
            schema,
        }
    }

    fn raw(&self) -> Value {
        let mut raw = self.runtime.config.oauth.read().unwrap();
        raw.as_object_mut().unwrap().shift_remove("updated_at");
        raw.as_object_mut().unwrap().shift_remove("updated_by");
        raw
    }

    async fn close(self) {
        self.runtime.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.owner.pool)
            .await
            .unwrap();
        self.owner.close().await;
    }
}

fn row(id: i64) -> Value {
    json!({"id":id,"name":format!("fixture-{id}"),"platform":"openai","type":"oauth",
        "status":"active","schedulable":true,"deleted_at":null,"parent_account_id":null,
        "credentials":{"plan_type":"plus"},"extra":{}})
}

fn without_detection_model(value: &Value) -> Value {
    let mut projected = value.clone();
    projected
        .as_object_mut()
        .expect("configuration value must be an object")
        .remove(D);
    projected
}

fn assert_snapshot_revision(snapshot: &Value, raw: &Value) {
    let oauth = snapshot["oauth"]
        .as_object()
        .expect("oauth snapshot must be an object");
    assert_eq!(oauth[D], DEFAULT_DETECTION_MODEL);

    let mut current_view = Value::Object(oauth.clone());
    let current_revision = current_view
        .as_object_mut()
        .unwrap()
        .remove("revision")
        .expect("oauth snapshot must include a revision");
    assert_eq!(
        current_revision,
        config_revision(&json!([current_view, raw])),
        "snapshot revision must include the global detection model"
    );
}

fn assert_initial_snapshot(h: &Harness, case: &Value) {
    let snapshot = h.runtime.config.snapshot(Some("oauth")).unwrap();
    let raw = h.raw();
    assert_snapshot_revision(&snapshot, &raw);

    let mut legacy_view = without_detection_model(&snapshot["oauth"]);
    legacy_view
        .as_object_mut()
        .unwrap()
        .remove("revision")
        .expect("oauth snapshot must include a revision");
    let expected_revision = case["initial"]["revision"].clone();
    assert_eq!(
        config_revision(&json!([legacy_view, without_detection_model(&raw)])),
        expected_revision,
        "legacy revision projection must match the fixed Python vector"
    );

    let mut projected = without_detection_model(&snapshot["oauth"]);
    projected["revision"] = expected_revision;
    assert_eq!(projected, case["initial"]);
}

fn assert_raw_compat(actual: &Value, expected: &Value, action: &str, name: &str, index: usize) {
    if action == "save" {
        assert_eq!(
            actual[D], DEFAULT_DETECTION_MODEL,
            "{name} step {index} save must persist the global detection model"
        );
    } else if let Some(model) = actual.get(D) {
        assert_eq!(
            model, DEFAULT_DETECTION_MODEL,
            "{name} step {index} persisted global detection model"
        );
    }
    let projected = without_detection_model(actual);
    assert_eq!(projected, *expected, "{name} step {index}");
}

async fn scenario(name: &str) {
    let fixture = fixture();
    assert_eq!(
        fixture["reference_sha"],
        "6be7c291ed38dcc337cb88f693639040a3463899"
    );
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap();
    let h = Harness::new(&case["raw"]).await;
    assert_initial_snapshot(&h, case);
    for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
        if step["action"] == "reconcile" {
            h.runtime
                .config
                .reconcile(step["rows"].as_array().unwrap())
                .await
                .unwrap();
        } else {
            let before = h.runtime.config.snapshot(Some("oauth")).unwrap();
            h.runtime
                .config
                .save(
                    "oauth",
                    &step["changes"],
                    "fixture-user",
                    before["oauth"]["revision"].as_str(),
                )
                .await
                .unwrap();
        }
        assert_raw_compat(
            &h.raw(),
            &case["expected"][index],
            step["action"].as_str().unwrap(),
            name,
            index,
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(h.runtime.settings.oauth_path.clone())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    if name == "partial_save" {
        let reloaded = Settings::from_env(
            &environment(h.root.path(), &h.runtime.settings.database_url),
            false,
        )
        .unwrap();
        assert_eq!(reloaded.oauth[C], json!([]));
        assert_eq!(reloaded.oauth[M], json!([]));
    }
    h.close().await;
}

#[tokio::test]
async fn legacy_selection_filters_inventory_and_adds_first_new_account_once() {
    scenario("legacy").await;
}
#[tokio::test]
async fn explicit_empty_selection_records_only_seen_baseline() {
    scenario("explicit_empty").await;
}
#[tokio::test]
async fn explicit_selection_keeps_both_modes_and_records_seen_baseline() {
    scenario("explicit_selected").await;
}
#[tokio::test]
async fn manual_removal_and_reappearance_never_reselect_seen_accounts() {
    scenario("sticky").await;
}
#[tokio::test]
async fn model_mode_switch_survives_inventory_reconcile() {
    scenario("model_mode").await;
}
#[tokio::test]
async fn partial_old_client_save_preserves_modes_and_explicit_empty_reload() {
    scenario("partial_save").await;
}

#[tokio::test]
async fn invalid_selection_and_overlap_never_partially_write() {
    let h = Harness::new(&json!({C:[31],M:[32]})).await;
    let before = fs::read(&h.runtime.settings.oauth_path).unwrap();
    let initial = h.runtime.config.snapshot(Some("oauth")).unwrap();
    for changes in [
        json!({C:[31,32]}),
        json!({M:[31,32]}),
        json!({C:[31],M:[31]}),
        json!({C:[true],M:[]}),
        json!({C:[0],M:[]}),
        json!({C:[-1],M:[]}),
        json!({C:[31.5],M:[]}),
        json!({C:"31",M:[]}),
        json!({C:[99],M:[]}),
    ] {
        let error = h
            .runtime
            .config
            .save("oauth", &changes, "fixture-user", None)
            .await
            .unwrap_err();
        assert_eq!(error.status.as_u16(), 422);
        assert_eq!(fs::read(&h.runtime.settings.oauth_path).unwrap(), before);
        assert_eq!(h.runtime.config.snapshot(Some("oauth")).unwrap(), initial);
    }
    h.close().await;
}

#[tokio::test]
async fn failed_reconcile_write_preserves_selection_seen_and_effective_config() {
    let h = Harness::new(&json!({C:[31],M:[],S:[31]})).await;
    let before = fs::read(&h.runtime.settings.oauth_path).unwrap();
    let current = h.runtime.config.current().unwrap();
    let lock = h.runtime.settings.oauth_path.with_extension("lock");
    fs::create_dir(&lock).unwrap();
    assert!(h
        .runtime
        .config
        .reconcile(&[row(31), row(39)])
        .await
        .is_err());
    assert_eq!(fs::read(&h.runtime.settings.oauth_path).unwrap(), before);
    assert_eq!(h.runtime.config.current().unwrap(), current);
    assert_eq!(h.raw()[S], json!([31]));
    fs::remove_dir(lock).unwrap();
    h.close().await;
}

#[tokio::test]
async fn corrupt_or_deleted_existing_config_cannot_be_reinitialized() {
    let raw = json!({C:[31],M:[],S:[31]});
    let h = Harness::new(&raw).await;
    h.runtime.config.reconcile(&[row(31)]).await.unwrap();
    fs::write(&h.runtime.settings.oauth_path, "{").unwrap();
    assert!(h
        .runtime
        .config
        .reconcile(&[row(31), row(39)])
        .await
        .is_err());
    assert_eq!(
        fs::read_to_string(&h.runtime.settings.oauth_path).unwrap(),
        "{"
    );
    fs::write(
        &h.runtime.settings.oauth_path,
        serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
    h.runtime.config.reconcile(&[row(31)]).await.unwrap();
    fs::remove_file(&h.runtime.settings.oauth_path).unwrap();
    assert!(h
        .runtime
        .config
        .reconcile(&[row(31), row(39)])
        .await
        .is_err());
    assert!(!h.runtime.settings.oauth_path.exists());
    h.close().await;
}

#[tokio::test]
async fn reloaded_runtime_keeps_seen_watermark_and_manual_removal() {
    let mut h = Harness::new(&json!({C:[32],M:[],S:[31,32]})).await;
    let settings = (*h.runtime.settings).clone();
    h.runtime.close().await;
    // Release the previous runtime's process lease before constructing its replacement.
    let Harness {
        root,
        runtime,
        owner,
        schema,
    } = h;
    drop(runtime);
    let restarted = Runtime::new(settings).await.unwrap();
    restarted
        .config
        .reconcile(&[row(31), row(32)])
        .await
        .unwrap();
    assert_eq!(restarted.config.oauth.read().unwrap()[C], json!([32]));
    assert_eq!(restarted.config.oauth.read().unwrap()[S], json!([31, 32]));
    h = Harness {
        root,
        runtime: restarted,
        owner,
        schema,
    };
    h.close().await;
}

#[tokio::test]
async fn stale_revision_cannot_overwrite_new_account_but_partial_save_keeps_it() {
    let h = Harness::new(&json!({C:[31,32],M:[],S:[31,32]})).await;
    let stale = h.runtime.config.snapshot(Some("oauth")).unwrap();
    assert_snapshot_revision(&stale, &h.raw());
    h.runtime
        .config
        .reconcile(&[row(31), row(32), row(39)])
        .await
        .unwrap();
    let before = fs::read(&h.runtime.settings.oauth_path).unwrap();
    let changes = json!({"oauth_daily_test_time":"08:00"});
    let error = h
        .runtime
        .config
        .save(
            "oauth",
            &changes,
            "fixture-user",
            stale["oauth"]["revision"].as_str(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 409);
    assert_eq!(fs::read(&h.runtime.settings.oauth_path).unwrap(), before);
    h.runtime
        .config
        .save("oauth", &changes, "fixture-user", None)
        .await
        .unwrap();
    let expected = fixture()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "stale_revision")
        .unwrap()["expected"][1]
        .clone();
    assert_raw_compat(&h.raw(), &expected, "save", "stale_revision", 1);
    h.close().await;
}

#[tokio::test]
async fn concurrent_reconcile_and_partial_save_preserve_all_fields_without_inventory() {
    let h = Harness::new(&json!({C:[31,32],M:[],S:[31,32]})).await;
    // All three operations already have their inputs. Making the live table
    // unavailable proves that a partial save does not fetch another inventory.
    sqlx::query("ALTER TABLE accounts RENAME TO fixture_accounts")
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    let first = [row(31), row(32), row(39)];
    let second = [row(31), row(32), row(40)];
    let changes = json!({"oauth_daily_test_time":"07:30"});
    let (a, b, c) = tokio::join!(
        h.runtime.config.reconcile(&first),
        h.runtime.config.reconcile(&second),
        h.runtime
            .config
            .save("oauth", &changes, "fixture-user", None)
    );
    a.unwrap();
    b.unwrap();
    c.unwrap();
    let expected = fixture()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "concurrent")
        .unwrap()["expected"][2]
        .clone();
    assert_raw_compat(&h.raw(), &expected, "save", "concurrent", 2);
    h.close().await;
}

#[tokio::test]
async fn failed_legacy_inventory_cannot_mark_selection_migrated() {
    let h = Harness::new(&json!({"oauth_daily_test_time":"06:15"})).await;
    let before = fs::read(&h.runtime.settings.oauth_path).unwrap();
    sqlx::query("ALTER TABLE accounts RENAME TO fixture_accounts")
        .execute(&h.runtime.db.pool)
        .await
        .unwrap();
    assert!(h.runtime.monitor.inventory(true).await.is_err());
    assert_eq!(fs::read(&h.runtime.settings.oauth_path).unwrap(), before);
    assert!(h.raw().get(S).is_none());
    h.close().await;
}
